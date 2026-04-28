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

use smithay::backend::input::{InputEvent, KeyboardKeyEvent};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::draw_render_elements;
use smithay::backend::renderer::{Color32F, Frame, Renderer};
use smithay::backend::winit::{self, WinitEvent};
use smithay::input::keyboard::FilterResult;
use smithay::reexports::wayland_server::Display;
use smithay::reexports::wayland_server::protocol::wl_surface;
use smithay::reexports::winit::platform::pump_events::PumpStatus;
use smithay::utils::{Rectangle, Transform};
use smithay::wayland::compositor::{
    CompositorState, SurfaceAttributes, TraversalAction, with_surface_tree_downward,
};
use smithay::wayland::selection::data_device::DataDeviceState;
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shm::ShmState;
use smithay::wayland::socket::ListeningSocketSource;

use demiurge_wl::client::ClientState;
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

    let mut state = Wm {
        compositor_state,
        xdg_shell_state,
        shm_state,
        seat_state,
        data_device_state,
        seat,
    };

    // Add a keyboard to the seat. 200/200 are the xkb repeat-rate
    // defaults (rate / delay); xkb keymap selection is a follow-on.
    let keyboard = state
        .seat
        .add_keyboard(Default::default(), 200, 200)
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

    // For convenience: spawn a test client if requested via env var.
    // `DEMIURGE_WL_AUTOSPAWN=foot` will fork a foot terminal once the
    // socket is up. Useful for "does it actually work" sanity runs.
    if let Ok(cmd) = std::env::var("DEMIURGE_WL_AUTOSPAWN") {
        unsafe {
            std::env::set_var("WAYLAND_DISPLAY", &socket_name);
        }
        match std::process::Command::new(&cmd).spawn() {
            Ok(child) => {
                tracing::info!(pid = child.id(), %cmd, "autospawned client");
            }
            Err(e) => {
                tracing::warn!(%cmd, error = %e, "autospawn failed");
            }
        }
    } else {
        unsafe {
            std::env::set_var("WAYLAND_DISPLAY", &socket_name);
        }
    }

    let start_time = std::time::Instant::now();

    // Main loop: dispatch winit events (input + lifecycle), render the
    // current frame, dispatch protocol, flush. The winit event pump is
    // not async-friendly; we drive it by polling once per iteration
    // and yielding the calloop loop with a short timeout so wayland
    // socket activity is still serviced promptly.
    loop {
        let pump = winit_loop.dispatch_new_events(|event| match event {
            WinitEvent::Resized { .. } => {}
            WinitEvent::Input(InputEvent::Keyboard { event }) => {
                // Forward every key to the focused surface. Keybind
                // dispatch (Action enum routing) lands when keys.rs is
                // ported from x11/src/keys.rs.
                keyboard.input::<(), _>(
                    &mut state,
                    event.key_code(),
                    event.state(),
                    0.into(),
                    0,
                    |_state, _modifiers, _handle| FilterResult::Forward,
                );
            }
            WinitEvent::Input(InputEvent::PointerMotionAbsolute { .. }) => {
                // Cheap "focus follows pointer enters first toplevel"
                // for v0. Real focus management lands with the
                // per-monitor model port from x11/src/wm.rs.
                if let Some(toplevel) = state
                    .xdg_shell_state
                    .toplevel_surfaces()
                    .iter()
                    .next()
                    .cloned()
                {
                    let surface = toplevel.wl_surface().clone();
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

        // Render frame. Clear to a dark gray (mirrors the bar bg
        // default) and composite every committed toplevel at (0, 0).
        // Real layout lands in the layout.rs port.
        let size = backend.window_size();
        let damage = Rectangle::from_size(size);
        {
            let (renderer, mut framebuffer) = backend
                .bind()
                .map_err(|e| format!("backend.bind: {}", e))?;

            let elements = state
                .xdg_shell_state
                .toplevel_surfaces()
                .iter()
                .flat_map(|toplevel| {
                    render_elements_from_surface_tree(
                        renderer,
                        toplevel.wl_surface(),
                        (0, 0),
                        1.0,
                        1.0,
                        Kind::Unspecified,
                    )
                })
                .collect::<Vec<WaylandSurfaceRenderElement<GlesRenderer>>>();

            let mut frame = renderer
                .render(&mut framebuffer, size, Transform::Flipped180)
                .map_err(|e| format!("render: {}", e))?;
            // 0x121212 -> the same bar.bg DEMIURGE-x11 ships by default.
            frame
                .clear(Color32F::new(0.07, 0.07, 0.07, 1.0), &[damage])
                .map_err(|e| format!("clear: {}", e))?;
            draw_render_elements(&mut frame, 1.0, &elements, &[damage])
                .map_err(|e| format!("draw_render_elements: {}", e))?;
            let _ = frame.finish().map_err(|e| format!("frame.finish: {}", e))?;

            // Tell every committed surface that we drew their content
            // so they can request the next frame.
            for toplevel in state.xdg_shell_state.toplevel_surfaces() {
                send_frames_surface_tree(
                    toplevel.wl_surface(),
                    start_time.elapsed().as_millis() as u32,
                );
            }
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
            .submit(Some(&[damage]))
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
