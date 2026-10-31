// Zero-dependency X11 clipboard holder.
//
// Replaces the xclip dependency. To put an image on the X11 CLIPBOARD
// the owner must persist and answer SelectionRequest events for as
// long as it holds the selection -- so we fork a detached child that
// owns CLIPBOARD and serves the PNG bytes (as image/png) until another
// client takes the selection. The parent (MEDUSA) returns immediately
// and exits.

use std::path::Path;

use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;

// Read `path` and serve it on the CLIPBOARD selection from a forked
// holder process. Returns true if the holder was launched. On false
// the caller can fall back to xclip.
pub fn copy_png(path: &Path) -> bool {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[medusa] clipboard read {}: {}", path.display(), e);
            return false;
        }
    };

    // Fork: parent returns, child becomes the clipboard holder.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return false;
    }
    if pid > 0 {
        return true; // parent: holder detached
    }

    // Child: detach from the session and serve until SelectionClear.
    unsafe {
        libc::setsid();
    }
    let _ = serve(data);
    unsafe {
        libc::_exit(0);
    }
}

fn serve(data: Vec<u8>) -> Result<(), String> {
    let (conn, screen_num) =
        RustConnection::connect(None).map_err(|e| format!("connect: {}", e))?;
    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;

    let clipboard = intern(&conn, "CLIPBOARD")?;
    let targets = intern(&conn, "TARGETS")?;
    let png = intern(&conn, "image/png")?;

    let win = conn.generate_id().map_err(|e| format!("gen id: {}", e))?;
    conn.create_window(
        0,
        win,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )
    .map_err(|e| format!("create_window: {}", e))?;

    conn.set_selection_owner(win, clipboard, x11rb::CURRENT_TIME)
        .map_err(|e| format!("set_selection_owner: {}", e))?;
    let owner = conn
        .get_selection_owner(clipboard)
        .map_err(|e| format!("get_selection_owner: {}", e))?
        .reply()
        .map_err(|e| format!("reply: {}", e))?
        .owner;
    if owner != win {
        return Err("could not own CLIPBOARD".into());
    }
    conn.flush().map_err(|e| format!("flush: {}", e))?;

    loop {
        let ev = conn
            .wait_for_event()
            .map_err(|e| format!("wait_for_event: {}", e))?;
        match ev {
            // Another client took the clipboard -- our job is done.
            Event::SelectionClear(_) => return Ok(()),
            Event::SelectionRequest(req) => {
                // ICCCM: property == None means an obsolete client; fall
                // back to using the target as the property name.
                let property = if req.property == x11rb::NONE {
                    req.target
                } else {
                    req.property
                };
                let mut ok = false;
                if req.target == targets {
                    // Advertise the formats we can provide.
                    let atoms = [targets, png];
                    let bytes: Vec<u8> =
                        atoms.iter().flat_map(|a| a.to_ne_bytes()).collect();
                    let _ = conn.change_property(
                        PropMode::REPLACE,
                        req.requestor,
                        property,
                        AtomEnum::ATOM,
                        32,
                        atoms.len() as u32,
                        &bytes,
                    );
                    ok = true;
                } else if req.target == png {
                    let _ = conn.change_property(
                        PropMode::REPLACE,
                        req.requestor,
                        property,
                        png,
                        8,
                        data.len() as u32,
                        &data,
                    );
                    ok = true;
                }
                let notify = SelectionNotifyEvent {
                    response_type: SELECTION_NOTIFY_EVENT,
                    sequence: 0,
                    time: req.time,
                    requestor: req.requestor,
                    selection: req.selection,
                    target: req.target,
                    property: if ok { property } else { x11rb::NONE },
                };
                let _ = conn.send_event(false, req.requestor, EventMask::NO_EVENT, notify);
                let _ = conn.flush();
            }
            _ => {}
        }
    }
}

fn intern(conn: &RustConnection, name: &str) -> Result<Atom, String> {
    conn.intern_atom(false, name.as_bytes())
        .map_err(|e| format!("intern {}: {}", name, e))?
        .reply()
        .map_err(|e| format!("intern {} reply: {}", name, e))
        .map(|r| r.atom)
}
