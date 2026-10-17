// TTY backend: real-session entry point for demiurge-wl.
//
// This is the production path: libseat-driven session, udev-discovered
// DRM device, DrmCompositor for swap-chain + page-flip, libinput for
// input. The same Wm + render model the winit dev backend uses,
// driven by a different transport.
//
// Single-output for v0: pick the first connected connector on the
// first DRM card we see, set up a DrmCompositor for it, render. Real
// multi-output (multi-DrmCompositor) lands when the dev environment
// has more than one display to test against.
//
// Structure of this file:
//   1. run() / run_inner() -- entry, builds session + event loop
//   2. on_udev_event -- DRM device add/remove
//   3. open_drm_device -- per-device setup: DrmDevice, GbmDevice,
//      EglDisplay, GlesRenderer, pick connector + mode, create
//      DrmSurface + DrmCompositor
//   4. on_vblank -- driven by DrmDeviceNotifier; render + queue_frame
//   5. on_input_event -- libinput event fan-out (keyboard -> seat)
//   6. render_frame -- elements list (clients + bar + lock-mode
//      branch), passed to DrmCompositor::render_frame
//
// Limitations as of this slice:
//   - Single output only (first connected connector wins)
//   - Pause/Resume from session events drops master but doesn't yet
//     re-acquire on resume
//   - DSR not yet wired in the TTY path (winit path keeps its DSR
//     pipeline; porting to DrmCompositor needs the offscreen-pass
//     adapted to its element model)
//   - Cursor plane is set up but no cursor is drawn

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use smithay::backend::allocator::Fourcc;
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::drm::{DrmDevice, DrmDeviceFd, DrmEvent, DrmNode};
use smithay::backend::drm::compositor::DrmCompositor;
use smithay::backend::drm::compositor::FrameFlags;
use smithay::backend::drm::exporter::gbm::GbmFramebufferExporter;
use smithay::backend::egl::{EGLContext, EGLDisplay};
use smithay::backend::input::{InputEvent, KeyState, KeyboardKeyEvent};
use smithay::backend::libinput::LibinputInputBackend;
use smithay::backend::renderer::Color32F;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::session::{AsErrno, Event as SessionEvent, Session};
use smithay::backend::udev::{UdevBackend, UdevEvent, primary_gpu};
use smithay::output::OutputModeSource;
use smithay::reexports::drm;
use smithay::reexports::drm::control::Device as _;
use smithay::xwayland::{X11Surface, X11Wm, XwmHandler};
use smithay::xwayland::xwm::{Reorder, ResizeEdge, XwmId};
use smithay::utils::{Logical, Rectangle};
use smithay::input::keyboard::{FilterResult, XkbConfig};
use smithay::output::{Mode as OutputMode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::input as libinput;
use smithay::reexports::rustix;
use smithay::reexports::wayland_server::{Display, DisplayHandle};
use smithay::utils::DeviceFd;
use smithay::wayland::compositor::CompositorState;
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::selection::data_device::DataDeviceState;
use smithay::wayland::session_lock::SessionLockManagerState;
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shm::ShmState;
use smithay::wayland::socket::ListeningSocketSource;
use smithay::wayland::xwayland_shell::{XWaylandShellHandler, XWaylandShellState};

use demiurge_core::config;

use crate::bar::Bar;
use crate::client::ClientState;
use crate::wm::Wm;

pub fn run() -> ExitCode {
    if let Ok(env_filter) = tracing_subscriber::EnvFilter::try_from_default_env() {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_writer(std::io::stderr)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter("info")
            .with_writer(std::io::stderr)
            .init();
    }

    match run_inner() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("demiurge-wl-tty fatal: {}", e);
            ExitCode::FAILURE
        }
    }
}

fn run_inner() -> Result<(), Box<dyn std::error::Error>> {
    // Config: same surface as the winit binary.
    let cfg = match config::Paths::init().and_then(|p| config::load(&p)) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("config load failed: {}; running with defaults", e);
            config::Config::default()
        }
    };

    // Calloop loop. Same shape as main.rs's: we'll insert the
    // wayland socket, libinput, udev, and DrmDeviceNotifier sources
    // here.
    let mut event_loop: calloop::EventLoop<TtyState> =
        calloop::EventLoop::try_new()?;
    let loop_handle = event_loop.handle();

    // Wayland display + state objects. Same Wm as the winit path so
    // tile/MRU/bar/session-lock/xwayland behavior is identical.
    let display: Display<Wm> = Display::new()?;
    let dh = display.handle();

    let compositor_state = CompositorState::new::<Wm>(&dh);
    let xdg_shell_state = XdgShellState::new::<Wm>(&dh);
    let shm_state = ShmState::new::<Wm>(&dh, Vec::new());
    let mut seat_state = smithay::input::SeatState::new();
    let seat = seat_state.new_wl_seat(&dh, "tty");
    let data_device_state = DataDeviceState::new::<Wm>(&dh);
    let output_manager_state = OutputManagerState::new_with_xdg_output::<Wm>(&dh);

    // Output is created on first DRM-device add (we don't know its
    // size until we pick a connector + mode). Pre-creating it with
    // dummy values would advertise a wl_output that doesn't match
    // reality. Hold a placeholder for the Wm constructor; the real
    // mode update happens in open_drm_device().
    let output = Output::new(
        "DRM-pending".to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "DEMIURGE".to_string(),
            model: "drm".to_string(),
        },
    );
    output.create_global::<Wm>(&dh);

    let session_lock_state =
        SessionLockManagerState::new::<Wm, _>(&dh, |_| true);
    let xwayland_shell_state = XWaylandShellState::new::<Wm>(&dh);

    let mut wm = Wm::new(
        compositor_state,
        xdg_shell_state,
        shm_state,
        seat_state,
        data_device_state,
        seat,
        output_manager_state,
        session_lock_state,
        output.clone(),
        xwayland_shell_state,
        &cfg,
    );

    // Add a keyboard to the seat with the configured xkb settings.
    let options_joined = if cfg.input.keyboard.options.is_empty() {
        None
    } else {
        Some(cfg.input.keyboard.options.join(","))
    };
    let xkb_config = XkbConfig {
        rules: "",
        model: "",
        layout: cfg.input.keyboard.layout.as_str(),
        variant: cfg.input.keyboard.variant.as_str(),
        options: options_joined,
    };
    let repeat_delay: i32 = if cfg.input.keyboard.repeat_delay == 0 {
        200
    } else {
        cfg.input.keyboard.repeat_delay as i32
    };
    let repeat_rate: i32 = if cfg.input.keyboard.repeat_rate == 0 {
        25
    } else {
        cfg.input.keyboard.repeat_rate as i32
    };
    let keyboard = wm
        .seat
        .add_keyboard(xkb_config, repeat_delay, repeat_rate)
        .map_err(|e| format!("add_keyboard: {}", e))?;

    // libseat session. Required before we can open /dev/dri/* or
    // /dev/input/* devices.
    let (session, session_notifier) = LibSeatSession::new()?;
    let seat_name = session.seat();
    tracing::info!(seat = %seat_name, "libseat session acquired");

    // Wayland socket. Same auto-pick path the winit backend uses.
    let socket = ListeningSocketSource::new_auto()?;
    let socket_name = socket.socket_name().to_os_string();
    tracing::info!(socket = %socket_name.to_string_lossy(), "wayland socket ready");
    let display_arc = Arc::new(std::sync::Mutex::new(display));
    let display_for_socket = Arc::clone(&display_arc);

    loop_handle.insert_source(socket, move |stream, _, _state| {
        let mut dh = display_for_socket.lock().unwrap().handle();
        if let Err(e) = dh.insert_client(stream, Arc::new(ClientState::default())) {
            tracing::warn!(error = %e, "insert_client failed");
        }
    })?;

    unsafe {
        std::env::set_var("WAYLAND_DISPLAY", &socket_name);
    }

    // Spawn XWayland (shared helper). On Ready, the closure stashes
    // the X11Wm onto state.wm.xwm so XwmHandler dispatch -- delegated
    // from TtyState to self.wm above -- can resolve it.
    let xwayland_dh = display_arc.lock().unwrap().handle();
    if let Err(e) = crate::xwm::spawn_xwayland(
        &xwayland_dh,
        loop_handle.clone(),
        |state: &mut TtyState, x11_wm| {
            state.wm.xwm = Some(x11_wm);
        },
    ) {
        tracing::warn!(error = %e, "spawn_xwayland");
    }

    // Run cfg.startup.commands now that the socket is exported. Same
    // helper the winit dev backend uses so behavior is identical
    // across both entry points -- the user's terminal / status apps
    // come up regardless of which compositor binary boots. Order
    // matters: run AFTER xwayland spawn so $DISPLAY is set when an
    // X11-only command in startup fires.
    crate::spawn_startup_commands(&cfg, &socket_name);

    // libinput context + backend. Devices fan into the seat keyboard
    // we configured above.
    let mut libinput_ctx = libinput::Libinput::new_with_udev(
        LibinputSessionInterface(session.clone()),
    );
    libinput_ctx
        .udev_assign_seat(&seat_name)
        .map_err(|()| "udev_assign_seat failed")?;
    let libinput_backend = LibinputInputBackend::new(libinput_ctx);
    let keyboard_for_input = keyboard.clone();
    loop_handle
        .insert_source(libinput_backend, move |event, _, state| {
            on_input_event(state, event, &keyboard_for_input);
        })
        .map_err(|e| format!("insert libinput backend: {}", e))?;

    // Bar (in-process compositor element) -- same as main.rs.
    let bar = Bar::new(&cfg).map_err(|e| format!("bar init: {}", e))?;

    // TtyState bundles everything the udev / drm callbacks mutate.
    // It's the analog of main.rs's local mut variables (state, bar,
    // dsr, backend) but in a struct so the calloop closures can
    // share access to the same data.
    let mut state = TtyState {
        wm,
        bar,
        cfg,
        session,
        seat_name: seat_name.clone(),
        dh: display_arc.lock().unwrap().handle(),
        loop_handle: loop_handle.clone(),
        devices: HashMap::new(),
        primary_gpu_path: None,
    };

    // Session events: VT switch, device-paused/resumed.
    loop_handle
        .insert_source(session_notifier, |event, _, state| {
            on_session_event(state, event);
        })
        .map_err(|e| format!("insert session_notifier: {}", e))?;

    // udev: enumerate existing DRM nodes + watch for hotplug.
    let udev = UdevBackend::new(&seat_name)?;
    // primary_gpu is the GPU /dev/dri/card<N> the kernel marks boot_vga.
    // If absent (some systems / VMs), fall through to the first device
    // udev fires Added for.
    state.primary_gpu_path = primary_gpu(&seat_name).ok().flatten();
    if let Some(p) = &state.primary_gpu_path {
        tracing::info!(primary = ?p, "primary GPU detected");
    }
    loop_handle
        .insert_source(udev, |event, _, state| {
            if let Err(e) = on_udev_event(state, event) {
                tracing::warn!(error = %e, "udev event handling failed");
            }
        })
        .map_err(|e| format!("insert udev backend: {}", e))?;

    // Main loop. Each iteration dispatches:
    //   - calloop sources (which drive everything: vblank, input,
    //     udev, wayland clients)
    //   - wayland protocol (display.dispatch_clients + flush)
    //
    // Render is event-driven: DrmDeviceNotifier fires on vblank and
    // schedules a render on the affected output. There's no polling
    // loop -- we sleep until something happens.
    let start_time = Instant::now();
    state.wm.bar_height = state.cfg.bar.height; // ensure arrange() honors height
    let _ = start_time;

    loop {
        // Dispatch wayland protocol + flush before sleeping in
        // calloop, so client commits become visible to the renderer.
        {
            let mut display = display_arc.lock().unwrap();
            display
                .dispatch_clients(&mut state.wm)
                .map_err(|e| format!("dispatch_clients: {}", e))?;
            display.flush_clients()?;
        }
        event_loop.dispatch(None, &mut state)?;

        // Quit action clears running; leave the session loop so the
        // compositor releases DRM master + the libseat session cleanly.
        if !state.wm.running {
            tracing::info!("quit requested; exiting tty session");
            break;
        }
    }
    Ok(())
}

// ---------- TtyState ----------

struct TtyState {
    wm: Wm,
    // Bar holds the GlesTexture cached across frames. Refreshed +
    // composited each frame via render::build_tty_elements.
    bar: Bar,
    cfg: config::Config,
    session: LibSeatSession,
    #[allow(dead_code)]
    seat_name: String,
    #[allow(dead_code)]
    dh: DisplayHandle,
    loop_handle: calloop::LoopHandle<'static, TtyState>,
    devices: HashMap<DrmNode, DeviceState>,
    primary_gpu_path: Option<PathBuf>,
}

struct DeviceState {
    // Held for proxy lifetime; smithay's DrmCompositor caches the
    // node + path internally but we keep them here for diagnostics
    // and future per-device routing.
    #[allow(dead_code)]
    drm_node: DrmNode,
    #[allow(dead_code)]
    drm_path: PathBuf,
    // GBM device for buffer allocation. AsFd-backed so smithay can
    // share it across the allocator + framebuffer exporter.
    #[allow(dead_code)]
    gbm: GbmDevice<DrmDeviceFd>,
    renderer: GlesRenderer,
    compositor: Option<DrmCompositor<
        GbmAllocator<DrmDeviceFd>,
        GbmFramebufferExporter<DrmDeviceFd>,
        (),
        DrmDeviceFd,
    >>,
    // Calloop registration token so we can drop the source on remove.
    _drm_token: calloop::RegistrationToken,
}

// ---------- XWayland handler delegation ----------
//
// Both XWaylandShellHandler (wayland-server side, surface-association
// callbacks) and XwmHandler (X11Wm event-source dispatch) need to be
// implemented on the loop's State type. For TTY that's TtyState.
// Both impls forward to self.wm, where the actual logic lives.

impl XWaylandShellHandler for TtyState {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        self.wm.xwayland_shell_state()
    }
}

// ---------- XwmHandler delegation ----------
//
// X11Wm's calloop event source dispatches XwmHandler callbacks
// against whatever state type the loop carries. The winit dev
// backend's loop carries Wm directly (which already implements
// XwmHandler in wm.rs). The TTY backend's loop carries TtyState,
// so we delegate every XwmHandler method through to self.wm. Boring
// boilerplate; the actual logic lives on Wm.

impl XwmHandler for TtyState {
    fn xwm_state(&mut self, xwm: XwmId) -> &mut X11Wm {
        self.wm.xwm_state(xwm)
    }
    fn new_window(&mut self, xwm: XwmId, window: X11Surface) {
        self.wm.new_window(xwm, window)
    }
    fn new_override_redirect_window(&mut self, xwm: XwmId, window: X11Surface) {
        self.wm.new_override_redirect_window(xwm, window)
    }
    fn map_window_request(&mut self, xwm: XwmId, window: X11Surface) {
        self.wm.map_window_request(xwm, window)
    }
    fn mapped_override_redirect_window(&mut self, xwm: XwmId, window: X11Surface) {
        self.wm.mapped_override_redirect_window(xwm, window)
    }
    fn unmapped_window(&mut self, xwm: XwmId, window: X11Surface) {
        self.wm.unmapped_window(xwm, window)
    }
    fn destroyed_window(&mut self, xwm: XwmId, window: X11Surface) {
        self.wm.destroyed_window(xwm, window)
    }
    fn configure_request(
        &mut self,
        xwm: XwmId,
        window: X11Surface,
        x: Option<i32>,
        y: Option<i32>,
        w: Option<u32>,
        h: Option<u32>,
        reorder: Option<Reorder>,
    ) {
        self.wm.configure_request(xwm, window, x, y, w, h, reorder)
    }
    fn configure_notify(
        &mut self,
        xwm: XwmId,
        window: X11Surface,
        geometry: Rectangle<i32, Logical>,
        above: Option<smithay::xwayland::xwm::X11Window>,
    ) {
        self.wm.configure_notify(xwm, window, geometry, above)
    }
    fn resize_request(
        &mut self,
        xwm: XwmId,
        window: X11Surface,
        button: u32,
        edge: ResizeEdge,
    ) {
        self.wm.resize_request(xwm, window, button, edge)
    }
    fn move_request(&mut self, xwm: XwmId, window: X11Surface, button: u32) {
        self.wm.move_request(xwm, window, button)
    }
}

// ---------- udev ----------

fn on_udev_event(state: &mut TtyState, event: UdevEvent) -> Result<(), String> {
    match event {
        UdevEvent::Added { device_id, path } => {
            tracing::info!(?device_id, ?path, "udev: drm device added");
            // Only program the primary GPU. Multi-GPU compositing
            // lands later (smithay's GpuManager handles import/export
            // of buffers across devices).
            if let Some(primary) = &state.primary_gpu_path {
                if &path != primary {
                    tracing::info!(?path, "skipping non-primary DRM device");
                    return Ok(());
                }
            }
            open_drm_device(state, device_id, path)?;
        }
        UdevEvent::Changed { device_id } => {
            tracing::info!(?device_id, "udev: drm device changed");
        }
        UdevEvent::Removed { device_id } => {
            tracing::info!(?device_id, "udev: drm device removed");
            if let Ok(node) = DrmNode::from_dev_id(device_id) {
                if let Some(removed) = state.devices.remove(&node) {
                    state.loop_handle.remove(removed._drm_token);
                }
            }
        }
    }
    Ok(())
}

fn open_drm_device(
    state: &mut TtyState,
    device_id: u64,
    path: PathBuf,
) -> Result<(), String> {
    let drm_node = DrmNode::from_dev_id(device_id)
        .map_err(|e| format!("DrmNode::from_dev_id: {}", e))?;

    // Open the device via libseat. The fd has the seat's privileged
    // access (no setuid needed in the compositor process).
    let raw_fd = state
        .session
        .open(&path, rustix::fs::OFlags::RDWR | rustix::fs::OFlags::CLOEXEC)
        .map_err(|e| format!("session.open {}: {}", path.display(), e))?;
    let device_fd = DrmDeviceFd::new(DeviceFd::from(raw_fd));

    // DrmDevice + notifier for vblank events.
    let (mut drm, drm_notifier) = DrmDevice::new(device_fd.clone(), false)
        .map_err(|e| format!("DrmDevice::new: {}", e))?;

    // GBM device on the same fd. Allocator backs the swap chain;
    // framebuffer exporter wraps GBM buffers as DRM framebuffers.
    let gbm = GbmDevice::new(device_fd.clone())
        .map_err(|e| format!("GbmDevice::new: {}", e))?;

    // EGL on top of GBM, then a GLES renderer.
    let egl_display = unsafe {
        EGLDisplay::new(gbm.clone())
            .map_err(|e| format!("EGLDisplay::new: {}", e))?
    };
    let egl_context = EGLContext::new(&egl_display)
        .map_err(|e| format!("EGLContext::new: {}", e))?;
    let renderer = unsafe {
        GlesRenderer::new(egl_context).map_err(|e| format!("GlesRenderer::new: {}", e))?
    };

    // Pick the first connected connector + its preferred mode.
    let (connector_handle, mode, encoder_handle) = pick_output(&drm)?;

    // Build the smithay Output to advertise. Mode size is in physical
    // pixels; we'll use it for the wm.monitors as well.
    let mode_w = mode.size().0 as i32;
    let mode_h = mode.size().1 as i32;
    let refresh = (mode.vrefresh() * 1000) as i32;
    let smithay_mode = OutputMode {
        size: (mode_w, mode_h).into(),
        refresh,
    };
    state.wm.output.set_preferred(smithay_mode);
    state.wm.output.change_current_state(
        Some(smithay_mode),
        Some(smithay::utils::Transform::Normal),
        Some(smithay::output::Scale::Integer(1)),
        Some((0, 0).into()),
    );
    state.wm.resize_primary_monitor(mode_w as u32, mode_h as u32);

    // Pick a CRTC compatible with the encoder. First match wins.
    let crtc_handle = drm
        .resource_handles()
        .map_err(|e| format!("resource_handles: {}", e))?
        .filter_crtcs(drm.get_encoder(encoder_handle).map_err(|e| format!("get_encoder: {}", e))?.possible_crtcs())
        .into_iter()
        .next()
        .ok_or_else(|| "no compatible CRTC".to_string())?;

    // Create the DRM surface for the chosen (CRTC, mode, connectors).
    let drm_surface = drm
        .create_surface(crtc_handle, mode, &[connector_handle])
        .map_err(|e| format!("create_surface: {}", e))?;

    // Build the DrmCompositor. It owns the swap chain + page-flip.
    let allocator = GbmAllocator::new(
        gbm.clone(),
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );
    let exporter = GbmFramebufferExporter::new(gbm.clone(), Some(drm_node));
    let color_formats = [Fourcc::Argb8888, Fourcc::Xrgb8888];
    let renderer_formats: Vec<_> = renderer
        .egl_context()
        .dmabuf_render_formats()
        .iter()
        .copied()
        .collect();
    let cursor_size = drm.cursor_size();

    let compositor = DrmCompositor::new(
        OutputModeSource::Auto(state.wm.output.clone()),
        drm_surface,
        None,
        allocator,
        exporter,
        color_formats,
        renderer_formats,
        cursor_size,
        Some(gbm.clone()),
    )
    .map_err(|e| format!("DrmCompositor::new: {}", e))?;

    // Insert the DrmDeviceNotifier into calloop. It fires DrmEvent::
    // VBlank on each page flip; we render the next frame in
    // response.
    let drm_token = state
        .loop_handle
        .insert_source(drm_notifier, move |event, metadata, state| {
            on_drm_event(state, drm_node, event, metadata);
        })
        .map_err(|e| format!("insert drm_notifier: {}", e))?;

    let device_state = DeviceState {
        drm_node,
        drm_path: path,
        gbm,
        renderer,
        compositor: Some(compositor),
        _drm_token: drm_token,
    };
    state.devices.insert(drm_node, device_state);

    // Kick the first frame. Subsequent frames are vblank-driven.
    let _ = render_device(state, drm_node);

    let _ = drm; // DrmDevice handle is consumed by create_surface's path; keep here for life
    Ok(())
}

fn pick_output(
    drm: &DrmDevice,
) -> Result<
    (
        drm::control::connector::Handle,
        drm::control::Mode,
        drm::control::encoder::Handle,
    ),
    String,
> {
    use drm::control::connector::State as ConnState;

    let resources = drm
        .resource_handles()
        .map_err(|e| format!("resource_handles: {}", e))?;
    for &connector_handle in resources.connectors() {
        let info = drm
            .get_connector(connector_handle, false)
            .map_err(|e| format!("get_connector: {}", e))?;
        if info.state() != ConnState::Connected {
            continue;
        }
        let mode = info
            .modes()
            .iter()
            .find(|m: &&drm::control::Mode| {
                m.mode_type().contains(drm::control::ModeTypeFlags::PREFERRED)
            })
            .or_else(|| info.modes().first())
            .copied()
            .ok_or_else(|| "connector has no modes".to_string())?;
        let encoder_handle = info
            .encoders()
            .first()
            .copied()
            .ok_or_else(|| "connector has no encoders".to_string())?;
        tracing::info!(
            connector = ?info.interface(),
            mode = ?mode.size(),
            "selected output",
        );
        return Ok((connector_handle, mode, encoder_handle));
    }
    Err("no connected outputs".into())
}

// ---------- DRM events ----------

fn on_drm_event(
    state: &mut TtyState,
    drm_node: DrmNode,
    event: DrmEvent,
    _metadata: &mut Option<smithay::backend::drm::DrmEventMetadata>,
) {
    match event {
        DrmEvent::VBlank(_crtc) => {
            // Page flip completed; render the next frame.
            if let Some(device) = state.devices.get_mut(&drm_node) {
                if let Some(compositor) = device.compositor.as_mut() {
                    // frame_submitted releases the previous frame's
                    // buffer back into the swap-chain pool. Required
                    // before render_frame can pick a buffer.
                    if let Err(e) = compositor.frame_submitted() {
                        tracing::warn!(error = ?e, "frame_submitted failed");
                    }
                }
            }
            let _ = render_device(state, drm_node);
        }
        DrmEvent::Error(e) => {
            tracing::error!(error = ?e, "drm error event");
        }
    }
}

// Render one frame for a single device. Delegates element-list
// construction to render::build_tty_elements so the bar + clients +
// lock-mode branch all match the winit dev backend exactly.
fn render_device(state: &mut TtyState, drm_node: DrmNode) -> Result<(), String> {
    let device = state
        .devices
        .get_mut(&drm_node)
        .ok_or_else(|| "device not registered".to_string())?;
    let compositor = device
        .compositor
        .as_mut()
        .ok_or_else(|| "compositor not initialized".to_string())?;
    let renderer = &mut device.renderer;

    let locked_mode = state.wm.lock_active || state.wm.pending_lock.is_some();

    let elements = crate::render::build_tty_elements(
        renderer,
        &mut state.bar,
        &state.wm,
        locked_mode,
    )?;

    let clear_color = if locked_mode {
        Color32F::new(0.0, 0.0, 0.0, 1.0)
    } else {
        Color32F::new(0.07, 0.07, 0.07, 1.0)
    };

    let render_result = compositor
        .render_frame::<GlesRenderer, crate::render::DemiurgeElement>(
            renderer,
            &elements,
            clear_color,
            FrameFlags::DEFAULT,
        )
        .map_err(|e| format!("render_frame: {:?}", e))?;

    // Confirm pending session lock now that a cleared frame has
    // gone through. Same hop the winit path does in main.rs.
    if locked_mode {
        if let Some(pending) = state.wm.pending_lock.take() {
            state.wm.lock_active = true;
            pending.lock();
            tracing::info!("session lock confirmed (tty)");
        }
    }

    // Always queue the page flip, even if render_result.is_empty.
    // Skipping when empty leaves the kernel framebuffer's last state
    // (often a blank or text-mode console) on screen indefinitely;
    // that's the "boots to black" behavior. Queueing an empty frame
    // commits the clear color and gives us something to look at, plus
    // wires the vblank loop so subsequent frames can land.
    compositor
        .queue_frame(())
        .map_err(|e| format!("queue_frame: {:?}", e))?;
    let _ = render_result;
    Ok(())
}

// ---------- input ----------

fn on_input_event(
    state: &mut TtyState,
    event: InputEvent<LibinputInputBackend>,
    keyboard: &smithay::input::keyboard::KeyboardHandle<Wm>,
) {
    match event {
        InputEvent::Keyboard { event } => {
            // Same keybind dispatch as the winit backend: intercept
            // bound presses as WM actions, forward everything else to
            // the focused client.
            let key_state = event.state();
            // Decide in the filter, dispatch after input() returns --
            // see the winit backend for why the action can't run inside
            // the closure (keyboard-state reentrancy).
            let action = keyboard.input::<demiurge_core::Action, _>(
                &mut state.wm,
                event.key_code(),
                key_state,
                0.into(),
                0,
                |wm, mods, handle| {
                    if key_state == KeyState::Pressed {
                        if let Some(action) =
                            crate::keys::match_binding(&wm.bindings, mods, &handle)
                        {
                            return FilterResult::Intercept(action);
                        }
                    }
                    FilterResult::Forward
                },
            );
            if let Some(action) = action {
                state.wm.handle_action(&action);
            }
        }
        InputEvent::DeviceAdded { device } => {
            tracing::info!(name = %device.name(), "input device added");
        }
        InputEvent::DeviceRemoved { device } => {
            tracing::info!(name = %device.name(), "input device removed");
        }
        _ => {}
    }
}

// ---------- session ----------

fn on_session_event(state: &mut TtyState, event: SessionEvent) {
    match event {
        SessionEvent::PauseSession => {
            tracing::info!("session paused (vt switched away)");
            // smithay's DrmDevice listens to its own session events
            // through the notifier; we don't need to drop master
            // manually. The compositor.surface() will refuse renders
            // until ActivateSession.
        }
        SessionEvent::ActivateSession => {
            tracing::info!("session activated (vt switched back)");
            // Re-arm: trigger a render on each device.
            let nodes: Vec<DrmNode> = state.devices.keys().copied().collect();
            for node in nodes {
                let _ = render_device(state, node);
            }
        }
    }
}

// ---------- libinput session interface ----------

struct LibinputSessionInterface(LibSeatSession);

impl libinput::LibinputInterface for LibinputSessionInterface {
    fn open_restricted(
        &mut self,
        path: &std::path::Path,
        flags: i32,
    ) -> Result<std::os::fd::OwnedFd, i32> {
        let oflags = rustix::fs::OFlags::from_bits_retain(flags as u32);
        self.0
            .open(path, oflags)
            .map_err(|err| err.as_errno().unwrap_or(libc::EIO))
    }

    fn close_restricted(&mut self, fd: std::os::fd::OwnedFd) {
        drop(fd);
    }
}
