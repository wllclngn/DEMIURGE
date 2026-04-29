use x11rb::connection::Connection;
use x11rb::properties::WmClass;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use crate::atoms::Atoms;
use crate::bar::Bar;
use crate::config::Config;
use crate::cursor::{self, CursorState};
use crate::display;
use crate::ewmh;
use crate::idle;
use crate::keys::{self, Action, Binding};
use crate::layout::Layout;
use crate::monitor::{self, Monitor, Monitors};
use crate::mouse::DragState;
use crate::mru;
use crate::spawn;

// Pending tag-targeted spawn: a class to match on the next incoming window,
// plus the tag that window should land on. FIFO consumption: first matching
// entry wins when a window with the given class appears.
#[derive(Debug, Clone)]
pub struct PendingSpawn {
    pub class_match: String,
    pub target_tag: usize,
}

// Transient message that temporarily replaces the focused window title in
// the bar center. Set by volume / brightness / media actions; cleared by
// the next redraw_bar tick after expires_at.
#[derive(Debug)]
pub struct Notification {
    pub text: String,
    pub expires_at: std::time::Instant,
}

pub const NOTIFICATION_TTL_MS: u64 = 1500;

// Per-client state
#[derive(Debug, Clone)]
pub struct Client {
    pub window: Window,
    pub tag: usize,
    pub floating: bool,
    pub fullscreen: bool,
    pub above: bool,
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub saved_floating: Option<(i32, i32, u32, u32)>,
    pub saved_fullscreen: Option<(i32, i32, u32, u32)>,
    pub title: String,
    // Counts WM-initiated unmaps whose UnmapNotify should not unmanage the
    // client. Bumped before each self-unmap, consumed in on_unmap_notify.
    pub unmap_ignore: u8,
}

pub struct Wm {
    pub conn: RustConnection,
    pub root: Window,
    pub atoms: Atoms,
    pub monitors: Monitors,
    pub clients: Vec<Client>,
    pub focus: Option<Window>,
    // Per-monitor active tag. Indexed parallel to self.monitors -- entry i
    // is the tag currently visible on monitors[i]. Length is always
    // monitors.len(); resized in refresh_monitors when RandR reports a
    // count change. The "current desktop" reported via EWMH is whichever
    // entry corresponds to focused_monitor.
    pub active_tags: Vec<usize>,
    // Index into self.monitors. The monitor that owns the next user
    // action: view_tag changes its active tag, new windows land on its
    // active tag, _NET_CURRENT_DESKTOP reports its tag. Updated on
    // click-to-focus (focus_window picks up the focused window's
    // monitor) and on bar tag clicks (the clicked panel's monitor
    // becomes focused).
    pub focused_monitor: usize,
    pub num_tags: usize,
    pub bindings: Vec<Binding>,
    pub bar_height: u32,
    pub check_window: Window,
    pub running: bool,
    pub layouts: Vec<Layout>,
    pub master_ratio: f64,
    pub drag: Option<DragState>,
    // Most-recently-used ring. mru[0] is the current focus; each window
    // appears exactly once. Alt+Tab cycles an index into this ring without
    // reordering it; the selection is committed on Alt release.
    pub mru: mru::WindowRing,
    pub mru_cycle: Option<mru::WindowCycle>,
    pub pending_spawns: Vec<PendingSpawn>,
    pub notification: Option<Notification>,
    pub alt_keycodes: Vec<Keycode>,
    pub keymap: keys::KeyMap,
    pub bar: Option<Bar>,
    // Set by RandR notify handlers; consumed once per event-loop iteration.
    // Coalesces the burst of CRTC/Output/ScreenChange events that xrandr
    // emits into a single refresh.
    pub monitors_dirty: bool,
    // Cursor auto-hide state. Driven by the cursor timerfd in
    // main.rs::run; toggled via reload_config when [cursor] changes.
    pub cursor: CursorState,
    // Last [input.keyboard] values applied. Cached so MappingNotify
    // (which fires when xkbcomp / setxkbmap / USB hot-plug remap the
    // keyboard) can re-apply the repeat delay / rate that the X
    // server may have just clobbered. 0/0 means "DEMIURGE doesn't
    // own these; leave server default" and skips re-application.
    pub input_repeat_delay: u32,
    pub input_repeat_rate: u32,
}

impl Wm {
    pub fn init(config: &Config) -> Result<Self, String> {
        let (conn, screen_num) =
            RustConnection::connect(None).map_err(|e| format!("x11 connect: {}", e))?;

        let screen = &conn.setup().roots[screen_num];
        let root = screen.root;

        // Claim the root window (become the WM)
        conn.change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new().event_mask(
                EventMask::SUBSTRUCTURE_REDIRECT
                    | EventMask::SUBSTRUCTURE_NOTIFY
                    | EventMask::STRUCTURE_NOTIFY
                    | EventMask::PROPERTY_CHANGE
                    | EventMask::BUTTON_PRESS,
            ),
        )
        .map_err(|e| format!("change root attributes: {}", e))?
        .check()
        .map_err(|_| {
            "another window manager is already running".to_string()
        })?;

        let atoms = Atoms::new(&conn)
            .map_err(|e| format!("intern atoms: {}", e))?
            .reply()
            .map_err(|e| format!("atoms reply: {}", e))?;

        // Initial monitor probe (just to get connector names for
        // [[display]] match patterns). The post-DSR re-query below
        // is what feeds EWMH / bar / arrange.
        let initial_monitors = monitor::query(&conn, root);
        for (i, mon) in initial_monitors.iter().enumerate() {
            eprintln!(
                "[demiurge] monitor {}: '{}' {}x{}+{}+{}",
                i, mon.name, mon.width, mon.height, mon.x, mon.y
            );
        }

        // Apply [[display]] entries first thing -- mode + DSR + DPI
        // changes all reshape the framebuffer, so we want them done
        // before everything that reads the post-config geometry
        // (EWMH workarea, bar panel sizing, ...). Re-query monitors
        // after so the rest of init sees the post-DSR sizes.
        if !config.displays.is_empty() {
            display::apply_all(&config.displays, &initial_monitors);
        }
        let monitors = monitor::query(&conn, root);
        if !config.displays.is_empty() {
            for (i, mon) in monitors.iter().enumerate() {
                eprintln!(
                    "[demiurge] post-display monitor {}: '{}' {}x{}+{}+{}",
                    i, mon.name, mon.width, mon.height, mon.x, mon.y
                );
            }
        }

        // Subscribe to RandR change notifies. Without this, xrandr -s,
        // hot-plug, and per-output mode swaps go unnoticed and the bar /
        // workarea / tile geometry stay frozen at startup values.
        if let Err(e) = monitor::select_input(&conn, root) {
            eprintln!("[demiurge] randr select_input failed: {} (display tracking disabled)", e);
        }

        // Activate XFixes so cursor::hide / show actually take effect.
        // Failure here just means auto_hide silently no-ops; not fatal.
        if let Err(e) = cursor::init_xfixes(&conn) {
            eprintln!("[demiurge] xfixes init failed: {} (cursor auto-hide disabled)", e);
        }

        // Apply [input.keyboard] settings if the user opted in.
        // 0/0 means "leave the server default in place" -- skip the
        // call so existing setxkbmap / xset state is preserved when
        // the user hasn't explicitly configured DEMIURGE to own it.
        if config.input.keyboard.repeat_delay > 0 || config.input.keyboard.repeat_rate > 0 {
            apply_keyboard_repeat(
                &conn,
                config.input.keyboard.repeat_delay,
                config.input.keyboard.repeat_rate,
            );
        }

        // Apply [input.keyboard] layout/variant/options via XKB if
        // the user has explicitly opted in (any non-empty field).
        // The X server compiles internally -- no setxkbmap or
        // xkbcomp shellout from us.
        if !config.input.keyboard.layout.is_empty()
            || !config.input.keyboard.variant.is_empty()
            || !config.input.keyboard.options.is_empty()
        {
            apply_xkb_layout(
                &conn,
                &config.input.keyboard.layout,
                &config.input.keyboard.variant,
                &config.input.keyboard.options,
            );
        }

        // Activate the DPMS extension if the user has any DPMS or
        // screensaver thresholds configured. Failing here just
        // means [input.idle] silently no-ops; not fatal.
        if config.input.idle.dpms_standby_seconds
            + config.input.idle.dpms_suspend_seconds
            + config.input.idle.dpms_off_seconds
            > 0
        {
            if let Err(e) = idle::init_dpms(&conn) {
                eprintln!("[demiurge] dpms init failed: {} (DPMS disabled)", e);
            }
            idle::apply_dpms(
                &conn,
                config.input.idle.dpms_standby_seconds,
                config.input.idle.dpms_suspend_seconds,
                config.input.idle.dpms_off_seconds,
            );
        }
        if config.input.idle.screensaver_seconds > 0 {
            idle::apply_screensaver(&conn, config.input.idle.screensaver_seconds);
        }

        // Apply [input.bell] if the user has touched any field.
        // None / None / None / None = leave server default.
        if config.input.bell.enabled.is_some()
            || config.input.bell.volume.is_some()
            || config.input.bell.pitch_hz.is_some()
            || config.input.bell.duration_ms.is_some()
        {
            apply_bell(&conn, &config.input.bell);
        }


        let num_tags = config.general.tags.len();
        let tag_names = config.general.tags.clone();
        let bar_height = config.bar.height;
        let master_ratio = config.general.master_ratio;
        let default_layout = Layout::from_str(&config.general.default_layout);
        let layouts = vec![default_layout; num_tags];

        // Cache keyboard mapping (avoids per-keystroke roundtrips)
        let keymap = keys::load_keymap(&conn);

        // Compile keybindings
        let bindings = keys::compile(&config.keybinds, &keymap)?;
        keys::grab_all(&conn, root, &bindings);

        // Resolve Alt keycodes for MRU cycle detection
        let alt_keycodes = keys::keymap_alt_keycodes(&keymap);

        // Passive button grabs for Super+drag move/resize
        crate::mouse::grab_buttons(&conn, root);

        // EWMH setup
        ewmh::set_supported(&conn, root, &atoms);
        let check_window = ewmh::set_wm_check(&conn, root, &atoms, "demiurge");
        ewmh::set_desktop_count(&conn, root, &atoms, num_tags as u32);
        ewmh::set_desktop_names(&conn, root, &atoms, &tag_names);
        ewmh::set_current_desktop(&conn, root, &atoms, 0);
        ewmh::set_active_window(&conn, root, &atoms, None);
        ewmh::set_client_list(&conn, root, &atoms, &[]);
        ewmh::set_workarea(&conn, root, &atoms, num_tags as u32, &monitors, bar_height);
        ewmh::set_desktop_geometry(&conn, root, &atoms, &monitors);
        ewmh::set_viewport(&conn, root, &atoms, num_tags as u32);

        conn.flush().map_err(|e| format!("flush: {}", e))?;

        // Create status bar
        let bar = Bar::create(&conn, root, &atoms, config, &monitors, &tag_names)?;

        // Adopt existing windows
        let existing = conn
            .query_tree(root)
            .map_err(|e| format!("query_tree: {}", e))?
            .reply()
            .map_err(|e| format!("query_tree reply: {}", e))?;

        // Initial active_tags: one entry per monitor. monitors.len() >= 1
        // by Monitors's type-level invariant. Each monitor starts on a
        // distinct tag if possible (monitor 0 -> tag 0, monitor 1 -> tag
        // 1, ...) so a fresh dual-head session shows two empty tags
        // rather than two views of the same one; extras wrap to the
        // last tag.
        let active_tags: Vec<usize> = {
            let last = num_tags.saturating_sub(1);
            (0..monitors.len()).map(|i| i.min(last)).collect()
        };

        let mut wm = Self {
            conn,
            root,
            atoms,
            monitors,
            clients: Vec::new(),
            focus: None,
            active_tags,
            focused_monitor: 0,
            num_tags,
            bindings,
            bar_height,
            check_window,
            running: true,
            layouts,
            master_ratio,
            drag: None,
            mru: mru::WindowRing::new(),
            mru_cycle: None,
            pending_spawns: Vec::new(),
            notification: None,
            alt_keycodes,
            keymap,
            bar: Some(bar),
            monitors_dirty: false,
            cursor: CursorState::new(
                config.cursor.auto_hide,
                config.cursor.auto_hide_seconds,
            ),
            input_repeat_delay: config.input.keyboard.repeat_delay,
            input_repeat_rate: config.input.keyboard.repeat_rate,
        };

        // Manage pre-existing windows
        for &win in &existing.children {
            if let Ok(attrs) = wm.conn.get_window_attributes(win).and_then(|c| Ok(c.reply())) {
                if let Ok(attrs) = attrs {
                    if attrs.map_state != MapState::UNMAPPED
                        && !attrs.override_redirect
                    {
                        wm.manage(win);
                    }
                }
            }
        }

        // Bar is already marked all-dirty on Bar::create; the first
        // commit in the event loop renders the initial state.

        eprintln!(
            "[demiurge] started (tags: {}, bindings: {}, existing: {})",
            wm.num_tags,
            wm.bindings.len(),
            wm.clients.len(),
        );

        Ok(wm)
    }

    pub fn connection_fd(&self) -> i32 {
        use std::os::fd::AsRawFd;
        self.conn.stream().as_raw_fd()
    }

    // WINDOW MANAGEMENT

    pub fn manage(&mut self, window: Window) {
        // Skip if already managed or is our own window
        if self.clients.iter().any(|c| c.window == window)
            || window == self.check_window
            || self.bar.as_ref().map_or(false, |b| b.contains_window(window))
        {
            return;
        }

        // Skip dock/desktop type windows
        if self.is_dock_or_desktop(window) {
            return;
        }

        // Subscribe to events on this client
        let _ = self.conn.change_window_attributes(
            window,
            &ChangeWindowAttributesAux::new().event_mask(
                EventMask::ENTER_WINDOW
                    | EventMask::STRUCTURE_NOTIFY
                    | EventMask::PROPERTY_CHANGE
                    | EventMask::FOCUS_CHANGE,
            ),
        );

        // Click-to-focus: passive Button1 grab with sync mode
        crate::mouse::grab_focus_button(&self.conn, window);

        let class = self.get_class(window);

        // Route to pending tag-targeted spawn if the class matches; otherwise
        // land on the active tag. FIFO: first matching entry wins.
        let target_tag = match self
            .pending_spawns
            .iter()
            .position(|p| p.class_match == class)
        {
            Some(idx) => self.pending_spawns.remove(idx).target_tag,
            None => self.active_tags[self.focused_monitor],
        };

        // Set frame extents (zeros -- no decorations)
        ewmh::set_frame_extents(&self.conn, window, &self.atoms);
        ewmh::set_client_desktop(&self.conn, window, &self.atoms, target_tag as u32);

        let (x, y, w, h) = self
            .conn
            .get_geometry(window)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|g| (g.x as i32, g.y as i32, g.width as u32, g.height as u32))
            .unwrap_or((0, 0, 800, 600));

        let should_float = self.is_floating_type(window) || self.is_transient(window);
        let title = self.get_title(window);

        let mut client = Client {
            window,
            tag: target_tag,
            floating: should_float || self.layouts[target_tag] == Layout::Floating,
            fullscreen: false,
            above: false,
            x,
            y,
            w,
            h,
            saved_floating: None,
            saved_fullscreen: None,
            title: title.clone(),
            unmap_ignore: 0,
        };

        // Respect initial _NET_WM_STATE
        let initial_state = self.get_wm_state_atoms(window);
        if initial_state.contains(&self.atoms._NET_WM_STATE_FULLSCREEN) {
            client.fullscreen = true;
            client.saved_fullscreen = Some((x, y, w, h));
        }
        if initial_state.contains(&self.atoms._NET_WM_STATE_ABOVE) {
            client.above = true;
        }

        eprintln!(
            "[{}] [INFO]   manage '{}' (class='{}', title='{}', tag={}, geom={}x{}+{}+{})",
            local_time(),
            window,
            class,
            title,
            target_tag,
            w,
            h,
            x,
            y,
        );

        self.clients.push(client);
        self.update_client_list();
        ewmh::set_allowed_actions(&self.conn, window, &self.atoms);

        // Apply initial fullscreen geometry
        if self.clients.last().map_or(false, |c| c.fullscreen) {
            let (mx, my, mw, mh) = {
                let mon = self.client_monitor(self.clients.last().unwrap());
                (mon.x, mon.y, mon.width, mon.height)
            };
            let _ = self.conn.configure_window(
                window,
                &ConfigureWindowAux::new()
                    .x(mx)
                    .y(my)
                    .width(mw)
                    .height(mh)
                    .stack_mode(StackMode::ABOVE),
            );
        }

        // Focus the new window only if its tag is currently visible on
        // some monitor. Cross-tag spawns (tag-targeted startup) stay in
        // the background until a monitor views that tag.
        if self.tag_visible(target_tag) {
            self.focus_window(Some(window));
        }

        // Always re-arrange: target_tag's monitor (if visible) needs to
        // retile to include the new window. arrange() is now per-monitor,
        // so it's safe to call unconditionally; the monitor showing a
        // floating-layout tag is a no-op for that monitor.
        self.arrange();

        // Tag-occupied state may have flipped (first window on this tag).
        if let Some(ref mut bar) = self.bar {
            bar.mark_tags_dirty();
        }
    }

    pub fn unmanage(&mut self, window: Window) {
        if let Some(pos) = self.clients.iter().position(|c| c.window == window) {
            let tag = self.clients[pos].tag;
            self.clients.remove(pos);
            mru::on_unmanage(self, window);
            self.update_client_list();

            // If a cycle was active, its implicit state is now invalid
            if self.mru_cycle.is_some() {
                mru::finish_cycle(self);
            }

            // If we lost focus, pick next window on the focused monitor's
            // active tag. on_unmanage scrubbed `window` from the MRU ring
            // already, so pick_focus_for_tag naturally skips it; `exclude`
            // is belt-and-suspenders.
            if self.focus == Some(window) {
                let next = self.pick_focus_for_tag(self.active_tag(), Some(window));
                self.focus_window(next);
            }

            // Re-tile if the closed window's tag is visible on any
            // monitor; arrange iterates monitors so it's safe to call
            // unconditionally for non-floating layouts.
            if self.tag_visible(tag) && self.layouts[tag] != Layout::Floating {
                self.arrange();
            }

            // Tag may have just emptied; tag-occupied coloring needs
            // a refresh.
            if let Some(ref mut bar) = self.bar {
                bar.mark_tags_dirty();
            }
        }
    }

    // FOCUS

    pub fn focus_window(&mut self, window: Option<Window>) {
        // Promote target to the front of the MRU ring. Suppressed while a
        // cycle is active — mru.rs raises the cycle target visually without
        // reordering the ring; the commit happens in finish_cycle.
        if self.mru_cycle.is_none() {
            if let Some(target) = window {
                mru::on_focus(self, target);
            }
        }

        // If the new focus belongs to a different monitor, the user has
        // implicitly switched focused_monitor. Update so subsequent
        // view_tag / new-window-spawn / current-desktop reporting all
        // route to the right monitor. Suppressed during MRU cycle (the
        // ring is mid-step; commit happens in finish_cycle).
        if self.mru_cycle.is_none()
            && let Some(win) = window
            && let Some(client) = self.clients.iter().find(|c| c.window == win)
        {
            let mi = self.client_monitor_index(client);
            if mi != self.focused_monitor {
                self.focused_monitor = mi;
                ewmh::set_current_desktop(
                    &self.conn,
                    self.root,
                    &self.atoms,
                    self.active_tags[mi] as u32,
                );
            }
        }

        self.focus = window;
        ewmh::set_active_window(&self.conn, self.root, &self.atoms, window);

        if let Some(win) = window {
            let _ = self.conn.set_input_focus(
                InputFocus::POINTER_ROOT,
                win,
                x11rb::CURRENT_TIME,
            );
            self.send_take_focus(win);
            let _ = self.conn.configure_window(
                win,
                &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
            );

            // Re-raise bar above focused window (unless fullscreen)
            let is_fs = self
                .clients
                .iter()
                .find(|c| c.window == win)
                .map_or(false, |c| c.fullscreen);
            if !is_fs {
                if let Some(ref bar) = self.bar {
                    bar.raise_all(&self.conn);
                }
            }
        } else {
            let _ = self.conn.set_input_focus(
                InputFocus::POINTER_ROOT,
                self.root,
                x11rb::CURRENT_TIME,
            );
        }

        // Focus changed -> title region needs to update. The event-loop
        // commit pass picks this up.
        if let Some(ref mut bar) = self.bar {
            bar.mark_title_dirty();
        }
        let _ = self.conn.flush();
    }

    // TAGS

    // Switch the focused monitor to `tag`. Used by Alt+Tab cross-tag
    // stepping where the cycle has already chosen a target window
    // (focus is set later).
    //
    // Multi-monitor semantics:
    //   - If `tag` is already active on the focused monitor, no-op.
    //   - If `tag` is active on another monitor B, swap: B takes the
    //     tag the focused monitor was on, focused monitor takes `tag`.
    //     This matches XMonad's behavior and avoids ever showing the
    //     same tag on two monitors at once (which would mean two
    //     simultaneous views of the same client list).
    //   - Otherwise the focused monitor's tag is replaced wholesale;
    //     whatever tag it was on is now hidden (no monitor shows it).
    //
    // Visibility is then recomputed: tags that lost their monitor get
    // their clients unmapped, tags that gained one get mapped.
    pub fn view_tag_no_focus(&mut self, tag: usize) {
        if tag >= self.num_tags {
            return;
        }
        let m = self.focused_monitor;
        let old_tag = self.active_tags[m];
        if old_tag == tag {
            return;
        }

        // Snapshot pre-state so we can diff visibility.
        let prev: Vec<usize> = self.active_tags.clone();

        // Apply the change with swap-on-collision.
        if let Some(other) = self.monitor_for_tag(tag) {
            // Swap: the other monitor takes our previous tag.
            self.active_tags[other] = old_tag;
        }
        self.active_tags[m] = tag;

        let now: Vec<usize> = self.active_tags.clone();
        let was_visible = |t: usize| prev.contains(&t);
        let is_visible = |t: usize| now.contains(&t);

        // Map clients on tags that became visible; unmap clients on
        // tags that became hidden. Bump unmap_ignore before each
        // self-unmap so the UnmapNotify echo doesn't unmanage.
        for client in &mut self.clients {
            let pre = was_visible(client.tag);
            let post = is_visible(client.tag);
            if !pre && post {
                let _ = self.conn.map_window(client.window);
            } else if pre && !post {
                client.unmap_ignore = client.unmap_ignore.saturating_add(1);
                let _ = self.conn.unmap_window(client.window);
            }
        }

        ewmh::set_current_desktop(&self.conn, self.root, &self.atoms, tag as u32);

        // Re-tile: every monitor's view may have changed (we may have
        // swapped). arrange() is per-monitor and a no-op for monitors
        // whose tag is on a floating layout.
        self.arrange();

        // Active tag highlight changed on the focused monitor (and the
        // swap target if any); tag region needs a repaint on the
        // affected panels. mark_tags_dirty repaints all panels which is
        // cheap enough.
        if let Some(ref mut bar) = self.bar {
            bar.mark_tags_dirty();
        }
        let _ = self.conn.flush();
    }

    pub fn view_tag(&mut self, tag: usize) {
        if tag >= self.num_tags || self.active_tags[self.focused_monitor] == tag {
            return;
        }
        self.view_tag_no_focus(tag);
        // Focus picks from the focused monitor's now-visible tag.
        let next = self.pick_focus_for_tag(self.active_tags[self.focused_monitor], None);
        self.focus_window(next);
    }

    // Pick the window to focus on a given tag, in priority order:
    //   1. Most-recently-used window on that tag (via wm.mru). This is
    //      the window the user was actually using when they last left
    //      this tag -- the "where I left off" answer.
    //   2. If no MRU entry matches (e.g. tag-targeted spawn that landed
    //      on a background tag and never got focused), fall back to
    //      most-recently-managed client on the tag (clients vector,
    //      reverse-iterated).
    // `exclude` drops a specific window from consideration (used by
    // unmanage and move_to_tag to skip the window being removed).
    fn pick_focus_for_tag(&self, tag: usize, exclude: Option<Window>) -> Option<Window> {
        self.mru
            .iter()
            .copied()
            .find(|&w| {
                Some(w) != exclude
                    && self
                        .clients
                        .iter()
                        .any(|c| c.window == w && c.tag == tag)
            })
            .or_else(|| {
                self.clients
                    .iter()
                    .rev()
                    .find(|c| c.tag == tag && Some(c.window) != exclude)
                    .map(|c| c.window)
            })
    }

    pub fn view_prev_tag(&mut self) {
        let cur = self.active_tags[self.focused_monitor];
        let prev = if cur == 0 { self.num_tags - 1 } else { cur - 1 };
        self.view_tag(prev);
    }

    pub fn view_next_tag(&mut self) {
        let cur = self.active_tags[self.focused_monitor];
        let next = (cur + 1) % self.num_tags;
        self.view_tag(next);
    }

    pub fn move_to_tag(&mut self, tag: usize) {
        if tag >= self.num_tags {
            return;
        }
        let win = match self.focus {
            Some(w) => w,
            None => return,
        };
        // Compute visibility before borrowing clients mutably; tag_visible
        // reads self.active_tags which would conflict with iter_mut.
        let dest_visible = self.tag_visible(tag);
        let moved = if let Some(client) = self.clients.iter_mut().find(|c| c.window == win) {
            if client.tag == tag {
                return;
            }
            client.tag = tag;
            if !dest_visible {
                client.unmap_ignore = client.unmap_ignore.saturating_add(1);
            }
            true
        } else {
            false
        };
        if !moved {
            return;
        }

        ewmh::set_client_desktop(&self.conn, win, &self.atoms, tag as u32);

        if !dest_visible {
            let _ = self.conn.unmap_window(win);
            // Focus pick: the focused monitor's tag (which is the tag
            // we just walked off if the user was on the focused
            // monitor's view). pick_focus_for_tag excludes `win`.
            let next = self.pick_focus_for_tag(self.active_tag(), Some(win));
            self.focus_window(next);
        } else {
            // Window stays mapped (destination tag is showing on some
            // monitor). Re-arrange so it lands in the right monitor's
            // work area.
            self.arrange();
        }
        let _ = self.conn.flush();
    }

    // ACTIONS

    pub fn handle_action(&mut self, action: &Action) {
        match action {
            Action::Spawn(cmd) => spawn::spawn(cmd),
            Action::CloseWindow => {
                if let Some(win) = self.focus {
                    ewmh::close_window(&self.conn, win, &self.atoms);
                }
            }
            Action::Quit => self.running = false,
            Action::ViewTag(t) => self.view_tag(*t),
            Action::ViewPrevTag => self.view_prev_tag(),
            Action::ViewNextTag => self.view_next_tag(),
            Action::MoveToTag(t) => self.move_to_tag(*t),
            Action::ToggleAbove => self.toggle_above(),
            Action::ToggleFullscreen => self.toggle_fullscreen(),
            Action::MruNext => crate::mru::start_or_advance(self, true, crate::mru::CycleScope::Tag),
            Action::MruPrev => crate::mru::start_or_advance(self, false, crate::mru::CycleScope::Tag),
            Action::MruNextGlobal => crate::mru::start_or_advance(self, true, crate::mru::CycleScope::All),
            Action::MruPrevGlobal => crate::mru::start_or_advance(self, false, crate::mru::CycleScope::All),
            Action::ToggleLayout => self.toggle_layout(),
            Action::RunPrompt => {
                if let Some(ref mut bar) = self.bar {
                    if bar.prompt.is_some() {
                        return;
                    }
                    let _ = self.conn.grab_keyboard(
                        false,
                        self.root,
                        x11rb::CURRENT_TIME,
                        GrabMode::ASYNC,
                        GrabMode::ASYNC,
                    );
                    bar.start_prompt();
                    bar.mark_prompt_dirty();
                }
            }
            Action::Screenshot => self.screenshot(),
            Action::Lock => spawn::spawn("gordian_knot"),
            Action::VolumeUp => self.volume_delta("5%+"),
            Action::VolumeDown => self.volume_delta("5%-"),
            Action::VolumeMute => self.volume_toggle_mute(false),
            Action::VolumeMicMute => self.volume_toggle_mute(true),
            Action::BrightnessUp => self.brightness_delta("-inc"),
            Action::BrightnessDown => self.brightness_delta("-dec"),
            Action::MediaPlayPause => self.media_command("play-pause"),
            Action::MediaNext => self.media_command("next"),
            Action::MediaPrev => self.media_command("previous"),
        }
    }

    // Transient bar notification. Replaces the focused-window title in the
    // bar center for NOTIFICATION_TTL_MS, rendered in bar.notification_fg.
    pub fn notify(&mut self, text: String) {
        self.notification = Some(Notification {
            text,
            expires_at: std::time::Instant::now()
                + std::time::Duration::from_millis(NOTIFICATION_TTL_MS),
        });
        if let Some(ref mut bar) = self.bar {
            bar.mark_title_dirty();
        }
    }

    fn volume_delta(&mut self, delta: &str) {
        let _ = std::process::Command::new("wpctl")
            .args(["set-volume", "@DEFAULT_AUDIO_SINK@", delta])
            .status();
        let value = read_wpctl_volume("@DEFAULT_AUDIO_SINK@")
            .unwrap_or_else(|| "unknown".into());
        self.notify(format!("NOTIFICATION: System Volume, {}.", value));
    }

    fn volume_toggle_mute(&mut self, is_mic: bool) {
        let target = if is_mic {
            "@DEFAULT_AUDIO_SOURCE@"
        } else {
            "@DEFAULT_AUDIO_SINK@"
        };
        let _ = std::process::Command::new("wpctl")
            .args(["set-mute", target, "toggle"])
            .status();
        let value = read_wpctl_volume(target).unwrap_or_else(|| "unknown".into());
        let label = if is_mic { "Microphone" } else { "System Volume" };
        self.notify(format!("NOTIFICATION: {}, {}.", label, value));
    }

    fn brightness_delta(&mut self, flag: &str) {
        let _ = std::process::Command::new("xbacklight")
            .args([flag, "5"])
            .status();
        let value = read_brightness().unwrap_or_else(|| "unknown".into());
        self.notify(format!("NOTIFICATION: Brightness, {}.", value));
    }

    fn media_command(&mut self, cmd: &str) {
        let _ = std::process::Command::new("playerctl").arg(cmd).status();
        match read_media_state() {
            Some((state, Some(title))) => {
                self.notify(format!("NOTIFICATION: {}, {}.", state, title));
            }
            Some((state, None)) => {
                self.notify(format!("NOTIFICATION: {}.", state));
            }
            None => {
                self.notify("NOTIFICATION: No media player.".into());
            }
        }
    }

    fn screenshot(&self) {
        let home = match std::env::var("HOME") {
            Ok(h) => h,
            Err(_) => {
                eprintln!("[demiurge] screenshot: HOME not set");
                return;
            }
        };
        let stamp = unsafe {
            let mut t: libc::time_t = 0;
            libc::time(&mut t);
            let mut tm: libc::tm = std::mem::zeroed();
            libc::localtime_r(&t, &mut tm);
            let mut buf = [0u8; 64];
            let fmt = std::ffi::CString::new("%Y%m%d-%H%M%S").unwrap();
            let len = libc::strftime(
                buf.as_mut_ptr() as *mut libc::c_char,
                buf.len(),
                fmt.as_ptr(),
                &tm,
            );
            String::from_utf8_lossy(&buf[..len]).to_string()
        };
        let path = std::path::PathBuf::from(home)
            .join("Pictures")
            .join(format!("screenshot-{}.png", stamp));
        match crate::torrentius::capture_root_to_png(&self.conn, self.root, &path) {
            Ok(()) => eprintln!("[demiurge] screenshot: {}", path.display()),
            Err(e) => eprintln!("[demiurge] screenshot failed: {}", e),
        }
    }

    fn toggle_above(&mut self) {
        if let Some(win) = self.focus {
            if let Some(client) = self.clients.iter_mut().find(|c| c.window == win) {
                client.above = !client.above;
                let action: u32 = if client.above { 1 } else { 0 };
                self.send_wm_state(win, action, self.atoms._NET_WM_STATE_ABOVE, 0);
            }
        }
    }

    fn toggle_fullscreen(&mut self) {
        let win = match self.focus {
            Some(w) => w,
            None => return,
        };
        let pos = match self.clients.iter().position(|c| c.window == win) {
            Some(p) => p,
            None => return,
        };

        let new_fs = !self.clients[pos].fullscreen;
        self.clients[pos].fullscreen = new_fs;

        if new_fs {
            let c = &self.clients[pos];
            self.clients[pos].saved_fullscreen = Some((c.x, c.y, c.w, c.h));
            let (mx, my, mw, mh) = {
                let mon = self.client_monitor(&self.clients[pos]);
                (mon.x, mon.y, mon.width, mon.height)
            };
            let _ = self.conn.configure_window(
                win,
                &ConfigureWindowAux::new()
                    .x(mx)
                    .y(my)
                    .width(mw)
                    .height(mh)
                    .stack_mode(StackMode::ABOVE),
            );
        } else if let Some((x, y, w, h)) = self.clients[pos].saved_fullscreen.take() {
            self.clients[pos].x = x;
            self.clients[pos].y = y;
            self.clients[pos].w = w;
            self.clients[pos].h = h;
            let _ = self.conn.configure_window(
                win,
                &ConfigureWindowAux::new().x(x).y(y).width(w).height(h),
            );
        }

        let action: u32 = if new_fs { 1 } else { 0 };
        self.send_wm_state(win, action, self.atoms._NET_WM_STATE_FULLSCREEN, 0);
        let _ = self.conn.flush();
    }

    fn send_wm_state(&self, window: Window, action: u32, prop1: Atom, prop2: Atom) {
        let mut state = self.get_wm_state_atoms(window);

        match action {
            1 => {
                // Add
                if !state.contains(&prop1) {
                    state.push(prop1);
                }
                if prop2 != 0 && !state.contains(&prop2) {
                    state.push(prop2);
                }
            }
            0 => {
                // Remove
                state.retain(|&a| a != prop1 && (prop2 == 0 || a != prop2));
            }
            2 => {
                // Toggle
                if state.contains(&prop1) {
                    state.retain(|&a| a != prop1);
                } else {
                    state.push(prop1);
                }
            }
            _ => {}
        }

        let _ = self.conn.change_property32(
            PropMode::REPLACE,
            window,
            self.atoms._NET_WM_STATE,
            AtomEnum::ATOM,
            &state,
        );
    }

    fn get_wm_state_atoms(&self, window: Window) -> Vec<Atom> {
        self.conn
            .get_property(
                false,
                window,
                self.atoms._NET_WM_STATE,
                AtomEnum::ATOM,
                0,
                64,
            )
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|p| p.value32().map(|v| v.collect()))
            .unwrap_or_default()
    }

    // CLIENT MESSAGE HANDLING

    pub fn handle_client_message(&mut self, msg: &ClientMessageEvent) {
        let data = msg.data.as_data32();

        if msg.type_ == self.atoms._NET_ACTIVE_WINDOW {
            // App requests focus
            if self.clients.iter().any(|c| c.window == msg.window) {
                self.focus_window(Some(msg.window));
            }
        } else if msg.type_ == self.atoms._NET_CLOSE_WINDOW {
            ewmh::close_window(&self.conn, msg.window, &self.atoms);
        } else if msg.type_ == self.atoms._NET_WM_STATE {
            let action = data[0];
            let prop1 = data[1];
            let prop2 = data[2];
            self.handle_state_request(msg.window, action, prop1, prop2);
        } else if msg.type_ == self.atoms._NET_CURRENT_DESKTOP {
            let desktop = data[0] as usize;
            self.view_tag(desktop);
        } else if msg.type_ == self.atoms._NET_WM_MOVERESIZE {
            let x_root = data[0] as i16;
            let y_root = data[1] as i16;
            let direction = data[2];
            match direction {
                8 => crate::mouse::start_csd_move(self, msg.window, x_root, y_root),
                11 => {
                    if self.drag.is_some() {
                        crate::mouse::end_drag(self);
                    }
                }
                _ => {}
            }
        } else if msg.type_ == self.atoms._NET_WM_DESKTOP {
            let desktop = data[0] as usize;
            if self.focus == Some(msg.window) || self.clients.iter().any(|c| c.window == msg.window)
            {
                // Temporarily focus this window to move it
                let old_focus = self.focus;
                self.focus = Some(msg.window);
                self.move_to_tag(desktop);
                if old_focus != Some(msg.window) {
                    self.focus = old_focus;
                }
            }
        }
    }

    fn handle_state_request(&mut self, window: Window, action: u32, prop1: u32, prop2: u32) {
        if prop1 == self.atoms._NET_WM_STATE_FULLSCREEN
            || prop2 == self.atoms._NET_WM_STATE_FULLSCREEN
        {
            if let Some(pos) = self.clients.iter().position(|c| c.window == window) {
                let should_fs = match action {
                    1 => true,
                    0 => false,
                    2 => !self.clients[pos].fullscreen,
                    _ => return,
                };
                self.clients[pos].fullscreen = should_fs;
                if should_fs {
                    let c = &self.clients[pos];
                    self.clients[pos].saved_fullscreen = Some((c.x, c.y, c.w, c.h));
                    let (mx, my, mw, mh) = {
                        let mon = self.client_monitor(&self.clients[pos]);
                        (mon.x, mon.y, mon.width, mon.height)
                    };
                    let _ = self.conn.configure_window(
                        window,
                        &ConfigureWindowAux::new()
                            .x(mx)
                            .y(my)
                            .width(mw)
                            .height(mh)
                            .stack_mode(StackMode::ABOVE),
                    );
                } else if let Some((x, y, w, h)) = self.clients[pos].saved_fullscreen.take() {
                    self.clients[pos].x = x;
                    self.clients[pos].y = y;
                    self.clients[pos].w = w;
                    self.clients[pos].h = h;
                    let _ = self.conn.configure_window(
                        window,
                        &ConfigureWindowAux::new().x(x).y(y).width(w).height(h),
                    );
                }
            }
        }

        self.send_wm_state(window, action, prop1, prop2);
        // Above/fullscreen toggles don't change any bar-rendered state,
        // but mark the title anyway in case focus subtly shifts.
        if let Some(ref mut bar) = self.bar {
            bar.mark_title_dirty();
        }
        let _ = self.conn.flush();
    }

    // PROPERTY HELPERS

    pub fn get_class(&self, window: Window) -> String {
        WmClass::get(&self.conn, window)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .and_then(|opt| opt)
            .map(|wm| String::from_utf8_lossy(wm.class()).to_string())
            .unwrap_or_default()
    }

    pub fn get_title(&self, window: Window) -> String {
        // Try _NET_WM_NAME first
        if let Some(title) = self.get_string_property(window, self.atoms._NET_WM_NAME) {
            return title;
        }
        self.get_string_property(window, self.atoms.WM_NAME)
            .unwrap_or_default()
    }

    fn get_string_property(&self, window: Window, atom: Atom) -> Option<String> {
        let reply = self
            .conn
            .get_property(false, window, atom, AtomEnum::ANY, 0, 1024)
            .ok()?
            .reply()
            .ok()?;

        if reply.value.is_empty() {
            return None;
        }
        Some(String::from_utf8_lossy(&reply.value).to_string())
    }

    fn is_floating_type(&self, window: Window) -> bool {
        let reply = self
            .conn
            .get_property(
                false,
                window,
                self.atoms._NET_WM_WINDOW_TYPE,
                AtomEnum::ATOM,
                0,
                8,
            )
            .ok()
            .and_then(|c| c.reply().ok());

        if let Some(prop) = reply {
            if let Some(types) = prop.value32() {
                for t in types {
                    if t == self.atoms._NET_WM_WINDOW_TYPE_DIALOG
                        || t == self.atoms._NET_WM_WINDOW_TYPE_UTILITY
                        || t == self.atoms._NET_WM_WINDOW_TYPE_TOOLBAR
                        || t == self.atoms._NET_WM_WINDOW_TYPE_MENU
                        || t == self.atoms._NET_WM_WINDOW_TYPE_SPLASH
                        || t == self.atoms._NET_WM_WINDOW_TYPE_NOTIFICATION
                    {
                        return true;
                    }
                }
            }
        }
        false
    }

    fn is_transient(&self, window: Window) -> bool {
        self.conn
            .get_property(
                false,
                window,
                AtomEnum::WM_TRANSIENT_FOR,
                AtomEnum::WINDOW,
                0,
                1,
            )
            .ok()
            .and_then(|c| c.reply().ok())
            .map_or(false, |p| p.value.len() >= 4)
    }

    fn send_take_focus(&self, window: Window) {
        if ewmh::supports_protocol(&self.conn, window, &self.atoms, self.atoms.WM_TAKE_FOCUS) {
            let data = ClientMessageData::from([
                self.atoms.WM_TAKE_FOCUS,
                x11rb::CURRENT_TIME,
                0,
                0,
                0,
            ]);
            let event = ClientMessageEvent::new(32, window, self.atoms.WM_PROTOCOLS, data);
            let _ = self.conn.send_event(false, window, EventMask::NO_EVENT, event);
        }
    }

    fn is_dock_or_desktop(&self, window: Window) -> bool {
        let reply = self
            .conn
            .get_property(
                false,
                window,
                self.atoms._NET_WM_WINDOW_TYPE,
                AtomEnum::ATOM,
                0,
                1,
            )
            .ok()
            .and_then(|c| c.reply().ok());

        match reply {
            Some(prop) if prop.value.len() >= 4 => {
                let type_atom = u32::from_ne_bytes([
                    prop.value[0],
                    prop.value[1],
                    prop.value[2],
                    prop.value[3],
                ]);
                type_atom == self.atoms._NET_WM_WINDOW_TYPE_DOCK
                    || type_atom == self.atoms._NET_WM_WINDOW_TYPE_DESKTOP
            }
            _ => false,
        }
    }

    fn update_client_list(&self) {
        let windows: Vec<Window> = self.clients.iter().map(|c| c.window).collect();
        ewmh::set_client_list(&self.conn, self.root, &self.atoms, &windows);
    }

    // Rebuild the cached keyboard mapping after a MappingNotify event.
    // The X server has changed the keycode-to-keysym table (e.g. user ran
    // setxkbmap). Refetch, recompute alt keycodes, and re-grab keys.
    //
    // Also re-applies the [input.keyboard] repeat settings if DEMIURGE
    // owns them. Layout-switching / xkbcomp / USB keyboard hot-plug all
    // reset auto-repeat to server defaults; without re-application,
    // the user's configured repeat rate silently reverts -- exactly
    // the inconsistency that motivated owning xset's surface in the
    // first place.
    pub fn rebuild_keymap(&mut self) {
        self.keymap = keys::load_keymap(&self.conn);
        self.alt_keycodes = keys::keymap_alt_keycodes(&self.keymap);
        keys::grab_all(&self.conn, self.root, &self.bindings);
        if self.input_repeat_delay > 0 || self.input_repeat_rate > 0 {
            apply_keyboard_repeat(&self.conn, self.input_repeat_delay, self.input_repeat_rate);
        }
    }

    pub fn reload_config(&mut self, config: &Config) {
        // Recompile and regrab keybindings
        match keys::compile(&config.keybinds, &self.keymap) {
            Ok(bindings) => {
                keys::grab_all(&self.conn, self.root, &bindings);
                eprintln!(
                    "[{}] [INFO]   reloaded {} keybindings",
                    local_time(),
                    bindings.len(),
                );
                self.bindings = bindings;
            }
            Err(e) => eprintln!("[{}] [WARN]   keybind reload failed: {}", local_time(), e),
        }

        self.master_ratio = config.general.master_ratio;

        // Cursor auto-hide may have flipped on / off, or the timeout
        // changed. cursor::reload force-shows on disable so the
        // cursor doesn't stay invisible after the user changes their
        // mind via config.toml.
        cursor::reload(
            &mut self.cursor,
            &self.conn,
            self.root,
            config.cursor.auto_hide,
            config.cursor.auto_hide_seconds,
        );

        // Keyboard repeat rate via XKB. 0/0 means "leave server
        // default in place" -- skip the call so we don't perturb
        // anything the user may have set externally. The cached
        // copies on Wm are also refreshed so MappingNotify
        // re-application uses the latest values.
        self.input_repeat_delay = config.input.keyboard.repeat_delay;
        self.input_repeat_rate = config.input.keyboard.repeat_rate;
        if self.input_repeat_delay > 0 || self.input_repeat_rate > 0 {
            apply_keyboard_repeat(&self.conn, self.input_repeat_delay, self.input_repeat_rate);
        }

        // XKB layout/variant/options. Re-apply if any field is set
        // OR if it was set previously and is now empty (so flipping
        // a layout off-and-back-to-default works). MappingNotify
        // re-application is intentionally NOT done for layout
        // because applying it FIRES MappingNotify -- looping.
        if !config.input.keyboard.layout.is_empty()
            || !config.input.keyboard.variant.is_empty()
            || !config.input.keyboard.options.is_empty()
        {
            apply_xkb_layout(
                &self.conn,
                &config.input.keyboard.layout,
                &config.input.keyboard.variant,
                &config.input.keyboard.options,
            );
        }

        // [input.idle] DPMS + screensaver. Re-apply on every reload
        // even if values are unchanged -- DPMS state can drift via
        // external xset calls, and re-applying is idempotent.
        if config.input.idle.dpms_standby_seconds
            + config.input.idle.dpms_suspend_seconds
            + config.input.idle.dpms_off_seconds
            > 0
        {
            idle::apply_dpms(
                &self.conn,
                config.input.idle.dpms_standby_seconds,
                config.input.idle.dpms_suspend_seconds,
                config.input.idle.dpms_off_seconds,
            );
        }
        if config.input.idle.screensaver_seconds > 0 {
            idle::apply_screensaver(&self.conn, config.input.idle.screensaver_seconds);
        }

        // [input.bell] re-apply if any field is set.
        if config.input.bell.enabled.is_some()
            || config.input.bell.volume.is_some()
            || config.input.bell.pitch_hz.is_some()
            || config.input.bell.duration_ms.is_some()
        {
            apply_bell(&self.conn, &config.input.bell);
        }

        // Re-apply [[display]] entries. Triggers an internal RandR
        // change which fires our own monitors_dirty path on the next
        // event-loop iteration, picking up post-DSR geometry.
        if !config.displays.is_empty() {
            display::apply_all(&config.displays, &self.monitors);
        }

        let old_height = self.bar_height;
        if let Some(ref mut bar) = self.bar {
            bar.update_appearance(config, &self.conn, &self.atoms, &self.monitors);
        }
        if config.bar.height != old_height {
            self.bar_height = config.bar.height;
            ewmh::set_workarea(
                &self.conn,
                self.root,
                &self.atoms,
                self.num_tags as u32,
                &self.monitors,
                self.bar_height,
            );
            self.arrange();
        }

        // Colors/font/clock format change = full bar repaint.
        if let Some(ref mut bar) = self.bar {
            bar.mark_all_dirty();
        }
    }

    // Called once per event-loop iteration. Expires stale notifications
    // (1-second timerfd tick drives this, so notifications clear with at
    // most ~1s overshoot), then hands the current bar state to
    // Bar::commit which renders + put_images only the regions whose
    // dirty flags are set. Fast path: no dirty flags set, commit returns
    // immediately.
    //
    // Replaces the old redraw_bar() which unconditionally did a full
    // re-render. v0.4.1: state mutations now call bar.mark_*_dirty()
    // and the actual X traffic happens once per event-loop iteration.
    pub fn commit_bar(&mut self) {
        // Notification expiry. If a notification just expired, the title
        // region needs to be re-rendered with the focused window's title
        // instead.
        if let Some(ref n) = self.notification
            && std::time::Instant::now() >= n.expires_at
        {
            self.notification = None;
            if let Some(ref mut bar) = self.bar {
                bar.mark_title_dirty();
            }
        }

        let occupied: Vec<bool> = (0..self.num_tags).map(|t| self.tag_occupied(t)).collect();
        let (center_text, is_notification) = match &self.notification {
            Some(n) => (n.text.clone(), true),
            None => (
                self.focus
                    .and_then(|w| self.clients.iter().find(|c| c.window == w))
                    .map(|c| c.title.clone())
                    .unwrap_or_default(),
                false,
            ),
        };
        if let Some(ref mut bar) = self.bar {
            bar.commit(
                &self.conn,
                &self.active_tags,
                &occupied,
                &center_text,
                is_notification,
            );
        }
    }

    // Tag has at least one client
    pub fn tag_occupied(&self, tag: usize) -> bool {
        self.clients.iter().any(|c| c.tag == tag)
    }

    // Cursor auto-hide tick. Called every ~100ms by the cursor
    // timerfd in main.rs::run. Polls the pointer position via
    // XQueryPointer; on motion shows + resets the timer, on
    // sustained idle past auto_hide_seconds hides. No-op when
    // self.cursor.auto_hide is false. Force-shows during drag.
    pub fn cursor_tick(&mut self) {
        let drag = self.drag.is_some();
        cursor::tick(&mut self.cursor, &self.conn, self.root, drag);
    }

    // RandR change handler. Drained once per event-loop iteration after
    // any RandR notify has flipped self.monitors_dirty; coalesces the
    // burst of CRTC/Output/ScreenChange events xrandr emits into a single
    // re-arrange. No-op (with the flag cleared) if the new layout matches
    // the cached one.
    //
    // On a real change:
    //   1. EWMH _NET_WORKAREA / _NET_DESKTOP_GEOMETRY are re-emitted so
    //      EWMH-aware clients (Chromium, et al.) maximize to the right rect.
    //   2. Bar panels are torn down and recreated against the new monitor
    //      list (handles hot-plug count change as well as resolution).
    //   3. arrange() retiles the active tag against the new work area.
    //   4. Bar is marked all-dirty for a full repaint at next commit.
    pub fn refresh_monitors(&mut self) {
        self.monitors_dirty = false;
        let new_monitors = monitor::query(&self.conn, self.root);
        if new_monitors == self.monitors {
            return;
        }
        eprintln!(
            "[{}] [INFO]   monitors changed ({} -> {})",
            local_time(),
            self.monitors.len(),
            new_monitors.len(),
        );
        for (i, mon) in new_monitors.iter().enumerate() {
            eprintln!(
                "[{}] [INFO]     monitor {}: '{}' {}x{}+{}+{}",
                local_time(),
                i,
                mon.name,
                mon.width,
                mon.height,
                mon.x,
                mon.y,
            );
        }
        self.monitors = new_monitors;

        // Resize active_tags in lockstep with monitors. Three cases:
        //   - len unchanged: keep existing entries (geometry-only RandR
        //     change, common case for xrandr -s).
        //   - len shrunk: truncate; clamp focused_monitor.
        //   - len grew: append fresh entries, picking tags that aren't
        //     already visible on existing monitors so newly-plugged
        //     screens land on a previously-hidden tag rather than
        //     duplicating an existing view.
        let new_count = self.monitors.len();
        let old_count = self.active_tags.len();
        match new_count.cmp(&old_count) {
            std::cmp::Ordering::Less => {
                self.active_tags.truncate(new_count);
            }
            std::cmp::Ordering::Greater => {
                for _ in old_count..new_count {
                    let pick = (0..self.num_tags)
                        .find(|t| !self.active_tags.contains(t))
                        .unwrap_or_else(|| self.num_tags.saturating_sub(1));
                    self.active_tags.push(pick);
                }
            }
            std::cmp::Ordering::Equal => {}
        }
        if self.focused_monitor >= new_count {
            self.focused_monitor = new_count - 1;
        }

        ewmh::set_workarea(
            &self.conn,
            self.root,
            &self.atoms,
            self.num_tags as u32,
            &self.monitors,
            self.bar_height,
        );
        ewmh::set_desktop_geometry(&self.conn, self.root, &self.atoms, &self.monitors);
        ewmh::set_current_desktop(
            &self.conn,
            self.root,
            &self.atoms,
            self.active_tags[self.focused_monitor] as u32,
        );

        if let Some(ref mut bar) = self.bar {
            if let Err(e) = bar.rebuild_panels(&self.conn, self.root, &self.atoms, &self.monitors) {
                eprintln!(
                    "[{}] [WARN]   bar rebuild failed: {}",
                    local_time(),
                    e,
                );
            }
        }

        // Visibility may have changed: a newly-attached monitor brings
        // a previously-hidden tag's clients back into view; a detached
        // monitor strands its tag's clients. Map/unmap accordingly.
        self.refresh_visibility();

        self.arrange();

        if let Some(ref mut bar) = self.bar {
            bar.mark_all_dirty();
        }

        let _ = self.conn.flush();
    }

    // Map clients on visible tags, unmap clients on hidden tags. Idempotent
    // map_window calls on already-mapped windows are no-ops at the X
    // server, but we still bump unmap_ignore conservatively before each
    // explicit unmap to swallow the echo.
    fn refresh_visibility(&mut self) {
        for client in &mut self.clients {
            if self.active_tags.contains(&client.tag) {
                let _ = self.conn.map_window(client.window);
            } else {
                client.unmap_ignore = client.unmap_ignore.saturating_add(1);
                let _ = self.conn.unmap_window(client.window);
            }
        }
    }

    // The active tag for the focused monitor. Used by call sites that
    // pre-v0.6.0 read self.active_tag as a global -- those paths now
    // ask "what tag is the user currently viewing?" and we route to the
    // focused monitor's slot. focused_monitor is always in-bounds (set
    // by Wm::init, clamped on refresh_monitors), so this is infallible.
    pub fn active_tag(&self) -> usize {
        self.active_tags[self.focused_monitor]
    }

    // Is `tag` currently visible anywhere -- on any monitor? When false,
    // clients on this tag are unmapped; when true, exactly one monitor
    // shows them. The visibility flag is what gates map/unmap, replacing
    // the old `tag == active_tag` check.
    pub fn tag_visible(&self, tag: usize) -> bool {
        self.active_tags.contains(&tag)
    }

    // Index of the monitor showing `tag`, if any. Tag-to-monitor is
    // 1:1 (a tag is on at most one monitor at a time), enforced by the
    // swap-on-collision logic in view_tag.
    pub fn monitor_for_tag(&self, tag: usize) -> Option<usize> {
        self.active_tags.iter().position(|&t| t == tag)
    }

    // Index in self.monitors of the monitor that owns `client`'s
    // center point. Falls back to focused_monitor if the client is
    // fully off-screen (e.g., dragged off a monitor that was then
    // unplugged). Symmetric to client_monitor() which returns &Monitor.
    pub fn client_monitor_index(&self, client: &Client) -> usize {
        let cx = client.x + client.w as i32 / 2;
        let cy = client.y + client.h as i32 / 2;
        self.monitors
            .iter()
            .position(|m| {
                cx >= m.x
                    && cx < m.x + m.width as i32
                    && cy >= m.y
                    && cy < m.y + m.height as i32
            })
            .unwrap_or(self.focused_monitor)
    }

    // Returns the monitor whose rect contains the client's center point.
    // Falls back to the head monitor if the client is fully off-screen.
    pub fn client_monitor(&self, client: &Client) -> &Monitor {
        let cx = client.x + client.w as i32 / 2;
        let cy = client.y + client.h as i32 / 2;
        self.monitors
            .iter()
            .find(|m| {
                cx >= m.x
                    && cx < m.x + m.width as i32
                    && cy >= m.y
                    && cy < m.y + m.height as i32
            })
            .unwrap_or_else(|| self.monitors.first())
    }
}

// Apply keyboard auto-repeat delay + rate via XKB. Replaces the
// `xset r rate <delay> <rate>` line many DEMIURGE configs ship in
// startup.commands. delay_ms is the milliseconds before repeat
// starts; rate_per_sec is repeats per second after that.
//
// One protocol call: XKB SetControls with three bits flipped.
//
//   - affect_enabled_controls + enabled_controls: BoolCtrl::REPEAT_KEYS
//     ensures auto-repeat itself is ENABLED (a stale `xset r off`
//     would otherwise silently swallow whatever rate we set).
//   - change_controls: REPEAT_KEYS_BIT (= 1 << 0 in the underlying
//     XkbControlsMask u32) is the "I'm setting repeat_delay and
//     repeat_interval" flag. x11rb's typed Control wrapper exposes
//     only its high-bit named constants directly; the low bits
//     (which double as BoolCtrl values in the X protocol) are
//     reachable via Control::from(<u32>).
//   - repeat_delay + repeat_interval: the actual values.
//
// The 30-argument signature is what it is. Most fields zero out.
pub fn apply_keyboard_repeat(conn: &RustConnection, delay_ms: u32, rate_per_sec: u32) {
    use x11rb::protocol::xkb::{
        AXOption, BoolCtrl, ConnectionExt as XkbExt, Control, VMod,
    };

    // Guard against divide-by-zero. A user setting rate = 0 in
    // [input.keyboard] is asking for "don't change anything"; in
    // that case the apply() caller skips us entirely, but
    // belt-and-suspenders.
    if rate_per_sec == 0 {
        return;
    }
    let interval_ms = (1000u32 / rate_per_sec).max(1);

    // DeviceSpec = 0x0100 selects the X server's core keyboard. XKB
    // also supports per-physical-device control (typically used by
    // input-method software); core keyboard is what setxkbmap and
    // xset use.
    const CORE_KBD: u16 = 0x0100;
    // XkbRepeatKeysMask -- bit 0 of the XkbControlsMask u32.
    const REPEAT_KEYS_BIT: u32 = 1 << 0;

    let _ = conn.xkb_set_controls(
        CORE_KBD,
        // affect_internal_real_mods, internal_real_mods,
        // affect_ignore_lock_real_mods, ignore_lock_real_mods,
        // affect_internal_virtual_mods, internal_virtual_mods,
        // affect_ignore_lock_virtual_mods, ignore_lock_virtual_mods
        0u8.into(),
        0u8.into(),
        0u8.into(),
        0u8.into(),
        VMod::default(),
        VMod::default(),
        VMod::default(),
        VMod::default(),
        // mouse_keys_dflt_btn, groups_wrap, access_x_options
        0,
        0,
        AXOption::default(),
        // affect_enabled_controls + enabled_controls: turn auto-repeat
        // ON globally. Both equal so the masked bit becomes 1.
        BoolCtrl::REPEAT_KEYS,
        BoolCtrl::REPEAT_KEYS,
        // change_controls: tell the server to apply repeat_delay +
        // repeat_interval (XkbRepeatKeysMask bit 0).
        Control::from(REPEAT_KEYS_BIT),
        // repeat_delay, repeat_interval
        delay_ms.min(u16::MAX as u32) as u16,
        interval_ms.min(u16::MAX as u32) as u16,
        // slow_keys_delay, debounce_delay
        0,
        0,
        // mouse_keys_*
        0,
        0,
        0,
        0,
        0,
        // access_x_timeout + masks/values
        0,
        BoolCtrl::default(),
        BoolCtrl::default(),
        AXOption::default(),
        AXOption::default(),
        // per_key_repeat: bitmap of which keycodes auto-repeat. All
        // zeros = leave server default.
        &[0u8; 32],
    );
    let _ = conn.flush();
}

// Apply XKB layout / variant / options. Replaces a setxkbmap line
// in startup.commands with TOML the user owns.
//
// Why setxkbmap as the implementation:
//   The X11 server takes XKB component names ("pc+us+inet(evdev)")
//   to compile a keymap, not RMLVO directly. Expanding RMLVO ->
//   component names requires parsing /usr/share/X11/xkb/rules/evdev,
//   which xkbcomp/setxkbmap exist to do. We could re-implement the
//   rules parser, or pull libxkbcommon, but for v0.7 we pragmatically
//   shell out to setxkbmap from inside DEMIURGE -- the dependency is
//   on xorg-setxkbmap (already required for any X11 session) rather
//   than on the user remembering to put the call in startup.commands.
//
// User-visible: a single TOML place owns the layout, hot-reloadable
// like the rest of the config.
//
// Wayland implementation will use libxkbcommon's RMLVO->keymap path
// directly since wl_keyboard takes the compiled keymap string; that
// version is genuinely shellout-free.
pub fn apply_xkb_layout(
    _conn: &RustConnection,
    layout: &str,
    variant: &str,
    options: &[String],
) {
    let mut cmd = std::process::Command::new("setxkbmap");
    if !layout.is_empty() {
        cmd.arg("-layout").arg(layout);
    }
    if !variant.is_empty() {
        cmd.arg("-variant").arg(variant);
    }
    // -option with empty arg first clears existing options so the
    // user's TOML list is the full set, not an addition. Without
    // this, an XKB option set previously (by a session-setup
    // script, an old setxkbmap invocation, or /etc/X11/xorg.conf.d)
    // would persist alongside the new ones.
    cmd.arg("-option").arg("");
    for opt in options {
        cmd.arg("-option").arg(opt);
    }
    match cmd.status() {
        Ok(status) if status.success() => {}
        Ok(status) => {
            eprintln!(
                "[{}] [WARN]   setxkbmap exited {}",
                local_time(),
                status.code().unwrap_or(-1),
            );
        }
        Err(e) => {
            eprintln!(
                "[{}] [WARN]   setxkbmap spawn failed: {} (XKB layout not applied)",
                local_time(),
                e,
            );
        }
    }
}

// Apply [input.bell] settings via ChangeKeyboardControl. The X server's
// bell parameters are (percent, pitch_hz, duration_ms); percent = 0
// silences. We map enabled=Some(false) -> percent=0, enabled=Some(true)
// + volume present -> percent = volume, neither set -> leave default.
pub fn apply_bell(conn: &RustConnection, bell: &crate::config::Bell) {
    use x11rb::protocol::xproto::ChangeKeyboardControlAux;

    let mut aux = ChangeKeyboardControlAux::new();

    // Volume / enable. Disabled wins regardless of volume.
    let effective_volume: Option<i8> = match (bell.enabled, bell.volume) {
        (Some(false), _) => Some(0),
        (Some(true), Some(v)) => Some((v.min(100)) as i8),
        (Some(true), None) => None, // leave server default
        (None, Some(v)) => Some((v.min(100)) as i8),
        (None, None) => None,
    };
    if let Some(v) = effective_volume {
        aux = aux.bell_percent(v as i32);
    }

    if let Some(hz) = bell.pitch_hz {
        aux = aux.bell_pitch(hz as i32);
    }
    if let Some(ms) = bell.duration_ms {
        aux = aux.bell_duration(ms as i32);
    }

    let _ = conn.change_keyboard_control(&aux);
    let _ = conn.flush();
}

// Parse `wpctl get-volume <target>` output:
//   "Volume: 0.50"            -> "50%"
//   "Volume: 0.50 [MUTED]"    -> "MUTED"
// When muted, return "MUTED" regardless of the numeric value.
pub(crate) fn read_wpctl_volume(target: &str) -> Option<String> {
    let out = std::process::Command::new("wpctl")
        .args(["get-volume", target])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    if s.contains("[MUTED]") {
        return Some("MUTED".into());
    }
    let value: f64 = s.split_whitespace().nth(1)?.parse().ok()?;
    Some(format!("{}%", (value * 100.0).round() as u32))
}

// Parse `xbacklight -get` output: a bare float like "60.000000".
pub(crate) fn read_brightness() -> Option<String> {
    let out = std::process::Command::new("xbacklight")
        .arg("-get")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let value: f64 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
    Some(format!("{}%", value.round() as u32))
}

// Returns (state, optional track). state is "Playing", "Paused", "Stopped".
// track is "Title — Artist" when metadata is available. None means no
// player is running or playerctl isn't installed.
pub(crate) fn read_media_state() -> Option<(String, Option<String>)> {
    let status = std::process::Command::new("playerctl")
        .arg("status")
        .output()
        .ok()?;
    if !status.status.success() {
        return None;
    }
    let state = String::from_utf8_lossy(&status.stdout).trim().to_string();
    if state.is_empty() || state == "No players found" {
        return None;
    }

    let meta = std::process::Command::new("playerctl")
        .args(["metadata", "--format", "{{title}} — {{artist}}"])
        .output()
        .ok()?;
    let track = String::from_utf8_lossy(&meta.stdout).trim().to_string();
    let track = if track.is_empty() || track == " — " {
        None
    } else {
        Some(track)
    };

    Some((state, track))
}

pub(crate) fn local_time() -> String {
    unsafe {
        let mut t: libc::time_t = 0;
        libc::time(&mut t);
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
    }
}
