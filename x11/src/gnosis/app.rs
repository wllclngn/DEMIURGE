// GNOSIS app: window, event loop, render, keyboard navigation.
//
// A normal top-level window (the WM manages / tiles it -- not
// override-redirect like the lock or MEDUSA). Renders the current
// directory as a scrolling list with a breadcrumb header, the
// selection highlighted in [appearance].accent.

use std::collections::HashMap;
use std::os::fd::AsRawFd;
use std::path::PathBuf;

use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::shape::{self, ConnectionExt as _};
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;

use crate::config::Config;
use crate::gnosis::fs::{self, Entry};
use crate::keys::{self, KeyMap};
use crate::torrentius;

const HEADER_H: f64 = 30.0;
const ROW_PAD: f64 = 4.0;
// macOS-style titlebar: a strip above the breadcrumb header holding the
// traffic-light buttons and the centered title.
const TITLEBAR_H: f64 = 30.0;
// Window-corner rounding radius (applied via the SHAPE extension).
const CORNER_R: i16 = 9;
// Traffic-light geometry: three circles from the left of the titlebar.
const LIGHT_R: f64 = 6.0;
const LIGHT_Y: f64 = TITLEBAR_H / 2.0;
const LIGHT_X: [f64; 3] = [18.0, 38.0, 58.0]; // close, minimize, zoom
const LIGHT_CLOSE: (f64, f64, f64) = (0.99, 0.32, 0.30);
const LIGHT_MIN: (f64, f64, f64) = (0.99, 0.74, 0.18);
const LIGHT_ZOOM: (f64, f64, f64) = (0.18, 0.78, 0.25);

struct Theme {
    bg: (f64, f64, f64),
    fg: (f64, f64, f64),
    dir_fg: (f64, f64, f64),
    accent: (f64, f64, f64),
    sel_fg: (f64, f64, f64),
    font: String,
}

pub struct App {
    conn: RustConnection,
    win: Window,
    root: Window,
    gc: Gcontext,
    depth: u8,
    width: u16,
    height: u16,
    cwd: PathBuf,
    entries: Vec<Entry>,
    selected: usize,
    scroll: usize,
    show_hidden: bool,
    theme: Theme,
    keymap: KeyMap,
    // W10 folder-preview cache: folder path -> (first-N is_dir flags,
    // total visible count). Populated lazily for visible folder rows
    // only; cleared when the directory changes.
    peek_cache: HashMap<PathBuf, (Vec<bool>, usize)>,
    // Display order: indices into `entries`, filtered by `filter`.
    // `selected` / `scroll` index into this view, not `entries`.
    view: Vec<usize>,
    // Incremental type-to-jump filter. Some = filter mode (typing
    // edits the query); the view narrows to matching names.
    filter: Option<String>,
    // Yanked path awaiting paste (copy).
    yank: Option<PathBuf>,
    // Bookmark picker overlay: (entries, selected). Some = picker open.
    picker: Option<(Vec<(String, PathBuf)>, usize)>,
    // Transient status message (e.g. "trashed foo", "pasted bar").
    status: String,
}

// Number of preview cards a folder shows.
const PREVIEW_CARDS: usize = 4;
// Width reserved at the left of each row for the preview stack.
const PREVIEW_W: f64 = 26.0;

impl App {
    pub fn run(config: &Config, start: PathBuf) -> Result<(), String> {
        let (conn, screen_num) =
            RustConnection::connect(None).map_err(|e| format!("x11 connect: {}", e))?;
        let screen = &conn.setup().roots[screen_num];
        let root = screen.root;
        let depth = screen.root_depth;
        let (width, height) = (820u16, 560u16);

        let win = conn.generate_id().map_err(|e| format!("gen id: {}", e))?;
        let gc = conn.generate_id().map_err(|e| format!("gen gc: {}", e))?;
        conn.create_window(
            depth,
            win,
            root,
            0,
            0,
            width,
            height,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new().background_pixel(screen.black_pixel).event_mask(
                EventMask::KEY_PRESS
                    | EventMask::BUTTON_PRESS
                    | EventMask::EXPOSURE
                    | EventMask::STRUCTURE_NOTIFY,
            ),
        )
        .map_err(|e| format!("create_window: {}", e))?;
        conn.create_gc(gc, win, &CreateGCAux::new())
            .map_err(|e| format!("create_gc: {}", e))?;

        // Window manager hints: title + class, so DEMIURGE (and
        // cherrypie matching) can target it.
        set_wm_name(&conn, win, "GNOSIS");
        set_wm_class(&conn, win, "gnosis");

        let _ = conn.map_window(win);
        let _ = conn.flush();

        let theme = Theme {
            bg: torrentius::parse_hex(&config.bar.bg),
            fg: torrentius::parse_hex(&config.bar.fg),
            dir_fg: torrentius::parse_hex(&config.appearance.accent),
            accent: torrentius::parse_hex(&config.appearance.accent),
            sel_fg: (0.97, 0.97, 0.97),
            font: config.font.default.clone(),
        };
        let keymap = keys::load_keymap(&conn);

        let mut app = App {
            conn,
            win,
            root,
            gc,
            depth,
            width,
            height,
            cwd: start.clone(),
            entries: fs::read_dir(&start, false),
            selected: 0,
            scroll: 0,
            show_hidden: false,
            theme,
            keymap,
            peek_cache: HashMap::new(),
            view: Vec::new(),
            filter: None,
            yank: None,
            picker: None,
            status: String::new(),
        };
        app.rebuild_view();
        app.apply_shape();
        app.event_loop()
    }

    // Round the window's outer corners via the SHAPE extension, for the
    // macOS look. Best-effort: a no-op if SHAPE is unavailable. Builds a
    // 1-bit mask (body rectangles + quarter-circle corners) and sets it
    // as the window's bounding shape. Recomputed on resize.
    fn apply_shape(&self) {
        let w = self.width;
        let h = self.height;
        let r = CORNER_R as u16;
        if w < r * 2 || h < r * 2 {
            return;
        }
        if self.conn.shape_query_version().ok().and_then(|c| c.reply().ok()).is_none() {
            return;
        }
        let mask = match self.conn.generate_id() {
            Ok(id) => id,
            Err(_) => return,
        };
        let gc1 = match self.conn.generate_id() {
            Ok(id) => id,
            Err(_) => return,
        };
        let gc0 = match self.conn.generate_id() {
            Ok(id) => id,
            Err(_) => return,
        };
        // Depth-1 pixmap mask.
        if self.conn.create_pixmap(1, mask, self.win, w, h).is_err() {
            return;
        }
        let _ = self.conn.create_gc(gc1, mask, &CreateGCAux::new().foreground(1));
        let _ = self.conn.create_gc(gc0, mask, &CreateGCAux::new().foreground(0));
        // Clear to 0 (outside the shape).
        let _ = self.conn.poly_fill_rectangle(
            mask,
            gc0,
            &[Rectangle { x: 0, y: 0, width: w, height: h }],
        );
        // Body: a plus made of two rectangles, leaving the four corners.
        let _ = self.conn.poly_fill_rectangle(
            mask,
            gc1,
            &[
                Rectangle { x: r as i16, y: 0, width: w - 2 * r, height: h },
                Rectangle { x: 0, y: r as i16, width: w, height: h - 2 * r },
            ],
        );
        // Four corner quarter-circles (angles in 1/64 degree).
        let d = 2 * r;
        let _ = self.conn.poly_fill_arc(
            mask,
            gc1,
            &[
                Arc { x: 0, y: 0, width: d, height: d, angle1: 90 * 64, angle2: 90 * 64 },
                Arc { x: (w - d) as i16, y: 0, width: d, height: d, angle1: 0, angle2: 90 * 64 },
                Arc { x: 0, y: (h - d) as i16, width: d, height: d, angle1: 180 * 64, angle2: 90 * 64 },
                Arc {
                    x: (w - d) as i16,
                    y: (h - d) as i16,
                    width: d,
                    height: d,
                    angle1: 270 * 64,
                    angle2: 90 * 64,
                },
            ],
        );
        let _ = self.conn.shape_mask(
            shape::SO::SET,
            shape::SK::BOUNDING,
            self.win,
            0,
            0,
            mask,
        );
        let _ = self.conn.free_gc(gc1);
        let _ = self.conn.free_gc(gc0);
        let _ = self.conn.free_pixmap(mask);
        let _ = self.conn.flush();
    }

    fn reload(&mut self) {
        self.entries = fs::read_dir(&self.cwd, self.show_hidden);
        self.selected = 0;
        self.scroll = 0;
        // Drop previews from the old directory; the new visible folders
        // re-peek lazily on render.
        self.peek_cache.clear();
        self.rebuild_view();
    }

    // Recompute the display order from the current filter.
    fn rebuild_view(&mut self) {
        self.view = match &self.filter {
            Some(q) if !q.is_empty() => {
                let ql = q.to_lowercase();
                self.entries
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.name.to_lowercase().contains(&ql))
                    .map(|(i, _)| i)
                    .collect()
            }
            _ => (0..self.entries.len()).collect(),
        };
        if self.selected >= self.view.len() {
            self.selected = self.view.len().saturating_sub(1);
        }
        self.scroll = self.scroll.min(self.selected);
    }

    // The entry currently selected (mapped through the view).
    fn selected_entry(&self) -> Option<&Entry> {
        self.view.get(self.selected).and_then(|&i| self.entries.get(i))
    }

    // Rows that fit below the header at the current height.
    fn visible_rows(&self) -> usize {
        let row_h = self.row_height();
        (((self.height as f64 - TITLEBAR_H - HEADER_H) / row_h).floor() as usize).max(1)
    }

    fn row_height(&self) -> f64 {
        // Derive from the font: measure once at a fixed pt and cache via
        // a fresh layout. Cheap; only called on render + nav.
        18.0 + ROW_PAD * 2.0
    }

    fn move_selection(&mut self, delta: isize) {
        if self.view.is_empty() {
            return;
        }
        let n = self.view.len() as isize;
        let mut s = self.selected as isize + delta;
        s = s.clamp(0, n - 1);
        self.selected = s as usize;
        // Keep the selection in view.
        let vis = self.visible_rows();
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + vis {
            self.scroll = self.selected + 1 - vis;
        }
    }

    fn enter(&mut self) {
        if let Some(e) = self.selected_entry() {
            if e.is_dir {
                self.cwd = e.path.clone();
                self.filter = None;
                self.reload();
            } else {
                // Launch via xdg-open; GNOSIS keeps running.
                let _ = std::process::Command::new("xdg-open").arg(&e.path).spawn();
            }
        }
    }

    fn go_up(&mut self) {
        if let Some(parent) = self.cwd.parent() {
            let prev = self.cwd.clone();
            self.cwd = parent.to_path_buf();
            self.filter = None;
            self.reload();
            // Select the directory we came from, for orientation.
            if let Some(name) = prev.file_name() {
                if let Some(vi) = self
                    .view
                    .iter()
                    .position(|&i| Some(self.entries[i].name.as_str()) == name.to_str())
                {
                    self.selected = vi;
                    let vis = self.visible_rows();
                    if self.selected >= vis {
                        self.scroll = self.selected + 1 - vis;
                    }
                }
            }
        }
    }

    // Navigate to an arbitrary directory (bookmark jump).
    fn go_to(&mut self, dir: PathBuf) {
        self.cwd = dir;
        self.filter = None;
        self.reload();
    }

    // Trash the selected entry, then reload.
    fn trash_selected(&mut self) {
        if let Some(e) = self.selected_entry() {
            let (path, name) = (e.path.clone(), e.name.clone());
            match fs::trash(&path) {
                Ok(()) => self.status = format!("trashed {}", name),
                Err(err) => self.status = format!("trash failed: {}", err),
            }
            self.reload();
        }
    }

    // Paste (copy) the yanked path into the current directory.
    fn paste(&mut self) {
        let Some(src) = self.yank.clone() else {
            self.status = "nothing yanked".into();
            return;
        };
        match fs::copy_into(&src, &self.cwd) {
            Ok(()) => {
                let name = src
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.status = format!("pasted {}", name);
            }
            Err(err) => self.status = format!("paste failed: {}", err),
        }
        self.reload();
    }

    fn event_loop(&mut self) -> Result<(), String> {
        self.render()?;
        let fd = self.conn.stream().as_raw_fd();
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        loop {
            // Drain events; only render when something changed.
            let mut dirty = false;
            loop {
                match self.conn.poll_for_event() {
                    Ok(Some(ev)) => match self.handle(ev) {
                        Outcome::Quit => return Ok(()),
                        Outcome::Dirty => dirty = true,
                        Outcome::Idle => {}
                    },
                    Ok(None) => break,
                    Err(e) => return Err(format!("poll_for_event: {}", e)),
                }
            }
            if dirty {
                self.render()?;
            }
            let _ = self.conn.flush();
            pfd.revents = 0;
            // Block until the next X event; idle = zero work.
            let _ = unsafe { libc::poll(&mut pfd, 1, -1) };
        }
    }

    fn handle(&mut self, ev: Event) -> Outcome {
        match ev {
            Event::Expose(e) if e.count == 0 => Outcome::Dirty,
            Event::ConfigureNotify(e) => {
                if e.width != self.width || e.height != self.height {
                    self.width = e.width;
                    self.height = e.height;
                    self.apply_shape();
                    Outcome::Dirty
                } else {
                    Outcome::Idle
                }
            }
            Event::KeyPress(kp) => self.on_key(kp),
            Event::ButtonPress(bp) => self.on_button(bp),
            _ => Outcome::Idle,
        }
    }

    fn on_key(&mut self, ev: KeyPressEvent) -> Outcome {
        let shift = u16::from(ev.state) & u16::from(ModMask::SHIFT) != 0;
        let sym = keycode_to_keysym(ev.detail, shift, &self.keymap);
        // Modal dispatch: the bookmark picker and the type-to-jump
        // filter each capture keys before the normal command set.
        if self.picker.is_some() {
            return self.on_key_picker(sym);
        }
        if self.filter.is_some() {
            return self.on_key_filter(sym);
        }
        self.on_key_normal(sym)
    }

    // Normal mode: navigation + commands.
    fn on_key_normal(&mut self, sym: u32) -> Outcome {
        self.status.clear();
        match sym {
            0x71 | 0xff1b => return Outcome::Quit, // q / Escape
            0x6a | 0xff54 => self.move_selection(1), // j / Down
            0x6b | 0xff52 => self.move_selection(-1), // k / Up
            0x6c | 0xff53 | 0xff0d | 0xff8d => self.enter(), // l / Right / Enter
            0x68 | 0xff51 | 0xff08 => self.go_up(),  // h / Left / Backspace
            0x67 => {
                // g: top.
                self.selected = 0;
                self.scroll = 0;
            }
            0x47 => {
                // G: bottom.
                if !self.view.is_empty() {
                    self.selected = self.view.len() - 1;
                    let vis = self.visible_rows();
                    self.scroll = self.selected.saturating_sub(vis - 1);
                }
            }
            0x2e => {
                // '.': toggle hidden files.
                self.show_hidden = !self.show_hidden;
                self.reload();
            }
            0x2f => {
                // '/': enter type-to-jump filter mode.
                self.filter = Some(String::new());
                self.rebuild_view();
            }
            0x62 => {
                // b: open the bookmark picker.
                self.picker = Some((fs::bookmarks(), 0));
            }
            0x79 => {
                // y: yank the selected path (for paste).
                if let Some(e) = self.selected_entry() {
                    let (path, name) = (e.path.clone(), e.name.clone());
                    self.yank = Some(path);
                    self.status = format!("yanked {}", name);
                }
            }
            0x70 => self.paste(), // p: paste yanked into cwd.
            0x78 | 0xffff => self.trash_selected(), // x / Delete: trash.
            _ => return Outcome::Idle,
        }
        Outcome::Dirty
    }

    // Filter mode: printable keys edit the query; the view narrows live.
    fn on_key_filter(&mut self, sym: u32) -> Outcome {
        match sym {
            0xff1b => {
                // Esc: clear the filter, back to the full list.
                self.filter = None;
                self.rebuild_view();
            }
            0xff0d | 0xff8d => {
                // Enter: open the match, leaving filter (cleared on
                // descend; on a file it just stays put).
                self.enter();
                self.filter = None;
                self.rebuild_view();
            }
            0xff54 => self.move_selection(1),  // Down still navigates
            0xff52 => self.move_selection(-1), // Up still navigates
            0xff08 => {
                // Backspace: drop a char (or exit when empty).
                if let Some(q) = self.filter.as_mut() {
                    if q.pop().is_none() {
                        self.filter = None;
                    }
                }
                self.rebuild_view();
            }
            _ => {
                if let Some(c) = printable(sym) {
                    if let Some(q) = self.filter.as_mut() {
                        q.push(c);
                    }
                    self.selected = 0;
                    self.scroll = 0;
                    self.rebuild_view();
                } else {
                    return Outcome::Idle;
                }
            }
        }
        Outcome::Dirty
    }

    // Picker mode: move through bookmarks, Enter jumps, Esc closes.
    fn on_key_picker(&mut self, sym: u32) -> Outcome {
        let Some((items, sel)) = self.picker.as_mut() else {
            return Outcome::Idle;
        };
        match sym {
            0xff1b | 0x71 => self.picker = None, // Esc / q: close.
            0x6a | 0xff54 => {
                if *sel + 1 < items.len() {
                    *sel += 1;
                }
            }
            0x6b | 0xff52 => {
                *sel = sel.saturating_sub(1);
            }
            0x6c | 0xff53 | 0xff0d | 0xff8d => {
                // Enter: jump to the bookmark.
                let dir = items.get(*sel).map(|(_, p)| p.clone());
                self.picker = None;
                if let Some(dir) = dir {
                    self.go_to(dir);
                }
            }
            _ => return Outcome::Idle,
        }
        Outcome::Dirty
    }

    // Mouse: only the titlebar traffic lights are interactive.
    fn on_button(&mut self, ev: ButtonPressEvent) -> Outcome {
        let (x, y) = (ev.event_x as f64, ev.event_y as f64);
        let hit_r = (LIGHT_R + 1.5) * (LIGHT_R + 1.5);
        for (i, &cx) in LIGHT_X.iter().enumerate() {
            let (dx, dy) = (x - cx, y - LIGHT_Y);
            if dx * dx + dy * dy <= hit_r {
                match i {
                    0 => return Outcome::Quit, // close
                    1 => self.iconify(),       // minimize
                    _ => self.toggle_fullscreen(), // zoom
                }
                return Outcome::Idle;
            }
        }
        Outcome::Idle
    }

    fn atom(&self, name: &str) -> Atom {
        self.conn
            .intern_atom(false, name.as_bytes())
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| r.atom)
            .unwrap_or(0)
    }

    // ICCCM WM_CHANGE_STATE -> IconicState (3): ask the WM to minimize.
    fn iconify(&self) {
        let wm_change_state = self.atom("WM_CHANGE_STATE");
        if wm_change_state == 0 {
            return;
        }
        let ev = ClientMessageEvent::new(32, self.win, wm_change_state, [3, 0, 0, 0, 0]);
        let _ = self.conn.send_event(
            false,
            self.root,
            EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
            ev,
        );
        let _ = self.conn.flush();
    }

    // EWMH _NET_WM_STATE toggle of _NET_WM_STATE_FULLSCREEN: zoom.
    fn toggle_fullscreen(&self) {
        let net_wm_state = self.atom("_NET_WM_STATE");
        let fs = self.atom("_NET_WM_STATE_FULLSCREEN");
        if net_wm_state == 0 || fs == 0 {
            return;
        }
        // data: [action(2=toggle), prop1, prop2, source(1=app), 0]
        let ev = ClientMessageEvent::new(32, self.win, net_wm_state, [2, fs, 0, 1, 0]);
        let _ = self.conn.send_event(
            false,
            self.root,
            EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
            ev,
        );
        let _ = self.conn.flush();
    }

    fn render(&mut self) -> Result<(), String> {
        let w = self.width as i32;
        let h = self.height as i32;
        let (surface, cr) = torrentius::new_surface(w, h)?;
        let font_opts = torrentius::make_font_options()?;
        cr.set_font_options(&font_opts);

        // Background.
        cr.set_source_rgb(self.theme.bg.0, self.theme.bg.1, self.theme.bg.2);
        let _ = cr.paint();

        let layout = pangocairo::functions::create_layout(&cr);
        let font_desc = pango::FontDescription::from_string(&self.theme.font);
        layout.set_font_description(Some(&font_desc));
        pangocairo::functions::context_set_font_options(&layout.context(), Some(&font_opts));

        // macOS-style titlebar: a slightly lighter strip with the three
        // traffic lights at the left and the title centered.
        self.draw_titlebar(&cr, &layout, w as f64);

        // Header: breadcrumb (current path), accent underline. Offset
        // below the titlebar.
        cr.set_source_rgb(self.theme.accent.0, self.theme.accent.1, self.theme.accent.2);
        cr.rectangle(0.0, TITLEBAR_H + HEADER_H - 2.0, w as f64, 2.0);
        let _ = cr.fill();
        layout.set_text(&self.cwd.to_string_lossy());
        cr.set_source_rgb(self.theme.fg.0, self.theme.fg.1, self.theme.fg.2);
        cr.move_to(10.0, TITLEBAR_H + 6.0);
        pangocairo::functions::show_layout(&cr, &layout);

        // Rows.
        let row_h = self.row_height();
        let vis = self.visible_rows();
        let end = (self.scroll + vis).min(self.view.len());

        // Precompute folder previews for visible folders not yet
        // cached. Done before the immutable draw borrow; only visible
        // folders are peeked, so this stays near-zero compute.
        let to_peek: Vec<PathBuf> = self.view[self.scroll..end]
            .iter()
            .map(|&ei| &self.entries[ei])
            .filter(|e| e.is_dir && !self.peek_cache.contains_key(&e.path))
            .map(|e| e.path.clone())
            .collect();
        for p in to_peek {
            let pk = fs::peek(&p, PREVIEW_CARDS);
            self.peek_cache.insert(p, pk);
        }

        let name_x = 10.0 + PREVIEW_W;
        for (i, &ei) in self.view[self.scroll..end].iter().enumerate() {
            let e = &self.entries[ei];
            let idx = self.scroll + i;
            let y = TITLEBAR_H + HEADER_H + i as f64 * row_h;
            let selected = idx == self.selected;
            if selected {
                cr.set_source_rgb(self.theme.accent.0, self.theme.accent.1, self.theme.accent.2);
                cr.rectangle(0.0, y, w as f64, row_h);
                let _ = cr.fill();
            }
            // W10 folder preview: a little stack of cards showing the
            // first few inner entries (accent for subfolders, grey for
            // files). Files get a single plain card.
            if e.is_dir {
                if let Some((cards, _total)) = self.peek_cache.get(&e.path) {
                    draw_folder_preview(&cr, 8.0, y + ROW_PAD, row_h - ROW_PAD * 2.0, cards, &self.theme);
                }
            } else {
                draw_file_card(&cr, 8.0, y + ROW_PAD, row_h - ROW_PAD * 2.0, &self.theme);
            }

            let label = if e.is_dir {
                format!("{}/", e.name)
            } else {
                e.name.clone()
            };
            let color = if selected {
                self.theme.sel_fg
            } else if e.is_dir {
                self.theme.dir_fg
            } else {
                self.theme.fg
            };
            cr.set_source_rgb(color.0, color.1, color.2);
            layout.set_text(&label);
            cr.move_to(name_x, y + ROW_PAD);
            pangocairo::functions::show_layout(&cr, &layout);
        }

        // Empty-directory / no-match note.
        if self.view.is_empty() {
            cr.set_source_rgb(self.theme.fg.0, self.theme.fg.1, self.theme.fg.2);
            let note = if self.filter.is_some() { "(no match)" } else { "(empty)" };
            layout.set_text(note);
            cr.move_to(12.0, TITLEBAR_H + HEADER_H + ROW_PAD);
            pangocairo::functions::show_layout(&cr, &layout);
        }

        // Footer: filter query while typing, else a transient status.
        let footer = if let Some(q) = &self.filter {
            Some(format!("/{}", q))
        } else if !self.status.is_empty() {
            Some(self.status.clone())
        } else {
            None
        };
        if let Some(text) = footer {
            let fy = h as f64 - 22.0;
            cr.set_source_rgba(self.theme.bg.0, self.theme.bg.1, self.theme.bg.2, 0.92);
            cr.rectangle(0.0, fy, w as f64, 22.0);
            let _ = cr.fill();
            cr.set_source_rgb(self.theme.accent.0, self.theme.accent.1, self.theme.accent.2);
            cr.rectangle(0.0, fy, w as f64, 1.0);
            let _ = cr.fill();
            cr.set_source_rgb(self.theme.fg.0, self.theme.fg.1, self.theme.fg.2);
            layout.set_text(&text);
            cr.move_to(10.0, fy + 2.0);
            pangocairo::functions::show_layout(&cr, &layout);
        }

        // Bookmark picker overlay: a centered panel listing bookmarks,
        // drawn on top of everything.
        if let Some((items, sel)) = &self.picker {
            self.draw_picker(&cr, &layout, items, *sel);
        }

        drop(cr);
        surface.flush();
        let _ = surface.with_data(|data| {
            let _ = self.conn.put_image(
                ImageFormat::Z_PIXMAP,
                self.win,
                self.gc,
                self.width,
                self.height,
                0,
                0,
                0,
                self.depth,
                data,
            );
        });
        let _ = self.conn.flush();
        Ok(())
    }

    // macOS-style titlebar: lighter strip, three traffic lights, a
    // centered "GNOSIS" title, and a hairline separating it from the
    // body.
    fn draw_titlebar(&self, cr: &cairo::Context, layout: &pango::Layout, w: f64) {
        let lighten = |c: f64| (c + 0.06).min(1.0);
        cr.set_source_rgb(lighten(self.theme.bg.0), lighten(self.theme.bg.1), lighten(self.theme.bg.2));
        cr.rectangle(0.0, 0.0, w, TITLEBAR_H);
        let _ = cr.fill();
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.25);
        cr.rectangle(0.0, TITLEBAR_H - 1.0, w, 1.0);
        let _ = cr.fill();

        let lights = [LIGHT_CLOSE, LIGHT_MIN, LIGHT_ZOOM];
        let tau = std::f64::consts::PI * 2.0;
        for (i, &cx) in LIGHT_X.iter().enumerate() {
            let (r, g, b) = lights[i];
            cr.arc(cx, LIGHT_Y, LIGHT_R, 0.0, tau);
            cr.set_source_rgb(r, g, b);
            let _ = cr.fill();
            cr.arc(cx, LIGHT_Y, LIGHT_R, 0.0, tau);
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.18);
            cr.set_line_width(1.0);
            let _ = cr.stroke();
        }

        layout.set_text("GNOSIS");
        let (tw, _) = layout.pixel_size();
        cr.set_source_rgb(self.theme.fg.0, self.theme.fg.1, self.theme.fg.2);
        cr.move_to((w - tw as f64) / 2.0, 6.0);
        pangocairo::functions::show_layout(cr, layout);
    }

    // Centered bookmark picker panel.
    fn draw_picker(
        &self,
        cr: &cairo::Context,
        layout: &pango::Layout,
        items: &[(String, PathBuf)],
        sel: usize,
    ) {
        let w = self.width as f64;
        let h = self.height as f64;
        let row_h = 24.0;
        let pad = 12.0;
        let panel_w = (w * 0.6).min(420.0);
        let panel_h = pad * 2.0 + items.len().max(1) as f64 * row_h + 26.0;
        let px = (w - panel_w) / 2.0;
        let py = (h - panel_h) / 2.0;

        // Dim the background.
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.45);
        cr.rectangle(0.0, 0.0, w, h);
        let _ = cr.fill();

        // Panel.
        rounded_rect(cr, px, py, panel_w, panel_h, 6.0);
        cr.set_source_rgb(self.theme.bg.0, self.theme.bg.1, self.theme.bg.2);
        let _ = cr.fill_preserve();
        cr.set_source_rgb(self.theme.accent.0, self.theme.accent.1, self.theme.accent.2);
        cr.set_line_width(1.5);
        let _ = cr.stroke();

        cr.set_source_rgb(self.theme.accent.0, self.theme.accent.1, self.theme.accent.2);
        layout.set_text("Bookmarks");
        cr.move_to(px + pad, py + 6.0);
        pangocairo::functions::show_layout(cr, layout);

        let list_y = py + 28.0;
        for (i, (name, path)) in items.iter().enumerate() {
            let ry = list_y + i as f64 * row_h;
            if i == sel {
                cr.set_source_rgb(self.theme.accent.0, self.theme.accent.1, self.theme.accent.2);
                rounded_rect(cr, px + 6.0, ry, panel_w - 12.0, row_h, 3.0);
                let _ = cr.fill();
                cr.set_source_rgb(self.theme.sel_fg.0, self.theme.sel_fg.1, self.theme.sel_fg.2);
            } else {
                cr.set_source_rgb(self.theme.fg.0, self.theme.fg.1, self.theme.fg.2);
            }
            let label = format!("{}   {}", name, path.to_string_lossy());
            layout.set_text(&label);
            cr.move_to(px + pad, ry + 3.0);
            pangocairo::functions::show_layout(cr, layout);
        }
    }
}

enum Outcome {
    Quit,
    Dirty,
    Idle,
}

// W10-style folder preview: an overlapping stack of mini-cards, one
// per peeked inner entry (accent-tinted for subfolders, grey for
// files), drawn back-to-front so the first entry sits on top.
fn draw_folder_preview(
    cr: &cairo::Context,
    x: f64,
    y: f64,
    h: f64,
    cards: &[bool],
    theme: &Theme,
) {
    let card_w = (h * 0.72).min(15.0);
    let card_h = (h * 0.85).min(19.0);
    let dx = 3.0;
    let base_y = y + (h - card_h) / 2.0;
    if cards.is_empty() {
        cr.set_source_rgba(theme.fg.0, theme.fg.1, theme.fg.2, 0.3);
        cr.set_line_width(1.0);
        rounded_rect(cr, x, base_y, card_w, card_h, 2.0);
        let _ = cr.stroke();
        return;
    }
    for i in (0..cards.len()).rev() {
        let cx = x + i as f64 * dx;
        let cy = base_y + i as f64 * 0.5;
        let (r, g, b) = if cards[i] {
            theme.accent
        } else {
            (0.55, 0.55, 0.60)
        };
        rounded_rect(cr, cx, cy, card_w, card_h, 2.0);
        cr.set_source_rgb(r, g, b);
        let _ = cr.fill();
        rounded_rect(cr, cx, cy, card_w, card_h, 2.0);
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.4);
        cr.set_line_width(1.0);
        let _ = cr.stroke();
    }
}

// A single plain card for a file row, so names stay aligned with
// folder rows.
fn draw_file_card(cr: &cairo::Context, x: f64, y: f64, h: f64, theme: &Theme) {
    let card_w = (h * 0.72).min(15.0);
    let card_h = (h * 0.85).min(19.0);
    let cy = y + (h - card_h) / 2.0;
    rounded_rect(cr, x, cy, card_w, card_h, 2.0);
    cr.set_source_rgba(theme.fg.0, theme.fg.1, theme.fg.2, 0.35);
    let _ = cr.fill();
}

fn rounded_rect(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h / 2.0);
    let deg = std::f64::consts::PI / 180.0;
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -90.0 * deg, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, 90.0 * deg);
    cr.arc(x + r, y + h - r, r, 90.0 * deg, 180.0 * deg);
    cr.arc(x + r, y + r, r, 180.0 * deg, 270.0 * deg);
    cr.close_path();
}

fn set_wm_name(conn: &RustConnection, win: Window, name: &str) {
    let _ = conn.change_property(
        PropMode::REPLACE,
        win,
        AtomEnum::WM_NAME,
        AtomEnum::STRING,
        8,
        name.len() as u32,
        name.as_bytes(),
    );
}

fn set_wm_class(conn: &RustConnection, win: Window, class: &str) {
    // WM_CLASS is "instance\0class\0".
    let mut bytes = Vec::new();
    bytes.extend_from_slice(class.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(class.as_bytes());
    bytes.push(0);
    let _ = conn.change_property(
        PropMode::REPLACE,
        win,
        AtomEnum::WM_CLASS,
        AtomEnum::STRING,
        8,
        bytes.len() as u32,
        &bytes,
    );
}

// Map a keysym to its printable ASCII char for filter input. Latin-1
// keysyms are their codepoint; we accept the printable range.
fn printable(sym: u32) -> Option<char> {
    if (0x20..=0x7e).contains(&sym) {
        char::from_u32(sym)
    } else {
        None
    }
}

fn keycode_to_keysym(keycode: u8, shift: bool, keymap: &KeyMap) -> u32 {
    if keycode < keymap.min_keycode {
        return 0;
    }
    let base = (keycode - keymap.min_keycode) as usize * keymap.syms_per_code;
    let idx = base + if shift && keymap.syms_per_code > 1 { 1 } else { 0 };
    keymap.keysyms.get(idx).copied().unwrap_or(0)
}
