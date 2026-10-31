// MEDUSA's overlay, as both stacks' binaries show it: The frozen desktop,
// the selection over it, the annotation tools and what the keys do, drawn
// into whatever part of the desktop a surface shows.
//
// Flow:
//   1. The binary freezes the desktop into a Cairo surface (the backdrop).
//      Everything drawn afterward composites over this still image, so
//      the screen appears to freeze the instant MEDUSA launches.
//   2. It shows the overlay over every monitor, holding the keyboard and
//      pointer, with a crosshair cursor, and hands their events here.
//   3. A drag selects a rectangle. The selection renders at full
//      brightness; everything outside dims. Border + corner handles + a
//      WxH dimension label track the drag.
//   4. Enter copies the cropped region to the clipboard AND saves it to
//      disk; Ctrl+S saves only; Ctrl+P pins it; Esc cancels. On accept the
//      frozen surface is cropped to the selection, its annotations
//      replayed into it, and a PNG written.

use std::path::{Path, PathBuf};

use crate::tools::{self, Annotation, ToolKind};
use demiurge_core::torrentius;

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

/// What a key asks of the binary.
pub enum Key {
    Continue,
    /// Close the overlay with nothing kept.
    Cancel,
    /// Save the annotated region, and copy it when `copy` and the options
    /// allow.
    Save { copy: bool },
    /// Save the annotated region and pin it to the screen.
    Pin,
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

/// The overlay over a frozen desktop `w` by `h`, in desktop pixels.
pub struct Overlay {
    shot: cairo::ImageSurface,
    w: u32,
    h: u32,
    accent: (f64, f64, f64),
    phase: Phase,
    // Anchor + current corner of the drag.
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
    // of a two-point shape in progress.
    cursor: (i32, i32),
    pending: Option<(i32, i32)>,
    // Freehand path being drawn (marker / pencil): accumulated points.
    current_path: Vec<(i32, i32)>,
    // Text being typed: insertion point + buffer. Some = text-input
    // mode, which captures all printable keys.
    text_edit: Option<(i32, i32, String)>,
    // Whether the mouse button is currently held (gates path-tool
    // point sampling during motion).
    pointer_down: bool,
    // Next number for the Counter tool. Increments on each placement.
    counter: u32,
}

impl Overlay {
    pub fn new(shot: cairo::ImageSurface, accent: (f64, f64, f64)) -> Self {
        let (w, h) = (shot.width() as u32, shot.height() as u32);
        Overlay {
            shot,
            w,
            h,
            accent,
            phase: Phase::Idle,
            ax: 0,
            ay: 0,
            bx: 0,
            by: 0,
            tool: None,
            annotations: Vec::new(),
            redo: Vec::new(),
            palette: tools::palette(accent),
            color_idx: 0,
            size: 3.0,
            cursor: (w as i32 / 2, h as i32 / 2),
            pending: None,
            current_path: Vec::new(),
            text_edit: None,
            pointer_down: false,
            counter: 1,
        }
    }

    // Normalized (x, y, w, h) of the current selection.
    fn rect(&self) -> (i32, i32, i32, i32) {
        let x = self.ax.min(self.bx);
        let y = self.ay.min(self.by);
        let w = (self.ax - self.bx).abs();
        let h = (self.ay - self.by).abs();
        (x, y, w, h)
    }

    // The region an export takes: The selection if set, else the desktop.
    fn region(&self) -> (i32, i32, i32, i32) {
        if self.phase == Phase::Selected { self.rect() } else { (0, 0, self.w as i32, self.h as i32) }
    }

    fn color(&self) -> (f64, f64, f64) {
        self.palette[self.color_idx]
    }

    // Make the whole desktop the canvas. Used when a tool is armed with no
    // region selected yet -- annotate full-screen, mouseless.
    fn select_full(&mut self) {
        self.ax = 0;
        self.ay = 0;
        self.bx = self.w as i32;
        self.by = self.h as i32;
        self.phase = Phase::Selected;
    }

    // Commit a two-point shape (or blur) from `pending` to `end`.
    fn commit_shape(&mut self, end: (i32, i32)) {
        if let (Some(start), Some(kind)) = (self.pending.take(), self.tool) {
            let ann = if kind == ToolKind::Blur {
                Annotation::Blur { x1: start.0, y1: start.1, x2: end.0, y2: end.1 }
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
        if !self.current_path.is_empty() {
            let marker = self.tool == Some(ToolKind::Marker);
            self.annotations.push(Annotation::Path {
                marker,
                points: std::mem::take(&mut self.current_path),
                color: self.color(),
                size: self.size,
            });
            self.redo.clear();
        }
    }

    // Drop a numbered marker at `at` and advance the counter.
    fn place_counter(&mut self, at: (i32, i32)) {
        self.annotations.push(Annotation::Counter { x: at.0, y: at.1, n: self.counter, color: self.color(), size: self.size });
        self.counter += 1;
        self.redo.clear();
    }

    // Commit the text being typed.
    fn commit_text(&mut self) {
        if let Some((x, y, text)) = self.text_edit.take()
            && !text.is_empty()
        {
            self.annotations.push(Annotation::Text { x, y, text, color: self.color(), size: (self.size * 6.0).max(14.0) });
            self.redo.clear();
        }
    }

    /// The pointer's button went down at `pos`, in desktop pixels.
    pub fn press(&mut self, pos: (i32, i32)) {
        self.cursor = pos;
        self.pointer_down = true;
        match self.tool {
            Some(k) if k.is_two_point() => self.pending = Some(pos),
            Some(k) if k.is_path() => self.current_path = vec![pos],
            Some(ToolKind::Counter) => self.place_counter(pos),
            Some(ToolKind::Text) => {
                self.commit_text(); // finish any prior text first
                self.text_edit = Some((pos.0, pos.1, String::new()));
            }
            Some(_) => {}
            None => {
                // Region: start a drag selection.
                self.phase = Phase::Dragging;
                (self.ax, self.ay, self.bx, self.by) = (pos.0, pos.1, pos.0, pos.1);
            }
        }
    }

    /// The pointer moved to `pos`.
    pub fn motion(&mut self, pos: (i32, i32)) {
        self.cursor = pos;
        if self.tool.is_none() && self.phase == Phase::Dragging {
            (self.bx, self.by) = pos;
        } else if self.pointer_down && self.tool.is_some_and(|k| k.is_path()) {
            self.current_path.push(pos);
        }
    }

    /// The pointer's button came up at `pos`.
    pub fn release(&mut self, pos: (i32, i32)) {
        self.cursor = pos;
        self.pointer_down = false;
        match self.tool {
            Some(k) if k.is_two_point() => {
                if self.pending.is_some() {
                    self.commit_shape(pos);
                }
            }
            Some(k) if k.is_path() => self.commit_path(),
            Some(_) => {}
            None => {
                if self.phase == Phase::Dragging {
                    (self.bx, self.by) = pos;
                    let (_, _, rw, rh) = self.rect();
                    self.phase = if rw >= MIN_SELECTION && rh >= MIN_SELECTION { Phase::Selected } else { Phase::Idle };
                }
            }
        }
    }

    /// A key, by its keysym with Ctrl and Shift as held.
    pub fn key(&mut self, sym: u32, ctrl: bool, shift: bool) -> Key {
        // TEXT-INPUT MODE: while typing a text annotation, all printable
        // keys go into the buffer; only Enter (commit) / Esc (cancel) /
        // Backspace are special. This must run before the command keys so
        // typing 'a' inserts 'a' rather than arming the arrow tool.
        if let Some((_, _, text)) = self.text_edit.as_mut() {
            match sym {
                0xff0d | 0xff8d => self.commit_text(),
                0xff1b => self.text_edit = None, // cancel
                0xff08 => {
                    text.pop();
                }
                s if (0x20..=0x7e).contains(&s) => text.push(s as u8 as char),
                _ => {}
            }
            return Key::Continue;
        }

        match sym {
            0xff1b => {
                // Escape: cancel in-progress placement first, else exit.
                if self.pending.take().is_some() || !self.current_path.is_empty() {
                    self.current_path.clear();
                    return Key::Continue;
                }
                return Key::Cancel;
            }
            0xff0d | 0xff8d => {
                // Return / KP_Enter: finish any in-progress path, then
                // export (copy + save).
                self.commit_path();
                return Key::Save { copy: true };
            }
            0x73 | 0x53 if ctrl => {
                self.commit_path();
                return Key::Save { copy: false };
            }
            0x70 if ctrl => {
                // Ctrl+P: pin the annotated crop to the screen + save.
                self.commit_path();
                return Key::Pin;
            }
            0x7a if ctrl => {
                // Ctrl+Z: undo.
                if let Some(a) = self.annotations.pop() {
                    self.redo.push(a);
                }
            }
            0x79 | 0x72 if ctrl => {
                // Ctrl+Y / Ctrl+R: redo.
                if let Some(a) = self.redo.pop() {
                    self.annotations.push(a);
                }
            }
            0x73 | 0x53 => {
                // 's': commit in-progress work, back to region-select mode.
                self.commit_path();
                self.tool = None;
                self.pending = None;
            }
            0x78 | 0x58 => self.color_idx = (self.color_idx + 1) % tools::PALETTE_LEN,
            0x5b => self.size = (self.size - SIZE_STEP).max(SIZE_MIN),
            0x5d => self.size = (self.size + SIZE_STEP).min(SIZE_MAX),
            0x20 => {
                // Space: place a point at the virtual cursor, per tool type.
                match self.tool {
                    Some(k) if k.is_two_point() => {
                        if self.pending.is_none() {
                            self.pending = Some(self.cursor);
                        } else {
                            let end = self.cursor;
                            self.commit_shape(end);
                        }
                    }
                    Some(k) if k.is_path() => self.current_path.push(self.cursor),
                    Some(ToolKind::Counter) => self.place_counter(self.cursor),
                    Some(ToolKind::Text) => {
                        self.commit_text();
                        self.text_edit = Some((self.cursor.0, self.cursor.1, String::new()));
                    }
                    _ => {}
                }
            }
            0xff51 | 0xff52 | 0xff53 | 0xff54 => {
                let step = if shift { 1 } else { CURSOR_STEP };
                let (mut cx, mut cy) = self.cursor;
                match sym {
                    0xff51 => cx -= step,
                    0xff53 => cx += step,
                    0xff52 => cy -= step,
                    0xff54 => cy += step,
                    _ => {}
                }
                self.cursor = (cx.clamp(0, self.w as i32), cy.clamp(0, self.h as i32));
            }
            _ => {
                // Tool keys (a/r/c/l/m/p/t/b/n). Switching tools commits
                // any in-progress path. Arming with no region makes the
                // whole desktop the canvas.
                if let Some(kind) = tool_for_sym(sym) {
                    self.commit_path();
                    if self.phase != Phase::Selected {
                        self.select_full();
                    }
                    self.tool = Some(kind);
                    self.pending = None;
                }
            }
        }
        Key::Continue
    }

    /// The annotated region and where it sits on the desktop, None for a
    /// region too small to keep: The frozen surface cropped to it, blur
    /// regions pixelated on the real pixels, the rest drawn on top.
    pub fn crop(&self) -> Result<Option<(cairo::ImageSurface, (i32, i32, i32, i32))>, String> {
        let rect = self.region();
        let (x, y, rw, rh) = rect;
        if rw < MIN_SELECTION || rh < MIN_SELECTION {
            return Ok(None);
        }
        let mut cropped =
            cairo::ImageSurface::create(cairo::Format::Rgb24, rw, rh).map_err(|e| format!("crop surface: {}", e))?;
        {
            let cr = cairo::Context::new(&cropped).map_err(|e| format!("crop ctx: {}", e))?;
            cr.set_source_surface(&self.shot, -x as f64, -y as f64).map_err(|e| format!("crop set_source: {}", e))?;
            let _ = cr.paint();
        }
        cropped.flush();

        for a in &self.annotations {
            if let Annotation::Blur { x1, y1, x2, y2 } = a {
                tools::apply_blur(&mut cropped, x1.min(x2) - x, y1.min(y2) - y, (x2 - x1).abs(), (y2 - y1).abs());
            }
        }
        cropped.flush();

        let font_opts = torrentius::make_font_options()?;
        {
            let cr = cairo::Context::new(&cropped).map_err(|e| format!("crop ctx: {}", e))?;
            cr.rectangle(0.0, 0.0, rw as f64, rh as f64);
            cr.clip();
            cr.translate(-x as f64, -y as f64);
            for a in &self.annotations {
                if !matches!(a, Annotation::Blur { .. }) {
                    tools::draw(&cr, a, &font_opts);
                }
            }
        }
        cropped.flush();
        Ok(Some((cropped, rect)))
    }

    /// Draw the part of the overlay the desktop rectangle `view` shows,
    /// with `view`'s top-left at the context's origin.
    pub fn draw(&self, cr: &cairo::Context, view: (i32, i32, i32, i32)) -> Result<(), String> {
        let (vx, vy, vw, vh) = view;
        let font_opts = torrentius::make_font_options()?;
        cr.set_font_options(&font_opts);
        cr.save().ok();
        cr.translate(-vx as f64, -vy as f64);

        // Frozen backdrop, then a dim wash over everything.
        cr.set_source_surface(&self.shot, 0.0, 0.0).map_err(|e| format!("set_source shot: {}", e))?;
        let _ = cr.paint();
        cr.set_source_rgba(0.0, 0.0, 0.0, DIM_ALPHA);
        let _ = cr.paint();

        let has_sel = self.phase != Phase::Idle;
        if has_sel {
            self.draw_selection(cr, &font_opts)?;
        }

        // Virtual annotation cursor (a small crosshair in the current
        // colour), shown while a tool is armed.
        if self.tool.is_some() {
            let (cx, cy) = (self.cursor.0 as f64, self.cursor.1 as f64);
            let col = self.color();
            cr.set_source_rgb(col.0, col.1, col.2);
            cr.set_line_width(1.0);
            cr.move_to(cx - 7.0, cy);
            cr.line_to(cx + 7.0, cy);
            cr.move_to(cx, cy - 7.0);
            cr.line_to(cx, cy + 7.0);
            let _ = cr.stroke();
        }
        cr.restore().ok();

        // The tool status and the hint bar, on each view's own center.
        if let Some(kind) = self.tool {
            draw_tool_status(cr, vw as u32, vh as u32, &font_opts, kind, self.color(), self.size);
        }
        draw_hint(cr, vw as u32, &font_opts, has_sel);
        Ok(())
    }

    fn draw_selection(&self, cr: &cairo::Context, font_opts: &cairo::FontOptions) -> Result<(), String> {
        let (x, y, rw, rh) = self.rect();
        let (fx, fy, fw, fh) = (x as f64, y as f64, rw as f64, rh as f64);
        let accent = self.accent;

        // Bright selection: repaint the frozen surface clipped to the
        // region so it shows at full brightness over the dim wash.
        cr.save().ok();
        cr.rectangle(fx, fy, fw, fh);
        cr.clip();
        cr.set_source_surface(&self.shot, 0.0, 0.0).map_err(|e| format!("set_source sel: {}", e))?;
        let _ = cr.paint();
        cr.restore().ok();

        // Annotations (committed + in-progress preview), clipped to the
        // selection so nothing spills past it.
        cr.save().ok();
        cr.rectangle(fx, fy, fw, fh);
        cr.clip();
        for a in &self.annotations {
            tools::draw(cr, a, font_opts);
        }
        // Live preview of the in-progress annotation, per tool type.
        let col = self.color();
        match self.tool {
            Some(k) if k.is_two_point() => {
                if let Some(start) = self.pending {
                    let prev = if k == ToolKind::Blur {
                        Annotation::Blur { x1: start.0, y1: start.1, x2: self.cursor.0, y2: self.cursor.1 }
                    } else {
                        Annotation::Shape {
                            kind: k,
                            x1: start.0,
                            y1: start.1,
                            x2: self.cursor.0,
                            y2: self.cursor.1,
                            color: col,
                            size: self.size,
                        }
                    };
                    tools::draw(cr, &prev, font_opts);
                }
            }
            Some(k) if k.is_path() => {
                if !self.current_path.is_empty() {
                    let mut pts = self.current_path.clone();
                    pts.push(self.cursor);
                    tools::draw(
                        cr,
                        &Annotation::Path { marker: k == ToolKind::Marker, points: pts, color: col, size: self.size },
                        font_opts,
                    );
                }
            }
            _ => {}
        }
        // Text being typed: show the buffer with a caret.
        if let Some((tx, ty, t)) = &self.text_edit {
            tools::draw(
                cr,
                &Annotation::Text { x: *tx, y: *ty, text: format!("{}|", t), color: col, size: (self.size * 6.0).max(14.0) },
                font_opts,
            );
        }
        cr.restore().ok();

        // Border.
        cr.set_source_rgb(accent.0, accent.1, accent.2);
        cr.set_line_width(1.0);
        cr.rectangle(fx + 0.5, fy + 0.5, fw - 1.0, fh - 1.0);
        let _ = cr.stroke();

        // Corner + midpoint handles.
        draw_handles(cr, fx, fy, fw, fh, accent);

        // Dimension label, just above the selection (or inside if it
        // would clip off the top edge).
        let label = format!("{} x {}", rw, rh);
        let font_desc = pango::FontDescription::from_string("monospace 10");
        let layout = pangocairo::functions::create_layout(cr);
        layout.set_font_description(Some(&font_desc));
        pangocairo::functions::context_set_font_options(&layout.context(), Some(font_opts));
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
        pangocairo::functions::show_layout(cr, &layout);
        Ok(())
    }
}

// Map a keysym to a tool. Matches flameshot muscle memory:
// A arrow, R rectangle, C circle/ellipse, L line.
fn tool_for_sym(sym: u32) -> Option<ToolKind> {
    match sym {
        0x61 | 0x41 => Some(ToolKind::Arrow),     // a / A
        0x72 | 0x52 => Some(ToolKind::Rectangle), // r / R
        0x63 | 0x43 => Some(ToolKind::Ellipse),   // c / C
        0x6c | 0x4c => Some(ToolKind::Line),      // l / L
        0x6d | 0x4d => Some(ToolKind::Marker),    // m / M
        0x70 | 0x50 => Some(ToolKind::Pencil),    // p / P
        0x74 | 0x54 => Some(ToolKind::Text),      // t / T
        0x62 | 0x42 => Some(ToolKind::Blur),      // b / B
        0x6e | 0x4e => Some(ToolKind::Counter),   // n / N
        _ => None,
    }
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

/// Write `shot` into `dir` as medusa-<local time>.png.
pub fn save(shot: &cairo::ImageSurface, dir: &Path) -> Result<PathBuf, String> {
    let path = dir.join(format!("medusa-{}.png", demiurge_core::clock::now("%Y-%m-%d_%H-%M-%S")));
    torrentius::surface_to_png(shot, &path)?;
    eprintln!("[medusa] saved {}", path.display());
    Ok(path)
}
