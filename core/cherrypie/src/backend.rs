// A display server cherrypie watches and acts on directly, standalone: The
// X11 binary's backend. The Wayland binary has none; there a compositor asks
// over the API for every window.

use crate::rules::CompiledRule;

// A normal, non-transient, titled, windowed (not fullscreen) window with a
// class that no rule matched: The daemon's candidate for a learned rule.
pub struct UnmatchedWindow {
    pub window: u32,
    pub class: String,
    pub title: String,
    pub role: String,
    pub process: String,
    pub window_type: String,
}

pub trait Backend {
    fn name(&self) -> &str;
    fn connection_fd(&self) -> i32;
    /// Apply the rules to the windows that came and went. With `passive`, an
    /// API client places every window, and the backend only keeps its
    /// bookkeeping.
    fn process_events(&self, rules: &[CompiledRule], dry_run: bool, passive: bool) -> Vec<UnmatchedWindow>;
    fn client_alive(&self, window: u32) -> bool;
    fn apply(&self, window: u32, rule: &CompiledRule);
}
