pub mod api;
pub mod backend;
pub mod cli;
pub mod client;
pub mod config;
pub mod daemon;
pub mod learn;
pub mod memory;
pub mod place;
pub mod rules;
pub mod server;

pub fn local_time() -> String {
    unsafe {
        let mut t: libc::time_t = 0;
        libc::time(&mut t);
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
    }
}
