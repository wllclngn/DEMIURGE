// The configuration layer, shared with tests/: The config module and the
// timing values it reads from the crate root.

pub mod config;

pub const WEATHER_REFRESH_SEC: i64 = 900; // 15 minutes

pub fn now_epoch() -> i64 {
    unsafe { libc::time(std::ptr::null_mut()) as i64 }
}
