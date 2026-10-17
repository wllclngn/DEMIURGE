// Inline FFI to libpam + a thin safe wrapper. No pam-sys crate -- the PAM C
// ABI has been stable for 25+ years and the surface we need is five calls.
//
// Design: the conversation callback answers prompts with a password that
// was pre-collected by our UI, not an interactive prompt on pam's side.
// Info and error messages (pam_faillock's lockout notice among them) are
// kept and returned, never acknowledged into the void.
//
// authenticate_forked runs the whole PAM transaction in a short-lived
// child, so a crash inside a PAM module costs one attempt, not the
// process holding the lock.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::io::{Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::ptr;

use crate::gordian_knot::secret::Secret;

// PAM return codes (subset -- full list in <security/_pam_types.h>).
const PAM_SUCCESS: c_int = 0;
const PAM_AUTH_ERR: c_int = 7;
const PAM_CRED_INSUFFICIENT: c_int = 11;
const PAM_AUTHINFO_UNAVAIL: c_int = 12;
const PAM_USER_UNKNOWN: c_int = 13;
const PAM_MAXTRIES: c_int = 14;
const PAM_ABORT: c_int = 26;

// PAM message styles.
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_PROMPT_ECHO_ON: c_int = 2;
const PAM_ERROR_MSG: c_int = 3;
const PAM_TEXT_INFO: c_int = 4;

// pam_setcred flags -- used to refresh kerberos-style credentials on success.
const PAM_REFRESH_CRED: c_int = 0x10;

#[repr(C)]
struct PamMessage {
    msg_style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct PamResponse {
    resp: *mut c_char,
    resp_retcode: c_int,
}

#[repr(C)]
struct PamConv {
    conv: Option<
        unsafe extern "C" fn(
            num_msg: c_int,
            msg: *const *const PamMessage,
            resp: *mut *mut PamResponse,
            appdata_ptr: *mut c_void,
        ) -> c_int,
    >,
    appdata_ptr: *mut c_void,
}

// Opaque. pam_start allocates, pam_end frees.
enum PamHandle {}

#[link(name = "pam")]
unsafe extern "C" {
    fn pam_start(
        service_name: *const c_char,
        user: *const c_char,
        conv: *const PamConv,
        pamh: *mut *mut PamHandle,
    ) -> c_int;
    fn pam_authenticate(pamh: *mut PamHandle, flags: c_int) -> c_int;
    fn pam_setcred(pamh: *mut PamHandle, flags: c_int) -> c_int;
    fn pam_end(pamh: *mut PamHandle, pam_status: c_int) -> c_int;
    fn pam_strerror(pamh: *mut PamHandle, errnum: c_int) -> *const c_char;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    WrongCredentials,
    UserUnknown,
    MaxTries,
    AuthInfoUnavailable,
    InternalAbort,
    Other(String),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::WrongCredentials => f.write_str("Authentication failed"),
            AuthError::UserUnknown => f.write_str("Unknown user"),
            AuthError::MaxTries => f.write_str("Too many failed attempts"),
            AuthError::AuthInfoUnavailable => f.write_str("Authentication service unavailable"),
            AuthError::InternalAbort => f.write_str("Authentication aborted"),
            AuthError::Other(s) => f.write_str(s),
        }
    }
}

// The outcome of one attempt plus every info / error message PAM sent.
pub struct Attempt {
    pub result: Result<(), AuthError>,
    pub messages: Vec<String>,
}

// Context passed to the conversation callback via appdata_ptr.
struct ConvContext {
    password: *const c_char,
    messages: Vec<String>,
}

// PAM expects responses allocated with libc malloc; it frees (and wipes)
// them itself.
unsafe extern "C" fn conversation(
    num_msg: c_int,
    msg: *const *const PamMessage,
    resp: *mut *mut PamResponse,
    appdata_ptr: *mut c_void,
) -> c_int {
    if num_msg <= 0 || msg.is_null() || resp.is_null() || appdata_ptr.is_null() {
        return PAM_AUTH_ERR;
    }
    let n = num_msg as usize;
    let ctx = unsafe { &mut *(appdata_ptr as *mut ConvContext) };

    let ptr = unsafe { libc::calloc(n, std::mem::size_of::<PamResponse>()) as *mut PamResponse };
    if ptr.is_null() {
        return PAM_ABORT;
    }
    unsafe { *resp = ptr };

    for i in 0..n {
        let m = unsafe { *msg.add(i) };
        if m.is_null() {
            continue;
        }
        let style = unsafe { (*m).msg_style };
        let answer: *mut c_char = match style {
            PAM_PROMPT_ECHO_OFF | PAM_PROMPT_ECHO_ON => unsafe { libc::strdup(ctx.password) },
            _ => {
                if (style == PAM_ERROR_MSG || style == PAM_TEXT_INFO) && unsafe { !(*m).msg.is_null() } {
                    let text = unsafe { CStr::from_ptr((*m).msg) }.to_string_lossy();
                    let text = text.trim();
                    if !text.is_empty() {
                        ctx.messages.push(text.to_string());
                    }
                }
                unsafe { libc::strdup(c"".as_ptr()) }
            }
        };
        if answer.is_null() {
            return PAM_ABORT;
        }
        unsafe {
            (*ptr.add(i)).resp = answer;
            (*ptr.add(i)).resp_retcode = 0;
        }
    }
    PAM_SUCCESS
}

// Authenticate `user` against the PAM service "gordian_knot"
// (/etc/pam.d/gordian_knot), reading the password in place from `secret`.
// pam_start -> pam_authenticate -> pam_setcred(REFRESH) -> pam_end, the
// sequence physlock and xsecurelock drive.
pub fn authenticate(user: &str, secret: &Secret) -> Attempt {
    let mut messages = Vec::new();
    let result = (|| {
        let service = CString::new("gordian_knot").map_err(|e| AuthError::Other(e.to_string()))?;
        let c_user = CString::new(user).map_err(|e| AuthError::Other(e.to_string()))?;
        let mut ctx = ConvContext { password: secret.as_c_ptr(), messages: Vec::new() };
        let conv = PamConv {
            conv: Some(conversation),
            appdata_ptr: &mut ctx as *mut ConvContext as *mut c_void,
        };

        let mut pamh: *mut PamHandle = ptr::null_mut();
        let rc = unsafe { pam_start(service.as_ptr(), c_user.as_ptr(), &conv, &mut pamh) };
        if rc != PAM_SUCCESS {
            return Err(map_rc(rc, ptr::null_mut()));
        }
        let auth_rc = unsafe { pam_authenticate(pamh, 0) };
        if auth_rc == PAM_SUCCESS {
            unsafe { pam_setcred(pamh, PAM_REFRESH_CRED) };
        }
        // Map the error while pamh is valid: pam_strerror after pam_end is a
        // use-after-free.
        let auth_err = (auth_rc != PAM_SUCCESS).then(|| map_rc(auth_rc, pamh));
        let end_rc = unsafe { pam_end(pamh, auth_rc) };
        messages = std::mem::take(&mut ctx.messages);
        if let Some(e) = auth_err {
            return Err(e);
        }
        if end_rc != PAM_SUCCESS {
            return Err(AuthError::Other(format!("pam_end failed ({})", end_rc)));
        }
        Ok(())
    })();
    Attempt { result, messages }
}

// Run authenticate in a forked child that reports over a pipe and exits.
// The child wipes its copy of the password before exiting. A child that
// dies without reporting is an attempt that failed, not a lock released.
pub fn authenticate_forked(user: &str, secret: &mut Secret) -> Attempt {
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return failed("pipe for the authentication helper failed");
    }
    let (rd, wr) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    match unsafe { libc::fork() } {
        -1 => failed("fork for the authentication helper failed"),
        0 => {
            drop(rd);
            let attempt = authenticate(user, secret);
            secret.clear();
            let mut out = std::fs::File::from(wr);
            let _ = out.write_all(encode(&attempt).as_bytes());
            drop(out);
            unsafe { libc::_exit(0) };
        }
        pid => {
            drop(wr);
            let mut text = String::new();
            let _ = std::fs::File::from(rd).read_to_string(&mut text);
            let mut status = 0;
            unsafe { libc::waitpid(pid, &mut status, 0) };
            decode(&text).unwrap_or_else(|| failed("the authentication helper died"))
        }
    }
}

fn failed(msg: &str) -> Attempt {
    Attempt { result: Err(AuthError::Other(msg.to_string())), messages: Vec::new() }
}

// Wire format: one "M\t<message>" line per message, then "R\t<code>\t<detail>".
pub fn encode(a: &Attempt) -> String {
    let mut s = String::new();
    for m in &a.messages {
        s.push_str("M\t");
        s.push_str(&m.replace(['\n', '\t'], " "));
        s.push('\n');
    }
    let (code, detail) = match &a.result {
        Ok(()) => (0, String::new()),
        Err(AuthError::WrongCredentials) => (1, String::new()),
        Err(AuthError::UserUnknown) => (2, String::new()),
        Err(AuthError::MaxTries) => (3, String::new()),
        Err(AuthError::AuthInfoUnavailable) => (4, String::new()),
        Err(AuthError::InternalAbort) => (5, String::new()),
        Err(AuthError::Other(d)) => (6, d.replace(['\n', '\t'], " ")),
    };
    s.push_str(&format!("R\t{}\t{}\n", code, detail));
    s
}

pub fn decode(text: &str) -> Option<Attempt> {
    let mut messages = Vec::new();
    for line in text.lines() {
        let mut f = line.splitn(3, '\t');
        match f.next()? {
            "M" => messages.push(f.next().unwrap_or("").to_string()),
            "R" => {
                let result = match f.next()?.parse::<u8>().ok()? {
                    0 => Ok(()),
                    1 => Err(AuthError::WrongCredentials),
                    2 => Err(AuthError::UserUnknown),
                    3 => Err(AuthError::MaxTries),
                    4 => Err(AuthError::AuthInfoUnavailable),
                    5 => Err(AuthError::InternalAbort),
                    _ => Err(AuthError::Other(f.next().unwrap_or("").to_string())),
                };
                return Some(Attempt { result, messages });
            }
            _ => return None,
        }
    }
    None
}

fn map_rc(rc: c_int, pamh: *mut PamHandle) -> AuthError {
    match rc {
        PAM_AUTH_ERR | PAM_CRED_INSUFFICIENT => AuthError::WrongCredentials,
        PAM_USER_UNKNOWN => AuthError::UserUnknown,
        PAM_MAXTRIES => AuthError::MaxTries,
        PAM_AUTHINFO_UNAVAIL => AuthError::AuthInfoUnavailable,
        PAM_ABORT => AuthError::InternalAbort,
        _ => {
            if !pamh.is_null() {
                let msg = unsafe { pam_strerror(pamh, rc) };
                if !msg.is_null() {
                    let s = unsafe { CStr::from_ptr(msg) }.to_string_lossy().into_owned();
                    return AuthError::Other(s);
                }
            }
            AuthError::Other(format!("PAM error ({})", rc))
        }
    }
}
