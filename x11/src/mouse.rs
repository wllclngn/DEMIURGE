use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;

use crate::layout::Layout;
use crate::wm::Wm;

const MIN_SIZE: u32 = 50;

#[derive(Debug)]
pub enum DragKind {
    Move,
    Resize(Edges),
}

// The window edges a resize drags; the opposite edges stay put.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edges {
    pub left: bool,
    pub top: bool,
    pub right: bool,
    pub bottom: bool,
}

impl Edges {
    pub const BOTTOM_RIGHT: Edges = Edges { left: false, top: false, right: true, bottom: true };

    // _NET_WM_MOVERESIZE directions 0-7, clockwise from the top-left corner.
    pub fn from_moveresize(direction: u32) -> Option<Edges> {
        let (left, top, right, bottom) = match direction {
            0 => (true, true, false, false),
            1 => (false, true, false, false),
            2 => (false, true, true, false),
            3 => (false, false, true, false),
            4 => (false, false, true, true),
            5 => (false, false, false, true),
            6 => (true, false, false, true),
            7 => (true, false, false, false),
            _ => return None,
        };
        Some(Edges { left, top, right, bottom })
    }
}

// Geometry after dragging `edges` by (dx, dy) from `orig`. Each dimension
// stays at least MIN_SIZE, the dragged left or top edge moves while the
// opposite edge holds, and the top edge never rises above `min_y` (the bar).
pub fn resize_geometry(
    edges: Edges,
    orig: (i32, i32, u32, u32),
    dx: i32,
    dy: i32,
    min_y: i32,
) -> (i32, i32, u32, u32) {
    let (ox, oy, ow, oh) = (orig.0, orig.1, orig.2 as i32, orig.3 as i32);
    let min = MIN_SIZE as i32;
    let (mut x, mut w) = (ox, ow);
    if edges.right {
        w = (ow + dx).max(min);
    } else if edges.left {
        w = (ow - dx).max(min);
        x = ox + ow - w;
    }
    let (mut y, mut h) = (oy, oh);
    if edges.bottom {
        h = (oh + dy).max(min);
    } else if edges.top {
        let bottom = oy + oh;
        y = (oy + dy).max(min_y).min(bottom - min);
        h = bottom - y;
    }
    (x, y, w as u32, h as u32)
}

#[derive(Debug)]
pub struct DragState {
    pub kind: DragKind,
    pub window: Window,
    pub start_x: i16,
    pub start_y: i16,
    pub orig_x: i32,
    pub orig_y: i32,
    pub orig_w: u32,
    pub orig_h: u32,
}

fn lock_masks() -> [u16; 4] {
    [
        0,
        u16::from(ModMask::LOCK),
        u16::from(ModMask::M2),
        u16::from(ModMask::LOCK) | u16::from(ModMask::M2),
    ]
}

pub fn grab_buttons(conn: &RustConnection, root: Window) {
    for button in [ButtonIndex::M1, ButtonIndex::M3] {
        for &lock in &lock_masks() {
            let combined = ModMask::from(u16::from(ModMask::M4) | lock);
            let _ = conn.grab_button(
                false,
                root,
                EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
                0u32,
                0u32,
                button,
                combined,
            );
        }
    }
}

// Passive grab on Button1 with no modifier on an unfocused client. SYNC mode
// lets the WM intercept the click, focus the window, then replay the click so
// the app still receives it. The focused client carries no grab (see
// ungrab_focus_button): Its clicks go straight to the app instead of freezing
// the pointer for a round trip through the WM on every press.
pub fn grab_focus_button(conn: &RustConnection, window: Window) {
    for &lock in &lock_masks() {
        let _ = conn.grab_button(
            false,
            window,
            EventMask::BUTTON_PRESS,
            GrabMode::SYNC,
            GrabMode::ASYNC,
            0u32,
            0u32,
            ButtonIndex::M1,
            ModMask::from(lock),
        );
    }
}

pub fn ungrab_focus_button(conn: &RustConnection, window: Window) {
    for &lock in &lock_masks() {
        let _ = conn.ungrab_button(ButtonIndex::M1, window, ModMask::from(lock));
    }
}

pub fn start_drag(wm: &mut Wm, window: Window, ev: &ButtonPressEvent) {
    let kind = if ev.detail == 1 {
        DragKind::Move
    } else {
        DragKind::Resize(Edges::BOTTOM_RIGHT)
    };

    // Skip fullscreen windows
    if wm.clients.iter().any(|c| c.window == window && c.fullscreen) {
        return;
    }

    // Focus the window
    wm.focus_window(Some(window));

    // If the window's tag is on a non-floating layout, pop it out so
    // the drag is actually meaningful (otherwise the next arrange()
    // would snap it back). We key off the window's own tag, not any
    // global active tag -- correct under per-monitor active tags too.
    let tag = wm.clients.iter().find(|c| c.window == window).map(|c| c.tag);
    if let Some(t) = tag
        && wm.layouts[t] != Layout::Floating
    {
        if let Some(client) = wm.clients.iter_mut().find(|c| c.window == window) {
            if !client.floating {
                client.floating = true;
                wm.arrange();
            }
        }
    }

    let client = match wm.clients.iter().find(|c| c.window == window) {
        Some(c) => c,
        None => return,
    };

    // Active pointer grab
    let cookie = match wm.conn.grab_pointer(
        false,
        wm.root,
        EventMask::POINTER_MOTION | EventMask::BUTTON_RELEASE,
        GrabMode::ASYNC,
        GrabMode::ASYNC,
        0u32,
        0u32,
        x11rb::CURRENT_TIME,
    ) {
        Ok(c) => c,
        Err(_) => return,
    };
    let reply = match cookie.reply() {
        Ok(r) => r,
        Err(_) => return,
    };
    if reply.status != GrabStatus::SUCCESS {
        return;
    }

    wm.drag = Some(DragState {
        kind,
        window,
        start_x: ev.root_x,
        start_y: ev.root_y,
        orig_x: client.x,
        orig_y: client.y,
        orig_w: client.w,
        orig_h: client.h,
    });
}

pub fn on_motion(wm: &mut Wm, ev: &MotionNotifyEvent) {
    let drag = match &wm.drag {
        Some(d) => d,
        None => return,
    };

    let dx = ev.root_x as i32 - drag.start_x as i32;
    let dy = ev.root_y as i32 - drag.start_y as i32;

    match drag.kind {
        DragKind::Move => {
            let new_x = drag.orig_x + dx;
            let new_y = (drag.orig_y + dy).max(wm.bar_height as i32);

            if let Some(client) = wm.clients.iter_mut().find(|c| c.window == drag.window) {
                client.x = new_x;
                client.y = new_y;
            }

            let _ = wm.conn.configure_window(
                drag.window,
                &ConfigureWindowAux::new().x(new_x).y(new_y),
            );
        }
        DragKind::Resize(edges) => {
            let (x, y, w, h) = resize_geometry(
                edges,
                (drag.orig_x, drag.orig_y, drag.orig_w, drag.orig_h),
                dx,
                dy,
                wm.bar_height as i32,
            );

            if let Some(client) = wm.clients.iter_mut().find(|c| c.window == drag.window) {
                client.x = x;
                client.y = y;
                client.w = w;
                client.h = h;
            }

            let _ = wm.conn.configure_window(
                drag.window,
                &ConfigureWindowAux::new().x(x).y(y).width(w).height(h),
            );
        }
    }

    let _ = wm.conn.flush();
}

// CSD move or edge resize: initiated by _NET_WM_MOVERESIZE from applications
// (e.g. Chromium) that draw their own decorations.
pub fn start_csd_drag(wm: &mut Wm, window: Window, root_x: i16, root_y: i16, kind: DragKind) {
    if wm.clients.iter().any(|c| c.window == window && c.fullscreen) {
        return;
    }

    wm.focus_window(Some(window));

    // Same per-monitor reasoning as start_drag: float out using the
    // window's own tag, not any global active tag.
    let tag = wm.clients.iter().find(|c| c.window == window).map(|c| c.tag);
    if let Some(t) = tag
        && wm.layouts[t] != Layout::Floating
    {
        if let Some(client) = wm.clients.iter_mut().find(|c| c.window == window) {
            if !client.floating {
                client.floating = true;
                wm.arrange();
            }
        }
    }

    let client = match wm.clients.iter().find(|c| c.window == window) {
        Some(c) => c,
        None => return,
    };

    let cookie = match wm.conn.grab_pointer(
        false,
        wm.root,
        EventMask::POINTER_MOTION | EventMask::BUTTON_RELEASE,
        GrabMode::ASYNC,
        GrabMode::ASYNC,
        0u32,
        0u32,
        x11rb::CURRENT_TIME,
    ) {
        Ok(c) => c,
        Err(_) => return,
    };
    let reply = match cookie.reply() {
        Ok(r) => r,
        Err(_) => return,
    };
    if reply.status != GrabStatus::SUCCESS {
        return;
    }

    wm.drag = Some(DragState {
        kind,
        window,
        start_x: root_x,
        start_y: root_y,
        orig_x: client.x,
        orig_y: client.y,
        orig_w: client.w,
        orig_h: client.h,
    });
}

pub fn end_drag(wm: &mut Wm) {
    let _ = wm.conn.ungrab_pointer(x11rb::CURRENT_TIME);
    let _ = wm.conn.flush();
    wm.drag = None;
}
