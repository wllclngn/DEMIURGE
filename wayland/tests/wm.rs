// The headless Wayland Wm: Tag views, layouts, quit, focus-relative
// actions and a real xdg_toplevel moved between tags.

use smithay::input::SeatState;
use smithay::output::Output;
use smithay::wayland::compositor::CompositorState;
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::selection::data_device::DataDeviceState;
use smithay::wayland::session_lock::SessionLockManagerState;
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shm::ShmState;
use smithay::wayland::xwayland_shell::XWaylandShellState;

use demiurge_core::{Action, Layout};
use demiurge_wl::wm::Wm;
use demiurge_core::config::Config;
use smithay::input::keyboard::XkbConfig;
use smithay::output::{PhysicalProperties, Subpixel};
use smithay::reexports::wayland_server::Display;

// Client-side imports for the in-process toplevel-mapping test. The
// dev-dependency wayland-client drives a real xdg_toplevel against
// the headless Wm so the focus / tag / visibility actions can be
// exercised on a genuinely managed client.
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_compositor, wl_registry, wl_surface};
use wayland_client::{Connection as ClientConnection, Dispatch, QueueHandle};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

use demiurge_wl::client::ClientState;

// Minimal client state. We only need to answer xdg_wm_base pings and
// ack xdg_surface configures to keep the connection healthy; nothing
// observes client-side state.
struct Client;
impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Client {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &ClientConnection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<wl_compositor::WlCompositor, ()> for Client {
    fn event(
        _: &mut Self,
        _: &wl_compositor::WlCompositor,
        _: wl_compositor::Event,
        _: &(),
        _: &ClientConnection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<wl_surface::WlSurface, ()> for Client {
    fn event(
        _: &mut Self,
        _: &wl_surface::WlSurface,
        _: wl_surface::Event,
        _: &(),
        _: &ClientConnection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<xdg_wm_base::XdgWmBase, ()> for Client {
    fn event(
        _: &mut Self,
        base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &ClientConnection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}
impl Dispatch<xdg_surface::XdgSurface, ()> for Client {
    fn event(
        _: &mut Self,
        surf: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &ClientConnection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surf.ack_configure(serial);
        }
    }
}
impl Dispatch<xdg_toplevel::XdgToplevel, ()> for Client {
    fn event(
        _: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        _: xdg_toplevel::Event,
        _: &(),
        _: &ClientConnection,
        _: &QueueHandle<Self>,
    ) {
    }
}

// Build a headless Wm: every protocol state object constructs from
// a DisplayHandle alone (no winit / GL / DRM), so the tiling-model
// actions can be exercised directly. The Display is returned so the
// caller keeps it alive for the Wm's lifetime (the state objects
// hold globals registered against it).
fn headless_wm() -> (Display<Wm>, Wm) {
    let display: Display<Wm> = Display::new().expect("display");
    let dh = display.handle();
    let compositor_state = CompositorState::new::<Wm>(&dh);
    let xdg_shell_state = XdgShellState::new::<Wm>(&dh);
    let shm_state = ShmState::new::<Wm>(&dh, Vec::new());
    let mut seat_state = SeatState::new();
    let seat = seat_state.new_wl_seat(&dh, "test");
    let data_device_state = DataDeviceState::new::<Wm>(&dh);
    let output_manager_state = OutputManagerState::new_with_xdg_output::<Wm>(&dh);
    let session_lock_state = SessionLockManagerState::new::<Wm, _>(&dh, |_| true);
    let output = Output::new(
        "test-0".to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "DEMIURGE".to_string(),
            model: "test".to_string(),
        },
    );
    let xwayland_shell_state = XWaylandShellState::new::<Wm>(&dh);
    let cfg = Config::default();
    let mut wm = Wm::new(
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
    // refocus_active_tag calls set_keyboard_focus, which needs a
    // keyboard on the seat.
    wm.seat
        .add_keyboard(XkbConfig::default(), 200, 25)
        .expect("add_keyboard");
    // Give it a real monitor so the active-tag / work-area math has
    // non-degenerate geometry.
    wm.resize_primary_monitor(1280, 800);
    (display, wm)
}

#[test]
fn default_config_has_three_tags() {
    let (_d, wm) = headless_wm();
    assert_eq!(wm.num_tags, 3);
    assert_eq!(wm.active_tags, vec![0]);
    assert_eq!(wm.layouts.len(), 3);
    assert!(wm.running);
}

#[test]
fn view_tag_switches_active_tag() {
    let (_d, mut wm) = headless_wm();
    wm.handle_action(&Action::ViewTag(2));
    assert_eq!(wm.active_tags[0], 2);
}

#[test]
fn view_tag_out_of_range_is_ignored() {
    let (_d, mut wm) = headless_wm();
    wm.handle_action(&Action::ViewTag(99));
    assert_eq!(wm.active_tags[0], 0);
}

#[test]
fn view_next_and_prev_tag_wrap() {
    let (_d, mut wm) = headless_wm();
    wm.handle_action(&Action::ViewNextTag);
    assert_eq!(wm.active_tags[0], 1);
    // From tag 0, prev wraps to the last tag (num_tags - 1).
    let (_d2, mut wm2) = headless_wm();
    wm2.handle_action(&Action::ViewPrevTag);
    assert_eq!(wm2.active_tags[0], 2);
}

#[test]
fn toggle_layout_cycles_active_tag() {
    let (_d, mut wm) = headless_wm();
    // Default layout is Floating.
    assert_eq!(wm.layouts[0], Layout::Floating);
    wm.handle_action(&Action::ToggleLayout);
    assert_eq!(wm.layouts[0], Layout::Tile);
    wm.handle_action(&Action::ToggleLayout);
    assert_eq!(wm.layouts[0], Layout::Monocle);
    wm.handle_action(&Action::ToggleLayout);
    assert_eq!(wm.layouts[0], Layout::Floating);
    // Cycling tag 0's layout must not touch the other tags.
    assert_eq!(wm.layouts[1], Layout::Floating);
}

#[test]
fn quit_clears_running() {
    let (_d, mut wm) = headless_wm();
    assert!(wm.running);
    wm.handle_action(&Action::Quit);
    assert!(!wm.running);
}

#[test]
fn focus_relative_actions_noop_without_focus() {
    // With no clients/focus, the focus-relative actions must be
    // harmless no-ops (not panic, not mutate tag state).
    let (_d, mut wm) = headless_wm();
    wm.handle_action(&Action::MoveToTag(1));
    wm.handle_action(&Action::ToggleFullscreen);
    wm.handle_action(&Action::ToggleAbove);
    wm.handle_action(&Action::CloseWindow);
    wm.handle_action(&Action::MruNext);
    assert_eq!(wm.active_tags[0], 0);
    assert!(wm.clients.is_empty());
    assert!(wm.focused.is_none());
}

// Drive a real xdg_toplevel against the headless Wm (client on a
// worker thread, server pumped on this thread), then exercise the
// focus-relative tag actions on the genuinely managed client.
#[test]
fn move_to_tag_hides_client_and_refocus_follows() {
    let (mut display, mut wm) = headless_wm();
    let mut dh = display.handle();

    let (server_end, client_end) = UnixStream::pair().expect("socketpair");
    dh.insert_client(server_end, std::sync::Arc::new(ClientState::default()))
        .expect("insert_client");

    let (ready_tx, ready_rx) = mpsc::channel::<()>();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let worker = std::thread::spawn(move || {
        let conn = ClientConnection::from_socket(client_end).expect("client conn");
        // registry_queue_init round-trips; the server pump on the
        // main thread answers it.
        let (globals, queue) = registry_queue_init::<Client>(&conn).expect("registry init");
        let qh = queue.handle();
        let compositor: wl_compositor::WlCompositor =
            globals.bind(&qh, 1..=6, ()).expect("bind wl_compositor");
        let wm_base: xdg_wm_base::XdgWmBase =
            globals.bind(&qh, 1..=6, ()).expect("bind xdg_wm_base");
        let surface = compositor.create_surface(&qh, ());
        let xsurf = wm_base.get_xdg_surface(&surface, &qh, ());
        let toplevel = xsurf.get_toplevel(&qh, ());
        surface.commit();
        let _ = conn.flush();
        ready_tx.send(()).ok();
        // Park, keeping the protocol objects alive so the toplevel
        // isn't destroyed mid-test.
        let _ = stop_rx.recv();
        drop((toplevel, xsurf, surface, wm_base, compositor, queue, conn));
    });

    // Pump the server until the toplevel maps (new_toplevel pushes a
    // ManagedClient on the get_toplevel request).
    let deadline = Instant::now() + Duration::from_secs(5);
    while wm.clients.is_empty() {
        let _ = display.dispatch_clients(&mut wm);
        let _ = display.flush_clients();
        if Instant::now() >= deadline {
            stop_tx.send(()).ok();
            worker.join().ok();
            panic!("toplevel never mapped");
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let _ = ready_rx.recv_timeout(Duration::from_secs(1));

    // Mapped on the focused monitor's active tag (0) and focused.
    assert_eq!(wm.clients.len(), 1);
    let id = wm.clients[0].id;
    assert_eq!(wm.clients[0].tag, 0);
    assert_eq!(wm.focused, Some(id));
    assert!(wm.is_visible(&wm.clients[0]));

    // Move it to tag 1 (not visible): it hides and focus clears.
    wm.handle_action(&Action::MoveToTag(1));
    assert_eq!(wm.clients[0].tag, 1);
    assert!(!wm.is_visible(&wm.clients[0]));
    assert!(wm.focused.is_none());

    // View tag 1: the client becomes visible and regains focus.
    wm.handle_action(&Action::ViewTag(1));
    assert!(wm.is_visible(&wm.clients[0]));
    assert_eq!(wm.focused, Some(id));

    stop_tx.send(()).ok();
    worker.join().ok();
}
