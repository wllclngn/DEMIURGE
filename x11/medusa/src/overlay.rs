// MEDUSA's overlay as an X window: core's overlay over the root's frozen
// image, in a fullscreen override-redirect window holding the keyboard and
// pointer with a crosshair cursor.
//
// The root is captured before the window maps, so the backdrop never shows
// the overlay itself. What a key asks for (save, copy, pin) is done once the
// window is gone.

use std::path::Path;

use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;

use medusa::overlay::{self, GuiOpts, Key, Overlay};
use x11_client::capture;
use x11_client::keymap::{self as keys, KeyMap};

// X11 cursor-font glyph index for a crosshair.
const XC_CROSSHAIR: u16 = 34;

pub fn run(opts: &GuiOpts) -> Result<(), String> {
    let (conn, screen_num) =
        RustConnection::connect(None).map_err(|e| format!("x11 connect: {}", e))?;
    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;
    let depth = screen.root_depth;
    let w = screen.width_in_pixels as u32;
    let h = screen.height_in_pixels as u32;

    // Freeze the screen first -- before mapping our window, so the
    // backdrop doesn't capture our own overlay.
    let shot = capture::capture_root_to_surface(&conn, root)?;
    let mut overlay = Overlay::new(shot, opts.accent);

    let win = conn
        .generate_id()
        .map_err(|e| format!("generate_id win: {}", e))?;
    let gc = conn
        .generate_id()
        .map_err(|e| format!("generate_id gc: {}", e))?;
    let cursor = make_crosshair(&conn).unwrap_or(x11rb::NONE);

    conn.create_window(
        depth,
        win,
        root,
        0,
        0,
        w as u16,
        h as u16,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new()
            .override_redirect(1)
            .background_pixel(0)
            .cursor(cursor)
            .event_mask(EventMask::EXPOSURE | EventMask::STRUCTURE_NOTIFY),
    )
    .map_err(|e| format!("create_window: {}", e))?;
    conn.create_gc(gc, win, &CreateGCAux::new())
        .map_err(|e| format!("create_gc: {}", e))?;
    let _ = conn.map_window(win);
    let _ = conn.configure_window(win, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
    let _ = conn.flush();

    if !grab_input(&conn, win, cursor) {
        let _ = conn.destroy_window(win);
        let _ = conn.flush();
        return Err("could not grab keyboard + pointer".into());
    }

    let keymap = keys::load_keymap(&conn);

    let result = event_loop(&conn, win, gc, w, h, depth, &mut overlay, &keymap);

    let _ = conn.ungrab_keyboard(x11rb::CURRENT_TIME);
    let _ = conn.ungrab_pointer(x11rb::CURRENT_TIME);
    let _ = conn.unmap_window(win);
    let _ = conn.destroy_window(win);
    let _ = conn.flush();
    drop(conn);

    match result? {
        Key::Save { copy } => {
            if let Some((crop, _)) = overlay.crop()? {
                let path = overlay::save(&crop, &opts.save_dir)?;
                if copy && opts.copy {
                    copy_to_clipboard(&path);
                }
            }
        }
        Key::Pin => {
            // A pin is never the only record: The crop is saved too.
            if let Some((crop, (x, y, rw, rh))) = overlay.crop()? {
                let _ = overlay::save(&crop, &opts.save_dir);
                pin(crop, x, y, rw, rh);
            }
        }
        Key::Cancel | Key::Continue => {}
    }
    Ok(())
}

// Until a key asks to close the overlay: What it asked.
fn event_loop(
    conn: &RustConnection,
    win: Window,
    gc: Gcontext,
    w: u32,
    h: u32,
    depth: u8,
    overlay: &mut Overlay,
    keymap: &KeyMap,
) -> Result<Key, String> {
    redraw(conn, win, gc, w, h, depth, overlay)?;
    loop {
        let ev = conn.wait_for_event().map_err(|e| format!("wait_for_event: {}", e))?;
        match ev {
            Event::Expose(e) if e.count == 0 => {}
            Event::ButtonPress(e) if e.detail == 1 => overlay.press((e.event_x as i32, e.event_y as i32)),
            Event::MotionNotify(e) => overlay.motion((e.event_x as i32, e.event_y as i32)),
            Event::ButtonRelease(e) if e.detail == 1 => overlay.release((e.event_x as i32, e.event_y as i32)),
            Event::KeyPress(e) => {
                let ctrl = u16::from(e.state) & u16::from(ModMask::CONTROL) != 0;
                let shift = u16::from(e.state) & u16::from(ModMask::SHIFT) != 0;
                let sym = keycode_to_keysym(e.detail, shift, keymap);
                match overlay.key(sym, ctrl, shift) {
                    Key::Continue => {}
                    asked => return Ok(asked),
                }
            }
            _ => {}
        }
        redraw(conn, win, gc, w, h, depth, overlay)?;
    }
}

// Pin the annotated crop to the screen as a floating window. Forks a
// detached holder so MEDUSA can exit; the pin stays put at the region's
// location until any key or click dismisses it.
fn pin(crop: cairo::ImageSurface, x: i32, y: i32, w: i32, h: i32) {
    let pid = unsafe { libc::fork() };
    if pid != 0 {
        // Parent (or fork failure): MEDUSA exits; the pin runs in the
        // child if the fork succeeded.
        return;
    }
    unsafe {
        libc::setsid();
    }
    let _ = pin_window(crop, x, y, w, h);
    unsafe {
        libc::_exit(0);
    }
}

fn pin_window(crop: cairo::ImageSurface, x: i32, y: i32, w: i32, h: i32) -> Result<(), String> {
    let (conn, screen_num) =
        RustConnection::connect(None).map_err(|e| format!("connect: {}", e))?;
    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;
    let depth = screen.root_depth;
    let win = conn.generate_id().map_err(|e| format!("gen id: {}", e))?;
    let gc = conn.generate_id().map_err(|e| format!("gen gc: {}", e))?;
    conn.create_window(
        depth,
        win,
        root,
        x as i16,
        y as i16,
        w as u16,
        h as u16,
        1,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new()
            .override_redirect(1)
            .background_pixel(0)
            .border_pixel(screen.white_pixel)
            .event_mask(EventMask::EXPOSURE | EventMask::KEY_PRESS | EventMask::BUTTON_PRESS),
    )
    .map_err(|e| format!("create_window: {}", e))?;
    conn.create_gc(gc, win, &CreateGCAux::new())
        .map_err(|e| format!("create_gc: {}", e))?;
    let _ = conn.map_window(win);
    let _ = conn.configure_window(win, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
    // Grab the keyboard so any key dismisses the pin (the mouseless
    // exit path). A click on the pin also dismisses it.
    let _ = conn.grab_keyboard(
        false,
        win,
        x11rb::CURRENT_TIME,
        GrabMode::ASYNC,
        GrabMode::ASYNC,
    );
    let _ = conn.flush();

    let blit = || {
        let _ = crop.with_data(|data| {
            let _ = conn.put_image(
                ImageFormat::Z_PIXMAP,
                win,
                gc,
                w as u16,
                h as u16,
                0,
                0,
                0,
                depth,
                data,
            );
            let _ = conn.flush();
        });
    };

    loop {
        let ev = conn
            .wait_for_event()
            .map_err(|e| format!("wait_for_event: {}", e))?;
        match ev {
            Event::Expose(e) if e.count == 0 => blit(),
            Event::KeyPress(_) | Event::ButtonPress(_) => break,
            _ => {}
        }
    }
    let _ = conn.ungrab_keyboard(x11rb::CURRENT_TIME);
    let _ = conn.destroy_window(win);
    let _ = conn.flush();
    Ok(())
}

pub fn copy_to_clipboard(path: &Path) {
    // DEMIURGE's own clipboard holder (zero deps): forks a process that
    // owns the X11 CLIPBOARD selection and serves the image until
    // another client takes it. Falls back to xclip only if the fork
    // fails. The file is saved regardless.
    if crate::clipboard::copy_png(path) {
        eprintln!("[medusa] copied to clipboard");
        return;
    }
    match std::process::Command::new("xclip")
        .args(["-selection", "clipboard", "-t", "image/png"])
        .arg(path)
        .spawn()
    {
        Ok(_) => eprintln!("[medusa] copied to clipboard (xclip fallback)"),
        Err(e) => eprintln!(
            "[medusa] clipboard copy skipped (holder + xclip both failed: {}); file saved",
            e
        ),
    }
}

fn redraw(
    conn: &RustConnection,
    win: Window,
    gc: Gcontext,
    w: u32,
    h: u32,
    depth: u8,
    overlay: &Overlay,
) -> Result<(), String> {
    let (surface, cr) = demiurge_core::torrentius::new_surface(w as i32, h as i32)?;
    overlay.draw(&cr, (0, 0, w as i32, h as i32))?;
    drop(cr);
    surface.flush();
    let _ = surface.with_data(|data| {
        let _ = conn.put_image(
            ImageFormat::Z_PIXMAP,
            win,
            gc,
            w as u16,
            h as u16,
            0,
            0,
            0,
            depth,
            data,
        );
    });
    let _ = conn.flush();
    Ok(())
}

fn grab_input(conn: &RustConnection, win: Window, cursor: Cursor) -> bool {
    for _ in 0..50 {
        let kb = conn.grab_keyboard(
            false,
            win,
            x11rb::CURRENT_TIME,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
        );
        let kb_ok = kb
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| r.status == GrabStatus::SUCCESS)
            .unwrap_or(false);
        let ptr = conn.grab_pointer(
            false,
            win,
            EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            x11rb::NONE,
            cursor,
            x11rb::CURRENT_TIME,
        );
        let ptr_ok = ptr
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| r.status == GrabStatus::SUCCESS)
            .unwrap_or(false);
        if kb_ok && ptr_ok {
            return true;
        }
        unsafe {
            let ts = libc::timespec { tv_sec: 0, tv_nsec: 100_000_000 };
            libc::nanosleep(&ts, std::ptr::null_mut());
        }
    }
    false
}

// Build a crosshair cursor from the X11 cursor font.
fn make_crosshair(conn: &RustConnection) -> Result<Cursor, String> {
    let font = conn.generate_id().map_err(|e| format!("gen font id: {}", e))?;
    conn.open_font(font, b"cursor")
        .map_err(|e| format!("open cursor font: {}", e))?;
    let cursor = conn.generate_id().map_err(|e| format!("gen cursor id: {}", e))?;
    conn.create_glyph_cursor(
        cursor,
        font,
        font,
        XC_CROSSHAIR,
        XC_CROSSHAIR + 1,
        0,
        0,
        0,
        u16::MAX,
        u16::MAX,
        u16::MAX,
    )
    .map_err(|e| format!("create_glyph_cursor: {}", e))?;
    let _ = conn.close_font(font);
    Ok(cursor)
}

fn keycode_to_keysym(keycode: u8, shift: bool, keymap: &KeyMap) -> u32 {
    if keycode < keymap.min_keycode {
        return 0;
    }
    let base = (keycode - keymap.min_keycode) as usize * keymap.syms_per_code;
    let idx = base + if shift && keymap.syms_per_code > 1 { 1 } else { 0 };
    keymap.keysyms.get(idx).copied().unwrap_or(0)
}
