// The VT lock's recovery path. VT_LOCKSWITCH is kernel-global and outlives
// the process that set it: A holder that dies with it set leaves the machine
// on a console nobody can leave. Before it locks switching, the holder forks
// a warden (root, in a session of its own, deaf to hangups and terminal
// signals) holding the read end of a pipe. The holder tells it which VT it
// took and, once everything is released after an unlock, that it is done.
// If the pipe closes without that, the holder died locked: The warden gives
// the console back (text mode, switching unlocked, the previous VT active,
// the taken VT freed), then drops to the user and re-execs the lock in its
// `relock` mode, so the session is locked again rather than left open.

use std::ffi::{CStr, CString};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use crate::privsep::Ident;

const KDSETMODE: libc::c_ulong = 0x4B3A;
const KD_TEXT: libc::c_int = 0;
const VT_ACTIVATE: libc::c_ulong = 0x5606;
const VT_WAITACTIVE: libc::c_ulong = 0x5607;
const VT_DISALLOCATE: libc::c_ulong = 0x5608;
const VT_UNLOCKSWITCH: libc::c_ulong = 0x560C;
// The signals a hangup or a terminal sends.
const DEAF_TO: [libc::c_int; 6] = [libc::SIGHUP, libc::SIGINT, libc::SIGTERM, libc::SIGQUIT, libc::SIGTSTP, libc::SIGPIPE];

pub struct Warden {
    pipe: OwnedFd,
}

impl Warden {
    pub fn start(ident: &Ident, relock: &CStr) -> Result<Warden, String> {
        // Everything the warden needs is built before fork.
        let exe = std::fs::read_link("/proc/self/exe").map_err(|e| format!("/proc/self/exe: {}", e))?;
        let exe = CString::new(exe.as_os_str().as_encoded_bytes()).map_err(|e| e.to_string())?;
        let argv = [c"gordian_knot".as_ptr(), relock.as_ptr(), std::ptr::null()];
        let mut fds = [0i32; 2];
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return Err(format!("pipe: {}", std::io::Error::last_os_error()));
        }
        let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        match unsafe { libc::fork() } {
            -1 => Err(format!("fork: {}", std::io::Error::last_os_error())),
            0 => {
                drop(write);
                unsafe { watch(read.as_raw_fd(), ident, &exe, &argv) }
            }
            _ => Ok(Warden { pipe: write }),
        }
    }

    // The VT the holder took, and the one to return to.
    pub fn took(&self, tty: i32, prev: i32) {
        let mut msg = [0u8; 9];
        msg[0] = b'T';
        msg[1..5].copy_from_slice(&tty.to_le_bytes());
        msg[5..9].copy_from_slice(&prev.to_le_bytes());
        unsafe { libc::write(self.pipe.as_raw_fd(), msg.as_ptr() as *const libc::c_void, msg.len()) };
    }

    // Everything is released; the warden can go.
    pub fn done(self) {
        unsafe { libc::write(self.pipe.as_raw_fd(), b"D".as_ptr() as *const libc::c_void, 1) };
    }
}

// The warden's whole life, after fork.
unsafe fn watch(pipe: i32, ident: &Ident, exe: &CStr, argv: &[*const libc::c_char; 3]) -> ! {
    unsafe {
        libc::setsid();
        for sig in DEAF_TO {
            libc::signal(sig, libc::SIG_IGN);
        }
        let (mut tty, mut prev) = (-1i32, -1i32);
        let mut msg = [0u8; 9];
        loop {
            let n = libc::read(pipe, msg.as_mut_ptr() as *mut libc::c_void, 1);
            if n == 1 && msg[0] == b'D' {
                libc::_exit(0);
            }
            if n == 1 && msg[0] == b'T' && read_exact(pipe, &mut msg[1..]) {
                tty = i32::from_le_bytes([msg[1], msg[2], msg[3], msg[4]]);
                prev = i32::from_le_bytes([msg[5], msg[6], msg[7], msg[8]]);
                continue;
            }
            if n < 0 && *libc::__errno_location() == libc::EINTR {
                continue;
            }
            break;
        }

        // The holder is gone without saying it was done.
        let say = b"[gordian_knot] warden: the lock holder died; console released, locking again\n";
        libc::write(2, say.as_ptr() as *const libc::c_void, say.len());
        if tty > 0 {
            let path = CString::new(format!("/dev/tty{}", tty)).unwrap();
            let fd = libc::open(path.as_ptr(), libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC);
            if fd >= 0 {
                libc::ioctl(fd, KDSETMODE, KD_TEXT);
                libc::close(fd);
            }
        }
        let console = libc::open(c"/dev/tty0".as_ptr(), libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC);
        if console >= 0 {
            libc::ioctl(console, VT_UNLOCKSWITCH, 1);
            if prev > 0 {
                libc::ioctl(console, VT_ACTIVATE, prev);
                libc::ioctl(console, VT_WAITACTIVE, prev);
            }
            if tty > 0 {
                libc::ioctl(console, VT_DISALLOCATE, tty);
            }
            libc::close(console);
        }

        // Lock again as the user, through the setuid binary, as any lock
        // starts. The dead holder's readiness fd means nothing now, and the
        // signals the warden ignored are the new lock's again: An ignored
        // disposition survives exec, and a renderer deaf to its stop signal
        // would hang the unlock.
        libc::unsetenv(c"GK_READY_FD".as_ptr());
        for sig in DEAF_TO {
            libc::signal(sig, libc::SIG_DFL);
        }
        if libc::setresgid(ident.gid, ident.gid, ident.gid) != 0 || libc::setresuid(ident.uid, ident.uid, ident.uid) != 0 {
            libc::_exit(1);
        }
        libc::execv(exe.as_ptr(), argv.as_ptr());
        libc::_exit(127);
    }
}

fn read_exact(fd: i32, buf: &mut [u8]) -> bool {
    let mut off = 0;
    while off < buf.len() {
        let n = unsafe { libc::read(fd, buf[off..].as_mut_ptr() as *mut libc::c_void, buf.len() - off) };
        if n <= 0 {
            return false;
        }
        off += n as usize;
    }
    true
}
