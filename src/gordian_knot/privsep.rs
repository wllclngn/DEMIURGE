// Privilege separation primitives.
//
// The VT locker runs setuid-root (it has to: VT_LOCKSWITCH is root-only).
// But the PAM conversation and the input-reading UI should NOT run as root.
// So the parent does VT ioctls, then forks a child that drops to the real
// UID via setresgid/setresuid/PR_SET_NO_NEW_PRIVS before touching PAM.
//
// PAM's shadow access is handled by pam_unix's setuid helper unix_chkpwd --
// we don't need to be root for pam_authenticate to work. This is how every
// non-root locker (i3lock, swaylock) handles PAM.
//
// The child runs as the invoking user. Inherited fds (the TTY, the X11
// socket) survive the privilege drop unchanged, so the child can still
// read/write them. The parent waits on the child and handles VT cleanup
// via the child's exit status.

use std::io;

// Drop from root to the invoking user's real UID/GID. Must be called only
// in a fresh child process (after fork). Order matters: setresgid before
// setresuid, because setresuid drops the capability to change groups.
//
// Also sets PR_SET_NO_NEW_PRIVS so the child can't regain privileges via a
// subsequent setuid binary exec. (unix_chkpwd still works because PAM does
// the setuid handoff at the kernel level, not through our exec.)
pub fn drop_to_real_user() -> Result<(), String> {
    unsafe {
        let uid = libc::getuid();
        let gid = libc::getgid();

        if libc::setresgid(gid, gid, gid) != 0 {
            return Err(format!("setresgid: {}", io::Error::last_os_error()));
        }
        if libc::setresuid(uid, uid, uid) != 0 {
            return Err(format!("setresuid: {}", io::Error::last_os_error()));
        }
        // Verify the drop actually happened. Guards against kernel bugs or
        // edge cases where setresuid silently fails -- shouldn't happen on
        // Linux but belt-and-braces for a security-critical path.
        if libc::geteuid() == 0 && uid != 0 {
            return Err("setresuid did not take effect (still euid 0)".into());
        }

        // Prevent regaining privs via setuid exec in the child's subtree.
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            return Err(format!("PR_SET_NO_NEW_PRIVS: {}", io::Error::last_os_error()));
        }
    }
    Ok(())
}

// Fork the process. The child invokes `run` (as the same user, root still)
// and exits with whatever code `run` returns. The parent gets the child's
// pid back. Callers of this function typically want to call
// drop_to_real_user() first thing inside `run`.
//
// The CAUGHT-THEN-REPLACED return type sidesteps FnOnce-in-unsafe issues:
// we hand the closure to the child via a Box, run it, exit. No return to
// the caller on the child side.
pub fn fork_child<F>(run: F) -> Result<i32, String>
where
    F: FnOnce() -> i32,
{
    let pid = unsafe { libc::fork() };
    match pid {
        -1 => Err(format!("fork: {}", io::Error::last_os_error())),
        0 => {
            // Child. Run the closure and exit. We _exit to avoid running
            // atexit handlers or global destructors that belong to the
            // parent.
            let code = run();
            unsafe { libc::_exit(code) };
        }
        _ => Ok(pid),
    }
}

// Wait for a child to exit. Returns the exit code (0-255) or -1 on signal.
// Ignores EINTR so signal storms don't drop us out of the wait.
pub fn wait_child(pid: i32) -> i32 {
    unsafe {
        let mut status: libc::c_int = 0;
        loop {
            let w = libc::waitpid(pid, &mut status, 0);
            if w == -1 {
                if *libc::__errno_location() == libc::EINTR {
                    continue;
                }
                return -1;
            }
            break;
        }
        if libc::WIFEXITED(status) {
            libc::WEXITSTATUS(status)
        } else {
            -1
        }
    }
}
