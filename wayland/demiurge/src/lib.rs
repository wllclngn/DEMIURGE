// demiurge-wl: The Wayland implementation. One compositor that draws
// either into the test window (`demiurge-wl`, the demiurge-wl-window
// crate) or onto the real screen (`demiurge-wl-tty`, the
// demiurge-wl-screen crate). The backends pass monitors and input up and
// take frames down; everything DEMIURGE does lives here:
//
//   wm.rs       the central state object and the protocol handlers
//   input.rs    keyboard and pointer input, keybinds
//   render.rs   what each frame shows, and when frames are drawn
//   cursor.rs   the cursor theme
//   backend.rs  the backend glue and what a running compositor holds
//   bar.rs      the status bar, drawn in-process
//   dsr.rs      Dynamic Super Resolution
//   gamma.rs    wlr-gamma-control, a client's gamma ramps on a CRTC
//   keys.rs     keybind compilation and spawning
//   policy.rs   cherrypie's window-policy API, where new windows go
//   xwm.rs      XWayland
//
// Server-agnostic logic (Action, Layout, the config schema, MruRing,
// the torrentius rendering primitives) lives in demiurge-core and is
// shared with the X11 implementation.

pub mod activation;
pub mod backend;
pub mod bar;
pub mod capture;
pub mod client;
pub mod color;
pub mod cursor;
pub mod cycle;
pub mod drag;
pub mod dsr;
pub mod focus;
pub mod idle;
pub mod ime;
pub mod gamma;
pub mod input;
pub mod keys;
pub mod layer;
pub mod policy;
pub mod render;
pub mod steam;
pub mod tag_menu;
pub mod titlebar;
pub mod wm;
pub mod xsettings;
pub mod xwm;

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::ExitCode;
use std::sync::Arc;

use smithay::backend::renderer::ImportDma;
use smithay::input::keyboard::XkbConfig;
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{EventLoop, Interest, Mode, PostAction};
use smithay::reexports::wayland_server::{Display, DisplayHandle};
use smithay::utils::{Clock, ClockSource, Monotonic};
use smithay::wayland::compositor::CompositorState;
use smithay::wayland::cursor_shape::CursorShapeManagerState;
use smithay::wayland::dmabuf::{DmabufFeedbackBuilder, DmabufState};
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::pointer_constraints::PointerConstraintsState;
use smithay::wayland::presentation::PresentationState;
use smithay::wayland::relative_pointer::RelativePointerManagerState;
use smithay::wayland::viewporter::ViewporterState;
use smithay::wayland::xdg_activation::XdgActivationState;
use smithay::wayland::commit_timing::CommitTimingManagerState;
use smithay::wayland::fifo::FifoManagerState;
use smithay::wayland::drm_syncobj::{DrmSyncobjState, supports_syncobj_eventfd};
use smithay::wayland::selection::data_device::DataDeviceState;
use smithay::wayland::session_lock::SessionLockManagerState;
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shm::ShmState;
use smithay::wayland::socket::ListeningSocketSource;
use smithay::wayland::xwayland_shell::XWaylandShellState;
use wayland_protocols::wp::color_management::v1::server::wp_color_manager_v1::WpColorManagerV1;
use wayland_protocols_wlr::gamma_control::v1::server::zwlr_gamma_control_manager_v1::ZwlrGammaControlManagerV1;

use demiurge_core::clock;
use demiurge_core::config::{self, Config};
use demiurge_core::session::Session;
use demiurge_wl_screen::Screen;
use demiurge_wl_window::Window;

use crate::backend::{Backend, BackendKind, LinuxDmabuf, Runtime};
use crate::bar::Bar;
use crate::client::ClientState;
use crate::cursor::Cursor;
use crate::dsr::Dsr;
use crate::render::OutputFrame;
use crate::wm::Wm;

/// Run the compositor on `kind` until it quits.
pub fn run(kind: BackendKind) -> ExitCode {
    if std::env::args().nth(1).as_deref() == Some(steam::HELPER) {
        return steam::helper();
    }
    // Started from a VT, as archd's session is, the log goes to the journal
    // (`journalctl --user -t demiurge-wl-tty`) instead of a console nobody
    // reads.
    demiurge_core::journal::route_stderr_from_vt(match kind {
        BackendKind::Screen => "demiurge-wl-tty",
        BackendKind::Window => "demiurge-wl",
    });
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    // Colored only on a terminal: In a file, as a session writes it, the
    // codes split each field from its value.
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .init();

    match start(kind) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("fatal: {}", e);
            ExitCode::FAILURE
        }
    }
}

fn start(kind: BackendKind) -> Result<(), Box<dyn std::error::Error>> {
    let quit = quit_signals()?;
    let children = child_signals()?;
    // A config that fails to load falls back to the defaults, so the
    // compositor still boots.
    let cfg = match config::Paths::init().and_then(|p| config::load(&p)) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("config load failed: {}; running with defaults", e);
            Config::default()
        }
    };

    let mut event_loop: EventLoop<'static, Wm> = EventLoop::try_new()?;
    let handle = event_loop.handle();
    handle.insert_source(Generic::new(quit, Interest::READ, Mode::Level), |_, fd, wm| {
        let mut info = std::mem::MaybeUninit::<libc::signalfd_siginfo>::uninit();
        let size = std::mem::size_of::<libc::signalfd_siginfo>();
        while unsafe { libc::read(fd.as_raw_fd(), info.as_mut_ptr().cast(), size) } == size as isize {
            let signal = unsafe { info.assume_init_ref() }.ssi_signo;
            tracing::info!(signal, "quit on a signal");
        }
        wm.running = false;
        Ok(PostAction::Continue)
    })?;
    // The children the compositor started are reaped as they exit; any
    // other, XWayland's own, is left to whoever started it.
    handle.insert_source(Generic::new(children, Interest::READ, Mode::Level), |_, fd, _| {
        let mut info = std::mem::MaybeUninit::<libc::signalfd_siginfo>::uninit();
        let size = std::mem::size_of::<libc::signalfd_siginfo>();
        while unsafe { libc::read(fd.as_raw_fd(), info.as_mut_ptr().cast(), size) } == size as isize {}
        demiurge_core::spawn::reap_tracked();
        Ok(PostAction::Continue)
    })?;
    let mut backend = match kind {
        BackendKind::Window => Backend::Window(Window::new(handle.clone())?),
        BackendKind::Screen => Backend::Screen(Screen::new(handle.clone(), display_modes(&cfg))?),
    };
    let outputs = backend.outputs();
    let primary = outputs.first().cloned().ok_or("no monitor to draw on")?;

    let display: Display<Wm> = Display::new()?;
    let dh = display.handle();
    let compositor_state = CompositorState::new::<Wm>(&dh);
    let xdg_shell_state = XdgShellState::new::<Wm>(&dh);
    // Default supported SHM formats (Argb8888 + Xrgb8888).
    let shm_state = ShmState::new::<Wm>(&dh, Vec::new());
    let mut seat_state = smithay::input::SeatState::new();
    let seat = seat_state.new_wl_seat(&dh, "seat0");
    let data_device_state = DataDeviceState::new::<Wm>(&dh);
    let output_manager_state = OutputManagerState::new_with_xdg_output::<Wm>(&dh);
    // Any client may take the session lock; which clients may is a
    // policy not enforced yet.
    let session_lock_state = SessionLockManagerState::new::<Wm, _>(&dh, |_| true);
    // Lets XWayland pair its wl_surfaces with X11 windows by serial.
    let xwayland_shell_state = XWaylandShellState::new::<Wm>(&dh);
    let presentation = PresentationState::new::<Wm>(&dh, Monotonic::ID as u32);
    dh.create_global::<Wm, WpColorManagerV1, ()>(color::VERSION, ());
    dh.create_global::<Wm, ZwlrGammaControlManagerV1, ()>(gamma::VERSION, ());
    let mut frames: Vec<OutputFrame> = outputs
        .into_iter()
        .map(|output| {
            let global = output.create_global::<Wm>(&dh);
            OutputFrame::new(output, global)
        })
        .collect();

    let mut wm = Wm::new(
        compositor_state,
        xdg_shell_state,
        shm_state,
        seat_state,
        data_device_state,
        seat,
        output_manager_state,
        session_lock_state,
        primary.clone(),
        xwayland_shell_state,
        &cfg,
    );
    wm.set_monitors(&frames.iter().map(|f| f.output.clone()).collect::<Vec<_>>());

    // [input.keyboard] feeds smithay's XkbConfig. Empty strings and an
    // empty options list leave xkbcommon's defaults (XKB_DEFAULT_*) in
    // place, matching the X11 path's "empty = don't override".
    let options = (!cfg.input.keyboard.options.is_empty()).then(|| cfg.input.keyboard.options.join(","));
    let xkb = XkbConfig {
        rules: "",
        model: "",
        layout: cfg.input.keyboard.layout.as_str(),
        variant: cfg.input.keyboard.variant.as_str(),
        options,
    };
    // X11 / xset semantics: 0 leaves the default in place. smithay needs
    // numbers, so 0 becomes its documented defaults (200ms, 25/s).
    let delay = match cfg.input.keyboard.repeat_delay {
        0 => 200,
        d => d as i32,
    };
    let rate = match cfg.input.keyboard.repeat_rate {
        0 => 25,
        r => r as i32,
    };
    wm.seat
        .add_keyboard(xkb, delay, rate)
        .map_err(|e| format!("add_keyboard: {}", e))?;
    wm.seat.add_pointer();
    if let Some(mode) = primary.current_mode() {
        wm.pointer_location = (mode.size.w as f64 / 2.0, mode.size.h as f64 / 2.0).into();
    }

    let dsr = Dsr::new(cfg.displays.first().map_or(1.0, |d| d.dsr_multiplier));
    if dsr.active() {
        tracing::info!("[dsr] enabled, multiplier {}", dsr.multiplier);
    }
    let dmabuf = match dmabuf_global(&mut backend, &mut wm.dmabuf_state, &dh) {
        Ok(dmabuf) => Some(dmabuf),
        Err(e) => {
            tracing::warn!("no linux-dmabuf: {}; GPU clients fall back to shared memory", e);
            None
        }
    };
    for frame in &mut frames {
        frame.scanout = dmabuf.as_ref().and_then(|d| d.scanout(&backend, &frame.output));
    }
    let syncobj = match backend.sync_device() {
        Ok(device) if supports_syncobj_eventfd(&device) => {
            tracing::info!("explicit sync through linux-drm-syncobj");
            Some(DrmSyncobjState::new::<Wm>(&dh, device))
        }
        Ok(_) => {
            tracing::info!("no explicit sync: the GPU's device cannot wait on a timeline through an eventfd");
            None
        }
        Err(e) => {
            tracing::info!("no explicit sync: {}", e);
            None
        }
    };
    let (notices, notified) = smithay::reexports::calloop::channel::channel();
    handle.insert_source(notified, |event, _, wm| {
        if let smithay::reexports::calloop::channel::Event::Msg(text) = event {
            wm.notify(text);
        }
    })?;
    wm.runtime = Some(Runtime {
        backend,
        bar: Bar::new(&cfg).map_err(|e| format!("bar init: {}", e))?,
        dsr,
        cursor: Cursor::new(&cfg.cursor.theme, cfg.cursor.size),
        cursor_shape: CursorShapeManagerState::new::<Wm>(&dh),
        relative_pointer: RelativePointerManagerState::new::<Wm>(&dh),
        pointer_constraints: PointerConstraintsState::new::<Wm>(&dh),
        viewporter: ViewporterState::new::<Wm>(&dh),
        activation: XdgActivationState::new::<Wm>(&dh),
        toplevel_icons: smithay::wayland::xdg_toplevel_icon::XdgToplevelIconManager::new::<Wm>(&dh),
        idle_inhibit: smithay::wayland::idle_inhibit::IdleInhibitManagerState::new::<Wm>(&dh),
        text_input: smithay::wayland::text_input::TextInputManagerState::new::<Wm>(&dh),
        decorations: smithay::wayland::shell::xdg::decoration::XdgDecorationState::new::<Wm>(&dh),
        input_method: smithay::wayland::input_method::InputMethodManagerState::new::<Wm, _>(&dh, |_| true),
        fifo: FifoManagerState::new::<Wm>(&dh),
        commit_timing: CommitTimingManagerState::new::<Wm>(&dh),
        layer_shell: smithay::wayland::shell::wlr_layer::WlrLayerShellState::new::<Wm>(&dh),
        output_capture_source: smithay::wayland::image_capture_source::OutputCaptureSourceState::new::<Wm>(&dh),
        image_copy_capture: smithay::wayland::image_copy_capture::ImageCopyCaptureState::new::<Wm>(&dh),
        data_control: smithay::wayland::selection::ext_data_control::DataControlState::new::<Wm, _>(&dh, None, |_| true),
        syncobj,
        handle: handle.clone(),
        display: dh.clone(),
        frames,
        clock: Clock::new(),
        presentation,
        dmabuf,
        // Clients that load their own cursors match the compositor's.
        env: vec![("XCURSOR_THEME", cfg.cursor.theme.clone()), ("XCURSOR_SIZE", cfg.cursor.size.to_string())],
        notices,
    });

    // Clients: new connections on the listening socket, requests on the
    // display's own fd.
    let socket = ListeningSocketSource::new_auto()?;
    let socket_name = socket.socket_name().to_string_lossy().into_owned();
    tracing::info!(socket = %socket_name, "wayland socket ready");
    let mut accept = dh.clone();
    handle.insert_source(socket, move |stream, _, _| {
        let state = ClientState::of(&stream);
        if let Err(e) = accept.insert_client(stream, Arc::new(state)) {
            tracing::warn!(error = %e, "insert_client failed");
        }
    })?;
    handle.insert_source(Generic::new(display, Interest::READ, Mode::Level), |_, display, wm| {
        // SAFETY: The display is never dropped while its source is
        // registered.
        if let Err(e) = unsafe { display.get_mut() }.dispatch_clients(wm) {
            tracing::warn!(error = %e, "dispatch_clients");
        }
        Ok(PostAction::Continue)
    })?;
    let mut env = vec![("WAYLAND_DISPLAY", socket_name.clone())];
    // XWayland's X11 apps get DEMIURGE's settings through XSETTINGS, as on
    // the X11 stack.
    let xsettings = demiurge_core::xsettings::serialize(&cfg, 0);
    let attach = move |wm: &mut Wm, x11, display| {
        wm.xwm = Some(x11);
        wm.x11_reader = xwm::X11Reader::connect(display)
            .map_err(|e| tracing::warn!(error = %e, "no reader connection to XWayland"))
            .ok();
        wm.xsettings = xsettings::XSettings::serve(display, &xsettings)
            .map_err(|e| tracing::warn!(error = %e, "no XSETTINGS on XWayland"))
            .ok();
    };
    match xwm::spawn_xwayland(&dh, handle.clone(), attach) {
        Ok(Some(display)) => env.push(("DISPLAY", format!(":{}", display))),
        Ok(None) => {}
        Err(e) => tracing::warn!(error = %e, "spawn_xwayland"),
    }
    // The session's services start on the real screen's displays; the test
    // window is never a session.
    let session = match kind {
        BackendKind::Screen => {
            Session::begin(demiurge_core::session::WAYLAND_TARGET, &env).map_err(|e| tracing::warn!("session: {}", e)).ok()
        }
        BackendKind::Window => None,
    };
    let rt = wm.runtime.as_mut().expect("the runtime was just set");
    rt.env.extend(env);
    // The session's children find this stack's binaries first; the user
    // manager's PATH is left alone.
    if let Some(path) = demiurge_core::spawn::path_with_own_dir() {
        rt.env.push(("PATH", path.to_string_lossy().into_owned()));
    }
    // Wayland GTK apps read GSettings: The real screen's session writes
    // DEMIURGE's settings there; the test window never touches the host's.
    if matches!(kind, BackendKind::Screen) {
        for cmd in xsettings::gsettings_commands(&cfg) {
            keys::spawn(&cmd, &rt.env);
        }
    }

    // DEMIURGE_WL_AUTOSPAWN=foot forks one client by name, for quick
    // "does it work" runs without touching config.
    if let Ok(cmd) = std::env::var("DEMIURGE_WL_AUTOSPAWN") {
        keys::spawn(&cmd, &rt.env);
    }
    let pending = spawn_startup_commands(&cfg, &rt.env);
    wm.pending_spawns = pending;
    // With auto_hide, the cursor hides after the session's first still spell,
    // and with an idle lock the session's clock starts.
    wm.pointer_stirred();
    wm.arm_idle();
    // cherrypie, on the compositor's own socket name.
    wm.attach_policy(&socket_name);

    // The bar's clock ticks on each second of the wall clock, the wait
    // taken from the wall clock every tick so no lateness carries over, and
    // the bar is drawn only when the time it shows changed.
    if cfg.bar.height > 0 {
        handle.insert_source(Timer::from_duration(clock::until_next_second()), |_, _, wm| {
            if wm.runtime.as_mut().is_some_and(|rt| rt.bar.tick_clock()) {
                wm.queue_redraw();
            }
            TimeoutAction::ToDuration(clock::until_next_second())
        })?;
    }

    wm.queue_redraw();
    while wm.running {
        event_loop.dispatch(None, &mut wm)?;
        if let Some(rt) = wm.runtime.as_mut() {
            rt.display.flush_clients()?;
        }
    }
    tracing::info!("quit requested");
    // The session's programs are told to stop, as the X11 stack tells them.
    demiurge_core::spawn::quit_children();
    if let Some(Err(e)) = session.map(Session::end) {
        tracing::warn!("session: {}", e);
    }
    Ok(())
}

// SIGINT, SIGTERM and SIGHUP, read from a signalfd so they quit the way the
// quit bind does and the session's end reaches the user manager. Blocked
// before the backend makes any thread, so every thread inherits the mask;
// children start with it cleared.
fn quit_signals() -> Result<OwnedFd, String> {
    unsafe {
        let mut mask: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut mask);
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            libc::sigaddset(&mut mask, signal);
        }
        let e = libc::pthread_sigmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut());
        if e != 0 {
            return Err(format!("block the quit signals: {}", std::io::Error::from_raw_os_error(e)));
        }
        let fd = libc::signalfd(-1, &mask, libc::SFD_NONBLOCK | libc::SFD_CLOEXEC);
        if fd < 0 {
            return Err(format!("signalfd: {}", std::io::Error::last_os_error()));
        }
        Ok(OwnedFd::from_raw_fd(fd))
    }
}

// SIGCHLD, read from a signalfd so the children are reaped on the loop
// rather than by a thread waiting on each. Blocked with the quit signals,
// before any thread exists.
fn child_signals() -> Result<OwnedFd, String> {
    unsafe {
        let mut mask: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut mask);
        libc::sigaddset(&mut mask, libc::SIGCHLD);
        let e = libc::pthread_sigmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut());
        if e != 0 {
            return Err(format!("block SIGCHLD: {}", std::io::Error::from_raw_os_error(e)));
        }
        let fd = libc::signalfd(-1, &mask, libc::SFD_NONBLOCK | libc::SFD_CLOEXEC);
        if fd < 0 {
            return Err(format!("signalfd: {}", std::io::Error::last_os_error()));
        }
        Ok(OwnedFd::from_raw_fd(fd))
    }
}

// linux-dmabuf v5, its default feedback naming the renderer's GPU by its
// render node and listing what the renderer imports. Without it NVIDIA's
// EGL refuses the display and GPU clients fall back to the CPU.
fn dmabuf_global(backend: &mut Backend, state: &mut DmabufState, dh: &DisplayHandle) -> Result<LinuxDmabuf, String> {
    let node = backend.render_node()?;
    let formats = backend.renderer().ok_or("no renderer")?.dmabuf_formats();
    if formats.iter().next().is_none() {
        return Err("the renderer imports no dmabuf format".to_string());
    }
    let feedback = DmabufFeedbackBuilder::new(node.dev_id(), formats.iter().copied())
        .build()
        .map_err(|e| format!("feedback: {}", e))?;
    tracing::info!(node = %node, "linux-dmabuf on the render node");
    let global = state.create_global_with_default_feedback::<Wm>(dh, &feedback);
    Ok(LinuxDmabuf { global, feedback, node, formats })
}

// The [[display]] entries' modes, as the real screen takes them: The entry's
// match against a connector's name, as on X11, and a "WIDTHxHEIGHT" size.
fn display_modes(cfg: &Config) -> Vec<demiurge_wl_screen::ModeRequest> {
    cfg.displays
        .iter()
        .filter(|d| !d.mode.is_empty())
        .filter_map(|d| {
            let size = d.mode.split_once('x').and_then(|(w, h)| Some((w.trim().parse().ok()?, h.trim().parse().ok()?)));
            if size.is_none() {
                tracing::warn!(mode = %d.mode, "a [[display]] mode is not WIDTHxHEIGHT");
            }
            size.map(|size| demiurge_wl_screen::ModeRequest { pattern: d.r#match.clone(), size })
        })
        .collect()
}

/// Fork each command in `cfg.startup.commands` via `sh -c` with `env`
/// ($WAYLAND_DISPLAY and $DISPLAY among it), so a client finds the
/// compositor and an X11 one finds XWayland, then each [[startup.spawn]]
/// as on X11: Launched, and one naming a tag sends the first window of its
/// class there. Returns the tagged entries, a class and a tag each.
///
/// Failures to spawn are logged but don't abort the compositor: a
/// missing terminal binary in startup.commands shouldn't black-hole
/// the session.
pub fn spawn_startup_commands(cfg: &Config, env: &[(&str, String)]) -> Vec<(String, usize)> {
    for cmd in &cfg.startup.commands {
        keys::spawn(cmd, env);
    }
    let mut pending = Vec::new();
    for sp in &cfg.startup.spawn {
        if let Some(tag) = sp.tag.as_ref().and_then(|tag| cfg.general.tags.iter().position(|t| t == tag)) {
            pending.push((sp.class_match(), tag));
        }
        keys::spawn(&sp.cmd, env);
    }
    pending
}
