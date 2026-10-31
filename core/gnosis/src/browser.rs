// GNOSIS's browser, as both stacks' binaries show it: The current
// directory as a scrolling list with a breadcrumb header, the selection
// highlighted in [appearance].accent, the type-to-jump filter, the bookmark
// picker, the sidebar of bookmarks and drives, thumbnails, and what every
// key and click does. Each binary gives it a window, hands it the window's
// size, keys and clicks, and shows what it draws.

use std::collections::{HashMap, HashSet};
use std::os::fd::RawFd;
use std::path::PathBuf;

use demiurge_core::config::Config;
use demiurge_core::titlebar::{self, Light};
use demiurge_core::torrentius;

use crate::drives::{self, Drive};
use crate::fs::{self, Entry};
use crate::thumbs;

/// The window's title and class, which DEMIURGE and cherrypie match on.
pub const TITLE: &str = "GNOSIS";
pub const CLASS: &str = "gnosis";
/// The window's size until the window manager gives it another.
pub const WIDTH: u16 = 820;
pub const HEIGHT: u16 = 560;
/// The rounding of the window's corners while GNOSIS draws its own title
/// bar.
pub const CORNER_R: i16 = 9;

const HEADER_H: f64 = 30.0;
const ROW_PAD: f64 = 4.0;

struct Theme {
    bg: (f64, f64, f64),
    fg: (f64, f64, f64),
    dir_fg: (f64, f64, f64),
    accent: (f64, f64, f64),
    sel_fg: (f64, f64, f64),
    font: String,
}

/// What a key or click asks of the binary.
pub enum Outcome {
    Quit,
    Dirty,
    Idle,
    /// The title bar's minimize light.
    Minimize,
    /// The title bar's zoom light: Maximize, or back.
    Zoom,
}

pub struct Browser {
    width: u16,
    height: u16,
    cwd: PathBuf,
    entries: Vec<Entry>,
    selected: usize,
    scroll: usize,
    show_hidden: bool,
    theme: Theme,
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
    // The window manager draws the title bar: GNOSIS then leaves out its
    // own bar and its rounded corners.
    framed: bool,
    // Keyboard focus, which lights the traffic lights.
    focused: bool,
    // Image thumbnails: The worker making them (None without a cache dir),
    // those ready (None for an image that cannot be read), and those asked
    // for and not yet back.
    thumbs: Option<thumbs::Worker>,
    thumb_cache: HashMap<PathBuf, Option<cairo::ImageSurface>>,
    thumb_pending: HashSet<PathBuf>,
    // The sidebar: Shown or not, holding the keyboard or not, its rows and
    // the selected one, where each row was drawn (for clicks), the drives
    // udisks reports, and the drive being mounted to be opened.
    sidebar: bool,
    side_focus: bool,
    side_items: Vec<SideItem>,
    side_sel: usize,
    side_hits: Vec<(f64, f64, usize)>,
    drives: Option<drives::Drives>,
    drive_list: Vec<Drive>,
    opening: Option<String>,
}

// Number of preview cards a folder shows.
const PREVIEW_CARDS: usize = 4;
// Width reserved at the left of each row for the preview stack.
const PREVIEW_W: f64 = 26.0;
// The sidebar's width when shown.
const SIDE_W: f64 = 200.0;

// A sidebar row: A bookmarked place, or a drive (an index into the drive
// list).
enum SideItem {
    Place(String, PathBuf),
    Drive(usize),
}

impl Browser {
    /// A browser at `start`, its colours and font from `config`.
    pub fn new(config: &Config, start: PathBuf) -> Self {
        let theme = Theme {
            bg: torrentius::parse_hex(&config.bar.bg),
            fg: torrentius::parse_hex(&config.bar.fg),
            dir_fg: torrentius::parse_hex(&config.appearance.accent),
            accent: torrentius::parse_hex(&config.appearance.accent),
            sel_fg: (0.97, 0.97, 0.97),
            font: config.font.default.clone(),
        };
        let mut browser = Browser {
            width: WIDTH,
            height: HEIGHT,
            cwd: start.clone(),
            entries: fs::read_dir(&start, false),
            selected: 0,
            scroll: 0,
            show_hidden: false,
            theme,
            peek_cache: HashMap::new(),
            view: Vec::new(),
            filter: None,
            yank: None,
            picker: None,
            status: String::new(),
            framed: false,
            focused: true,
            thumbs: thumbs::cache_root().and_then(thumbs::Worker::start),
            thumb_cache: HashMap::new(),
            thumb_pending: HashSet::new(),
            sidebar: false,
            side_focus: false,
            side_items: Vec::new(),
            side_sel: 0,
            side_hits: Vec::new(),
            drives: drives::Drives::start(),
            drive_list: Vec::new(),
            opening: None,
        };
        browser.rebuild_view();
        browser
    }

    pub fn size(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    /// The window took a new size: True if it changed.
    pub fn resize(&mut self, width: u16, height: u16) -> bool {
        if (width, height) == (self.width, self.height) {
            return false;
        }
        (self.width, self.height) = (width, height);
        true
    }

    pub fn framed(&self) -> bool {
        self.framed
    }

    /// Whether the window manager draws the title bar: True if it changed.
    pub fn set_framed(&mut self, framed: bool) -> bool {
        std::mem::replace(&mut self.framed, framed) != framed
    }

    /// Whether the window holds the keyboard: True if it changed.
    pub fn set_focused(&mut self, focused: bool) -> bool {
        std::mem::replace(&mut self.focused, focused) != focused
    }

    /// The fds that wake the binary's loop, -1 for none: The thumbnail
    /// worker's, then the drives worker's.
    pub fn wake_fds(&self) -> [RawFd; 2] {
        [self.thumbs.as_ref().map_or(-1, |w| w.fd()), self.drives.as_ref().map_or(-1, |d| d.fd())]
    }

    /// The workers whose fds woke the loop have news: Taken. True if it
    /// changed what shows.
    pub fn woke(&mut self, thumbs: bool, drives: bool) -> bool {
        let mut dirty = false;
        if thumbs {
            dirty |= self.take_thumbnails();
        }
        if drives {
            self.take_drives();
            dirty = true;
        }
        dirty
    }

    fn reload(&mut self) {
        self.entries = fs::read_dir(&self.cwd, self.show_hidden);
        self.selected = 0;
        self.scroll = 0;
        // Drop previews from the old directory; the new visible folders
        // re-peek lazily on render, and thumbnails come back from the cache
        // on disk.
        self.peek_cache.clear();
        self.thumb_cache.clear();
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
        (((self.height as f64 - self.top() - HEADER_H) / row_h).floor() as usize).max(1)
    }

    // Where GNOSIS's own content starts: Below its title bar, or at the top
    // when the window manager draws the bar.
    fn top(&self) -> f64 {
        if self.framed { 0.0 } else { titlebar::HEIGHT as f64 }
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

    // What udisks reported: The drive list, and the answers to mounts and
    // unmounts. A drive mounted to be opened is opened.
    fn take_drives(&mut self) {
        let Some(worker) = self.drives.as_ref() else {
            return;
        };
        for event in worker.take() {
            match event {
                drives::Event::Drives(list) => {
                    self.drive_list = list;
                    self.rebuild_side();
                }
                drives::Event::Mounted(object, Ok(path)) => {
                    if self.opening.as_deref() == Some(object.as_str()) {
                        self.opening = None;
                        self.side_focus = false;
                        self.go_to(path);
                    }
                }
                drives::Event::Mounted(object, Err(e)) => {
                    if self.opening.as_deref() == Some(object.as_str()) {
                        self.opening = None;
                    }
                    self.status = format!("mount failed: {e}");
                }
                drives::Event::Unmounted(_, Ok(())) => self.status = "unmounted".into(),
                drives::Event::Unmounted(_, Err(e)) => self.status = format!("unmount failed: {e}"),
            }
        }
    }

    // The sidebar's rows: Bookmarks, then drives.
    fn rebuild_side(&mut self) {
        self.side_items = fs::bookmarks().into_iter().map(|(name, path)| SideItem::Place(name, path)).collect();
        self.side_items.extend((0..self.drive_list.len()).map(SideItem::Drive));
        self.side_sel = self.side_sel.min(self.side_items.len().saturating_sub(1));
    }

    // Open the selected sidebar row: Go to a place, or to a drive, mounting
    // it first.
    fn side_activate(&mut self) {
        let target = match self.side_items.get(self.side_sel) {
            Some(SideItem::Place(_, path)) => Some(path.clone()),
            Some(SideItem::Drive(i)) => {
                let drive = &self.drive_list[*i];
                match &drive.mount {
                    Some(mount) => Some(mount.clone()),
                    None => {
                        if let Some(worker) = self.drives.as_ref() {
                            worker.mount(&drive.object);
                            self.opening = Some(drive.object.clone());
                            self.status = format!("mounting {}", drive.label);
                        }
                        None
                    }
                }
            }
            None => None,
        };
        if let Some(path) = target {
            self.side_focus = false;
            self.go_to(path);
        }
    }

    // Unmount the selected drive, leaving it first if GNOSIS is on it.
    fn side_unmount(&mut self) {
        let Some(SideItem::Drive(i)) = self.side_items.get(self.side_sel) else {
            return;
        };
        let drive = self.drive_list[*i].clone();
        let Some(mount) = drive.mount else {
            return;
        };
        if drives::on_mount(&self.cwd, &mount) {
            let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
            self.go_to(home);
        }
        if let Some(worker) = self.drives.as_ref() {
            worker.unmount(&drive.object);
            self.status = format!("unmounting {}", drive.label);
        }
    }

    // Keys while the sidebar holds the keyboard.
    fn on_key_side(&mut self, sym: u32) -> Outcome {
        self.status.clear();
        match sym {
            0x6a | 0xff54 if !self.side_items.is_empty() => {
                self.side_sel = (self.side_sel + 1).min(self.side_items.len() - 1); // j / Down
            }
            0x6b | 0xff52 => self.side_sel = self.side_sel.saturating_sub(1), // k / Up
            0x6c | 0xff53 | 0xff0d | 0xff8d => self.side_activate(),        // l / Right / Enter
            0x75 => self.side_unmount(),                                     // u: unmount
            0xff09 | 0xff1b | 0x68 | 0xff51 => self.side_focus = false,      // Tab / Escape / h / Left
            0x73 => {
                // s: hide the sidebar.
                self.sidebar = false;
                self.side_focus = false;
            }
            _ => return Outcome::Idle,
        }
        Outcome::Dirty
    }

    // Thumbnails the worker finished, as Cairo surfaces. True if any belongs
    // to a row on screen.
    fn take_thumbnails(&mut self) -> bool {
        let Some(worker) = self.thumbs.as_ref() else {
            return false;
        };
        let mut shown = false;
        for (path, thumb) in worker.take() {
            self.thumb_pending.remove(&path);
            shown |= self.view.iter().any(|&i| self.entries[i].path == path);
            self.thumb_cache.insert(path, thumb.and_then(|t| surface(&t)));
        }
        shown
    }

    /// A key, by its keysym at the level the modifiers chose.
    pub fn key(&mut self, sym: u32) -> Outcome {
        // Modal dispatch: the bookmark picker and the type-to-jump
        // filter each capture keys before the normal command set.
        if self.picker.is_some() {
            return self.on_key_picker(sym);
        }
        if self.filter.is_some() {
            return self.on_key_filter(sym);
        }
        if self.side_focus {
            return self.on_key_side(sym);
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
            0x73 => {
                // s: show the sidebar (bookmarks and drives) and move into
                // it, or hide it.
                self.sidebar = !self.sidebar;
                self.side_focus = self.sidebar;
                if self.sidebar {
                    self.rebuild_side();
                }
            }
            0xff09 if self.sidebar => self.side_focus = true, // Tab: into the sidebar
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

    /// A button pressed at (x, y) in the window: A sidebar row opens, and
    /// GNOSIS's own traffic lights work when it draws them.
    pub fn click(&mut self, x: f64, y: f64) -> Outcome {
        // A click on a sidebar row opens it.
        if self.sidebar && x < SIDE_W {
            let Some(&(_, _, i)) = self.side_hits.iter().find(|&&(y0, y1, _)| y >= y0 && y < y1) else {
                return Outcome::Idle;
            };
            self.side_sel = i;
            self.side_activate();
            return Outcome::Dirty;
        }
        if self.framed {
            return Outcome::Idle;
        }
        match titlebar::light_at(x, y) {
            Some(Light::Close) => Outcome::Quit,
            Some(Light::Minimize) => Outcome::Minimize,
            Some(Light::Zoom) => Outcome::Zoom,
            None => Outcome::Idle,
        }
    }

    /// Draw the window, its size in pixels, into `cr`.
    pub fn draw(&mut self, cr: &cairo::Context) -> Result<(), String> {
        let w = self.width as i32;
        let h = self.height as i32;
        let font_opts = torrentius::make_font_options()?;
        cr.set_font_options(&font_opts);

        // Background.
        cr.set_source_rgb(self.theme.bg.0, self.theme.bg.1, self.theme.bg.2);
        let _ = cr.paint();

        let layout = pangocairo::functions::create_layout(cr);
        let font_desc = pango::FontDescription::from_string(&self.theme.font);
        layout.set_font_description(Some(&font_desc));
        pangocairo::functions::context_set_font_options(&layout.context(), Some(&font_opts));

        // The shared title bar, unless the window manager draws it.
        if !self.framed {
            let colors = titlebar::Colors { bg: self.theme.bg, fg: self.theme.fg };
            let bar_layout = pangocairo::functions::create_layout(cr);
            bar_layout.set_font_description(Some(&font_desc));
            pangocairo::functions::context_set_font_options(&bar_layout.context(), Some(&font_opts));
            titlebar::draw(cr, &bar_layout, w as f64, TITLE, self.focused, &colors);
        }

        // The list starts right of the sidebar when it shows.
        let left = if self.sidebar { SIDE_W } else { 0.0 };
        if self.sidebar {
            self.draw_sidebar(cr, &layout, h as f64);
        }

        // Header: breadcrumb (current path), accent underline. Offset
        // below the titlebar.
        cr.set_source_rgb(self.theme.accent.0, self.theme.accent.1, self.theme.accent.2);
        cr.rectangle(left, self.top() + HEADER_H - 2.0, w as f64 - left, 2.0);
        let _ = cr.fill();
        layout.set_text(&self.cwd.to_string_lossy());
        cr.set_source_rgb(self.theme.fg.0, self.theme.fg.1, self.theme.fg.2);
        cr.move_to(left + 10.0, self.top() + 6.0);
        pangocairo::functions::show_layout(cr, &layout);

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
        // Ask for thumbnails of the visible images not yet seen.
        if let Some(worker) = self.thumbs.as_ref() {
            for &ei in &self.view[self.scroll..end] {
                let path = &self.entries[ei].path;
                if !self.entries[ei].is_dir
                    && thumbs::wanted(path)
                    && !self.thumb_cache.contains_key(path)
                    && self.thumb_pending.insert(path.clone())
                {
                    worker.ask(path.clone());
                }
            }
        }

        let name_x = left + 10.0 + PREVIEW_W;
        for (i, &ei) in self.view[self.scroll..end].iter().enumerate() {
            let e = &self.entries[ei];
            let idx = self.scroll + i;
            let y = self.top() + HEADER_H + i as f64 * row_h;
            let selected = idx == self.selected && !self.side_focus;
            if selected {
                cr.set_source_rgb(self.theme.accent.0, self.theme.accent.1, self.theme.accent.2);
                cr.rectangle(left, y, w as f64 - left, row_h);
                let _ = cr.fill();
            }
            // W10 folder preview: a little stack of cards showing the
            // first few inner entries (accent for subfolders, grey for
            // files). Files get a single plain card.
            if e.is_dir {
                if let Some((cards, _total)) = self.peek_cache.get(&e.path) {
                    draw_folder_preview(cr, left + 8.0, y + ROW_PAD, row_h - ROW_PAD * 2.0, cards, &self.theme);
                }
            } else if let Some(Some(thumb)) = self.thumb_cache.get(&e.path) {
                draw_thumbnail(cr, left + 8.0, y + ROW_PAD, row_h - ROW_PAD * 2.0, thumb);
            } else {
                draw_file_card(cr, left + 8.0, y + ROW_PAD, row_h - ROW_PAD * 2.0, &self.theme);
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
            pangocairo::functions::show_layout(cr, &layout);
        }

        // Empty-directory / no-match note.
        if self.view.is_empty() {
            cr.set_source_rgb(self.theme.fg.0, self.theme.fg.1, self.theme.fg.2);
            let note = if self.filter.is_some() { "(no match)" } else { "(empty)" };
            layout.set_text(note);
            cr.move_to(left + 12.0, self.top() + HEADER_H + ROW_PAD);
            pangocairo::functions::show_layout(cr, &layout);
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
            pangocairo::functions::show_layout(cr, &layout);
        }

        // Bookmark picker overlay: a centered panel listing bookmarks,
        // drawn on top of everything.
        if let Some((items, sel)) = &self.picker {
            self.draw_picker(cr, &layout, items, *sel);
        }
        Ok(())
    }

    // The sidebar: Bookmarks, then drives, each drive with its size and an
    // unmounted one dimmed; the selected row lit while it holds the keyboard.
    fn draw_sidebar(&mut self, cr: &cairo::Context, layout: &pango::Layout, h: f64) {
        let top = self.top();
        let row_h = self.row_height();
        let lift = |c: f64| (c + 0.03).min(1.0);
        let dim = (self.theme.fg.0 * 0.6, self.theme.fg.1 * 0.6, self.theme.fg.2 * 0.6);
        cr.set_source_rgb(lift(self.theme.bg.0), lift(self.theme.bg.1), lift(self.theme.bg.2));
        cr.rectangle(0.0, top, SIDE_W, h - top);
        let _ = cr.fill();
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.3);
        cr.rectangle(SIDE_W - 1.0, top, 1.0, h - top);
        let _ = cr.fill();

        self.side_hits.clear();
        let mut y = top + 8.0;
        let places = self.side_items.iter().take_while(|i| matches!(i, SideItem::Place(..))).count();
        for (section, range) in [("Bookmarks", 0..places), ("Drives", places..self.side_items.len())] {
            cr.set_source_rgb(dim.0, dim.1, dim.2);
            layout.set_text(section);
            cr.move_to(12.0, y);
            pangocairo::functions::show_layout(cr, layout);
            y += row_h;
            if range.is_empty() {
                layout.set_text("(none)");
                cr.move_to(20.0, y + ROW_PAD);
                pangocairo::functions::show_layout(cr, layout);
                y += row_h;
            }
            for i in range {
                if self.side_focus && i == self.side_sel {
                    cr.set_source_rgb(self.theme.accent.0, self.theme.accent.1, self.theme.accent.2);
                    cr.rectangle(0.0, y, SIDE_W - 1.0, row_h);
                    let _ = cr.fill();
                }
                let lit = self.side_focus && i == self.side_sel;
                let (label, size, mounted) = match &self.side_items[i] {
                    SideItem::Place(name, _) => (name.clone(), String::new(), true),
                    SideItem::Drive(d) => {
                        let drive = &self.drive_list[*d];
                        (drive.label.clone(), drives::human_size(drive.size), drive.mount.is_some())
                    }
                };
                let ink = if lit { self.theme.sel_fg } else if mounted { self.theme.fg } else { dim };
                layout.set_text(&size);
                let (size_w, _) = layout.pixel_size();
                if !size.is_empty() {
                    cr.set_source_rgb(dim.0, dim.1, dim.2);
                    cr.move_to(SIDE_W - 12.0 - size_w as f64, y + ROW_PAD);
                    pangocairo::functions::show_layout(cr, layout);
                }
                layout.set_width(((SIDE_W - 36.0 - size_w as f64) * pango::SCALE as f64) as i32);
                layout.set_ellipsize(pango::EllipsizeMode::End);
                layout.set_text(&label);
                cr.set_source_rgb(ink.0, ink.1, ink.2);
                cr.move_to(20.0, y + ROW_PAD);
                pangocairo::functions::show_layout(cr, layout);
                layout.set_width(-1);
                layout.set_ellipsize(pango::EllipsizeMode::None);
                self.side_hits.push((y, y + row_h, i));
                y += row_h;
            }
            y += 6.0;
        }
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

// An image's thumbnail in place of its card: Fit to the preview column and
// the row, aspect kept, corners rounded like the cards.
fn draw_thumbnail(cr: &cairo::Context, x: f64, y: f64, h: f64, thumb: &cairo::ImageSurface) {
    let (tw, th) = (thumb.width() as f64, thumb.height() as f64);
    let scale = ((PREVIEW_W - 6.0) / tw).min(h / th);
    let (w, dh) = (tw * scale, th * scale);
    let cy = y + (h - dh) / 2.0;
    cr.save().ok();
    rounded_rect(cr, x, cy, w, dh, 2.0);
    cr.clip();
    cr.translate(x, cy);
    cr.scale(scale, scale);
    let _ = cr.set_source_surface(thumb, 0.0, 0.0);
    let _ = cr.paint();
    cr.restore().ok();
}

// A worker thumbnail (straight RGBA) as a Cairo ARGB32 surface, which is
// premultiplied and stored native-endian.
fn surface(thumb: &thumbs::Thumb) -> Option<cairo::ImageSurface> {
    let mut s = cairo::ImageSurface::create(cairo::Format::ARgb32, thumb.width as i32, thumb.height as i32).ok()?;
    let stride = s.stride() as usize;
    {
        let mut data = s.data().ok()?;
        for (row, pixels) in thumb.rgba.chunks(thumb.width as usize * 4).enumerate() {
            for (col, p) in pixels.chunks(4).enumerate() {
                let a = p[3] as u32;
                let pre = |c: u8| (c as u32 * a + 127) / 255;
                let word = (a << 24) | (pre(p[0]) << 16) | (pre(p[1]) << 8) | pre(p[2]);
                let at = row * stride + col * 4;
                data[at..at + 4].copy_from_slice(&word.to_ne_bytes());
            }
        }
    }
    s.mark_dirty();
    Some(s)
}

/// A rounded rectangle path, as the window's corners and the cards use.
pub fn rounded_rect(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h / 2.0);
    let deg = std::f64::consts::PI / 180.0;
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -90.0 * deg, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, 90.0 * deg);
    cr.arc(x + r, y + h - r, r, 90.0 * deg, 180.0 * deg);
    cr.arc(x + r, y + r, r, 180.0 * deg, 270.0 * deg);
    cr.close_path();
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
