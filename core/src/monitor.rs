// Monitor geometry: a name + work-area rect. Server-agnostic shape;
// queried from RandR on X11 and from wl_output on Wayland. Each
// implementation populates this struct in its own monitor module
// alongside protocol-specific subscription / query code.
//
// The Monitors newtype enforces "at least one element" at the type
// level. Both implementations rely on this -- arrange() and
// client_monitor() index .first() infallibly.

use std::ops::Deref;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Monitor {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

// Non-empty wrapper around Vec<Monitor>. Construction is gated through
// try_new / single, so the `at least one element` invariant is enforced
// at the type level rather than an implicit assumption protected by
// monitor query's fallback path. Lets first() return &Monitor instead
// of Option<&Monitor>; eliminates the bare self.monitors[0] indexes
// that previously panicked on a degenerate display config.
//
// Deref<Target = [Monitor]> means &Monitors autoderefs to &[Monitor] in
// argument position, so callers that accept &[Monitor] (set_workarea,
// build_panels, etc.) need no signature change. Inherent first() shadows
// slice::first via method resolution, returning the infallible variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Monitors(Vec<Monitor>);

impl Monitors {
    pub fn try_new(v: Vec<Monitor>) -> Option<Self> {
        if v.is_empty() { None } else { Some(Self(v)) }
    }

    pub fn single(m: Monitor) -> Self {
        Self(vec![m])
    }

    pub fn first(&self) -> &Monitor {
        // Invariant: Monitors holds at least one element. try_new and
        // single are the only constructors; both reject / preclude empty.
        &self.0[0]
    }
}

impl Deref for Monitors {
    type Target = [Monitor];
    fn deref(&self) -> &[Monitor] {
        &self.0
    }
}
