use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;

use crate::layout::Layout;
use crate::wm::Wm;

const MIN_SIZE: u32 = 50;

#[derive(Debug)]
pub enum DragKind {
    Move,
    Resize,
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

// Passive grab on Button1 with no modifier on a managed client. SYNC mode lets
// the WM intercept the click, focus the window, then replay the click so the
// app still receives it. Without this, plain clicks bypass the WM entirely and
// click-to-focus doesn't work.
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

pub fn start_drag(wm: &mut Wm, window: Window, ev: &ButtonPressEvent) {
    let kind = if ev.detail == 1 {
        DragKind::Move
    } else {
        DragKind::Resize
    };

    // Skip fullscreen windows
    if wm.clients.iter().any(|c| c.window == window && c.fullscreen) {
        return;
    }

    // Focus the window
    wm.focus_window(Some(window));

    // If in tiled layout, float this window out of the layout
    let tag = wm.active_tag;
    if wm.layouts[tag] != Layout::Floating {
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
        DragKind::Resize => {
            let new_w = (drag.orig_w as i32 + dx).max(MIN_SIZE as i32) as u32;
            let new_h = (drag.orig_h as i32 + dy).max(MIN_SIZE as i32) as u32;

            if let Some(client) = wm.clients.iter_mut().find(|c| c.window == drag.window) {
                client.w = new_w;
                client.h = new_h;
            }

            let _ = wm.conn.configure_window(
                drag.window,
                &ConfigureWindowAux::new().width(new_w).height(new_h),
            );
        }
    }

    let _ = wm.conn.flush();
}

// CSD move: initiated by _NET_WM_MOVERESIZE from applications (e.g. Chromium)
pub fn start_csd_move(wm: &mut Wm, window: Window, root_x: i16, root_y: i16) {
    if wm.clients.iter().any(|c| c.window == window && c.fullscreen) {
        return;
    }

    wm.focus_window(Some(window));

    let tag = wm.active_tag;
    if wm.layouts[tag] != Layout::Floating {
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
        kind: DragKind::Move,
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
