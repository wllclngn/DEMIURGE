use x11rb::connection::Connection;
use x11rb::properties::WmClass;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use crate::atoms::Atoms;
use crate::bar::Bar;
use crate::config::Config;
use crate::ewmh;
use crate::keys::{self, Action, Binding};
use crate::layout::Layout;
use crate::monitor::{self, Monitor};
use crate::mouse::DragState;
use crate::mru;
use crate::spawn;

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
    pub monitors: Vec<Monitor>,
    pub clients: Vec<Client>,
    pub focus: Option<Window>,
    pub active_tag: usize,
    pub num_tags: usize,
    pub bindings: Vec<Binding>,
    pub bar_height: u32,
    pub check_window: Window,
    pub running: bool,
    pub layouts: Vec<Layout>,
    pub master_ratio: f64,
    pub drag: Option<DragState>,
    // Browser-style back/forward navigation stacks. `history` holds windows
    // visited before `focus` (back = most recent). `future` holds windows
    // walked past via Alt+Tab back-stepping (back = most recent forward step).
    pub history: Vec<Window>,
    pub future: Vec<Window>,
    pub mru_cycle: Option<mru::CycleState>,
    pub alt_keycodes: Vec<Keycode>,
    pub keymap: keys::KeyMap,
    pub bar: Option<Bar>,
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

        let monitors = monitor::query(&conn, root);
        for (i, mon) in monitors.iter().enumerate() {
            eprintln!(
                "[demiurge] monitor {}: '{}' {}x{}+{}+{}",
                i, mon.name, mon.width, mon.height, mon.x, mon.y
            );
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

        let mut wm = Self {
            conn,
            root,
            atoms,
            monitors,
            clients: Vec::new(),
            focus: None,
            active_tag: 0,
            num_tags,
            bindings,
            bar_height,
            check_window,
            running: true,
            layouts,
            master_ratio,
            drag: None,
            history: Vec::new(),
            future: Vec::new(),
            mru_cycle: None,
            alt_keycodes,
            keymap,
            bar: Some(bar),
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

        wm.redraw_bar();

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

        // Set frame extents (zeros -- no decorations)
        ewmh::set_frame_extents(&self.conn, window, &self.atoms);
        ewmh::set_client_desktop(&self.conn, window, &self.atoms, self.active_tag as u32);

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
            tag: self.active_tag,
            floating: should_float || self.layouts[self.active_tag] == Layout::Floating,
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

        let class = self.get_class(window);
        eprintln!(
            "[{}] [INFO]   manage '{}' (class='{}', title='{}', geom={}x{}+{}+{})",
            local_time(),
            window,
            class,
            title,
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

        // Focus the new window
        self.focus_window(Some(window));

        if self.layouts[self.active_tag] != Layout::Floating {
            self.arrange();
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

            // If we lost focus, pick next window. Clear self.focus first so
            // focus_window's history-push sees None and doesn't enqueue the
            // dead window.
            if self.focus == Some(window) {
                let next = self
                    .clients
                    .iter()
                    .rev()
                    .find(|c| c.tag == self.active_tag)
                    .map(|c| c.window);
                self.focus = None;
                self.focus_window(next);
            }

            if tag == self.active_tag && self.layouts[tag] != Layout::Floating {
                self.arrange();
            }
        }
    }

    // FOCUS

    pub fn focus_window(&mut self, window: Option<Window>) {
        // Browser-style history: any direct focus change pushes the previous
        // focus onto history and clears the future stack. Skipped during
        // Alt+Tab cycling — cycle steps mutate the stacks directly in mru.rs.
        if self.mru_cycle.is_none() {
            if let Some(target) = window {
                if let Some(old) = self.focus {
                    if old != target {
                        self.history.push(old);
                    }
                }
                self.future.clear();
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

        self.redraw_bar();
        let _ = self.conn.flush();
    }

    // TAGS

    // Switch to a tag without picking a focus. Used by Alt+Tab cross-tag
    // stepping where the cycle has already chosen a target window.
    pub fn view_tag_no_focus(&mut self, tag: usize) {
        if tag >= self.num_tags || tag == self.active_tag {
            return;
        }

        self.active_tag = tag;
        ewmh::set_current_desktop(&self.conn, self.root, &self.atoms, tag as u32);

        // Show/hide windows. Bump unmap_ignore before each self-unmap so
        // the UnmapNotify echo doesn't unmanage the client.
        for client in &mut self.clients {
            if client.tag == tag {
                let _ = self.conn.map_window(client.window);
            } else {
                client.unmap_ignore = client.unmap_ignore.saturating_add(1);
                let _ = self.conn.unmap_window(client.window);
            }
        }

        if self.layouts[tag] != Layout::Floating {
            self.arrange();
        }
        let _ = self.conn.flush();
    }

    pub fn view_tag(&mut self, tag: usize) {
        if tag >= self.num_tags || tag == self.active_tag {
            return;
        }
        self.view_tag_no_focus(tag);

        // Focus topmost on new tag
        let next = self
            .clients
            .iter()
            .rev()
            .find(|c| c.tag == tag)
            .map(|c| c.window);
        self.focus_window(next);
    }

    pub fn view_prev_tag(&mut self) {
        let prev = if self.active_tag == 0 {
            self.num_tags - 1
        } else {
            self.active_tag - 1
        };
        self.view_tag(prev);
    }

    pub fn view_next_tag(&mut self) {
        let next = (self.active_tag + 1) % self.num_tags;
        self.view_tag(next);
    }

    pub fn move_to_tag(&mut self, tag: usize) {
        if tag >= self.num_tags {
            return;
        }
        if let Some(win) = self.focus {
            if let Some(client) = self.clients.iter_mut().find(|c| c.window == win) {
                client.tag = tag;
                ewmh::set_client_desktop(&self.conn, win, &self.atoms, tag as u32);

                if tag != self.active_tag {
                    client.unmap_ignore = client.unmap_ignore.saturating_add(1);
                    let _ = self.conn.unmap_window(win);
                    // Focus next on current tag
                    let next = self
                        .clients
                        .iter()
                        .rev()
                        .find(|c| c.tag == self.active_tag && c.window != win)
                        .map(|c| c.window);
                    self.focus_window(next);
                }
                let _ = self.conn.flush();
            }
        }
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
            Action::MruNext => crate::mru::start_or_advance(self, true),
            Action::MruPrev => crate::mru::start_or_advance(self, false),
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
                }
                self.redraw_bar();
            }
            Action::Screenshot => self.screenshot(),
            Action::Lock => spawn::spawn("gordian_knot"),
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
        self.redraw_bar();
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
    pub fn rebuild_keymap(&mut self) {
        self.keymap = keys::load_keymap(&self.conn);
        self.alt_keycodes = keys::keymap_alt_keycodes(&self.keymap);
        keys::grab_all(&self.conn, self.root, &self.bindings);
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

        if let Some(ref mut bar) = self.bar {
            bar.update_appearance(config);
        }

        self.redraw_bar();
    }

    pub fn redraw_bar(&mut self) {
        let occupied: Vec<bool> = (0..self.num_tags).map(|t| self.tag_occupied(t)).collect();
        let title = self
            .focus
            .and_then(|w| self.clients.iter().find(|c| c.window == w))
            .map(|c| c.title.clone())
            .unwrap_or_default();
        let layout = self.layouts[self.active_tag];
        if let Some(ref mut bar) = self.bar {
            bar.draw(&self.conn, self.active_tag, &occupied, &title, layout);
        }
    }

    // Tag has at least one client
    pub fn tag_occupied(&self, tag: usize) -> bool {
        self.clients.iter().any(|c| c.tag == tag)
    }

    // Returns the monitor whose rect contains the client's center point.
    // Falls back to monitors[0] if the client is fully off-screen.
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
            .unwrap_or(&self.monitors[0])
    }
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
