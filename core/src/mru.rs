// Cyclical most-recently-used ring, generic over the window-id type so
// it works for both X11 (W = x11rb's Window, a u32 alias) and Wayland
// (W = whatever id we mint for managed surfaces).
//
// Each ring holds focusable windows in most-recently-used order with
// position 0 = current focus; each window appears exactly once.
//
// Cycling: while CycleState is held by the WM, the keyboard is grabbed
// (X11) or the keymap is in cycle mode (Wayland) and `index` points at
// the currently-raised cycle target within `candidates`. The ring is
// NOT reordered during cycling; the selection is committed on modifier
// release by promoting candidates[index] via on_focus.
//
// Two scopes:
//   CycleScope::Tag -- candidates restricted to a single tag (the
//                      focused monitor's active tag). Drives Alt+Tab.
//   CycleScope::All -- candidates = full ring. Drives Super+Tab
//                      (cross-tag cycling; tag switch happens as
//                      needed in the per-protocol focus_target).

use std::ops::Deref;

#[derive(Debug, Clone, Copy)]
pub enum CycleScope {
    Tag,
    All,
}

#[derive(Debug)]
pub struct CycleState<W> {
    pub candidates: Vec<W>,
    pub index: usize,
}

impl<W: Copy + Eq> CycleState<W> {
    /// Advance the cycle index, wrapping in the requested direction.
    /// No-op for candidate lists shorter than 2.
    pub fn advance(&mut self, forward: bool) {
        let len = self.candidates.len();
        if len < 2 {
            return;
        }
        self.index = if forward {
            (self.index + 1) % len
        } else {
            (self.index + len - 1) % len
        };
    }

    pub fn current(&self) -> Option<W> {
        self.candidates.get(self.index).copied()
    }
}

#[derive(Debug, Clone)]
pub struct MruRing<W> {
    items: Vec<W>,
}

impl<W> Default for MruRing<W> {
    fn default() -> Self {
        Self { items: Vec::new() }
    }
}

impl<W: Copy + Eq> MruRing<W> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Promote `w` to the front of the ring. Existing occurrences are
    /// removed so each window appears exactly once.
    pub fn on_focus(&mut self, w: W) {
        self.items.retain(|&x| x != w);
        self.items.insert(0, w);
    }

    /// Window destroyed: remove from ring.
    pub fn on_unmanage(&mut self, gone: W) {
        self.items.retain(|&w| w != gone);
    }

    pub fn as_slice(&self) -> &[W] {
        &self.items
    }
}

// Deref<Target = [W]> lets callers use slice operations (iter, copied,
// len, is_empty) without naming a specific accessor. Mirrors how the
// pre-extraction code did `wm.mru.iter()` directly on Vec<Window>.
impl<W> Deref for MruRing<W> {
    type Target = [W];
    fn deref(&self) -> &[W] {
        &self.items
    }
}
