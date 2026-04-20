use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use crate::atoms::Atoms;
use crate::monitor::Monitor;

// Set _NET_SUPPORTED on root to advertise our capabilities
pub fn set_supported(conn: &RustConnection, root: Window, atoms: &Atoms) {
    let supported: Vec<Atom> = vec![
        atoms._NET_SUPPORTED,
        atoms._NET_CLIENT_LIST,
        atoms._NET_CLIENT_LIST_STACKING,
        atoms._NET_NUMBER_OF_DESKTOPS,
        atoms._NET_DESKTOP_NAMES,
        atoms._NET_DESKTOP_GEOMETRY,
        atoms._NET_DESKTOP_VIEWPORT,
        atoms._NET_CURRENT_DESKTOP,
        atoms._NET_ACTIVE_WINDOW,
        atoms._NET_SUPPORTING_WM_CHECK,
        atoms._NET_WORKAREA,
        atoms._NET_CLOSE_WINDOW,
        atoms._NET_WM_NAME,
        atoms._NET_WM_DESKTOP,
        atoms._NET_WM_STATE,
        atoms._NET_WM_STATE_FULLSCREEN,
        atoms._NET_WM_STATE_MAXIMIZED_VERT,
        atoms._NET_WM_STATE_MAXIMIZED_HORZ,
        atoms._NET_WM_STATE_ABOVE,
        atoms._NET_WM_STATE_BELOW,
        atoms._NET_WM_STATE_HIDDEN,
        atoms._NET_WM_WINDOW_TYPE,
        atoms._NET_FRAME_EXTENTS,
        atoms._NET_WM_STRUT_PARTIAL,
        atoms._NET_WM_MOVERESIZE,
    ];
    let _ = conn.change_property32(
        PropMode::REPLACE,
        root,
        atoms._NET_SUPPORTED,
        AtomEnum::ATOM,
        &supported,
    );
}

// Create a child window for _NET_SUPPORTING_WM_CHECK
pub fn set_wm_check(
    conn: &RustConnection,
    root: Window,
    atoms: &Atoms,
    wm_name: &str,
) -> Window {
    let check_win = conn.generate_id().unwrap();
    let _ = conn.create_window(
        0,
        check_win,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_ONLY,
        0,
        &CreateWindowAux::new(),
    );

    // Set _NET_SUPPORTING_WM_CHECK on both root and check window
    let _ = conn.change_property32(
        PropMode::REPLACE,
        root,
        atoms._NET_SUPPORTING_WM_CHECK,
        AtomEnum::WINDOW,
        &[check_win],
    );
    let _ = conn.change_property32(
        PropMode::REPLACE,
        check_win,
        atoms._NET_SUPPORTING_WM_CHECK,
        AtomEnum::WINDOW,
        &[check_win],
    );

    // Set WM name on check window
    let _ = conn.change_property8(
        PropMode::REPLACE,
        check_win,
        atoms._NET_WM_NAME,
        atoms.UTF8_STRING,
        wm_name.as_bytes(),
    );

    check_win
}

pub fn set_desktop_count(conn: &RustConnection, root: Window, atoms: &Atoms, count: u32) {
    let _ = conn.change_property32(
        PropMode::REPLACE,
        root,
        atoms._NET_NUMBER_OF_DESKTOPS,
        AtomEnum::CARDINAL,
        &[count],
    );
}

pub fn set_desktop_names(conn: &RustConnection, root: Window, atoms: &Atoms, names: &[String]) {
    // UTF8 null-separated
    let mut data = Vec::new();
    for name in names {
        data.extend_from_slice(name.as_bytes());
        data.push(0);
    }
    let _ = conn.change_property8(
        PropMode::REPLACE,
        root,
        atoms._NET_DESKTOP_NAMES,
        atoms.UTF8_STRING,
        &data,
    );
}

pub fn set_current_desktop(conn: &RustConnection, root: Window, atoms: &Atoms, idx: u32) {
    let _ = conn.change_property32(
        PropMode::REPLACE,
        root,
        atoms._NET_CURRENT_DESKTOP,
        AtomEnum::CARDINAL,
        &[idx],
    );
}

pub fn set_active_window(conn: &RustConnection, root: Window, atoms: &Atoms, win: Option<Window>) {
    let val = win.unwrap_or(0);
    let _ = conn.change_property32(
        PropMode::REPLACE,
        root,
        atoms._NET_ACTIVE_WINDOW,
        AtomEnum::WINDOW,
        &[val],
    );
}

pub fn set_client_list(conn: &RustConnection, root: Window, atoms: &Atoms, clients: &[Window]) {
    let _ = conn.change_property32(
        PropMode::REPLACE,
        root,
        atoms._NET_CLIENT_LIST,
        AtomEnum::WINDOW,
        clients,
    );
    let _ = conn.change_property32(
        PropMode::REPLACE,
        root,
        atoms._NET_CLIENT_LIST_STACKING,
        AtomEnum::WINDOW,
        clients,
    );
}

// Compute the bounding rectangle of all monitors. Used for EWMH properties
// that report a single rect for the whole virtual screen. For non-rectangular
// (gap or L-shape) layouts this overestimates, but EWMH has no slot for
// per-monitor work areas — the bounding rect is the spec-compliant answer.
fn bounding_rect(monitors: &[Monitor]) -> (i32, i32, u32, u32) {
    let min_x = monitors.iter().map(|m| m.x).min().unwrap_or(0);
    let min_y = monitors.iter().map(|m| m.y).min().unwrap_or(0);
    let max_x = monitors
        .iter()
        .map(|m| m.x + m.width as i32)
        .max()
        .unwrap_or(0);
    let max_y = monitors
        .iter()
        .map(|m| m.y + m.height as i32)
        .max()
        .unwrap_or(0);
    (min_x, min_y, (max_x - min_x) as u32, (max_y - min_y) as u32)
}

pub fn set_workarea(
    conn: &RustConnection,
    root: Window,
    atoms: &Atoms,
    num_desktops: u32,
    monitors: &[Monitor],
    bar_height: u32,
) {
    let (bx, by, bw, bh) = bounding_rect(monitors);
    let work_x = bx as u32;
    let work_y = by as u32 + bar_height;
    let work_w = bw;
    let work_h = bh - bar_height;
    let mut data = Vec::with_capacity(num_desktops as usize * 4);
    for _ in 0..num_desktops {
        data.push(work_x);
        data.push(work_y);
        data.push(work_w);
        data.push(work_h);
    }
    let _ = conn.change_property32(
        PropMode::REPLACE,
        root,
        atoms._NET_WORKAREA,
        AtomEnum::CARDINAL,
        &data,
    );
}

pub fn set_desktop_geometry(
    conn: &RustConnection,
    root: Window,
    atoms: &Atoms,
    monitors: &[Monitor],
) {
    let (_, _, bw, bh) = bounding_rect(monitors);
    let _ = conn.change_property32(
        PropMode::REPLACE,
        root,
        atoms._NET_DESKTOP_GEOMETRY,
        AtomEnum::CARDINAL,
        &[bw, bh],
    );
}

pub fn set_viewport(conn: &RustConnection, root: Window, atoms: &Atoms, num_desktops: u32) {
    let data: Vec<u32> = vec![0; num_desktops as usize * 2];
    let _ = conn.change_property32(
        PropMode::REPLACE,
        root,
        atoms._NET_DESKTOP_VIEWPORT,
        AtomEnum::CARDINAL,
        &data,
    );
}

pub fn set_frame_extents(conn: &RustConnection, window: Window, atoms: &Atoms) {
    // Non-reparenting, no decorations: all zeros
    let _ = conn.change_property32(
        PropMode::REPLACE,
        window,
        atoms._NET_FRAME_EXTENTS,
        AtomEnum::CARDINAL,
        &[0, 0, 0, 0],
    );
}

pub fn set_client_desktop(conn: &RustConnection, window: Window, atoms: &Atoms, desktop: u32) {
    let _ = conn.change_property32(
        PropMode::REPLACE,
        window,
        atoms._NET_WM_DESKTOP,
        AtomEnum::CARDINAL,
        &[desktop],
    );
}

// Send WM_DELETE_WINDOW to gracefully close a client
pub fn close_window(conn: &RustConnection, window: Window, atoms: &Atoms) -> bool {
    if supports_protocol(conn, window, atoms, atoms.WM_DELETE_WINDOW) {
        let data = ClientMessageData::from([
            atoms.WM_DELETE_WINDOW,
            0, // CurrentTime
            0,
            0,
            0,
        ]);
        let event = ClientMessageEvent::new(32, window, atoms.WM_PROTOCOLS, data);
        let _ = conn.send_event(false, window, EventMask::NO_EVENT, event);
        let _ = conn.flush();
        true
    } else {
        // Forcefully kill
        let _ = conn.kill_client(window);
        let _ = conn.flush();
        false
    }
}

pub fn set_allowed_actions(conn: &RustConnection, window: Window, atoms: &Atoms) {
    let actions = [
        atoms._NET_WM_ACTION_MOVE,
        atoms._NET_WM_ACTION_RESIZE,
        atoms._NET_WM_ACTION_CLOSE,
        atoms._NET_WM_ACTION_FULLSCREEN,
        atoms._NET_WM_ACTION_ABOVE,
        atoms._NET_WM_ACTION_CHANGE_DESKTOP,
    ];
    let _ = conn.change_property32(
        PropMode::REPLACE,
        window,
        atoms._NET_WM_ALLOWED_ACTIONS,
        AtomEnum::ATOM,
        &actions,
    );
}

pub fn supports_protocol(
    conn: &RustConnection,
    window: Window,
    atoms: &Atoms,
    protocol: Atom,
) -> bool {
    let reply = conn
        .get_property(false, window, atoms.WM_PROTOCOLS, AtomEnum::ATOM, 0, 64)
        .ok()
        .and_then(|c| c.reply().ok());

    match reply {
        Some(prop) => prop
            .value32()
            .map(|vals| vals.into_iter().any(|a| a == protocol))
            .unwrap_or(false),
        None => false,
    }
}
