// demiurge-wl entry point.
//
// Boot sequence:
//   1. tracing-subscriber: log to stderr with env-filter (RUST_LOG).
//   2. Display<Wm>: smithay's per-protocol dispatch hub.
//   3. Wm: handlers + state for compositor/xdg-shell/shm/seat/data-device.
//   4. Bind a Wayland socket (auto-pick a free wayland-N name) so
//      clients can connect via WAYLAND_DISPLAY.
//   5. Winit backend: opens a host window inside the X11 session and
//      gives us a GLES2 renderer. This is the dev backend; the real
//      DRM/udev session lands in a follow-on slice.
//   6. Add a keyboard to the seat.
//   7. Loop: pump winit events (resize, input), render committed
//      toplevel surfaces, accept new clients, dispatch + flush.

use std::process::ExitCode;
use std::sync::Arc;

use smithay::backend::input::{InputEvent, KeyState, KeyboardKeyEvent};
use smithay::backend::renderer::Bind;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::element::texture::{TextureBuffer, TextureRenderElement};
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::utils::draw_render_elements;
use smithay::backend::renderer::{Color32F, Frame, Renderer};
use smithay::backend::winit::{self, WinitEvent};
use smithay::input::keyboard::{FilterResult, XkbConfig};
use smithay::reexports::wayland_server::Display;
use smithay::reexports::wayland_server::protocol::wl_surface;
use smithay::reexports::winit::platform::pump_events::PumpStatus;
use smithay::utils::{Point, Rectangle, Size, Transform};
use smithay::output::{Mode as OutputMode, Output, PhysicalProperties, Subpixel};
use smithay::wayland::compositor::{
    CompositorState, SurfaceAttributes, TraversalAction, with_surface_tree_downward,
};
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::selection::data_device::DataDeviceState;
use smithay::wayland::session_lock::SessionLockManagerState;
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shm::ShmState;
use smithay::wayland::socket::ListeningSocketSource;
use smithay::wayland::xwayland_shell::XWaylandShellState;

use demiurge_core::config;
use demiurge_wl::bar::Bar;
use demiurge_wl::client::ClientState;
use demiurge_wl::dsr::Dsr;
use demiurge_wl::wm::Wm;

fn main() -> ExitCode {
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

    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("fatal: {}", e);
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Load config (best-effort -- demiurge-wl has its own minimal
    // surface for v0; the [[display]] section is the only block we
    // currently consume). On parse failure we fall back to defaults
    // so the compositor still boots.
    let cfg = match config::Paths::init().and_then(|p| config::load(&p)) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("config load failed: {}; running with defaults", e);
            config::Config::default()
        }
    };
    // First [[display]] entry's multiplier drives DSR. demiurge-wl is
    // single-output (winit); when the DRM backend lands the per-output
    // match pattern from x11/src/display.rs ports cleanly.
    let dsr_multiplier = cfg
        .displays
        .first()
        .map(|d| d.dsr_multiplier)
        .unwrap_or(1.0);
    let mut dsr = Dsr::new(dsr_multiplier);
    if dsr.active() {
        tracing::info!("[dsr] enabled, multiplier {}", dsr.multiplier);
    }

    // Bar: in-process compositor element. Renders bg + tag strip +
    // clock at the top of each output. The first GlesTexture upload
    // happens lazily on the first frame (no allocations until winit
    // is up and a renderer is bound). bar.height == 0 disables the
    // bar entirely -- the render path checks for that and skips the
    // upload + composite to keep Cairo from rejecting a 0-tall
    // surface.
    let mut bar = Bar::new(&cfg).map_err(|e| format!("bar init: {}", e))?;
    let bar_enabled = bar.height > 0;

    // Display + state.
    let display: Display<Wm> = Display::new()?;
    let dh = display.handle();

    let compositor_state = CompositorState::new::<Wm>(&dh);
    let xdg_shell_state = XdgShellState::new::<Wm>(&dh);
    // Default supported SHM formats (Argb8888 + Xrgb8888). Clients use
    // wl_shm for software rendering; native GL / dmabuf clients
    // bypass this path entirely.
    let shm_state = ShmState::new::<Wm>(&dh, Vec::new());
    let mut seat_state = smithay::input::SeatState::new();
    // Single seat named "winit". Multi-seat support is a real-session
    // concern (libinput devices fan into seats); v0 has one.
    let seat = seat_state.new_wl_seat(&dh, "winit");
    let data_device_state = DataDeviceState::new::<Wm>(&dh);

    // wl_output for the winit window. Required for clients that bind
    // surfaces to outputs (notably ext-session-lock-v1 lock surfaces).
    // Single output for the dev backend; the multi-output story lands
    // with the DRM backend slice when wl_output enumeration follows
    // udev events.
    let output_manager_state = OutputManagerState::new_with_xdg_output::<Wm>(&dh);
    let output = Output::new(
        "winit-0".to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "DEMIURGE".to_string(),
            model: "winit".to_string(),
        },
    );
    output.create_global::<Wm>(&dh);
    // Initial mode: 1280x800 matches winit::init's default window
    // size (smithay/winit/mod.rs::init -> with_inner_size 1280x800).
    // The first WinitEvent::Resized refines this with the real size.
    let initial_mode = OutputMode {
        size: (1280, 800).into(),
        refresh: 60_000,
    };
    output.set_preferred(initial_mode);
    output.change_current_state(
        Some(initial_mode),
        Some(smithay::utils::Transform::Normal),
        Some(smithay::output::Scale::Integer(1)),
        Some((0, 0).into()),
    );

    // session-lock-v1 manager. Filter |_| true accepts every client;
    // restricting which clients can issue ext_session_lock_manager_v1
    // is a downstream policy decision we don't enforce yet.
    let session_lock_state =
        SessionLockManagerState::new::<Wm, _>(&dh, |_| true);

    // xwayland-shell global. Lets XWayland associate its wl_surfaces
    // with X11 windows by serial. The actual XWayland process is
    // spawned later via XWayland::spawn so it can be inserted into
    // the calloop loop.
    let xwayland_shell_state = XWaylandShellState::new::<Wm>(&dh);

    let mut state = Wm::new(
        compositor_state,
        xdg_shell_state,
        shm_state,
        seat_state,
        data_device_state,
        seat,
        output_manager_state,
        session_lock_state,
        output,
        xwayland_shell_state,
        &cfg,
    );

    // Add a keyboard to the seat.
    //
    // [input.keyboard] feeds smithay's XkbConfig directly. Empty
    // strings / empty vec mean "leave the xkbcommon defaults in
    // place" -- xkbcommon falls back to the XKB_DEFAULT_* env vars
    // (or system defaults) for any field left blank. This matches
    // the X11 path's "0 / empty = don't override" semantics.
    //
    // Options is a comma-separated list per xkbcommon's RMLVO; we
    // join the Vec<String> with "," and pass None when empty so
    // xkbcommon falls back to XKB_DEFAULT_OPTIONS.
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

    // X11 / xset semantics: 0 means "leave server default in place".
    // smithay needs concrete numbers, so substitute its documented
    // defaults (200ms delay, 25 chars/sec rate) when the user opted
    // out. Non-zero values pass through as-is.
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

    let keyboard = state
        .seat
        .add_keyboard(xkb_config, repeat_delay, repeat_rate)
        .map_err(|e| format!("add_keyboard: {}", e))?;

    // Bind a Wayland socket. ListeningSocketSource auto-picks a free
    // wayland-N name and exports it as the env var WAYLAND_DISPLAY for
    // any process the compositor spawns.
    let socket = ListeningSocketSource::new_auto()?;
    let socket_name = socket.socket_name().to_os_string();
    tracing::info!(socket = %socket_name.to_string_lossy(), "wayland socket ready");

    // Winit backend (nested host-window dev mode). GLES2 renderer.
    let (mut backend, mut winit_loop) = winit::init::<GlesRenderer>()?;
    tracing::info!("winit backend up");

    // Calloop loop. Smithay accepts client connections + dispatches
    // protocol via the socket source; winit drives input + render via
    // its own pump_events handle below.
    let mut event_loop: calloop::EventLoop<Wm> = calloop::EventLoop::try_new()?;
    let loop_handle = event_loop.handle();

    let display_arc = Arc::new(std::sync::Mutex::new(display));
    let display_for_socket = Arc::clone(&display_arc);

    loop_handle.insert_source(socket, move |stream, _, _state| {
        let mut dh = display_for_socket.lock().unwrap().handle();
        if let Err(e) = dh.insert_client(stream, Arc::new(ClientState::default())) {
            tracing::warn!(error = %e, "insert_client failed");
        }
    })?;

    // Spawn XWayland (shared helper; same wiring used by the TTY
    // backend). On Ready, set $DISPLAY and stash the X11Wm on the Wm
    // so XwmHandler callbacks resolve.
    let xwayland_dh = display_arc.lock().unwrap().handle();
    if let Err(e) = demiurge_wl::xwm::spawn_xwayland(
        &xwayland_dh,
        loop_handle.clone(),
        |state: &mut Wm, wm| {
            state.xwm = Some(wm);
        },
    ) {
        tracing::warn!(error = %e, "spawn_xwayland");
    }

    // Export WAYLAND_DISPLAY for any child we spawn next. SAFETY:
    // we're still single-threaded at this point (winit + calloop
    // haven't started their pumps); no concurrent reader can race.
    unsafe {
        std::env::set_var("WAYLAND_DISPLAY", &socket_name);
    }

    // Auto-spawn convenience: DEMIURGE_WL_AUTOSPAWN=foot forks one
    // client by name. Useful for "does it actually work" smoke runs
    // without touching config.
    if let Ok(cmd) = std::env::var("DEMIURGE_WL_AUTOSPAWN") {
        match std::process::Command::new(&cmd).spawn() {
            Ok(child) => tracing::info!(pid = child.id(), %cmd, "autospawned client"),
            Err(e) => tracing::warn!(%cmd, error = %e, "autospawn failed"),
        }
    }

    // Run cfg.startup.commands. Same shape as the X11 path's
    // spawn::spawn loop; lives in lib.rs so the TTY backend can
    // call into the same helper.
    demiurge_wl::spawn_startup_commands(&cfg, &socket_name);

    let start_time = std::time::Instant::now();

    // Main loop: dispatch winit events (input + lifecycle), render the
    // current frame, dispatch protocol, flush. The winit event pump is
    // not async-friendly; we drive it by polling once per iteration
    // and yielding the calloop loop with a short timeout so wayland
    // socket activity is still serviced promptly.
    loop {
        let pump = winit_loop.dispatch_new_events(|event| match event {
            WinitEvent::Resized { size, .. } => {
                // Push the new dimensions into the Wm's monitor model
                // and re-arrange. Wm::resize_primary_monitor calls
                // arrange() so any tiled clients get an updated
                // configure for the new work area. Also update the
                // wl_output's advertised mode so clients that track
                // output dimensions see the change.
                state.resize_primary_monitor(size.w as u32, size.h as u32);
                let mode = OutputMode {
                    size,
                    refresh: 60_000,
                };
                state.output.change_current_state(
                    Some(mode),
                    None,
                    None,
                    None,
                );
                state.output.set_preferred(mode);
            }
            WinitEvent::Input(InputEvent::Keyboard { event }) => {
                // Match each press against the compiled keybinds. A hit
                // is intercepted (consumed as a WM action and not
                // delivered to the client); everything else forwards to
                // the focused surface. Releases always forward -- we
                // only act on the pressing edge.
                let key_state = event.state();
                // The filter only DECIDES (intercept vs forward) and
                // bubbles the matched Action out as the Intercept
                // payload; we dispatch it AFTER input() returns. Acting
                // inside the closure would reenter the keyboard state
                // input() is mid-borrow on (handle_action can call
                // set_focus), so the action must run outside.
                let action = keyboard.input::<demiurge_core::Action, _>(
                    &mut state,
                    event.key_code(),
                    key_state,
                    0.into(),
                    0,
                    |wm, mods, handle| {
                        if key_state == KeyState::Pressed {
                            if let Some(action) =
                                demiurge_wl::keys::match_binding(&wm.bindings, mods, &handle)
                            {
                                return FilterResult::Intercept(action);
                            }
                        }
                        FilterResult::Forward
                    },
                );
                if let Some(action) = action {
                    state.handle_action(&action);
                }
            }
            WinitEvent::Input(InputEvent::PointerMotionAbsolute { .. }) => {
                // Cheap "focus follows pointer enters first managed
                // client" for v0. Restricted to the first client on a
                // visible tag so the pointer shim can't park keyboard
                // focus on a hidden-tag window (which would misdirect
                // the focus-relative keybind actions). Real
                // pointer-aware focus targeting lands with cursor.rs.
                if let Some(surface) = state
                    .clients
                    .iter()
                    .find(|c| state.is_visible(c))
                    .map(|c| c.toplevel.wl_surface().clone())
                {
                    keyboard.set_focus(&mut state, Some(surface), 0.into());
                }
            }
            _ => {}
        });

        match pump {
            PumpStatus::Continue => {}
            PumpStatus::Exit(_) => {
                tracing::info!("winit event loop exited");
                return Ok(());
            }
        }

        // Quit action (Super+Shift+Q etc.) clears running; break the
        // loop and shut the compositor down cleanly.
        if !state.running {
            tracing::info!("quit requested");
            return Ok(());
        }

        // Bar is hidden when a fullscreen client owns the active tag,
        // so it doesn't clip the fullscreen window. Recomputed each
        // frame since fullscreen state changes via keybind.
        let show_bar = bar_enabled && !state.active_tag_fullscreen();

        // Render frame. Three paths:
        //   - Locked (lock_active OR pending_lock): clear screen,
        //     render only ext_session_lock_surface_v1 surfaces. No
        //     bar, no clients. Takes precedence over DSR. Required
        //     by ext-session-lock-v1: while locked, the compositor
        //     MUST hide the desktop. After the cleared frame is
        //     drawn, confirm any pending_lock so the locker client
        //     receives the `locked` event.
        //   - DSR off (multiplier == 1.0): direct render into the
        //     winit framebuffer at native size. Original v0.6 path.
        //   - DSR on  (multiplier  > 1.0): render into an offscreen
        //     GLES texture at native_size * multiplier, then a second
        //     pass downsamples that texture into the winit framebuffer.
        //     Bilinear via GL_LINEAR sampling; Lanczos-via-compute is
        //     a follow-on quality upgrade with the same external
        //     interface.
        let native_size = backend.window_size();
        let native_damage = Rectangle::from_size(native_size);

        let locked_mode = state.lock_active || state.pending_lock.is_some();

        if locked_mode {
            // Lock branch: clear + render lock surfaces only. We bind
            // and render at native size regardless of DSR multiplier
            // (lock surfaces fill the output and we want them crisp,
            // not downscaled).
            let (renderer, mut framebuffer) = backend
                .bind()
                .map_err(|e| format!("backend.bind: {}", e))?;

            let lock_elements = state
                .lock_surfaces
                .iter()
                .flat_map(|(s, _)| {
                    render_elements_from_surface_tree(
                        renderer,
                        s.wl_surface(),
                        (0, 0),
                        1.0,
                        1.0,
                        Kind::Unspecified,
                    )
                })
                .collect::<Vec<WaylandSurfaceRenderElement<GlesRenderer>>>();

            let mut frame = renderer
                .render(&mut framebuffer, native_size, Transform::Flipped180)
                .map_err(|e| format!("render lock: {}", e))?;
            frame
                .clear(Color32F::new(0.0, 0.0, 0.0, 1.0), &[native_damage])
                .map_err(|e| format!("clear lock: {}", e))?;
            draw_render_elements(&mut frame, 1.0, &lock_elements, &[native_damage])
                .map_err(|e| format!("draw lock surfaces: {}", e))?;
            let _ = frame.finish().map_err(|e| format!("frame.finish lock: {}", e))?;

            // Confirm pending lock now that a fully-cleared frame has
            // been drawn. SessionLocker::lock takes self by value;
            // taking the Option here also drops the failure-on-Drop
            // path (which would have sent `finished` to the client).
            if let Some(pending) = state.pending_lock.take() {
                state.lock_active = true;
                pending.lock();
                tracing::info!("session lock confirmed");
            }
        } else if dsr.active() {
            let multiplier = dsr.multiplier;
            let (rw, rh) = dsr.render_size(native_size.w, native_size.h);
            let render_size: Size<i32, _> = Size::from((rw, rh));
            let render_damage = Rectangle::from_size(render_size);

            // Pass 1: render clients into the high-res offscreen
            // target. The output_scale arg to
            // render_elements_from_surface_tree + draw_render_elements
            // is the multiplier, which makes a buffer surface render
            // at multiplier-size in the offscreen.
            {
                let renderer = backend.renderer();
                let offscreen = dsr
                    .ensure_offscreen(renderer, native_size.w, native_size.h)
                    .map_err(|e| format!("dsr ensure_offscreen: {}", e))?;
                let mut hi_fb = renderer
                    .bind(offscreen)
                    .map_err(|e| format!("bind offscreen: {}", e))?;

                let mut elements = state
                    .clients
                    .iter()
                    .filter(|c| state.is_visible(c))
                    .flat_map(|c| {
                        render_elements_from_surface_tree(
                            renderer,
                            c.toplevel.wl_surface(),
                            (c.x, c.y),
                            1.0,
                            multiplier as f32,
                            Kind::Unspecified,
                        )
                    })
                    .collect::<Vec<WaylandSurfaceRenderElement<GlesRenderer>>>();
                for x11 in &state.x11_surfaces {
                    if let Some(wl_surface) = x11.wl_surface() {
                        if !x11.is_mapped() {
                            continue;
                        }
                        let geo = x11.geometry();
                        elements.extend(render_elements_from_surface_tree(
                            renderer,
                            &wl_surface,
                            (geo.loc.x, geo.loc.y),
                            1.0,
                            multiplier as f32,
                            Kind::Unspecified,
                        ));
                    }
                }

                let mut frame = renderer
                    .render(&mut hi_fb, render_size, Transform::Flipped180)
                    .map_err(|e| format!("render hi: {}", e))?;
                frame
                    .clear(Color32F::new(0.07, 0.07, 0.07, 1.0), &[render_damage])
                    .map_err(|e| format!("clear hi: {}", e))?;
                draw_render_elements(&mut frame, multiplier, &elements, &[render_damage])
                    .map_err(|e| format!("draw hi: {}", e))?;
                let _ = frame.finish().map_err(|e| format!("finish hi: {}", e))?;
            }

            // Pass 2: downscale the offscreen texture into the winit
            // framebuffer. We wrap the texture in a TextureBuffer
            // (smithay's static-texture render-element source) and
            // ask it to render at native_size; the GLES sampler
            // performs bilinear downscale during sampling. The bar
            // joins this pass at native resolution so it stays crisp
            // regardless of the DSR multiplier.
            let offscreen_tex = dsr
                .offscreen_clone()
                .ok_or_else(|| "dsr: offscreen missing after pass 1".to_string())?;
            let (renderer, mut native_fb) = backend
                .bind()
                .map_err(|e| format!("backend.bind: {}", e))?;

            // Bar texture (refresh + upload) before we start building
            // render elements; same import_memory borrow constraint as
            // the DSR-off path. Skipped when bar_enabled is false.
            let bar_tex = if show_bar {
                let mon_w = state.monitors.first().width;
                let active_tag = state.active_tags[state.focused_monitor];
                let occupied = state.occupied_tags();
                Some(
                    bar.texture(renderer, mon_w, active_tag, &occupied)
                        .map_err(|e| format!("bar texture: {}", e))?
                        .clone(),
                )
            } else {
                None
            };

            let texbuf = TextureBuffer::from_texture(
                renderer,
                offscreen_tex,
                1,
                Transform::Normal,
                None,
            );
            let downscale_elem: TextureRenderElement<smithay::backend::renderer::gles::GlesTexture> =
                TextureRenderElement::from_texture_buffer(
                    Point::from((0.0, 0.0)),
                    &texbuf,
                    None,
                    None,
                    Some(Size::from((native_size.w, native_size.h))),
                    Kind::Unspecified,
                );

            let mut elements: Vec<TextureRenderElement<GlesTexture>> = vec![downscale_elem];
            if let Some(tex) = bar_tex {
                let bar_buf =
                    TextureBuffer::from_texture(renderer, tex, 1, Transform::Normal, None);
                let mon_origin = state.monitors.first();
                elements.push(TextureRenderElement::from_texture_buffer(
                    Point::from((mon_origin.x as f64, mon_origin.y as f64)),
                    &bar_buf,
                    None,
                    None,
                    None,
                    Kind::Unspecified,
                ));
            }

            let mut frame = renderer
                .render(&mut native_fb, native_size, Transform::Flipped180)
                .map_err(|e| format!("render native: {}", e))?;
            frame
                .clear(Color32F::new(0.0, 0.0, 0.0, 1.0), &[native_damage])
                .map_err(|e| format!("clear native: {}", e))?;
            draw_render_elements::<GlesRenderer, f64, TextureRenderElement<smithay::backend::renderer::gles::GlesTexture>>(
                &mut frame,
                1.0,
                &elements,
                &[native_damage],
            )
            .map_err(|e| format!("draw native: {}", e))?;
            let _ = frame
                .finish()
                .map_err(|e| format!("finish native: {}", e))?;
        } else {
            // DSR off: original direct-render path. The bar lives in
            // this pass too (renders directly into the native
            // framebuffer at 1:1 -- no downscale stage to traverse).
            let (renderer, mut framebuffer) = backend
                .bind()
                .map_err(|e| format!("backend.bind: {}", e))?;

            // Render (or refresh) the bar texture before we build the
            // client element list. import_memory / update_memory both
            // borrow the renderer mutably, so we sequence them ahead
            // of the surface-tree iteration. Skipped entirely when
            // bar_enabled is false (height == 0 in config).
            let bar_tex = if show_bar {
                let mon_w = state.monitors.first().width;
                let active_tag = state.active_tags[state.focused_monitor];
                let occupied = state.occupied_tags();
                Some(
                    bar.texture(renderer, mon_w, active_tag, &occupied)
                        .map_err(|e| format!("bar texture: {}", e))?
                        .clone(),
                )
            } else {
                None
            };

            // Wayland xdg toplevels: positioned via arrange().
            let mut client_elements = state
                .clients
                .iter()
                .filter(|c| state.is_visible(c))
                .flat_map(|c| {
                    render_elements_from_surface_tree(
                        renderer,
                        c.toplevel.wl_surface(),
                        (c.x, c.y),
                        1.0,
                        1.0,
                        Kind::Unspecified,
                    )
                })
                .collect::<Vec<WaylandSurfaceRenderElement<GlesRenderer>>>();

            // X11 surfaces: each X11Surface holds its own geometry
            // (the X11 client picks position via ConfigureWindow).
            // Once xwayland_shell associates a wl_surface, we can
            // render it. Pre-association, wl_surface() returns None
            // and we skip.
            for x11 in &state.x11_surfaces {
                if let Some(wl_surface) = x11.wl_surface() {
                    if !x11.is_mapped() {
                        continue;
                    }
                    let geo = x11.geometry();
                    client_elements.extend(render_elements_from_surface_tree(
                        renderer,
                        &wl_surface,
                        (geo.loc.x, geo.loc.y),
                        1.0,
                        1.0,
                        Kind::Unspecified,
                    ));
                }
            }

            // Bar render element. When bar_enabled is false, we use
            // an empty Vec so the same draw_render_elements call can
            // run without an extra branch.
            let bar_elems: Vec<TextureRenderElement<GlesTexture>> = if let Some(tex) = bar_tex {
                let bar_buf =
                    TextureBuffer::from_texture(renderer, tex, 1, Transform::Normal, None);
                let mon_origin = state.monitors.first();
                vec![TextureRenderElement::from_texture_buffer(
                    Point::from((mon_origin.x as f64, mon_origin.y as f64)),
                    &bar_buf,
                    None,
                    None,
                    None,
                    Kind::Unspecified,
                )]
            } else {
                Vec::new()
            };

            let mut frame = renderer
                .render(&mut framebuffer, native_size, Transform::Flipped180)
                .map_err(|e| format!("render: {}", e))?;
            frame
                .clear(Color32F::new(0.07, 0.07, 0.07, 1.0), &[native_damage])
                .map_err(|e| format!("clear: {}", e))?;
            draw_render_elements(&mut frame, 1.0, &client_elements, &[native_damage])
                .map_err(|e| format!("draw_render_elements clients: {}", e))?;
            // Bar drawn AFTER clients so it overlays them. With
            // bar_height reserved from work_area in arrange(), the
            // common case has nothing under the bar to overlap; this
            // matters for the Floating layout where clients can sit
            // anywhere on screen.
            if !bar_elems.is_empty() {
                draw_render_elements::<GlesRenderer, f64, TextureRenderElement<GlesTexture>>(
                    &mut frame,
                    1.0,
                    &bar_elems,
                    &[native_damage],
                )
                .map_err(|e| format!("draw_render_elements bar: {}", e))?;
            }
            let _ = frame.finish().map_err(|e| format!("frame.finish: {}", e))?;
        }

        // Tell every committed surface that we drew their content
        // so they can request the next frame. Common to both paths.
        for c in state.clients.iter() {
            send_frames_surface_tree(
                c.toplevel.wl_surface(),
                start_time.elapsed().as_millis() as u32,
            );
        }

        // Dispatch wayland protocol + flush, then run the calloop
        // pass with a 16ms budget (≈60Hz) so the socket source picks
        // up new clients promptly.
        {
            let mut display = display_arc.lock().unwrap();
            display
                .dispatch_clients(&mut state)
                .map_err(|e| format!("dispatch_clients: {}", e))?;
            display.flush_clients()?;
        }

        event_loop.dispatch(Some(std::time::Duration::from_millis(16)), &mut state)?;

        backend
            .submit(Some(&[native_damage]))
            .map_err(|e| format!("backend.submit: {}", e))?;
    }
}

// Walk the surface tree and tell each surface "frame done at $time" so
// the client knows it can send the next frame. Smithay's example from
// the `minimal` reference compositor; verbatim because there's nothing
// DEMIURGE-specific to add here yet.
pub fn send_frames_surface_tree(surface: &wl_surface::WlSurface, time: u32) {
    with_surface_tree_downward(
        surface,
        (),
        |_, _, &()| TraversalAction::DoChildren(()),
        |_surf, states, &()| {
            for callback in states
                .cached_state
                .get::<SurfaceAttributes>()
                .current()
                .frame_callbacks
                .drain(..)
            {
                callback.done(time);
            }
        },
        |_, _, &()| true,
    );
}
