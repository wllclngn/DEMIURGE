use std::os::fd::AsRawFd;

use x11rb::atom_manager;
use x11rb::connection::Connection;
use x11rb::properties::WmClass;
use x11rb::protocol::randr::ConnectionExt as RandrExt;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use crate::rules::{
    CompiledRule, DimensionVal, MonitorTarget, NamedPosition, PositionTarget, SizeTarget,
};

atom_manager! {
    pub Atoms: AtomsCookie {
        WM_NAME,
        WM_CLASS,
        WM_WINDOW_ROLE,
        WM_CHANGE_STATE,
        UTF8_STRING,
        _NET_CLIENT_LIST,
        _NET_WM_NAME,
        _NET_WM_PID,
        _NET_WM_DESKTOP,
        _NET_WM_STATE,
        _NET_WM_STATE_MAXIMIZED_VERT,
        _NET_WM_STATE_MAXIMIZED_HORZ,
        _NET_WM_STATE_ABOVE,
        _NET_WM_STATE_BELOW,
        _NET_WM_STATE_STICKY,
        _NET_WM_STATE_FULLSCREEN,
        _NET_WM_STATE_SHADED,
        _NET_WM_STATE_HIDDEN,
        _NET_WM_WINDOW_TYPE,
        _NET_WM_WINDOW_TYPE_NORMAL,
        _NET_WM_WINDOW_TYPE_DESKTOP,
        _NET_WM_WINDOW_TYPE_DOCK,
        _NET_WM_WINDOW_TYPE_DIALOG,
        _NET_WM_WINDOW_TYPE_TOOLBAR,
        _NET_WM_WINDOW_TYPE_MENU,
        _NET_WM_WINDOW_TYPE_UTILITY,
        _NET_WM_WINDOW_TYPE_SPLASH,
        _NET_WM_WINDOW_OPACITY,
        _NET_ACTIVE_WINDOW,
        _MOTIF_WM_HINTS,
    }
}

#[derive(Debug, Clone)]
pub struct MonitorGeometry {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

pub struct X11Backend {
    conn: RustConnection,
    root: Window,
    atoms: Atoms,
    monitors: Vec<MonitorGeometry>,
    known_clients: std::cell::RefCell<Vec<Window>>,
    handled: std::cell::RefCell<Vec<Window>>,
    pending_startup: std::cell::RefCell<Vec<Window>>,
}

impl X11Backend {
    const RETRY_MAX: u32 = 60;
    const RETRY_MS: u64 = 500;

    pub fn init(signal_fd: i32) -> Result<Self, String> {
        for attempt in 0..Self::RETRY_MAX {
            match Self::try_connect() {
                Ok(backend) => return Ok(backend),
                Err(e) => {
                    if attempt == Self::RETRY_MAX - 1 {
                        return Err(format!("x11 connect failed after {}s: {}", Self::RETRY_MAX as u64 * Self::RETRY_MS / 1000, e));
                    }
                    if attempt == 0 {
                        eprintln!("[x11] waiting for display...");
                    }
                    // Check for shutdown signal between retries
                    if signal_fd >= 0 {
                        let mut pfd = libc::pollfd { fd: signal_fd, events: libc::POLLIN, revents: 0 };
                        if unsafe { libc::poll(&mut pfd, 1, 0) } > 0 {
                            eprintln!("[x11] received signal during init, exiting");
                            std::process::exit(0);
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(Self::RETRY_MS));
                }
            }
        }
        unreachable!()
    }

    fn try_connect() -> Result<Self, String> {
        let (conn, screen_num) =
            RustConnection::connect(None).map_err(|e| format!("x11 connect: {}", e))?;

        let screen = &conn.setup().roots[screen_num];
        let root = screen.root;

        conn.change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )
        .map_err(|e| format!("change root attributes: {}", e))?
        .check()
        .map_err(|e| format!("change root attributes: {}", e))?;

        let atoms = Atoms::new(&conn)
            .map_err(|e| format!("intern atoms: {}", e))?
            .reply()
            .map_err(|e| format!("intern atoms reply: {}", e))?;

        let monitors = query_monitors(&conn, root)?;

        let initial_clients = get_client_list(&conn, root, &atoms);

        conn.flush().map_err(|e| format!("flush: {}", e))?;

        for (i, mon) in monitors.iter().enumerate() {
            eprintln!(
                "[x11] monitor {}: '{}' {}x{}+{}+{}",
                i, mon.name, mon.width, mon.height, mon.x, mon.y
            );
        }
        eprintln!("[x11] found {} existing windows", initial_clients.len());

        Ok(Self {
            conn,
            root,
            atoms,
            monitors,
            known_clients: std::cell::RefCell::new(initial_clients.clone()),
            handled: std::cell::RefCell::new(Vec::new()),
            pending_startup: std::cell::RefCell::new(initial_clients),
        })
    }

    pub fn connection_fd(&self) -> i32 {
        self.conn.stream().as_raw_fd()
    }

    pub fn process_events(&self, rules: &[CompiledRule], dry_run: bool) {
        let mut need_flush = false;

        // Apply rules to windows that existed at startup
        let startup = self.pending_startup.take();
        if !startup.is_empty() {
            let mut handled = self.handled.borrow_mut();
            for window in startup {
                self.handle_new_window(window, rules, dry_run);
                handled.push(window);
                need_flush = true;
            }
        }

        // Loop: handling new windows involves get_property round-trips.
        // During those reads, x11rb may buffer additional events from the
        // socket. If we don't re-drain, those events sit in the internal
        // queue while poll() sees no socket data and never wakes us.
        loop {
            let mut client_list_changed = false;

            while let Some(event) = self.conn.poll_for_event().ok().flatten() {
                if let x11rb::protocol::Event::PropertyNotify(ev) = event
                    && ev.window == self.root
                    && ev.atom == self.atoms._NET_CLIENT_LIST
                {
                    client_list_changed = true;
                }
            }

            if !client_list_changed {
                break;
            }

            let current = get_client_list(&self.conn, self.root, &self.atoms);
            let mut known = self.known_clients.borrow_mut();
            let mut handled = self.handled.borrow_mut();

            for &window in &current {
                if !known.contains(&window) && !handled.contains(&window) {
                    self.handle_new_window(window, rules, dry_run);
                    handled.push(window);
                    need_flush = true;
                }
            }

            // Prune closed windows from handled list to prevent unbounded growth
            handled.retain(|w| current.contains(w));
            *known = current;
        }

        if need_flush {
            let _ = self.conn.flush();
        }
    }

    fn handle_new_window(&self, window: Window, rules: &[CompiledRule], dry_run: bool) {
        let class = self.get_class(window);
        let title = self.get_title(window);
        let role = self.get_role(window);
        let process = self.get_process_name(window);
        let window_type = self.get_window_type(window);

        for rule in rules {
            if rule.matches(&class, &title, &role, &process, &window_type) {
                let now = local_time();
                eprintln!(
                    "[{}] [INFO]   matched '{}' (class='{}', title='{}', process='{}')",
                    now, class, class, title, process
                );

                if !dry_run {
                    self.apply_rule(window, rule);
                } else {
                    self.log_actions(rule);
                }
            }
        }
    }

    // PROPERTY GETTERS

    fn get_class(&self, window: Window) -> String {
        WmClass::get(&self.conn, window)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .and_then(|opt| opt)
            .map(|wm| String::from_utf8_lossy(wm.class()).to_string())
            .unwrap_or_default()
    }

    fn get_title(&self, window: Window) -> String {
        if let Some(title) = self.get_string_property(window, self.atoms._NET_WM_NAME) {
            return title;
        }
        self.get_string_property(window, self.atoms.WM_NAME)
            .unwrap_or_default()
    }

    fn get_role(&self, window: Window) -> String {
        self.get_string_property(window, self.atoms.WM_WINDOW_ROLE)
            .unwrap_or_default()
    }

    fn get_process_name(&self, window: Window) -> String {
        let pid = self.get_cardinal_property(window, self.atoms._NET_WM_PID);
        match pid {
            Some(pid) => {
                let comm_path = format!("/proc/{}/comm", pid);
                std::fs::read_to_string(&comm_path)
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default()
            }
            None => String::new(),
        }
    }

    fn get_window_type(&self, window: Window) -> String {
        let type_atom = match self.get_atom_property(window, self.atoms._NET_WM_WINDOW_TYPE) {
            Some(a) => a,
            None => return "normal".into(),
        };

        if type_atom == self.atoms._NET_WM_WINDOW_TYPE_NORMAL {
            "normal"
        } else if type_atom == self.atoms._NET_WM_WINDOW_TYPE_DIALOG {
            "dialog"
        } else if type_atom == self.atoms._NET_WM_WINDOW_TYPE_DOCK {
            "dock"
        } else if type_atom == self.atoms._NET_WM_WINDOW_TYPE_TOOLBAR {
            "toolbar"
        } else if type_atom == self.atoms._NET_WM_WINDOW_TYPE_MENU {
            "menu"
        } else if type_atom == self.atoms._NET_WM_WINDOW_TYPE_UTILITY {
            "utility"
        } else if type_atom == self.atoms._NET_WM_WINDOW_TYPE_SPLASH {
            "splash"
        } else if type_atom == self.atoms._NET_WM_WINDOW_TYPE_DESKTOP {
            "desktop"
        } else {
            "unknown"
        }
        .into()
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

    fn get_cardinal_property(&self, window: Window, atom: Atom) -> Option<u32> {
        let reply = self
            .conn
            .get_property(false, window, atom, AtomEnum::CARDINAL, 0, 1)
            .ok()?
            .reply()
            .ok()?;

        if reply.value.len() >= 4 {
            Some(u32::from_ne_bytes([
                reply.value[0],
                reply.value[1],
                reply.value[2],
                reply.value[3],
            ]))
        } else {
            None
        }
    }

    fn get_atom_property(&self, window: Window, atom: Atom) -> Option<Atom> {
        let reply = self
            .conn
            .get_property(false, window, atom, AtomEnum::ATOM, 0, 1)
            .ok()?
            .reply()
            .ok()?;

        if reply.value.len() >= 4 {
            Some(u32::from_ne_bytes([
                reply.value[0],
                reply.value[1],
                reply.value[2],
                reply.value[3],
            ]))
        } else {
            None
        }
    }

    fn get_window_geometry(&self, window: Window) -> Option<(i32, i32, u32, u32)> {
        let geo = self.conn.get_geometry(window).ok()?.reply().ok()?;
        // Translate to root coordinates
        let coords = self
            .conn
            .translate_coordinates(window, self.root, 0, 0)
            .ok()?
            .reply()
            .ok()?;
        Some((
            coords.dst_x as i32,
            coords.dst_y as i32,
            geo.width as u32,
            geo.height as u32,
        ))
    }

    // ACTION APPLICATION

    fn apply_rule(&self, window: Window, rule: &CompiledRule) {
        let target_monitor = self.resolve_monitor(window, rule);

        // Size first (position may depend on resolved size for centering)
        let resolved_size = rule.size.as_ref().map(|sz| self.resolve_size(sz, &target_monitor));

        if let Some((w, h)) = resolved_size {
            let _ = self.conn.configure_window(
                window,
                &ConfigureWindowAux::new().width(w).height(h),
            );
        }

        if let Some(ref pos) = rule.position {
            let win_size = resolved_size.or_else(|| {
                self.get_window_geometry(window).map(|(_, _, w, h)| (w, h))
            });
            let (x, y) = self.resolve_position(pos, &target_monitor, win_size);
            let _ = self.conn.configure_window(
                window,
                &ConfigureWindowAux::new().x(x).y(y),
            );
        }

        if let Some(ws) = rule.workspace {
            self.send_client_message(window, self.atoms._NET_WM_DESKTOP, [ws, 1, 0, 0, 0]);
        }

        if let Some(true) = rule.maximize {
            self.set_wm_state(
                window,
                1,
                self.atoms._NET_WM_STATE_MAXIMIZED_VERT,
                self.atoms._NET_WM_STATE_MAXIMIZED_HORZ,
            );
        }

        if let Some(true) = rule.fullscreen {
            self.set_wm_state(window, 1, self.atoms._NET_WM_STATE_FULLSCREEN, 0);
        }

        if let Some(true) = rule.pin {
            self.send_client_message(
                window,
                self.atoms._NET_WM_DESKTOP,
                [0xFFFFFFFF, 1, 0, 0, 0],
            );
            self.set_wm_state(window, 1, self.atoms._NET_WM_STATE_STICKY, 0);
        }

        if let Some(true) = rule.minimize {
            // WM_CHANGE_STATE with IconicState (3)
            let event = ClientMessageEvent::new(32, window, self.atoms.WM_CHANGE_STATE, [3u32, 0, 0, 0, 0]);
            let _ = self.conn.send_event(
                false,
                self.root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                event,
            );
        }

        if let Some(true) = rule.shade {
            self.set_wm_state(window, 1, self.atoms._NET_WM_STATE_SHADED, 0);
        }

        if let Some(true) = rule.above {
            self.set_wm_state(window, 1, self.atoms._NET_WM_STATE_ABOVE, 0);
        }

        if let Some(true) = rule.below {
            self.set_wm_state(window, 1, self.atoms._NET_WM_STATE_BELOW, 0);
        }

        if let Some(false) = rule.decorate {
            self.set_decoration(window, false);
        }
        if let Some(true) = rule.decorate {
            self.set_decoration(window, true);
        }

        if let Some(true) = rule.focus {
            self.send_client_message(
                window,
                self.atoms._NET_ACTIVE_WINDOW,
                [1, 0, 0, 0, 0], // source = application
            );
        }

        if let Some(opacity) = rule.opacity {
            let value = (opacity.clamp(0.0, 1.0) * 0xFFFFFFFF_u64 as f64) as u32;
            let _ = self.conn.change_property32(
                PropMode::REPLACE,
                window,
                self.atoms._NET_WM_WINDOW_OPACITY,
                AtomEnum::CARDINAL,
                &[value],
            );
        }
    }

    // MONITOR RESOLUTION

    fn resolve_monitor(&self, window: Window, rule: &CompiledRule) -> MonitorGeometry {
        if let Some(ref target) = rule.monitor {
            match target {
                MonitorTarget::Index(idx) => {
                    if let Some(mon) = self.monitors.get(*idx as usize) {
                        return mon.clone();
                    }
                }
                MonitorTarget::Name(name) => {
                    if let Some(mon) = self.monitors.iter().find(|m| m.name == *name) {
                        return mon.clone();
                    }
                    // Also try matching against EWMH desktop names / awesomewm tags
                    // (workspace names that map to monitor outputs)
                }
            }
        }

        // Default: monitor the window is on, or first monitor
        if let Some(geo) = self.get_window_geometry(window) {
            let cx = geo.0 + geo.2 as i32 / 2;
            let cy = geo.1 + geo.3 as i32 / 2;
            for mon in &self.monitors {
                if cx >= mon.x
                    && cx < mon.x + mon.width as i32
                    && cy >= mon.y
                    && cy < mon.y + mon.height as i32
                {
                    return mon.clone();
                }
            }
        }

        self.monitors
            .first()
            .cloned()
            .unwrap_or(MonitorGeometry {
                name: String::new(),
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            })
    }

    // POSITION RESOLUTION

    fn resolve_position(
        &self,
        pos: &PositionTarget,
        monitor: &MonitorGeometry,
        win_size: Option<(u32, u32)>,
    ) -> (i32, i32) {
        let (win_w, win_h) = win_size.unwrap_or((0, 0));
        let mx = monitor.x;
        let my = monitor.y;
        let mw = monitor.width as i32;
        let mh = monitor.height as i32;
        let ww = win_w as i32;
        let wh = win_h as i32;

        match pos {
            PositionTarget::Absolute(x, y) => (*x, *y),
            PositionTarget::Named(anchor) => match anchor {
                NamedPosition::Center => (mx + (mw - ww) / 2, my + (mh - wh) / 2),
                NamedPosition::TopLeft => (mx, my),
                NamedPosition::TopRight => (mx + mw - ww, my),
                NamedPosition::BottomLeft => (mx, my + mh - wh),
                NamedPosition::BottomRight => (mx + mw - ww, my + mh - wh),
                NamedPosition::Left => (mx, my + (mh - wh) / 2),
                NamedPosition::Right => (mx + mw - ww, my + (mh - wh) / 2),
                NamedPosition::Top => (mx + (mw - ww) / 2, my),
                NamedPosition::Bottom => (mx + (mw - ww) / 2, my + mh - wh),
            },
            PositionTarget::Flexible(xv, yv) => {
                let x = resolve_dim(*xv, mw) + mx;
                let y = resolve_dim(*yv, mh) + my;
                (x, y)
            }
        }
    }

    // SIZE RESOLUTION

    fn resolve_size(&self, sz: &SizeTarget, monitor: &MonitorGeometry) -> (u32, u32) {
        match sz {
            SizeTarget::Absolute(w, h) => (*w, *h),
            SizeTarget::Flexible(wv, hv) => {
                let w = resolve_dim(*wv, monitor.width as i32).max(1) as u32;
                let h = resolve_dim(*hv, monitor.height as i32).max(1) as u32;
                (w, h)
            }
        }
    }

    // EWMH HELPERS

    fn set_wm_state(&self, window: Window, action: u32, prop1: Atom, prop2: Atom) {
        self.send_client_message(
            window,
            self.atoms._NET_WM_STATE,
            [action, prop1, prop2, 1, 0],
        );
    }

    fn send_client_message(&self, window: Window, msg_type: Atom, data: [u32; 5]) {
        let event = ClientMessageEvent::new(32, window, msg_type, data);
        let _ = self.conn.send_event(
            false,
            self.root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            event,
        );
    }

    fn set_decoration(&self, window: Window, decorated: bool) {
        // _MOTIF_WM_HINTS: [flags, functions, decorations, input_mode, status]
        // flags = 2 (MWM_HINTS_DECORATIONS), decorations = 0 or 1
        let decorations: u32 = if decorated { 1 } else { 0 };
        let hints: [u32; 5] = [2, 0, decorations, 0, 0];
        let _ = self.conn.change_property32(
            PropMode::REPLACE,
            window,
            self.atoms._MOTIF_WM_HINTS,
            self.atoms._MOTIF_WM_HINTS,
            &hints,
        );
    }

    fn log_actions(&self, rule: &CompiledRule) {
        let now = local_time();
        if let Some(ref mon) = rule.monitor {
            match mon {
                MonitorTarget::Index(i) => eprintln!("[{}] [DRY]    monitor -> {}", now, i),
                MonitorTarget::Name(n) => eprintln!("[{}] [DRY]    monitor -> '{}'", now, n),
            }
        }
        if let Some(ref pos) = rule.position {
            eprintln!("[{}] [DRY]    position -> {:?}", now, pos);
        }
        if let Some(ref sz) = rule.size {
            eprintln!("[{}] [DRY]    size -> {:?}", now, sz);
        }
        if let Some(ws) = rule.workspace {
            eprintln!("[{}] [DRY]    workspace -> {}", now, ws);
        }
        if let Some(true) = rule.maximize {
            eprintln!("[{}] [DRY]    maximize", now);
        }
        if let Some(true) = rule.fullscreen {
            eprintln!("[{}] [DRY]    fullscreen", now);
        }
        if let Some(true) = rule.pin {
            eprintln!("[{}] [DRY]    pin (all workspaces)", now);
        }
        if let Some(true) = rule.minimize {
            eprintln!("[{}] [DRY]    minimize", now);
        }
        if let Some(true) = rule.shade {
            eprintln!("[{}] [DRY]    shade", now);
        }
        if let Some(true) = rule.above {
            eprintln!("[{}] [DRY]    above", now);
        }
        if let Some(true) = rule.below {
            eprintln!("[{}] [DRY]    below", now);
        }
        if let Some(d) = rule.decorate {
            eprintln!("[{}] [DRY]    decorate -> {}", now, d);
        }
        if let Some(true) = rule.focus {
            eprintln!("[{}] [DRY]    focus", now);
        }
        if let Some(opacity) = rule.opacity {
            eprintln!("[{}] [DRY]    opacity -> {}", now, opacity);
        }
    }
}

// MONITOR QUERY

fn query_monitors(conn: &RustConnection, root: Window) -> Result<Vec<MonitorGeometry>, String> {
    let resources = conn
        .randr_get_screen_resources_current(root)
        .map_err(|e| format!("randr get resources: {}", e))?
        .reply()
        .map_err(|e| format!("randr get resources reply: {}", e))?;

    let mut monitors = Vec::new();

    for &output_id in &resources.outputs {
        let output_info = match conn.randr_get_output_info(output_id, 0) {
            Ok(cookie) => match cookie.reply() {
                Ok(info) => info,
                Err(_) => continue,
            },
            Err(_) => continue,
        };

        // Skip disconnected outputs
        if output_info.crtc == 0 || output_info.connection != x11rb::protocol::randr::Connection::CONNECTED {
            continue;
        }

        let crtc_info = match conn.randr_get_crtc_info(output_info.crtc, 0) {
            Ok(cookie) => match cookie.reply() {
                Ok(info) => info,
                Err(_) => continue,
            },
            Err(_) => continue,
        };

        let name = String::from_utf8_lossy(&output_info.name).to_string();

        monitors.push(MonitorGeometry {
            name,
            x: crtc_info.x as i32,
            y: crtc_info.y as i32,
            width: crtc_info.width as u32,
            height: crtc_info.height as u32,
        });
    }

    if monitors.is_empty() {
        // Fallback: use root window geometry
        let screen = &conn.setup().roots[0];
        monitors.push(MonitorGeometry {
            name: "default".into(),
            x: 0,
            y: 0,
            width: screen.width_in_pixels as u32,
            height: screen.height_in_pixels as u32,
        });
    }

    Ok(monitors)
}

fn get_client_list(conn: &RustConnection, root: Window, atoms: &Atoms) -> Vec<Window> {
    let reply = conn
        .get_property(false, root, atoms._NET_CLIENT_LIST, AtomEnum::WINDOW, 0, 4096)
        .ok()
        .and_then(|cookie| cookie.reply().ok());

    match reply {
        Some(prop) if prop.value.len() >= 4 => {
            prop.value
                .chunks_exact(4)
                .map(|chunk| u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                .collect()
        }
        _ => Vec::new(),
    }
}

fn resolve_dim(val: DimensionVal, total: i32) -> i32 {
    match val {
        DimensionVal::Pixels(px) => px,
        DimensionVal::Percent(pct) => (total as f64 * pct) as i32,
    }
}

fn local_time() -> String {
    unsafe {
        let mut t: libc::time_t = 0;
        libc::time(&mut t);
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
    }
}
