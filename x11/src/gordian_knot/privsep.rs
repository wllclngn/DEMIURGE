// Privilege separation primitives.
//
// The binary is setuid-root. Every mode decides its privilege first thing:
//
//   lock holder (X11 / VT)  becomes root fully (real uid 0 too), so the
//                           user's own processes cannot signal it; holds the
//                           grab or the VT and runs PAM in forked children
//   UI child                drops to the user, sets NO_NEW_PRIVS, re-execs,
//                           then sandboxes itself after its first frame
//   daemon, --wl, --login   drop to the user at once and stay there
//
// NO_NEW_PRIVS is never set on a process that runs PAM: It makes the
// kernel ignore unix_chkpwd's setuid bit, and every password then fails.

use std::ffi::CStr;
use std::io;

// The invoking user, captured before any credential change.
#[derive(Clone)]
pub struct Ident {
    pub uid: libc::uid_t,
    pub gid: libc::gid_t,
    pub name: String,
}

pub fn invoking_user() -> Ident {
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };
    let pw = unsafe { libc::getpwuid(uid) };
    let name = if pw.is_null() {
        String::from("user")
    } else {
        unsafe { CStr::from_ptr((*pw).pw_name) }.to_string_lossy().into_owned()
    };
    Ident { uid, gid, name }
}

// Real, effective and saved ids all become the user's. Order matters:
// setresgid before setresuid, which drops the right to change groups.
pub fn drop_to_user(id: &Ident) -> Result<(), String> {
    unsafe {
        if libc::setresgid(id.gid, id.gid, id.gid) != 0 {
            return Err(format!("setresgid: {}", io::Error::last_os_error()));
        }
        if libc::setresuid(id.uid, id.uid, id.uid) != 0 {
            return Err(format!("setresuid: {}", io::Error::last_os_error()));
        }
        if id.uid != 0 && (libc::geteuid() == 0 || libc::getuid() == 0) {
            return Err("privilege drop did not take effect".into());
        }
    }
    Ok(())
}

// Real uid 0 as well as effective: A process whose real uid is the user's
// can be killed by that user, and killing the grab holder unlocks X11.
pub fn become_root() -> Result<(), String> {
    unsafe {
        if libc::geteuid() != 0 {
            return Err("gordian_knot must be installed setuid root (mode 4755)".into());
        }
        if libc::setresgid(0, 0, 0) != 0 || libc::setresuid(0, 0, 0) != 0 {
            return Err(format!("setresuid(0): {}", io::Error::last_os_error()));
        }
    }
    Ok(())
}

// The holder keeps the password in memory: No core dump, no ptrace from the
// user, and the OOM killer leaves it alone (a killed holder is an unlock).
pub fn harden_holder() {
    unsafe {
        libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
        let zero = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        libc::setrlimit(libc::RLIMIT_CORE, &zero);
    }
    let _ = std::fs::write("/proc/self/oom_score_adj", "-1000");
}

pub fn no_new_privs() -> Result<(), String> {
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(format!("PR_SET_NO_NEW_PRIVS: {}", io::Error::last_os_error()));
    }
    Ok(())
}

// Fork; the child runs `run` and _exits with its code.
pub fn fork_child<F>(run: F) -> Result<i32, String>
where
    F: FnOnce() -> i32,
{
    match unsafe { libc::fork() } {
        -1 => Err(format!("fork: {}", io::Error::last_os_error())),
        0 => {
            let code = run();
            unsafe { libc::_exit(code) };
        }
        pid => Ok(pid),
    }
}

// Exit code of a child, or -1 if a signal ended it. EINTR is retried.
pub fn wait_child(pid: i32) -> i32 {
    let mut status: libc::c_int = 0;
    loop {
        if unsafe { libc::waitpid(pid, &mut status, 0) } == -1 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return -1;
        }
        break;
    }
    if libc::WIFEXITED(status) { libc::WEXITSTATUS(status) } else { -1 }
}

// Write one byte to the readiness fd the daemon passes in GK_READY_FD, so
// it knows the lock is up (lock-before-suspend waits on it), then close it.
pub fn signal_ready() {
    let Some(fd) = std::env::var("GK_READY_FD").ok().and_then(|v| v.parse::<i32>().ok()) else {
        return;
    };
    unsafe {
        libc::write(fd, b"1".as_ptr() as *const libc::c_void, 1);
        libc::close(fd);
    }
}
