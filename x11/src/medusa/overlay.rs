// MEDUSA overlay: the fullscreen capture surface.
//
// Flow:
//   1. Capture the whole root into an in-memory Cairo surface (the
//      frozen backdrop). Everything drawn afterward composites over
//      this still image, so the screen appears to freeze the instant
//      MEDUSA launches.
//   2. Map a fullscreen override-redirect window, grab keyboard +
//      pointer, show a crosshair cursor.
//   3. Drag a rectangle. The selection region renders at full
//      brightness; everything outside dims. Border + corner handles +
//      a WxH dimension label track the drag.
//   4. Enter copies the cropped region to the clipboard AND saves it
//      to disk; Ctrl+S saves only; Esc cancels. On accept we crop the
//      frozen surface to the selection and write a PNG.
//
// Phase 1 has no annotation tools -- the selection IS the output.
// Tools layer in later phases by drawing onto the cropped surface
// before export.

use std::path::{Path, PathBuf};

use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;

use crate::keys::{self, KeyMap};
use crate::medusa::tools::{self, Annotation, ToolKind};
use crate::torrentius;

// Virtual-cursor movement step (px) for keyboard annotation placement.
// Shift makes it fine (1px).
const CURSOR_STEP: i32 = 10;
// Stroke-size bounds + step for the [ / ] keys.
const SIZE_MIN: f64 = 1.0;
const SIZE_MAX: f64 = 40.0;
const SIZE_STEP: f64 = 1.0;

// Dim level applied to everything outside the selection.
const DIM_ALPHA: f64 = 0.5;
// Minimum drag size (px) that counts as a real selection.
const MIN_SELECTION: i32 = 4;
// X11 cursor-font glyph index for a crosshair.
const XC_CROSSHAIR: u16 = 34;

pub struct GuiOpts {
    /// Directory screenshots are written to. Default $HOME.
    pub save_dir: PathBuf,
    /// Copy the result to the clipboard on accept (Enter).
    pub copy: bool,
    /// Selection border / handle / label colour, (r, g, b) in 0..1.
    /// Sourced from [appearance].accent so MEDUSA tracks the shared
    /// desktop accent instead of a hardcoded value.
    pub accent: (f64, f64, f64),
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    /// No region yet; waiting for the first drag.
    Idle,
    /// Mouse held, rubber-banding a rectangle.
    Dragging,
    /// Region defined; Enter accepts, a fresh drag re-selects.
    Selected,
}

struct State {
    phase: Phase,
    // Anchor + current corner of the drag, in root pixels.
    ax: i32,
    ay: i32,
    bx: i32,
    by: i32,

    // Annotation state. `tool` None = region mode (drag selects /
    // re-selects). `tool` Some = annotate mode: the virtual cursor +
    // mouse place two-point shapes onto the region.
    tool: Option<ToolKind>,
    annotations: Vec<Annotation>,
    redo: Vec<Annotation>,
    palette: [(f64, f64, f64); tools::PALETTE_LEN],
    color_idx: usize,
    size: f64,
    // Virtual annotation cursor (keyboard placement) + the first point
    // of a two-point shape in progress. Both in screen pixels.
    cursor: (i32, i32),
    pending: Option<(i32, i32)>,
    // Freehand path being drawn (marker / pencil): accumulated points.
    current_path: Vec<(i32, i32)>,
    // Text being typed: insertion point + buffer. Some = text-input
    // mode, which captures all printable keys.
    text_edit: Option<(i32, i32, String)>,
    // Whether the mouse button is currently held (gates path-tool
    // point sampling during MotionNotify).
    pointer_down: bool,
    // Next number for the Counter tool. Increments on each placement.
    counter: u32,
}

impl State {
    // Normalized (x, y, w, h) of the current selection.
    fn rect(&self) -> (i32, i32, i32, i32) {
        let x = self.ax.min(self.bx);
        let y = self.ay.min(self.by);
        let w = (self.ax - self.bx).abs();
        let h = (self.ay - self.by).abs();
        (x, y, w, h)
    }

    fn color(&self) -> (f64, f64, f64) {
        self.palette[self.color_idx]
    }

    // Make the whole screen the canvas. Used when a tool is armed with
    // no region selected yet -- annotate full-screen, mouseless.
    fn select_full(&mut self, w: u32, h: u32) {
        self.ax = 0;
        self.ay = 0;
        self.bx = w as i32;
        self.by = h as i32;
        self.phase = Phase::Selected;
    }

    // Commit a two-point shape (or blur) from `pending` to `end`.
    fn commit_shape(&mut self, end: (i32, i32)) {
        if let (Some(start), Some(kind)) = (self.pending.take(), self.tool) {
            let ann = if kind == ToolKind::Blur {
                Annotation::Blur {
                    x1: start.0,
                    y1: start.1,
                    x2: end.0,
                    y2: end.1,
                }
            } else {
                Annotation::Shape {
                    kind,
                    x1: start.0,
                    y1: start.1,
                    x2: end.0,
                    y2: end.1,
                    color: self.color(),
                    size: self.size,
                }
            };
            self.annotations.push(ann);
            self.redo.clear();
        }
    }

    // Finish the freehand path in progress, if it has 1+ points.
    fn commit_path(&mut self) {
        if self.current_path.len() >= 1 {
            let marker = self.tool == Some(ToolKind::Marker);
            self.annotations.push(Annotation::Path {
                marker,
                points: std::mem::take(&mut self.current_path),
                color: self.color(),
                size: self.size,
            });
            self.redo.clear();
        } else {
            self.current_path.clear();
        }
    }

    // Drop a numbered marker at `at` and advance the counter.
    fn place_counter(&mut self, at: (i32, i32)) {
        self.annotations.push(Annotation::Counter {
            x: at.0,
            y: at.1,
            n: self.counter,
            color: self.color(),
            size: self.size,
        });
        self.counter += 1;
        self.redo.clear();
    }

    // Commit the text being typed.
    fn commit_text(&mut self) {
        if let Some((x, y, text)) = self.text_edit.take() {
            if !text.is_empty() {
                self.annotations.push(Annotation::Text {
                    x,
                    y,
                    text,
                    color: self.color(),
                    size: (self.size * 6.0).max(14.0),
                });
                self.redo.clear();
            }
        }
    }
}

// Map a keysym to a tool. Matches flameshot muscle memory:
// A arrow, R rectangle, C circle/ellipse, L line.
fn tool_for_sym(sym: u32) -> Option<ToolKind> {
    match sym {
        0x61 | 0x41 => Some(ToolKind::Arrow),     // a / A
        0x72 | 0x52 => Some(ToolKind::Rectangle),  // r / R
        0x63 | 0x43 => Some(ToolKind::Ellipse),    // c / C
        0x6c | 0x4c => Some(ToolKind::Line),       // l / L
        0x6d | 0x4d => Some(ToolKind::Marker),     // m / M
        0x70 | 0x50 => Some(ToolKind::Pencil),     // p / P
        0x74 | 0x54 => Some(ToolKind::Text),       // t / T
        0x62 | 0x42 => Some(ToolKind::Blur),       // b / B
        0x6e | 0x4e => Some(ToolKind::Counter),    // n / N
        _ => None,
    }
}

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
    let shot = torrentius::capture_root_to_surface(&conn, root)?;

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

    let result = event_loop(&conn, win, gc, w, h, depth, &shot, &keymap, opts);

    let _ = conn.ungrab_keyboard(x11rb::CURRENT_TIME);
    let _ = conn.ungrab_pointer(x11rb::CURRENT_TIME);
    let _ = conn.unmap_window(win);
    let _ = conn.destroy_window(win);
    let _ = conn.flush();
    result
}

fn event_loop(
    conn: &RustConnection,
    win: Window,
    gc: Gcontext,
    w: u32,
    h: u32,
    depth: u8,
    shot: &cairo::ImageSurface,
    keymap: &KeyMap,
    opts: &GuiOpts,
) -> Result<(), String> {
    let mut state = State {
        phase: Phase::Idle,
        ax: 0,
        ay: 0,
        bx: 0,
        by: 0,
        tool: None,
        annotations: Vec::new(),
        redo: Vec::new(),
        palette: tools::palette(opts.accent),
        color_idx: 0,
        size: 3.0,
        cursor: (w as i32 / 2, h as i32 / 2),
        pending: None,
        current_path: Vec::new(),
        text_edit: None,
        pointer_down: false,
        counter: 1,
    };
    redraw(conn, win, gc, w, h, depth, shot, &state, opts.accent)?;

    loop {
        let ev = conn.wait_for_event().map_err(|e| format!("wait_for_event: {}", e))?;
        match ev {
            Event::Expose(e) if e.count == 0 => {}
            Event::ButtonPress(e) if e.detail == 1 => {
                let pos = (e.event_x as i32, e.event_y as i32);
                state.cursor = pos;
                state.pointer_down = true;
                match state.tool {
                    Some(k) if k.is_two_point() => state.pending = Some(pos),
                    Some(k) if k.is_path() => state.current_path = vec![pos],
                    Some(ToolKind::Counter) => state.place_counter(pos),
                    Some(ToolKind::Text) => {
                        state.commit_text(); // finish any prior text first
                        state.text_edit = Some((pos.0, pos.1, String::new()));
                    }
                    Some(_) => {}
                    None => {
                        // Region: start a drag selection.
                        state.phase = Phase::Dragging;
                        state.ax = pos.0;
                        state.ay = pos.1;
                        state.bx = pos.0;
                        state.by = pos.1;
                    }
                }
            }
            Event::MotionNotify(e) => {
                let pos = (e.event_x as i32, e.event_y as i32);
                state.cursor = pos;
                if state.tool.is_none() && state.phase == Phase::Dragging {
                    state.bx = pos.0;
                    state.by = pos.1;
                } else if state.pointer_down
                    && state.tool.map(|k| k.is_path()).unwrap_or(false)
                {
                    state.current_path.push(pos);
                }
            }
            Event::ButtonRelease(e) if e.detail == 1 => {
                let pos = (e.event_x as i32, e.event_y as i32);
                state.cursor = pos;
                state.pointer_down = false;
                match state.tool {
                    Some(k) if k.is_two_point() => {
                        if state.pending.is_some() {
                            state.commit_shape(pos);
                        }
                    }
                    Some(k) if k.is_path() => state.commit_path(),
                    Some(_) => {}
                    None => {
                        if state.phase == Phase::Dragging {
                            state.bx = pos.0;
                            state.by = pos.1;
                            let (_, _, rw, rh) = state.rect();
                            state.phase = if rw >= MIN_SELECTION && rh >= MIN_SELECTION {
                                Phase::Selected
                            } else {
                                Phase::Idle
                            };
                        }
                    }
                }
            }
            Event::KeyPress(e) => {
                let ctrl = u16::from(e.state) & u16::from(ModMask::CONTROL) != 0;
                let shift = u16::from(e.state) & u16::from(ModMask::SHIFT) != 0;
                let sym = keycode_to_keysym(e.detail, shift, keymap);
                if let Some(outcome) =
                    on_key(&mut state, sym, ctrl, shift, w, h, shot, opts)?
                {
                    return outcome;
                }
            }
            _ => {}
        }
        redraw(conn, win, gc, w, h, depth, shot, &state, opts.accent)?;
    }
}

// Handle one keypress. Returns Some(result) to exit MEDUSA (export or
// cancel), None to keep going.
fn on_key(
    state: &mut State,
    sym: u32,
    ctrl: bool,
    shift: bool,
    w: u32,
    h: u32,
    shot: &cairo::ImageSurface,
    opts: &GuiOpts,
) -> Result<Option<Result<(), String>>, String> {
    // Region to export: the selection if set, else full screen.
    let rect = if state.phase == Phase::Selected {
        state.rect()
    } else {
        (0, 0, w as i32, h as i32)
    };

    // TEXT-INPUT MODE: while typing a text annotation, all printable
    // keys go into the buffer; only Enter (commit) / Esc (cancel) /
    // Backspace are special. This must run before the command keys so
    // typing 'a' inserts 'a' rather than arming the arrow tool.
    if state.text_edit.is_some() {
        match sym {
            0xff0d | 0xff8d => state.commit_text(),
            0xff1b => {
                state.text_edit = None; // cancel
            }
            0xff08 => {
                if let Some((_, _, t)) = state.text_edit.as_mut() {
                    t.pop();
                }
            }
            s if (0x20..=0x7e).contains(&s) => {
                if let Some((_, _, t)) = state.text_edit.as_mut() {
                    t.push(s as u8 as char);
                }
            }
            _ => {}
        }
        return Ok(None);
    }

    match sym {
        0xff1b => {
            // Escape: cancel in-progress placement first, else exit.
            if state.pending.take().is_some() || !state.current_path.is_empty() {
                state.current_path.clear();
                return Ok(None);
            }
            return Ok(Some(Ok(())));
        }
        0xff0d | 0xff8d => {
            // Return / KP_Enter: finish any in-progress path, then
            // export (copy + save).
            state.commit_path();
            return Ok(Some(export(shot, rect, &state.annotations, opts, true)));
        }
        0x73 | 0x53 if ctrl => {
            state.commit_path();
            return Ok(Some(export(shot, rect, &state.annotations, opts, false)));
        }
        0x70 if ctrl => {
            // Ctrl+P: pin the annotated crop to the screen + save.
            state.commit_path();
            return Ok(Some(pin(shot, rect, &state.annotations, opts)));
        }
        0x7a if ctrl => {
            // Ctrl+Z: undo.
            if let Some(a) = state.annotations.pop() {
                state.redo.push(a);
            }
        }
        0x79 | 0x72 if ctrl => {
            // Ctrl+Y / Ctrl+R: redo.
            if let Some(a) = state.redo.pop() {
                state.annotations.push(a);
            }
        }
        0x73 | 0x53 => {
            // 's': commit in-progress work, back to region-select mode.
            state.commit_path();
            state.tool = None;
            state.pending = None;
        }
        0x78 | 0x58 => {
            state.color_idx = (state.color_idx + 1) % tools::PALETTE_LEN;
        }
        0x5b => state.size = (state.size - SIZE_STEP).max(SIZE_MIN),
        0x5d => state.size = (state.size + SIZE_STEP).min(SIZE_MAX),
        0x20 => {
            // Space: place a point at the virtual cursor, per tool type.
            match state.tool {
                Some(k) if k.is_two_point() => {
                    if state.pending.is_none() {
                        state.pending = Some(state.cursor);
                    } else {
                        let end = state.cursor;
                        state.commit_shape(end);
                    }
                }
                Some(k) if k.is_path() => state.current_path.push(state.cursor),
                Some(ToolKind::Counter) => state.place_counter(state.cursor),
                Some(ToolKind::Text) => {
                    state.commit_text();
                    state.text_edit =
                        Some((state.cursor.0, state.cursor.1, String::new()));
                }
                _ => {}
            }
        }
        0xff51 | 0xff52 | 0xff53 | 0xff54 => {
            let step = if shift { 1 } else { CURSOR_STEP };
            let (mut cx, mut cy) = state.cursor;
            match sym {
                0xff51 => cx -= step,
                0xff53 => cx += step,
                0xff52 => cy -= step,
                0xff54 => cy += step,
                _ => {}
            }
            state.cursor = (cx.clamp(0, w as i32), cy.clamp(0, h as i32));
        }
        _ => {
            // Tool keys (a/r/c/l/m/p/t/b). Switching tools commits any
            // in-progress path. Arming with no region makes the whole
            // screen the canvas.
            if let Some(kind) = tool_for_sym(sym) {
                state.commit_path();
                if state.phase != Phase::Selected {
                    state.select_full(w, h);
                }
                state.tool = Some(kind);
                state.pending = None;
            }
        }
    }
    Ok(None)
}

// Crop the frozen surface to `rect` and replay annotations into it:
// pass 1 paints the region, pass 2 pixelates blur regions on the real
// pixels, pass 3 draws the rest on top. Shared by export + pin.
fn build_crop(
    shot: &cairo::ImageSurface,
    rect: (i32, i32, i32, i32),
    annotations: &[Annotation],
) -> Result<cairo::ImageSurface, String> {
    let (x, y, rw, rh) = rect;
    let mut cropped = cairo::ImageSurface::create(cairo::Format::Rgb24, rw, rh)
        .map_err(|e| format!("crop surface: {}", e))?;
    {
        let cr = cairo::Context::new(&cropped).map_err(|e| format!("crop ctx: {}", e))?;
        cr.set_source_surface(shot, -x as f64, -y as f64)
            .map_err(|e| format!("crop set_source: {}", e))?;
        let _ = cr.paint();
    }
    cropped.flush();

    for a in annotations {
        if let Annotation::Blur { x1, y1, x2, y2 } = a {
            tools::apply_blur(
                &mut cropped,
                x1.min(x2) - x,
                y1.min(y2) - y,
                (x2 - x1).abs(),
                (y2 - y1).abs(),
            );
        }
    }
    cropped.flush();

    let font_opts = torrentius::make_font_options()?;
    {
        let cr = cairo::Context::new(&cropped).map_err(|e| format!("crop ctx: {}", e))?;
        cr.rectangle(0.0, 0.0, rw as f64, rh as f64);
        cr.clip();
        cr.translate(-x as f64, -y as f64);
        for a in annotations {
            if !matches!(a, Annotation::Blur { .. }) {
                tools::draw(&cr, a, &font_opts);
            }
        }
    }
    cropped.flush();
    Ok(cropped)
}

// Build the annotated crop, write a PNG, and optionally copy it.
fn export(
    shot: &cairo::ImageSurface,
    rect: (i32, i32, i32, i32),
    annotations: &[Annotation],
    opts: &GuiOpts,
    copy: bool,
) -> Result<(), String> {
    let (_, _, rw, rh) = rect;
    if rw < MIN_SELECTION || rh < MIN_SELECTION {
        return Ok(());
    }
    let cropped = build_crop(shot, rect, annotations)?;
    let path = opts.save_dir.join(format!("medusa-{}.png", timestamp()));
    torrentius::surface_to_png(&cropped, &path)?;
    eprintln!("[medusa] saved {}", path.display());
    if copy && opts.copy {
        copy_to_clipboard(&path);
    }
    Ok(())
}

// Pin the annotated crop to the screen as a floating window. Forks a
// detached holder so MEDUSA can exit; the pin stays put at the
// region's location until any key or click dismisses it. The crop is
// also saved to disk.
fn pin(
    shot: &cairo::ImageSurface,
    rect: (i32, i32, i32, i32),
    annotations: &[Annotation],
    opts: &GuiOpts,
) -> Result<(), String> {
    let (x, y, rw, rh) = rect;
    if rw < MIN_SELECTION || rh < MIN_SELECTION {
        return Ok(());
    }
    let cropped = build_crop(shot, rect, annotations)?;
    // Save a copy too, so a pin is never the only record.
    let path = opts.save_dir.join(format!("medusa-{}.png", timestamp()));
    let _ = torrentius::surface_to_png(&cropped, &path);

    let pid = unsafe { libc::fork() };
    if pid != 0 {
        // Parent (or fork failure): the overlay exits; the pin runs in
        // the child if the fork succeeded.
        return Ok(());
    }
    unsafe {
        libc::setsid();
    }
    let _ = pin_window(cropped, x, y, rw, rh);
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

fn copy_to_clipboard(path: &Path) {
    // DEMIURGE's own clipboard holder (zero deps): forks a process that
    // owns the X11 CLIPBOARD selection and serves the image until
    // another client takes it. Falls back to xclip only if the fork
    // fails. The file is saved regardless.
    if crate::medusa::clipboard::copy_png(path) {
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
    shot: &cairo::ImageSurface,
    state: &State,
    accent: (f64, f64, f64),
) -> Result<(), String> {
    let (surface, cr) = torrentius::new_surface(w as i32, h as i32)?;
    let font_opts = torrentius::make_font_options()?;
    cr.set_font_options(&font_opts);

    // Frozen backdrop, then a dim wash over everything.
    cr.set_source_surface(shot, 0.0, 0.0)
        .map_err(|e| format!("set_source shot: {}", e))?;
    let _ = cr.paint();
    cr.set_source_rgba(0.0, 0.0, 0.0, DIM_ALPHA);
    let _ = cr.paint();

    let has_sel = state.phase != Phase::Idle;
    if has_sel {
        let (x, y, rw, rh) = state.rect();
        let (fx, fy, fw, fh) = (x as f64, y as f64, rw as f64, rh as f64);

        // Bright selection: repaint the frozen surface clipped to the
        // region so it shows at full brightness over the dim wash.
        cr.save().ok();
        cr.rectangle(fx, fy, fw, fh);
        cr.clip();
        cr.set_source_surface(shot, 0.0, 0.0)
            .map_err(|e| format!("set_source sel: {}", e))?;
        let _ = cr.paint();
        cr.restore().ok();

        // Annotations (committed + in-progress preview), clipped to the
        // selection so nothing spills past it.
        cr.save().ok();
        cr.rectangle(fx, fy, fw, fh);
        cr.clip();
        for a in &state.annotations {
            tools::draw(&cr, a, &font_opts);
        }
        // Live preview of the in-progress annotation, per tool type.
        let col = state.color();
        match state.tool {
            Some(k) if k.is_two_point() => {
                if let Some(start) = state.pending {
                    let prev = if k == ToolKind::Blur {
                        Annotation::Blur {
                            x1: start.0,
                            y1: start.1,
                            x2: state.cursor.0,
                            y2: state.cursor.1,
                        }
                    } else {
                        Annotation::Shape {
                            kind: k,
                            x1: start.0,
                            y1: start.1,
                            x2: state.cursor.0,
                            y2: state.cursor.1,
                            color: col,
                            size: state.size,
                        }
                    };
                    tools::draw(&cr, &prev, &font_opts);
                }
            }
            Some(k) if k.is_path() => {
                if !state.current_path.is_empty() {
                    let mut pts = state.current_path.clone();
                    pts.push(state.cursor);
                    tools::draw(
                        &cr,
                        &Annotation::Path {
                            marker: k == ToolKind::Marker,
                            points: pts,
                            color: col,
                            size: state.size,
                        },
                        &font_opts,
                    );
                }
            }
            _ => {}
        }
        // Text being typed: show the buffer with a caret.
        if let Some((tx, ty, t)) = &state.text_edit {
            tools::draw(
                &cr,
                &Annotation::Text {
                    x: *tx,
                    y: *ty,
                    text: format!("{}|", t),
                    color: col,
                    size: (state.size * 6.0).max(14.0),
                },
                &font_opts,
            );
        }
        cr.restore().ok();

        // Border.
        cr.set_source_rgb(accent.0, accent.1, accent.2);
        cr.set_line_width(1.0);
        cr.rectangle(fx + 0.5, fy + 0.5, fw - 1.0, fh - 1.0);
        let _ = cr.stroke();

        // Corner + midpoint handles.
        draw_handles(&cr, fx, fy, fw, fh, accent);

        // Dimension label, just above the selection (or inside if it
        // would clip off the top edge).
        let label = format!("{} x {}", rw, rh);
        let font_desc = pango::FontDescription::from_string("monospace 10");
        let layout = pangocairo::functions::create_layout(&cr);
        layout.set_font_description(Some(&font_desc));
        pangocairo::functions::context_set_font_options(
            &layout.context(),
            Some(&font_opts),
        );
        layout.set_text(&label);
        let (lw, lh) = layout.pixel_size();
        let lx = fx;
        let mut ly = fy - lh as f64 - 4.0;
        if ly < 2.0 {
            ly = fy + 4.0;
        }
        // Label background chip for legibility.
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.7);
        cr.rectangle(lx - 3.0, ly - 2.0, lw as f64 + 6.0, lh as f64 + 4.0);
        let _ = cr.fill();
        cr.set_source_rgb(accent.0, accent.1, accent.2);
        cr.move_to(lx, ly);
        pangocairo::functions::show_layout(&cr, &layout);
    }

    // Virtual annotation cursor (a small crosshair in the current
    // colour) + the tool status line, shown while a tool is armed.
    if let Some(kind) = state.tool {
        let (cx, cy) = (state.cursor.0 as f64, state.cursor.1 as f64);
        let col = state.color();
        cr.set_source_rgb(col.0, col.1, col.2);
        cr.set_line_width(1.0);
        cr.move_to(cx - 7.0, cy);
        cr.line_to(cx + 7.0, cy);
        cr.move_to(cx, cy - 7.0);
        cr.line_to(cx, cy + 7.0);
        let _ = cr.stroke();
        draw_tool_status(&cr, w, h, &font_opts, kind, col, state.size);
    }

    // Hint bar, top-center.
    draw_hint(&cr, w, &font_opts, has_sel);

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

fn draw_handles(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, accent: (f64, f64, f64)) {
    let s = 3.0; // half-size of each handle square
    let pts = [
        (x, y),
        (x + w / 2.0, y),
        (x + w, y),
        (x, y + h / 2.0),
        (x + w, y + h / 2.0),
        (x, y + h),
        (x + w / 2.0, y + h),
        (x + w, y + h),
    ];
    cr.set_source_rgb(accent.0, accent.1, accent.2);
    for (px, py) in pts {
        cr.rectangle(px - s, py - s, s * 2.0, s * 2.0);
        let _ = cr.fill();
    }
}

// Bottom-center status while a tool is armed: the tool name, a colour
// swatch, the stroke size, and the annotation keys.
fn draw_tool_status(
    cr: &cairo::Context,
    w: u32,
    h: u32,
    font_opts: &cairo::FontOptions,
    kind: ToolKind,
    color: (f64, f64, f64),
    size: f64,
) {
    let text = format!(
        "tool {}   size {:.0}    space place · arrows move · x colour · [ ] size · Ctrl+Z undo · s region · Enter done",
        kind.label(),
        size,
    );
    let font_desc = pango::FontDescription::from_string("monospace 10");
    let layout = pangocairo::functions::create_layout(cr);
    layout.set_font_description(Some(&font_desc));
    pangocairo::functions::context_set_font_options(&layout.context(), Some(font_opts));
    layout.set_text(&text);
    let (lw, lh) = layout.pixel_size();
    let swatch = lh as f64;
    let total_w = lw as f64 + swatch + 8.0;
    let bx = (w as f64 - total_w) / 2.0;
    let by = h as f64 - lh as f64 - 22.0;
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.7);
    cr.rectangle(bx - 10.0, by - 5.0, total_w + 20.0, lh as f64 + 10.0);
    let _ = cr.fill();
    // Colour swatch.
    cr.set_source_rgb(color.0, color.1, color.2);
    cr.rectangle(bx, by, swatch, swatch);
    let _ = cr.fill();
    cr.set_source_rgb(0.85, 0.85, 0.85);
    cr.move_to(bx + swatch + 8.0, by);
    pangocairo::functions::show_layout(cr, &layout);
}

fn draw_hint(cr: &cairo::Context, w: u32, font_opts: &cairo::FontOptions, has_sel: bool) {
    let text = if has_sel {
        "a r c l m p t b n tools  ·  Enter copy+save  ·  Ctrl+S save  ·  Ctrl+P pin  ·  s reselect  ·  Esc cancel"
    } else {
        "Enter full screen  ·  a r c l m p t b n annotate  ·  Ctrl+S save  ·  Ctrl+P pin  ·  Esc cancel"
    };
    let font_desc = pango::FontDescription::from_string("monospace 10");
    let layout = pangocairo::functions::create_layout(cr);
    layout.set_font_description(Some(&font_desc));
    pangocairo::functions::context_set_font_options(&layout.context(), Some(font_opts));
    layout.set_text(text);
    let (lw, lh) = layout.pixel_size();
    let bx = (w as f64 - lw as f64) / 2.0;
    let by = 18.0;
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.7);
    cr.rectangle(bx - 10.0, by - 5.0, lw as f64 + 20.0, lh as f64 + 10.0);
    let _ = cr.fill();
    cr.set_source_rgb(0.85, 0.85, 0.85);
    cr.move_to(bx, by);
    pangocairo::functions::show_layout(cr, &layout);
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

// Re-exports for the `medusa full` path in bin/medusa.rs, which
// captures + saves without going through the overlay.
pub fn timestamp_pub() -> String {
    timestamp()
}
pub fn copy_to_clipboard_pub(path: &Path) {
    copy_to_clipboard(path);
}

fn keycode_to_keysym(keycode: u8, shift: bool, keymap: &KeyMap) -> u32 {
    if keycode < keymap.min_keycode {
        return 0;
    }
    let base = (keycode - keymap.min_keycode) as usize * keymap.syms_per_code;
    let idx = base + if shift && keymap.syms_per_code > 1 { 1 } else { 0 };
    keymap.keysyms.get(idx).copied().unwrap_or(0)
}

// Local-time "YYYY-MM-DD_HH-MM-SS" via libc strftime, matching the
// house pattern (no chrono dependency).
fn timestamp() -> String {
    unsafe {
        let mut buf = [0i8; 32];
        let mut tm: libc::tm = std::mem::zeroed();
        let now = libc::time(std::ptr::null_mut());
        libc::localtime_r(&now, &mut tm);
        let fmt = std::ffi::CString::new("%Y-%m-%d_%H-%M-%S").unwrap();
        let n = libc::strftime(buf.as_mut_ptr(), buf.len(), fmt.as_ptr(), &tm);
        if n == 0 {
            return "capture".to_string();
        }
        std::ffi::CStr::from_ptr(buf.as_ptr())
            .to_string_lossy()
            .into_owned()
    }
}
