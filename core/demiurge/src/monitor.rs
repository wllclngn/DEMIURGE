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

impl Monitor {
    // The part of the monitor below a bar `bar` pixels tall.
    pub fn work_area(&self, bar: u32) -> (i32, i32, u32, u32) {
        (self.x, self.y + bar as i32, self.width, self.height.saturating_sub(bar))
    }
}

/// Show `tag` on monitor `monitor`. A tag shows on one monitor at most, so
/// a monitor already showing it takes `monitor`'s tag in exchange. False
/// when `monitor` already shows it.
pub fn view(active_tags: &mut [usize], monitor: usize, tag: usize) -> bool {
    let Some(&old) = active_tags.get(monitor) else {
        return false;
    };
    if old == tag {
        return false;
    }
    if let Some(other) = active_tags.iter().position(|&t| t == tag) {
        active_tags[other] = old;
    }
    active_tags[monitor] = tag;
    true
}

/// A monitor mode's timing, as `desktop_mode` reads it: Its resolution, its
/// dot clock (in one unit across a monitor's modes), its horizontal and
/// vertical totals, whether it is progressive (neither interlaced nor
/// double-scanned), and whether the monitor's EDID prefers it.
pub struct Timing {
    pub size: (u16, u16),
    pub clock: u32,
    pub htotal: u16,
    pub vtotal: u16,
    pub progressive: bool,
    pub preferred: bool,
}

/// The mode a monitor's desktop runs in, as an index into its modes: Its
/// first preferred mode's resolution at the highest refresh rate it offers,
/// progressive modes only, so a 240 Hz panel whose EDID prefers 60 Hz runs
/// at 240. None for a monitor that prefers no mode.
pub fn desktop_mode(modes: &[Timing]) -> Option<usize> {
    let p = modes.iter().position(|m| m.preferred)?;
    sized_mode(modes, modes[p].size).or(Some(p))
}

/// The mode of `size` at the highest refresh rate offered, as an index into
/// the modes, progressive modes only; None where none is that size.
pub fn sized_mode(modes: &[Timing], size: (u16, u16)) -> Option<usize> {
    let rate = |m: &Timing| {
        let frame = m.htotal as f64 * m.vtotal as f64;
        if frame > 0.0 { m.clock as f64 / frame } else { 0.0 }
    };
    (0..modes.len())
        .filter(|&i| modes[i].size == size && modes[i].progressive)
        .max_by(|&a, &b| rate(&modes[a]).total_cmp(&rate(&modes[b])))
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
