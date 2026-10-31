// ABRAXAS -- the color temperature daemon, as both stacks' binaries share
// it: The solar schedule, weather, config, the io_uring daemon loop, its
// sandbox and the CLI. Each binary brings its display server's gamma.

pub mod cli;
pub mod config;
pub mod daemon;
pub mod gamma;
pub mod landlock;
pub mod seccomp;
pub mod sigmoid;
pub mod solar;
pub mod uring;
pub mod weather;
pub mod zipdb;

/// Temperature bounds (Kelvin)
pub const TEMP_MIN: i32 = 1000;
pub const TEMP_MAX: i32 = 25000;

/// Temperature targets
pub const TEMP_DAY_CLEAR: i32 = 6500;
pub const TEMP_DAY_DARK: i32 = 4500;
pub const TEMP_NIGHT: i32 = 2900;

/// Cloud threshold (% cover that triggers dark mode)
pub const CLOUD_THRESHOLD: i32 = 60;

/// Cloud transition: sigmoid duration in minutes
pub const CLOUD_TRANSITION_MIN: f64 = 10.0;

/// Timing
pub const TEMP_UPDATE_SEC: i64 = 60; // 1 minute

/// Transition windows (minutes)
pub const DAWN_DURATION: f64 = 90.0;
pub const DUSK_DURATION: f64 = 180.0;

/// Dawn offset: shift sigmoid midpoint this many minutes after sunrise
pub const DAWN_OFFSET: f64 = 30.0;

/// Dusk offset: shift sigmoid midpoint this many minutes before sunset
pub const DUSK_OFFSET: f64 = 30.0;

/// Sigmoid steepness for transitions
pub const SIGMOID_STEEPNESS: f64 = 8.0;

/// Exit status for a config the daemon cannot run from (sysexits EX_CONFIG).
/// abraxas.service names it in RestartPreventExitStatus, so a bad config
/// stops once instead of restarting forever.
pub const EXIT_CONFIG: i32 = 78;

pub const WEATHER_REFRESH_SEC: i64 = 900; // 15 minutes

pub fn now_epoch() -> i64 {
    unsafe { libc::time(std::ptr::null_mut()) as i64 }
}
