use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;

use crate::wm::Wm;

// Cyclical MRU ring. `wm.mru` holds all focusable windows in most-recently-
// used order, with mru[0] = current focus. Each window appears exactly once.
//
// Cycling: while `wm.mru_cycle` is Some, the keyboard is grabbed and
// `index` points at the currently-raised cycle target within `candidates`.
// The ring is NOT reordered during cycling; the selection is committed on
// modifier release by moving candidates[index] to the front of wm.mru via
// the normal on_focus path.
//
// Two scopes:
//   CycleScope::Tag  -- candidates restricted to the active tag. Drives
//                       Alt+Tab / Alt+` (per-tag window cycling).
//   CycleScope::All  -- candidates = full mru ring. Drives Super+Tab
//                       (cross-tag cycling; tag switch happens as needed).
#[derive(Debug, Clone, Copy)]
pub enum CycleScope {
    Tag,
    All,
}

#[derive(Debug)]
pub struct CycleState {
    pub candidates: Vec<Window>,
    pub index: usize,
}

// Promote a window to the front of the ring. Existing occurrences are
// removed so each window appears exactly once.
pub fn on_focus(wm: &mut Wm, window: Window) {
    wm.mru.retain(|&w| w != window);
    wm.mru.insert(0, window);
}

// Window destroyed: remove from ring.
pub fn on_unmanage(wm: &mut Wm, gone: Window) {
    wm.mru.retain(|&w| w != gone);
}

// Cycle press. `forward` advances toward older windows; backward walks
// toward newer. Wraps both ways. `scope` chooses Tag-only or all windows.
pub fn start_or_advance(wm: &mut Wm, forward: bool, scope: CycleScope) {
    let (candidates, index) = match wm.mru_cycle.take() {
        Some(cur) => {
            // Continuing an active cycle: advance the index against the
            // candidate list we froze on first press. This keeps cycling
            // stable even if the ring were mutated mid-cycle.
            let len = cur.candidates.len();
            if len < 2 {
                wm.mru_cycle = Some(cur);
                return;
            }
            let next_index = if forward {
                (cur.index + 1) % len
            } else {
                (cur.index + len - 1) % len
            };
            (cur.candidates, next_index)
        }
        None => {
            // First press: compute candidates under the requested scope.
            let candidates: Vec<Window> = match scope {
                CycleScope::All => wm.mru.clone(),
                CycleScope::Tag => wm
                    .mru
                    .iter()
                    .copied()
                    .filter(|&w| {
                        wm.clients
                            .iter()
                            .any(|c| c.window == w && c.tag == wm.active_tag)
                    })
                    .collect(),
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
    if let Some(t) = target_tag
        && t != wm.active_tag
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
        if let Some(&target) = cycle.candidates.get(cycle.index) {
            on_focus(wm, target);
        }
    }
}

pub fn on_key_release(wm: &mut Wm, ev: &KeyReleaseEvent) {
    if wm.mru_cycle.is_some() && wm.alt_keycodes.contains(&ev.detail) {
        finish_cycle(wm);
    }
}
