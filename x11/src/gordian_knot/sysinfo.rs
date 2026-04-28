// Read-only accessors for the data rendered into the SYSTEM panel.
// Every field is read at lock time; time and uptime are re-read on the
// 1 Hz tick. No subprocesses, no allocations in the hot path.

use std::ffi::CStr;
use std::mem::MaybeUninit;

// Linux kernel release via uname(2). e.g. "7.0.0-1-cachyos".
pub fn kernel_release() -> String {
    unsafe {
        let mut uts: MaybeUninit<libc::utsname> = MaybeUninit::uninit();
        if libc::uname(uts.as_mut_ptr()) != 0 {
            return String::from("unknown");
        }
        let uts = uts.assume_init();
        CStr::from_ptr(uts.release.as_ptr())
            .to_string_lossy()
            .into_owned()
    }
}

// Short hostname. gethostname(2) into a fixed buffer; HOST_NAME_MAX
// on Linux is 64, we use 256 to be safe.
pub fn hostname() -> String {
    unsafe {
        let mut buf = [0u8; 256];
        if libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) != 0 {
            return String::from("unknown");
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..end]).into_owned()
    }
}

// Formatted current date "MM/DD/YYYY".
pub fn current_date() -> String {
    format_local_time("%m/%d/%Y")
}

// Formatted current time "HH:MM:SS TZ", e.g. "19:42:13 CDT".
pub fn current_time() -> String {
    format_local_time("%H:%M:%S %Z")
}

fn format_local_time(fmt: &str) -> String {
    unsafe {
        let mut t: libc::time_t = 0;
        libc::time(&mut t);
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        let mut buf = [0u8; 128];
        let cfmt = match std::ffi::CString::new(fmt) {
            Ok(s) => s,
            Err(_) => return String::new(),
        };
        let len = libc::strftime(
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
            cfmt.as_ptr(),
            &tm,
        );
        String::from_utf8_lossy(&buf[..len]).into_owned()
    }
}

// System uptime read from /proc/uptime, formatted "0D 3H 46M 12S".
// Two columns of floats in /proc/uptime: total uptime, total idle time.
// We only care about the first.
pub fn uptime_formatted() -> String {
    match std::fs::read_to_string("/proc/uptime") {
        Ok(s) => {
            let secs: f64 = s
                .split_whitespace()
                .next()
                .and_then(|t| t.parse().ok())
                .unwrap_or(0.0);
            format_uptime(secs as u64)
        }
        Err(_) => String::from("?"),
    }
}

// Pure fn: seconds -> "DD H M S". Extracted so tests can pin it without
// touching /proc.
pub fn format_uptime(secs: u64) -> String {
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let mins = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{}D {}H {}M {}S", days, hours, mins, s)
}
