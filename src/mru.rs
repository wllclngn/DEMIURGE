use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;

use crate::wm::Wm;

// Marker: presence in wm.mru_cycle means the keyboard is grabbed and the
// user is mid Alt+Tab. Cycle stepping mutates wm.history/wm.future directly,
// so the focus_window history-push path must be skipped while this is set.
#[derive(Debug)]
pub struct CycleState;

// Scrub a destroyed window from both navigation stacks.
pub fn on_unmanage(wm: &mut Wm, gone: Window) {
    wm.history.retain(|&w| w != gone);
    wm.future.retain(|&w| w != gone);
}

pub fn start_or_advance(wm: &mut Wm, forward: bool) {
    if wm.mru_cycle.is_some() {
        if forward {
            step_forward(wm);
        } else {
            step_back(wm);
        }
        return;
    }

    // First press: refuse if there is nothing to step toward
    let empty = if forward {
        wm.future.is_empty()
    } else {
        wm.history.is_empty()
    };
    if empty {
        return;
    }

    // Grab the keyboard so we receive Alt release reliably
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

    wm.mru_cycle = Some(CycleState);

    if forward {
        step_forward(wm);
    } else {
        step_back(wm);
    }
}

// Alt+Tab one step into the past:
//   future.push(current); current = history.pop()
fn step_back(wm: &mut Wm) {
    let target = match wm.history.pop() {
        Some(t) => t,
        None => return,
    };
    if let Some(cur) = wm.focus {
        wm.future.push(cur);
    }
    focus_target(wm, target);
}

// Alt+Shift+Tab one step into the future (only meaningful after walking back):
//   history.push(current); current = future.pop()
fn step_forward(wm: &mut Wm) {
    let target = match wm.future.pop() {
        Some(t) => t,
        None => return,
    };
    if let Some(cur) = wm.focus {
        wm.history.push(cur);
    }
    focus_target(wm, target);
}

// Switch tags if needed, then focus the target. focus_window's history push
// is suppressed because mru_cycle is Some.
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
    wm.mru_cycle = None;
}

pub fn on_key_release(wm: &mut Wm, ev: &KeyReleaseEvent) {
    if wm.mru_cycle.is_some() && wm.alt_keycodes.contains(&ev.detail) {
        finish_cycle(wm);
    }
}
