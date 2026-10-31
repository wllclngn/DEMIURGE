// Local time for both stacks: The bar's clock, screenshot and capture
// names, the lock's panels and GNOSIS's stamps, all strftime of now; and
// the wait until the wall clock's next second, which a seconds clock
// ticks on.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The local time now in strftime `format`; empty for a format strftime
/// cannot fill in 256 bytes. The second is CLOCK_REALTIME's, the clock
/// `until_next_second` waits on: glibc's time() reads the kernel's coarse
/// clock, which a wait ending on the second can still find on the one
/// before.
pub fn now(format: &str) -> String {
    let Ok(format) = std::ffi::CString::new(format) else {
        return String::new();
    };
    let mut buf = [0u8; 256];
    let len = unsafe {
        let t = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        libc::strftime(buf.as_mut_ptr().cast(), buf.len(), format.as_ptr(), &tm)
    };
    String::from_utf8_lossy(&buf[..len]).into_owned()
}

/// How long until the wall clock reaches its next whole second.
pub fn until_next_second() -> Duration {
    let into = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    Duration::from_nanos(1_000_000_000 - into as u64)
}
