// X11 MRU cycle wiring. The MruRing data structure and CycleState
// pure-data live in demiurge_core::mru and are re-exported here.
// What stays X11-specific: keyboard grab/ungrab, the cross-tag focus
// hop (which goes through wm.view_tag_no_focus), and the modifier-
// release detection via X11 KeyReleaseEvent.

use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;

use crate::wm::Wm;

pub use demiurge_core::mru::{CycleScope, CycleState, MruRing};

// Type alias: wm.mru / wm.mru_cycle store ring + state typed against
// x11rb's Window. Wayland's mru module will alias against its own
// surface-id type while reusing the same generic core impl.
pub type WindowRing = MruRing<Window>;
pub type WindowCycle = CycleState<Window>;

// Promote a window to the front of the ring. Thin pass-through to
// the core ring so existing call sites (`mru::on_focus(self, w)`)
// keep compiling without touching wm.rs.
pub fn on_focus(wm: &mut Wm, window: Window) {
    wm.mru.on_focus(window);
}

pub fn on_unmanage(wm: &mut Wm, gone: Window) {
    wm.mru.on_unmanage(gone);
}

// Cycle press. `forward` advances toward older windows; backward walks
// toward newer. Wraps both ways. `scope` chooses Tag-only or all windows.
pub fn start_or_advance(wm: &mut Wm, forward: bool, scope: CycleScope) {
    let (candidates, index) = match wm.mru_cycle.take() {
        Some(mut cur) => {
            if cur.candidates.len() < 2 {
                wm.mru_cycle = Some(cur);
                return;
            }
            cur.advance(forward);
            (cur.candidates, cur.index)
        }
        None => {
            // First press: compute candidates under the requested scope.
            let candidates: Vec<Window> = match scope {
                CycleScope::All => wm.mru.as_slice().to_vec(),
                CycleScope::Tag => {
                    let active = wm.active_tag();
                    wm.mru
                        .iter()
                        .copied()
                        .filter(|&w| {
                            wm.clients
                                .iter()
                                .any(|c| c.window == w && c.tag == active)
                        })
                        .collect()
                }
            };
            if candidates.len() < 2 {
                return;
            }

            // Grab the keyboard so we see the modifier release.
            let result = wm.conn.grab_keyboard(
                true,
                wm.root,
                x11rb::CURRENT_TIME,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            );
            let ok = result
                .ok()
                .and_then(|c| c.reply().ok())
                .is_some_and(|r| r.status == GrabStatus::SUCCESS);
            if !ok {
                return;
            }
            // candidates[0] is the current focus (it's the most-recent
            // entry in the mru ring, post-filter). First forward step
            // lands on [1]; first backward step lands on the tail.
            let next_index = if forward { 1 } else { candidates.len() - 1 };
            (candidates, next_index)
        }
    };

    let target = candidates[index];
    wm.mru_cycle = Some(CycleState { candidates, index });
    focus_target(wm, target);
}

// Switch tags if needed, then focus the target. focus_window suppresses
// its on_focus promotion because mru_cycle is Some.
fn focus_target(wm: &mut Wm, target: Window) {
    let target_tag = wm.clients.iter().find(|c| c.window == target).map(|c| c.tag);
    // If the target lives on a tag that no monitor is currently showing,
    // bring it onto the focused monitor (view_tag_no_focus handles the
    // swap-on-collision and visibility transitions).
    if let Some(t) = target_tag
        && !wm.tag_visible(t)
    {
        wm.view_tag_no_focus(t);
    }
    wm.focus_window(Some(target));
}

pub fn finish_cycle(wm: &mut Wm) {
    let _ = wm.conn.ungrab_keyboard(x11rb::CURRENT_TIME);
    let _ = wm.conn.flush();
    // Commit: promote the cycle target to the front of the ring.
    if let Some(cycle) = wm.mru_cycle.take() {
        if let Some(target) = cycle.current() {
            wm.mru.on_focus(target);
        }
    }
}

pub fn on_key_release(wm: &mut Wm, ev: &KeyReleaseEvent) {
    if wm.mru_cycle.is_some() && wm.alt_keycodes.contains(&ev.detail) {
        finish_cycle(wm);
    }
}
