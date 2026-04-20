// Inline FFI to libpam + a thin safe wrapper. No pam-sys crate -- the PAM C
// ABI has been stable for 25+ years and the surface we need is five calls.
//
// Design: the conversation callback returns a password that was pre-collected
// by our UI (VT or X11), not an interactive prompt on pam's side. This
// matches how physlock, i3lock, xsecurelock, and swaylock all drive PAM.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ptr;

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

#[derive(Debug, Clone)]
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

// Context passed to the conversation callback via appdata_ptr. Holds the
// pre-collected password. Lifetime: valid only during the pam_authenticate
// call; we ensure the box survives until pam_end.
struct ConvContext {
    password: CString,
}

// PAM expects responses allocated with libc malloc. We hand-roll the layout
// rather than pulling in a helper crate.
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
    let ctx = unsafe { &*(appdata_ptr as *const ConvContext) };

    // Allocate the response array via libc::calloc so pam_end can free it.
    let size = n * std::mem::size_of::<PamResponse>();
    let ptr = unsafe { libc::calloc(1, size) as *mut PamResponse };
    if ptr.is_null() {
        return PAM_ABORT;
    }
    unsafe {
        *resp = ptr;
    }

    for i in 0..n {
        let m = unsafe { *msg.add(i) };
        if m.is_null() {
            continue;
        }
        let style = unsafe { (*m).msg_style };
        let answer: *mut c_char = match style {
            PAM_PROMPT_ECHO_OFF | PAM_PROMPT_ECHO_ON => {
                // Hand the password over. PAM takes ownership of the malloc.
                let bytes = ctx.password.as_bytes_with_nul();
                let p = unsafe { libc::malloc(bytes.len()) as *mut c_char };
                if p.is_null() {
                    return PAM_ABORT;
                }
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        bytes.as_ptr() as *const c_char,
                        p,
                        bytes.len(),
                    );
                }
                p
            }
            // Info/error messages: acknowledge with an empty malloc'd string.
            _ => {
                let p = unsafe { libc::malloc(1) as *mut c_char };
                if p.is_null() {
                    return PAM_ABORT;
                }
                unsafe {
                    *p = 0;
                }
                p
            }
        };
        unsafe {
            let entry = ptr.add(i);
            (*entry).resp = answer;
            (*entry).resp_retcode = 0;
        }
    }

    PAM_SUCCESS
}

// Authenticate `user` with `password` against the PAM service named
// "gordian_knot" (looks up /etc/pam.d/gordian_knot at runtime).
//
// Returns Ok(()) on success, Err(AuthError) otherwise. Internally uses
// pam_start -> pam_authenticate -> pam_setcred(REFRESH) -> pam_end, which
// is the same sequence physlock and xsecurelock drive.
pub fn authenticate(user: &str, password: &str) -> Result<(), AuthError> {
    let service = CString::new("gordian_knot").map_err(|e| AuthError::Other(e.to_string()))?;
    let c_user = CString::new(user).map_err(|e| AuthError::Other(e.to_string()))?;
    let password = CString::new(password).map_err(|e| AuthError::Other(e.to_string()))?;

    let ctx = Box::new(ConvContext { password });
    let ctx_ptr = Box::into_raw(ctx);

    let conv = PamConv {
        conv: Some(conversation),
        appdata_ptr: ctx_ptr as *mut c_void,
    };

    let mut pamh: *mut PamHandle = ptr::null_mut();
    let rc = unsafe { pam_start(service.as_ptr(), c_user.as_ptr(), &conv, &mut pamh) };
    if rc != PAM_SUCCESS {
        let _ = unsafe { Box::from_raw(ctx_ptr) };
        return Err(map_rc(rc, ptr::null_mut()));
    }

    let auth_rc = unsafe { pam_authenticate(pamh, 0) };
    if auth_rc == PAM_SUCCESS {
        // Refresh kerberos/gssapi creds if the stack has them. Ignored
        // otherwise. Matches physlock behavior.
        unsafe { pam_setcred(pamh, PAM_REFRESH_CRED) };
    }

    let end_rc = unsafe { pam_end(pamh, auth_rc) };
    // Take the box back so the CString (password) is zeroed/dropped.
    let _ = unsafe { Box::from_raw(ctx_ptr) };

    if auth_rc != PAM_SUCCESS {
        return Err(map_rc(auth_rc, pamh));
    }
    if end_rc != PAM_SUCCESS {
        return Err(AuthError::Other(format!("pam_end failed ({})", end_rc)));
    }
    Ok(())
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
