// Session logging. Under `startx` from a login shell, demiurge's stderr is
// the virtual console it was started on, which nobody reads once X owns the
// screen, so every log line is lost. When stderr is a VT, it is moved onto
// journald's stream socket (the protocol systemd-cat speaks), and the lines
// land in `journalctl --user -t demiurge` / `journalctl -t demiurge`. A
// terminal emulator, the systemd unit (already the journal) and /dev/null
// are left alone.

use std::io::{self, Write};
use std::os::fd::{AsRawFd, IntoRawFd};
use std::os::unix::net::UnixStream;

const JOURNAL_STDOUT: &str = "/run/systemd/journal/stdout";
// Linux TTY_MAJOR: /dev/tty0../dev/tty63 are virtual consoles.
const TTY_MAJOR: u32 = 4;
const VT_MINOR_MAX: u32 = 63;

pub fn route_stderr_from_vt(identifier: &str) {
    if !fd_is_vt(2) {
        return;
    }
    if let Ok(stream) = connect(identifier) {
        let fd = stream.into_raw_fd();
        unsafe {
            libc::dup2(fd, 1);
            libc::dup2(fd, 2);
            libc::close(fd);
        }
    }
}

pub fn fd_is_vt(fd: i32) -> bool {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } != 0 {
        return false;
    }
    (st.st_mode & libc::S_IFMT) == libc::S_IFCHR
        && libc::major(st.st_rdev) == TTY_MAJOR
        && libc::minor(st.st_rdev) <= VT_MINOR_MAX
}

// Stream header: identifier, unit id, priority, level_prefix,
// forward_to_syslog, forward_to_kmsg, forward_to_console -- one per line.
pub fn stream_header(identifier: &str) -> String {
    format!("{}\n\n6\n0\n0\n0\n0\n", identifier)
}

pub fn connect(identifier: &str) -> io::Result<UnixStream> {
    let mut stream = UnixStream::connect(JOURNAL_STDOUT)?;
    // journald only reads from this socket.
    unsafe { libc::shutdown(stream.as_raw_fd(), libc::SHUT_RD) };
    stream.write_all(stream_header(identifier).as_bytes())?;
    Ok(stream)
}
