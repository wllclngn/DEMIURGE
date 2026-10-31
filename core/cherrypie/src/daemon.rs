use std::collections::HashMap;
use std::os::fd::RawFd;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::api::{self, Moved, NewWindow, Placement, Rect, Request, Response};
use crate::backend::{Backend, UnmatchedWindow};
use crate::config;
use crate::learn;
use crate::local_time;
use crate::memory::{self, Spot};
use crate::place::{self, MonitorGeometry};
use crate::rules::{self, CompiledRule};
use crate::server::Server;

// Exit status for a config the daemon cannot start from (sysexits
// EX_CONFIG). cherrypie.service names it in RestartPreventExitStatus, so a
// bad config stops once instead of restarting forever.
pub const EXIT_CONFIG: i32 = 78;

// How long a window of an unmatched class must stay open before its learned
// rule is written to the config. Installers, consoles and launch helpers
// (Wine's conhost and wineboot, Proton's VC_redist runs) close well inside
// it and leave nothing behind; an application the user keeps open does not.
pub const LEARN_AFTER: Duration = Duration::from_secs(30);

// A learned rule held in memory until one of its class's windows has stayed
// open for the learn-after interval.
struct Pending {
    class: String,
    text: String,
    rule: CompiledRule,
    windows: Vec<u32>,
    since: Instant,
}

// The API client's world as it has described it: Its monitors and the
// windows it announced and has not yet closed.
#[derive(Default)]
struct ApiState {
    monitors: Vec<api::Monitor>,
    live: HashMap<u32, NewWindow>,
}

// Take an exclusive flock on $XDG_RUNTIME_DIR/cherrypie-<display>.lock.
// False when another cherrypie holds it. The fd is deliberately kept open
// for the life of the process; the kernel drops the lock at exit.
pub fn claim_instance(display: &str) -> bool {
    let display = api::display_tag(display);
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    let Ok(path) = std::ffi::CString::new(format!("{}/cherrypie-{}.lock", dir, display)) else {
        return true;
    };
    let fd = unsafe {
        libc::open(path.as_ptr(), libc::O_RDWR | libc::O_CREAT | libc::O_CLOEXEC, 0o600)
    };
    if fd < 0 {
        // No lock file to take means no way to tell; run rather than not.
        return true;
    }
    unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

pub fn setup_signalfd() -> i32 {
    unsafe {
        let mut mask: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut mask);
        libc::sigaddset(&mut mask, libc::SIGTERM);
        libc::sigaddset(&mut mask, libc::SIGINT);
        libc::sigprocmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut());
        libc::signalfd(-1, &mask, libc::SFD_CLOEXEC)
    }
}

// Serve the API on `display`'s socket, and act through `backend` on the
// windows it reports, when there is one.
pub fn run(
    backend: Option<Box<dyn Backend>>,
    display: &str,
    config_path: &Path,
    dry_run: bool,
    signal_fd: i32,
    learn_after: Duration,
) {
    let compiled = match load_rules(config_path) {
        Some(r) => r,
        None => std::process::exit(EXIT_CONFIG),
    };

    let inotify_fd = setup_inotify(config_path);

    let socket = api::socket_path(display);
    let server = match Server::bind(&socket) {
        Ok(s) => {
            eprintln!("[cherrypie] api: listening on {}", socket.display());
            Some(s)
        }
        Err(e) => {
            eprintln!("[cherrypie] api: cannot listen on {}: {} (standalone only)", socket.display(), e);
            None
        }
    };

    eprintln!(
        "[cherrypie] daemon started (backend: {}, rules: {}, dry_run: {})",
        backend.as_ref().map_or("none, the API only", |b| b.name()),
        compiled.len(),
        dry_run,
    );

    event_loop(backend.as_deref(), compiled, server, signal_fd, inotify_fd, config_path, dry_run, learn_after);

    // Cleanup
    if signal_fd >= 0 {
        unsafe { libc::close(signal_fd); }
    }
    if inotify_fd >= 0 {
        unsafe { libc::close(inotify_fd); }
    }

    eprintln!("[cherrypie] shutdown");
}

fn pollfd(fd: RawFd) -> libc::pollfd {
    libc::pollfd { fd, events: if fd >= 0 { libc::POLLIN } else { 0 }, revents: 0 }
}

// Indices into the poll set; the API connections follow LISTENER.
const BACKEND: usize = 0;
const SIGNALS: usize = 1;
const INOTIFY: usize = 2;
const LISTENER: usize = 3;

#[allow(clippy::too_many_arguments)]
fn event_loop(
    wm: Option<&dyn Backend>,
    mut rules: Vec<CompiledRule>,
    mut server: Option<Server>,
    signal_fd: i32,
    inotify_fd: i32,
    config_path: &Path,
    dry_run: bool,
    learn_after: Duration,
) {
    let mut pending: Vec<Pending> = Vec::new();
    let mut state = ApiState::default();
    let backend_fd = wm.map_or(-1, |b| b.connection_fd());

    // Apply rules to windows that already existed at startup
    if let Some(b) = wm {
        let unmatched = b.process_events(&rules, dry_run, false);
        learn_rules(b, &mut rules, &mut pending, unmatched, dry_run);
    }

    loop {
        // poll() skips negative fds, so every slot is always present.
        let mut fds = vec![pollfd(backend_fd), pollfd(signal_fd), pollfd(inotify_fd)];
        fds.push(pollfd(server.as_ref().map_or(-1, |s| s.listener_fd())));
        if let Some(s) = &server {
            fds.extend(s.conn_fds().into_iter().map(pollfd));
        }

        let timeout = pending
            .iter()
            .map(|p| (p.since + learn_after).saturating_duration_since(Instant::now()))
            .min()
            .map_or(-1, |d| d.as_millis().min(i32::MAX as u128) as i32 + 1);
        let ret = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
        if ret < 0 {
            let errno = unsafe { *libc::__errno_location() };
            if errno == libc::EINTR {
                continue;
            }
            eprintln!("[cherrypie] poll error: {}", errno);
            break;
        }

        // Clean shutdown
        if fds[SIGNALS].revents & libc::POLLIN != 0 {
            drain_signalfd(signal_fd);
            break;
        }

        // Config reload
        if fds[INOTIFY].revents & libc::POLLIN != 0 {
            drain_inotify(inotify_fd);
            if let Some(new_rules) = load_rules(config_path) {
                eprintln!(
                    "[cherrypie] config reloaded ({} rules)",
                    new_rules.len()
                );
                rules = new_rules;
            }
        }

        // API: new clients, then requests on the ready connections.
        if let Some(s) = server.as_mut() {
            if fds[LISTENER].revents & libc::POLLIN != 0 {
                s.accept();
            }
            let ready: Vec<RawFd> = fds[LISTENER + 1..]
                .iter()
                .filter(|p| p.revents != 0)
                .map(|p| p.fd)
                .collect();
            if !ready.is_empty() {
                s.service(&ready, |req| {
                    answer(req, &mut rules, &mut pending, &mut state, config_path, dry_run)
                });
            }
        }

        // Window events. While an API client is connected it places every
        // window, so the backend only keeps its bookkeeping.
        if let Some(b) = wm
            && fds[BACKEND].revents & libc::POLLIN != 0
        {
            let passive = server.as_ref().is_some_and(|s| s.connected());
            let unmatched = b.process_events(&rules, dry_run, passive);
            learn_rules(b, &mut rules, &mut pending, unmatched, dry_run);
        }

        persist_lasting(wm, &state, &mut rules, &mut pending, config_path, learn_after);
    }
}

fn answer(
    req: &Request,
    rules: &mut Vec<CompiledRule>,
    pending: &mut Vec<Pending>,
    state: &mut ApiState,
    config_path: &Path,
    dry_run: bool,
) -> Response {
    let segments: Vec<&str> = req.path.trim_matches('/').split('/').collect();
    match (req.op.as_str(), segments.as_slice()) {
        ("POST", ["windows"]) => match serde_json::from_value::<NewWindow>(req.body.clone()) {
            Ok(w) => {
                let placement = place_new(&w, rules, pending, state, dry_run);
                state.live.insert(w.window, w);
                Response::new(req.id, 200, placement)
            }
            Err(e) => Response::new(req.id, 400, format!("bad window: {e}")),
        },
        ("PUT", ["windows", id, "geometry"]) => {
            let Ok(id) = id.parse::<u32>() else {
                return Response::new(req.id, 400, format!("bad window id: {id}"));
            };
            let Some(w) = state.live.get(&id) else {
                return Response::new(req.id, 404, format!("no window {id}"));
            };
            match serde_json::from_value::<Moved>(req.body.clone()) {
                Ok(moved) => match remember(w, &moved, pending, &state.monitors, config_path, dry_run) {
                    Ok(()) => Response::new(req.id, 204, ()),
                    Err((status, e)) => Response::new(req.id, status, e),
                },
                Err(e) => Response::new(req.id, 400, format!("bad geometry: {e}")),
            }
        }
        ("DELETE", ["windows", id]) => match id.parse::<u32>() {
            Ok(id) => {
                state.live.remove(&id);
                Response::new(req.id, 204, ())
            }
            Err(_) => Response::new(req.id, 400, format!("bad window id: {id}")),
        },
        ("PUT", ["monitors"]) => match serde_json::from_value::<Vec<api::Monitor>>(req.body.clone()) {
            Ok(monitors) => {
                state.monitors = monitors;
                Response::new(req.id, 204, ())
            }
            Err(e) => Response::new(req.id, 400, format!("bad monitors: {e}")),
        },
        _ => Response::new(req.id, 404, format!("no route for {} {}", req.op, req.path)),
    }
}

// A window no rule matches is learned from only when it is a titled, normal,
// non-transient, windowed application window; the same test the X11 backend
// applies.
fn learnable(w: &NewWindow) -> bool {
    !w.class.is_empty() && !w.title.is_empty() && w.window_type == "normal" && !w.transient && !w.fullscreen
}

// The placement for a window about to map: Every matching rule in order, or
// the class's learned rule (held pending until a window of it lasts), or
// nothing for the client to fill in.
fn place_new(
    w: &NewWindow,
    rules: &[CompiledRule],
    pending: &mut Vec<Pending>,
    state: &ApiState,
    dry_run: bool,
) -> Placement {
    let matched: Vec<&CompiledRule> = rules
        .iter()
        .filter(|r| r.matches(&w.class, &w.title, &w.role, &w.process, &w.window_type))
        .collect();
    if !matched.is_empty() {
        for _ in &matched {
            eprintln!(
                "[{}] [INFO]   matched '{}' (class='{}', title='{}', process='{}')",
                local_time(),
                w.class,
                w.class,
                w.title,
                w.process
            );
        }
        return if dry_run { Placement::default() } else { placement(&matched, w, &state.monitors) };
    }
    if !learnable(w) {
        return Placement::default();
    }
    if dry_run {
        eprintln!("[cherrypie] [DRY] would learn a rule for class '{}'", w.class);
        return Placement::default();
    }
    let index = match pending.iter().position(|p| p.class == w.class) {
        Some(i) => i,
        None => {
            let text = learn::rule_text(&w.class, &learn::today());
            let rule = match learn::compile(&text) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("[cherrypie] learned rule for '{}' does not compile: {}", w.class, e);
                    return Placement::default();
                }
            };
            pending.push(Pending { class: w.class.clone(), text, rule, windows: Vec::new(), since: Instant::now() });
            pending.len() - 1
        }
    };
    pending[index].windows.push(w.window);
    placement(&[&pending[index].rule], w, &state.monitors)
}

// Record where the user left `w` in the rule that placed it: Its class's
// pending learned rule when that is what placed it, else the config file's
// rule (memory::record picks it and decides whether it remembers).
fn remember(
    w: &NewWindow,
    moved: &Moved,
    pending: &mut [Pending],
    monitors: &[api::Monitor],
    config_path: &Path,
    dry_run: bool,
) -> Result<(), (u16, String)> {
    let Some(area) = monitors.iter().find(|m| m.name == moved.monitor).map(|m| m.work_area) else {
        return Err((400, format!("no monitor {}", moved.monitor)));
    };
    let spot = Spot::new(&moved.monitor, moved.rect, area);
    if dry_run {
        eprintln!("[cherrypie] [DRY] would remember '{}' at {:?}", w.class, spot);
        return Ok(());
    }

    if let Some(p) = pending.iter_mut().find(|p| p.windows.contains(&w.window)) {
        if let Some(text) = memory::record(&p.text, w, &spot).map_err(|e| (500, e))? {
            p.rule = learn::compile(&text).map_err(|e| (500, e))?;
            p.text = text;
        }
        return Ok(());
    }

    let text = match std::fs::read_to_string(config_path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err((500, format!("{}: {}", config_path.display(), e))),
    };
    let Some(text) = memory::record(&text, w, &spot).map_err(|e| (500, e))? else {
        return Ok(());
    };
    // In place, so a symlinked config stays one; the inotify reload picks it up.
    std::fs::write(config_path, text).map_err(|e| (500, format!("{}: {}", config_path.display(), e)))?;
    eprintln!(
        "[{}] [INFO]   remembered '{}' on {} at {} {}, size {} {}",
        local_time(),
        w.class,
        spot.monitor,
        spot.position[0],
        spot.position[1],
        spot.size[0],
        spot.size[1]
    );
    Ok(())
}

// Fold the rules into one placement, later rules overriding earlier ones the
// way the X11 backend applies them in turn. Geometry resolves against the
// work area of the monitor a rule names, else the window's own monitor; a
// window asking for fullscreen keeps its geometry.
fn placement(rules: &[&CompiledRule], w: &NewWindow, monitors: &[api::Monitor]) -> Placement {
    let areas: Vec<MonitorGeometry> = monitors
        .iter()
        .map(|m| MonitorGeometry {
            name: m.name.clone(),
            x: m.work_area.x,
            y: m.work_area.y,
            width: m.work_area.w,
            height: m.work_area.h,
        })
        .collect();
    let fallback = MonitorGeometry { name: w.monitor.clone(), x: 0, y: 0, width: 1920, height: 1080 };
    let mut out = Placement::default();
    let mut current = (w.requested.x, w.requested.y, w.requested.w, w.requested.h);
    for rule in rules {
        let area = place::named_monitor(rule, &areas)
            .or_else(|| areas.iter().find(|a| a.name == w.monitor))
            .or(areas.first())
            .unwrap_or(&fallback);
        if !w.fullscreen
            && let Some(g) = place::geometry(rule, area, current)
        {
            current = g;
            out.geometry = Some(Rect { x: g.0, y: g.1, w: g.2, h: g.3 });
        }
        out.tag = rule.workspace.clone().or(out.tag);
        out.focus = rule.focus.or(out.focus);
        out.fullscreen = rule.fullscreen.or(out.fullscreen);
        out.above = rule.above.or(out.above);
    }
    out
}

// Give each unmatched window class a rule: Apply it to the window now, and
// hold it in memory (pending) so further windows of the class match it
// instead of learning again. persist_lasting writes it to the config once a
// window of the class has stayed open; the inotify reload that append
// triggers recompiles the same rule.
fn learn_rules(
    wm: &dyn Backend,
    rules: &mut Vec<CompiledRule>,
    pending: &mut Vec<Pending>,
    unmatched: Vec<UnmatchedWindow>,
    dry_run: bool,
) {
    if unmatched.is_empty() {
        return;
    }
    let date = learn::today();
    for w in unmatched {
        if let Some(rule) = rules
            .iter()
            .find(|r| r.matches(&w.class, &w.title, &w.role, &w.process, &w.window_type))
        {
            if !dry_run {
                wm.apply(w.window, rule);
            }
            continue;
        }
        if let Some(p) = pending.iter_mut().find(|p| p.class == w.class) {
            wm.apply(w.window, &p.rule);
            p.windows.push(w.window);
            continue;
        }
        let text = learn::rule_text(&w.class, &date);
        let rule = match learn::compile(&text) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[cherrypie] learned rule for '{}' does not compile: {}", w.class, e);
                continue;
            }
        };
        if dry_run {
            eprintln!("[cherrypie] [DRY] would learn a rule for class '{}'", w.class);
            rules.push(rule);
            continue;
        }
        wm.apply(w.window, &rule);
        pending.push(Pending {
            class: w.class,
            text,
            rule,
            windows: vec![w.window],
            since: Instant::now(),
        });
    }
}

// Write each pending rule whose interval has run out, if any window of its
// class is still open (as the API client last described it, or on the
// backend's display server); a class whose windows all closed first is
// dropped.
fn persist_lasting(
    wm: Option<&dyn Backend>,
    state: &ApiState,
    rules: &mut Vec<CompiledRule>,
    pending: &mut Vec<Pending>,
    config_path: &Path,
    learn_after: Duration,
) {
    let now = Instant::now();
    let (due, waiting): (Vec<Pending>, Vec<Pending>) = std::mem::take(pending)
        .into_iter()
        .partition(|p| now.duration_since(p.since) >= learn_after);
    *pending = waiting;
    for p in due {
        if !p.windows.iter().any(|&w| state.live.contains_key(&w) || wm.is_some_and(|b| b.client_alive(w))) {
            eprintln!("[cherrypie] not learning '{}': no window of it stayed open", p.class);
            continue;
        }
        match learn::append(config_path, &p.text) {
            Ok(()) => eprintln!(
                "[cherrypie] learned a rule for class '{}' -> {}",
                p.class,
                config_path.display()
            ),
            Err(e) => eprintln!(
                "[cherrypie] could not write the learned rule for '{}': {}",
                p.class, e
            ),
        }
        rules.push(p.rule);
    }
}

fn load_rules(config_path: &Path) -> Option<Vec<CompiledRule>> {
    let paths = config::Paths::with_config(config_path.to_path_buf());
    match config::load(&paths) {
        Ok(cfg) => match rules::compile(&cfg) {
            Ok(compiled) => Some(compiled),
            Err(e) => {
                eprintln!("[cherrypie] rule compile error: {}", e);
                None
            }
        },
        Err(e) => {
            eprintln!("[cherrypie] config error: {}", e);
            None
        }
    }
}

fn setup_inotify(config_path: &Path) -> i32 {
    let parent = match config_path.parent() {
        Some(p) => p,
        None => return -1,
    };

    let dir_str = match std::ffi::CString::new(parent.to_string_lossy().as_bytes()) {
        Ok(s) => s,
        Err(_) => return -1,
    };

    unsafe {
        let fd = libc::inotify_init1(libc::IN_CLOEXEC);
        if fd < 0 {
            return -1;
        }

        let wd = libc::inotify_add_watch(fd, dir_str.as_ptr(), libc::IN_CLOSE_WRITE);
        if wd < 0 {
            libc::close(fd);
            return -1;
        }

        fd
    }
}

fn drain_signalfd(fd: i32) {
    unsafe {
        let mut buf = [0u8; 128];
        libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len());
    }
}

fn drain_inotify(fd: i32) {
    unsafe {
        let mut buf = [0u8; 4096];
        libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len());
    }
}
