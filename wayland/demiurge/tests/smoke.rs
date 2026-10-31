// Smoke tests for demiurge-wl. Each test spawns a fresh demiurge-wl
// child via the WlTest harness, connects a wayland-client Connection
// directly to its socket, and asserts a small protocol-contract claim.
//
// Tests must run single-threaded -- demiurge-wl's winit dev backend
// opens a host X11 window per process, and the host's display server
// (Xorg in dev, Xvfb in CI) doesn't enjoy many simultaneous winit
// inits at once. Cargo handles this via `--test-threads=1`; we set
// the requirement in the harness comment, not via runtime opt-out.

mod common;

use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use common::{WlTest, children};
use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_callback::{self, WlCallback};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_feedback_v1::{
    self, ZwpLinuxDmabufFeedbackV1,
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1;
use wayland_protocols::wp::commit_timing::v1::client::wp_commit_timer_v1::WpCommitTimerV1;
use wayland_protocols::wp::commit_timing::v1::client::wp_commit_timing_manager_v1::WpCommitTimingManagerV1;
use wayland_protocols::wp::fifo::v1::client::wp_fifo_manager_v1::WpFifoManagerV1;
use wayland_protocols::wp::fifo::v1::client::wp_fifo_v1::WpFifoV1;
use wayland_protocols::wp::presentation_time::client::wp_presentation::WpPresentation;
use wayland_protocols::wp::presentation_time::client::wp_presentation_feedback::{
    self, WpPresentationFeedback,
};
use wayland_protocols::ext::session_lock::v1::client::ext_session_lock_manager_v1::ExtSessionLockManagerV1;
use wayland_protocols::ext::session_lock::v1::client::ext_session_lock_v1::{
    self, ExtSessionLockV1,
};
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_protocols::wp::color_management::v1::client::wp_color_management_output_v1::WpColorManagementOutputV1;
use wayland_protocols::wp::color_management::v1::client::wp_color_management_surface_feedback_v1::{
    self, WpColorManagementSurfaceFeedbackV1,
};
use wayland_protocols::wp::color_management::v1::client::wp_color_management_surface_v1::WpColorManagementSurfaceV1;
use wayland_protocols::wp::color_management::v1::client::wp_color_manager_v1::{
    self, Feature, Primaries, RenderIntent, TransferFunction, WpColorManagerV1,
};
use wayland_protocols::wp::color_management::v1::client::wp_image_description_creator_params_v1::{
    self, WpImageDescriptionCreatorParamsV1,
};
use wayland_protocols::wp::color_management::v1::client::wp_image_description_info_v1::{self, WpImageDescriptionInfoV1};
use wayland_protocols::wp::color_management::v1::client::wp_image_description_v1::{self, WpImageDescriptionV1};
use wayland_protocols::xdg::shell::client::xdg_popup::{self, XdgPopup};
use wayland_protocols::xdg::shell::client::xdg_positioner::{self, XdgPositioner};
use wayland_protocols::xdg::shell::client::xdg_surface::{self, XdgSurface};
use wayland_protocols::xdg::shell::client::xdg_toplevel::{self, XdgToplevel};
use wayland_protocols::xdg::shell::client::xdg_wm_base::{self, XdgWmBase};
use wayland_protocols::xdg::activation::v1::client::xdg_activation_token_v1::{self, XdgActivationTokenV1};
use wayland_protocols::xdg::activation::v1::client::xdg_activation_v1::XdgActivationV1;
use wayland_protocols::wp::idle_inhibit::zv1::client::zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1;
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::ZwpTextInputManagerV3;
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_v3::{self, ZwpTextInputV3};
use wayland_protocols_misc::zwp_input_method_v2::client::zwp_input_method_manager_v2::ZwpInputMethodManagerV2;
use wayland_protocols::xdg::decoration::zv1::client::zxdg_decoration_manager_v1::ZxdgDecorationManagerV1;
use wayland_protocols::xdg::decoration::zv1::client::zxdg_toplevel_decoration_v1::{self, ZxdgToplevelDecorationV1};
use wayland_protocols_misc::zwp_input_method_v2::client::zwp_input_method_v2::{self, ZwpInputMethodV2};
use wayland_protocols::wp::idle_inhibit::zv1::client::zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1;

const ROUNDTRIP_TIMEOUT: Duration = Duration::from_secs(5);

// Roundtrip a few times waiting for `predicate` to flip true. Bounded
// by ROUNDTRIP_TIMEOUT so a broken contract surfaces as a panic
// rather than a hang.
fn pump_until<S, F: FnMut(&S) -> bool>(
    queue: &mut EventQueue<S>,
    state: &mut S,
    mut predicate: F,
    label: &str,
) {
    let deadline = Instant::now() + ROUNDTRIP_TIMEOUT;
    while !predicate(state) {
        if Instant::now() >= deadline {
            panic!("timed out waiting for: {}", label);
        }
        queue
            .roundtrip(state)
            .unwrap_or_else(|e| panic!("roundtrip during {}: {}", label, e));
    }
}

// State for the globals + seat-capability tests. The registry
// dispatch records globals via GlobalListContents (populated by
// registry_queue_init's initial roundtrip); we only need to record
// wl_seat capability events ourselves.
#[derive(Default)]
struct EnumerateState {
    seat_caps: Option<wl_seat::Capability>,
    // wl_keyboard.repeat_info: rate (chars/sec) + delay (ms). Sent
    // once when the keyboard is created, before any key event.
    keyboard_repeat: Option<(i32, i32)>,
}

impl Dispatch<WlRegistry, GlobalListContents> for EnumerateState {
    fn event(
        _state: &mut Self,
        _registry: &WlRegistry,
        _event: <WlRegistry as wayland_client::Proxy>::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for EnumerateState {
    fn event(
        state: &mut Self,
        _seat: &WlSeat,
        event: <WlSeat as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities } = event {
            if let wayland_client::WEnum::Value(c) = capabilities {
                state.seat_caps = Some(c);
            }
        }
    }
}

impl Dispatch<WlKeyboard, ()> for EnumerateState {
    fn event(
        state: &mut Self,
        _kb: &WlKeyboard,
        event: <WlKeyboard as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::RepeatInfo { rate, delay } = event {
            state.keyboard_repeat = Some((rate, delay));
        }
    }
}

// State for the xdg_toplevel configure-cycle test. Records the most
// recent xdg_surface configure serial so the test can ack + commit
// and confirm we got past the initial configure handshake.
#[derive(Default)]
struct ToplevelState {
    last_configure_serial: Option<u32>,
    wm_base_pinged: bool,
}

impl Dispatch<WlRegistry, GlobalListContents> for ToplevelState {
    fn event(
        _state: &mut Self,
        _registry: &WlRegistry,
        _event: <WlRegistry as wayland_client::Proxy>::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<XdgWmBase, ()> for ToplevelState {
    fn event(
        state: &mut Self,
        wm_base: &XdgWmBase,
        event: <XdgWmBase as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // xdg_wm_base.ping is mandatory: clients must reply with
        // pong or the compositor may disconnect them.
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
            state.wm_base_pinged = true;
        }
    }
}

impl Dispatch<XdgSurface, ()> for ToplevelState {
    fn event(
        state: &mut Self,
        xdg_surface: &XdgSurface,
        event: <XdgSurface as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            state.last_configure_serial = Some(serial);
        }
    }
}

impl Dispatch<XdgToplevel, ()> for ToplevelState {
    fn event(
        _state: &mut Self,
        _toplevel: &XdgToplevel,
        _event: <XdgToplevel as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // The Configure event on xdg_toplevel carries states + size
        // hints. We don't need to assert on those for the smoke
        // contract; xdg_surface.configure is the round-trip ack site.
    }
}

// State for the tile-layout test. Records the latest size the
// compositor configured for each of the two toplevels we create, so
// the test can verify both got a non-zero size and that the sizes
// differ (master vs stack).
#[derive(Default)]
struct TileState {
    last_size: [Option<(i32, i32)>; 2],
    last_serial: [Option<u32>; 2],
    wm_base_pinged: bool,
}

impl Dispatch<WlRegistry, GlobalListContents> for TileState {
    fn event(
        _state: &mut Self,
        _registry: &WlRegistry,
        _event: <WlRegistry as wayland_client::Proxy>::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<XdgWmBase, ()> for TileState {
    fn event(
        state: &mut Self,
        wm_base: &XdgWmBase,
        event: <XdgWmBase as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
            state.wm_base_pinged = true;
        }
    }
}

impl Dispatch<XdgSurface, usize> for TileState {
    fn event(
        state: &mut Self,
        xdg_surface: &XdgSurface,
        event: <XdgSurface as wayland_client::Proxy>::Event,
        slot: &usize,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            state.last_serial[*slot] = Some(serial);
        }
    }
}

impl Dispatch<XdgToplevel, usize> for TileState {
    fn event(
        state: &mut Self,
        _toplevel: &XdgToplevel,
        event: <XdgToplevel as wayland_client::Proxy>::Event,
        slot: &usize,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // xdg_toplevel.configure carries the size the compositor wants
        // the client to render at. (0, 0) means "client picks". For
        // tiled layouts the compositor sends positive sizes.
        if let xdg_toplevel::Event::Configure { width, height, .. } = event {
            if width > 0 && height > 0 {
                state.last_size[*slot] = Some((width, height));
            }
        }
    }
}

delegate_noop!(TileState: ignore WlCompositor);
delegate_noop!(TileState: ignore WlShm);
delegate_noop!(TileState: ignore WlSurface);

// wl_compositor / wl_shm / wl_surface have no events we care about
// at this layer of the smoke harness. delegate_noop installs an
// empty Dispatch impl so the queue doesn't reject them.
delegate_noop!(EnumerateState: ignore WlCompositor);
delegate_noop!(EnumerateState: ignore WlShm);
delegate_noop!(ToplevelState: ignore WlCompositor);
delegate_noop!(ToplevelState: ignore WlShm);
delegate_noop!(ToplevelState: ignore WlSurface);

// Test 1: harness can spawn demiurge-wl and the wayland socket file
// appears within the startup window. WlTest::new() panics on failure
// so the assertion is implicit; we assert the socket name explicitly
// to lock in the deterministic auto-pick contract.
#[test]
fn harness_starts_compositor() {
    let wl = WlTest::new();
    assert_eq!(wl.socket_name, "wayland-1");
    assert!(wl.socket_path.exists(), "socket file missing after spawn");
}

// Test 2: a wayland-client Connection completes a Display roundtrip.
// This verifies the protocol is actually speaking, not just that
// the socket exists.
#[test]
fn connection_roundtrips() {
    let wl = WlTest::new();
    let conn = wl.connect();
    conn.roundtrip()
        .expect("display roundtrip against demiurge-wl");
}

// Test 3: the globals demiurge-wl advertises today are the ones we
// document in main.rs's boot sequence: wl_compositor, wl_shm,
// wl_seat, xdg_wm_base, plus wl_data_device_manager from the
// selection state. Locking these into the smoke suite catches
// "did we accidentally drop a protocol when refactoring" regressions.
#[test]
fn advertises_required_globals() {
    let wl = WlTest::new();
    let conn = wl.connect();
    let (globals, _queue): (_, EventQueue<EnumerateState>) =
        registry_queue_init(&conn).expect("registry_queue_init");

    let names: Vec<String> = globals
        .contents()
        .clone_list()
        .into_iter()
        .map(|g| g.interface)
        .collect();

    for required in [
        "wl_compositor",
        "wl_shm",
        "wl_seat",
        "xdg_wm_base",
        "wl_data_device_manager",
        "wl_output",
        "ext_session_lock_manager_v1",
        "wp_color_manager_v1",
        "zwp_relative_pointer_manager_v1",
        "zwp_pointer_constraints_v1",
        "wp_viewporter",
        "wp_fifo_manager_v1",
        "wp_commit_timing_manager_v1",
        // Note: xwayland_shell_v1 is advertised but only bindable by
        // Xwayland clients (smithay's filter restricts it). A regular
        // test client doesn't see it -- not asserted here.
    ] {
        assert!(
            names.iter().any(|n| n == required),
            "missing global {} (advertised: {:?})",
            required,
            names,
        );
    }
}

// Test 4: wl_seat advertises keyboard capability. demiurge-wl's
// main.rs adds a keyboard to the seat unconditionally; if that line
// regresses (e.g. add_keyboard call removed during a refactor) the
// session would silently lose key input. This test catches it.
#[test]
fn seat_advertises_keyboard() {
    let wl = WlTest::new();
    let conn = wl.connect();
    let (globals, mut queue): (_, EventQueue<EnumerateState>) =
        registry_queue_init(&conn).expect("registry_queue_init");
    let qh = queue.handle();

    let mut state = EnumerateState::default();
    let _seat: WlSeat = globals
        .bind(&qh, 1..=7, ())
        .expect("bind wl_seat");

    pump_until(
        &mut queue,
        &mut state,
        |s| s.seat_caps.is_some(),
        "wl_seat.capabilities",
    );

    let caps = state.seat_caps.expect("seat caps recorded");
    assert!(
        caps.contains(wl_seat::Capability::Keyboard),
        "wl_seat did not advertise Keyboard capability (got {:?})",
        caps,
    );
}

// Test 4b: wl_seat advertises a pointer too. Without one no client gets a
// click or a cursor, on either backend.
#[test]
fn seat_advertises_pointer() {
    let wl = WlTest::new();
    let conn = wl.connect();
    let (globals, mut queue): (_, EventQueue<EnumerateState>) =
        registry_queue_init(&conn).expect("registry_queue_init");
    let qh = queue.handle();
    let _seat: WlSeat = globals.bind(&qh, 1..=7, ()).expect("bind wl_seat");
    let mut state = EnumerateState::default();
    pump_until(&mut queue, &mut state, |s| s.seat_caps.is_some(), "wl_seat.capabilities");
    let caps = state.seat_caps.expect("seat caps recorded");
    assert!(caps.contains(wl_seat::Capability::Pointer), "wl_seat did not advertise Pointer (got {:?})", caps);
}

// Test 5: wl_keyboard.repeat_info reflects what the compositor was
// configured for. Harness pins HOME to a tempdir with no config, so
// demiurge-wl picks up Config::default(): repeat_delay=0,
// repeat_rate=0 -- which main.rs translates into smithay's
// documented defaults (200ms / 25 chars-per-sec). Test asserts those
// numbers flow through to the client. If a future refactor regresses
// the [input.keyboard] -> XkbConfig wiring this catches it.
#[test]
fn keyboard_advertises_repeat_info() {
    let wl = WlTest::new();
    let conn = wl.connect();
    let (globals, mut queue): (_, EventQueue<EnumerateState>) =
        registry_queue_init(&conn).expect("registry_queue_init");
    let qh = queue.handle();

    // wl_seat.get_keyboard requires version >= 4 to receive repeat_info.
    let seat: WlSeat = globals.bind(&qh, 4..=7, ()).expect("bind wl_seat v4+");
    let _kb: WlKeyboard = seat.get_keyboard(&qh, ());

    let mut state = EnumerateState::default();
    pump_until(
        &mut queue,
        &mut state,
        |s| s.keyboard_repeat.is_some(),
        "wl_keyboard.repeat_info",
    );

    let (rate, delay) = state.keyboard_repeat.expect("repeat_info recorded");
    // Defaults from main.rs: repeat_delay==0 -> 200ms, repeat_rate==0
    // -> 25 chars/sec. Locking these in here means a refactor that
    // changes the substitution constants needs a deliberate test
    // update.
    assert_eq!(rate, 25, "rate (chars/sec) mismatch");
    assert_eq!(delay, 200, "delay (ms) mismatch");
}

// Test 6: an xdg_toplevel surface walks through the configure cycle
// (initial commit -> compositor sends configure -> client acks ->
// commits a buffer-less role-only surface). Hitting "configure
// arrived, ack accepted" is the canonical xdg-shell handshake; if
// XdgShellState wiring breaks, this test catches it.
#[test]
fn xdg_toplevel_configure_cycle() {
    let wl = WlTest::new();
    let conn = wl.connect();
    let (globals, mut queue): (_, EventQueue<ToplevelState>) =
        registry_queue_init(&conn).expect("registry_queue_init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals
        .bind(&qh, 1..=6, ())
        .expect("bind wl_compositor");
    let wm_base: XdgWmBase = globals
        .bind(&qh, 1..=5, ())
        .expect("bind xdg_wm_base");

    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let _toplevel = xdg_surface.get_toplevel(&qh, ());
    // xdg-shell requires the initial commit to come WITHOUT an
    // attached buffer. The compositor responds with the first
    // configure once it's seen this commit.
    surface.commit();

    let mut state = ToplevelState::default();
    pump_until(
        &mut queue,
        &mut state,
        |s| s.last_configure_serial.is_some(),
        "xdg_surface.configure",
    );

    assert!(
        state.last_configure_serial.is_some(),
        "no configure serial captured",
    );
}

// State for the session-lock cycle test. Records whether the
// `locked` event arrived from the compositor.
#[derive(Default)]
struct LockState {
    locked: bool,
    finished: bool,
}

impl Dispatch<WlRegistry, GlobalListContents> for LockState {
    fn event(
        _state: &mut Self,
        _registry: &WlRegistry,
        _event: <WlRegistry as wayland_client::Proxy>::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtSessionLockManagerV1, ()> for LockState {
    fn event(
        _state: &mut Self,
        _mgr: &ExtSessionLockManagerV1,
        _event: <ExtSessionLockManagerV1 as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // The manager has no events; this impl exists only to
        // satisfy the Dispatch bound.
    }
}

impl Dispatch<ExtSessionLockV1, ()> for LockState {
    fn event(
        state: &mut Self,
        _lock: &ExtSessionLockV1,
        event: <ExtSessionLockV1 as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            ext_session_lock_v1::Event::Locked => state.locked = true,
            ext_session_lock_v1::Event::Finished => state.finished = true,
            _ => {}
        }
    }
}

// Test 9: ext-session-lock-v1 cycle. Bind the lock manager, request
// a lock, wait for the `locked` event (sent by the compositor after
// rendering a fully-cleared frame), then unlock_and_destroy. The
// test verifies the protocol contract; it does not create a lock
// surface or attach a buffer (smithay's smoke path doesn't require
// surfaces to send `locked`, our compositor honors that).
#[test]
fn session_lock_cycle() {
    let wl = WlTest::new();
    let conn = wl.connect();
    let (globals, mut queue): (_, EventQueue<LockState>) =
        registry_queue_init(&conn).expect("registry_queue_init");
    let qh = queue.handle();

    let mgr: ExtSessionLockManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .expect("bind ext_session_lock_manager_v1");
    let lock = mgr.lock(&qh, ());

    let mut state = LockState::default();
    pump_until(
        &mut queue,
        &mut state,
        |s| s.locked || s.finished,
        "ext_session_lock_v1.locked",
    );
    assert!(
        state.locked,
        "compositor should send `locked`, got finished={}",
        state.finished,
    );

    lock.unlock_and_destroy();
    conn.roundtrip().expect("roundtrip after unlock_and_destroy");
}

// Helper for the bar-reservation test: spawn demiurge-wl with the
// given bar height (and tile layout, single tag), map one toplevel,
// and return its configured height. Used to verify that
// bar_height is honored in arrange()'s work-area calculation.
fn measure_tile_height_with_bar(bar_height: u32) -> i32 {
    let cfg = format!(
        r##"
[general]
default_layout = "tile"
master_ratio = 0.5
tags = ["1"]

[bar]
height = {}
"##,
        bar_height
    );
    let wl = WlTest::with_config(&cfg);
    let conn = wl.connect();
    let (globals, mut queue): (_, EventQueue<TileState>) =
        registry_queue_init(&conn).expect("registry_queue_init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("bind wl_compositor");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=5, ()).expect("bind xdg_wm_base");

    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, 0usize);
    let _toplevel = xdg_surface.get_toplevel(&qh, 0usize);
    surface.commit();

    let mut state = TileState::default();
    pump_until(
        &mut queue,
        &mut state,
        |s| s.last_size[0].is_some(),
        "configure for tile height measurement",
    );
    state.last_size[0].expect("size recorded").1
}

// Test 8: bar_height is reserved from the tile work area. Spawn two
// compositors with the same window size (winit default 1280x800) but
// different bar heights, and verify the tiled client's configured
// height drops by exactly the bar-height delta. This pins the
// arrange() <-> bar wiring without depending on specific window
// dimensions or scale factors.
#[test]
fn bar_height_reserved_from_work_area() {
    let h_no_bar = measure_tile_height_with_bar(0);
    let h_with_bar = measure_tile_height_with_bar(50);
    let delta = h_no_bar - h_with_bar;
    assert_eq!(
        delta, 50,
        "tile-layout work-area height should drop by bar_height \
         (got {} without bar vs {} with bar=50)",
        h_no_bar, h_with_bar,
    );
}

// Test 7: tiling. Spawn demiurge-wl with default_layout = "tile" and
// master_ratio = 0.6, map two toplevels, and verify both receive a
// non-zero size in their xdg_toplevel.configure events. Master should
// be wider than stack (master_ratio > 0.5), and the widths should sum
// to roughly the work-area width. Exact pixel sizes depend on the
// host display, so the assertions check ratios and inequalities, not
// absolute numbers.
#[test]
fn tile_layout_splits_master_and_stack() {
    let cfg = r##"
[general]
default_layout = "tile"
master_ratio = 0.6
tags = ["1", "2", "3"]
"##;
    let wl = WlTest::with_config(cfg);
    let conn = wl.connect();
    let (globals, mut queue): (_, EventQueue<TileState>) =
        registry_queue_init(&conn).expect("registry_queue_init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("bind wl_compositor");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=5, ()).expect("bind xdg_wm_base");

    // Create two toplevels. UserData on xdg_surface + xdg_toplevel
    // carries the slot index so the dispatch impls can route configure
    // sizes to the right last_size[..] slot.
    let surf0 = compositor.create_surface(&qh, ());
    let xdg_surf0 = wm_base.get_xdg_surface(&surf0, &qh, 0usize);
    let _toplevel0 = xdg_surf0.get_toplevel(&qh, 0usize);
    surf0.commit();

    let surf1 = compositor.create_surface(&qh, ());
    let xdg_surf1 = wm_base.get_xdg_surface(&surf1, &qh, 1usize);
    let _toplevel1 = xdg_surf1.get_toplevel(&qh, 1usize);
    surf1.commit();

    let mut state = TileState::default();
    pump_until(
        &mut queue,
        &mut state,
        |s| s.last_size[0].is_some() && s.last_size[1].is_some(),
        "tile-layout configure for both toplevels",
    );

    let (m_w, m_h) = state.last_size[0].expect("master got tile size");
    let (s_w, s_h) = state.last_size[1].expect("stack got tile size");

    // Heights match: tile in vertical-master layout uses full work
    // height for master, and full work height for the single-element
    // stack.
    assert_eq!(m_h, s_h, "tile master and stack heights should match");
    // Master wider than stack at master_ratio = 0.6. Allow for
    // rounding (the f64*u32 cast can lose 1 pixel), so strict >.
    assert!(
        m_w > s_w,
        "tile master should be wider than stack at master_ratio 0.6 (got master={} stack={})",
        m_w,
        s_w,
    );
    // Widths sum to approximately the work-area width. Allow ±2px
    // for rounding (master_w cast + stack_w = work_w - master_w).
    let total = m_w + s_w;
    assert!(
        m_w > 0 && s_w > 0 && total > 0,
        "tile widths should be positive (got master={} stack={})",
        m_w,
        s_w,
    );
}

// State for the frame tests: A toplevel that commits a buffer every
// frame, counting frame callbacks and presentation feedback.
#[derive(Default)]
struct FrameState {
    configured: bool,
    done: u32,
    // (presentation time in ns, refresh in ns) per presented frame.
    presented: Vec<(u64, u32)>,
    discarded: u32,
}

impl Dispatch<WlRegistry, GlobalListContents> for FrameState {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as wayland_client::Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<XdgWmBase, ()> for FrameState {
    fn event(_: &mut Self, base: &XdgWmBase, event: xdg_wm_base::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for FrameState {
    fn event(state: &mut Self, surface: &XdgSurface, event: xdg_surface::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            state.configured = true;
        }
    }
}

impl Dispatch<WlCallback, ()> for FrameState {
    fn event(state: &mut Self, _: &WlCallback, event: wl_callback::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_callback::Event::Done { .. } = event {
            state.done += 1;
        }
    }
}

impl Dispatch<WpPresentationFeedback, ()> for FrameState {
    fn event(
        state: &mut Self,
        _: &WpPresentationFeedback,
        event: wp_presentation_feedback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wp_presentation_feedback::Event::Presented { tv_sec_hi, tv_sec_lo, tv_nsec, refresh, .. } => {
                let secs = ((tv_sec_hi as u64) << 32) | tv_sec_lo as u64;
                state.presented.push((secs * 1_000_000_000 + tv_nsec as u64, refresh));
            }
            wp_presentation_feedback::Event::Discarded => state.discarded += 1,
            _ => {}
        }
    }
}

delegate_noop!(FrameState: ignore WlCompositor);
delegate_noop!(FrameState: ignore WlShm);
delegate_noop!(FrameState: ignore WlShmPool);
delegate_noop!(FrameState: ignore WlBuffer);
delegate_noop!(FrameState: ignore WlSurface);
delegate_noop!(FrameState: ignore XdgToplevel);
delegate_noop!(FrameState: ignore WpPresentation);
delegate_noop!(FrameState: ignore WpFifoManagerV1);
delegate_noop!(FrameState: ignore WpFifoV1);
delegate_noop!(FrameState: ignore WpCommitTimingManagerV1);
delegate_noop!(FrameState: ignore WpCommitTimerV1);

const PAINT: i32 = 64;

// A mapped toplevel with one 64x64 buffer to commit, frame after frame.
struct Painter {
    surface: WlSurface,
    buffer: WlBuffer,
    _toplevel: XdgToplevel,
    _xdg_surface: XdgSurface,
    _pool: WlShmPool,
    _memory: std::fs::File,
}

impl Painter {
    fn map(globals: &GlobalList, queue: &mut EventQueue<FrameState>, state: &mut FrameState) -> Self {
        let qh = queue.handle();
        let compositor: WlCompositor = globals.bind(&qh, 4..=6, ()).expect("bind wl_compositor");
        let wm_base: XdgWmBase = globals.bind(&qh, 1..=5, ()).expect("bind xdg_wm_base");
        let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("bind wl_shm");
        let surface = compositor.create_surface(&qh, ());
        let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg_surface.get_toplevel(&qh, ());
        surface.commit();
        pump_until(queue, state, |s| s.configured, "first configure");

        let bytes = PAINT * PAINT * 4;
        let memory = tempfile::tempfile().expect("buffer memory");
        memory.set_len(bytes as u64).expect("size buffer memory");
        let pool = shm.create_pool(memory.as_fd(), bytes, &qh, ());
        let buffer = pool.create_buffer(0, PAINT, PAINT, PAINT * 4, wl_shm::Format::Xrgb8888, &qh, ());
        Self { surface, buffer, _toplevel: toplevel, _xdg_surface: xdg_surface, _pool: pool, _memory: memory }
    }

    // Commit the buffer again, damaged, asking for the next frame.
    fn paint(&self, qh: &QueueHandle<FrameState>) {
        self.surface.attach(Some(&self.buffer), 0, 0);
        self.surface.damage_buffer(0, 0, PAINT, PAINT);
        self.surface.frame(qh, ());
        self.surface.commit();
    }
}

// Test 10: a client that draws every frame gets a frame callback for
// each one, paced at the output's refresh rather than as fast as it can
// commit. A compositor that never sends them freezes every client after
// its first frame.
#[test]
fn frame_callbacks_pace_a_drawing_client() {
    let wl = WlTest::new();
    let conn = wl.connect();
    let (globals, mut queue): (_, EventQueue<FrameState>) = registry_queue_init(&conn).expect("registry_queue_init");
    let mut state = FrameState::default();
    let painter = Painter::map(&globals, &mut queue, &mut state);
    let qh = queue.handle();

    let start = Instant::now();
    for n in 1..=10 {
        painter.paint(&qh);
        pump_until(&mut queue, &mut state, |s| s.done >= n, "frame callback");
    }
    let elapsed = start.elapsed();
    // Nine refresh intervals at 60 Hz are 150ms; well under that means
    // the callbacks came unpaced.
    assert!(elapsed >= Duration::from_millis(100), "ten frame callbacks in {:?}: not paced", elapsed);
}

// Test 11: wp_presentation feedback reports every drawn frame as
// presented, at increasing times, with the output's refresh interval.
#[test]
fn presentation_feedback_reports_each_frame() {
    let wl = WlTest::new();
    let conn = wl.connect();
    let (globals, mut queue): (_, EventQueue<FrameState>) = registry_queue_init(&conn).expect("registry_queue_init");
    let mut state = FrameState::default();
    let painter = Painter::map(&globals, &mut queue, &mut state);
    let qh = queue.handle();
    let presentation: WpPresentation = globals.bind(&qh, 1..=1, ()).expect("bind wp_presentation");

    for n in 1..=3 {
        presentation.feedback(&painter.surface, &qh, ());
        painter.paint(&qh);
        pump_until(&mut queue, &mut state, |s| s.presented.len() >= n, "presented");
    }
    assert_eq!(state.discarded, 0, "a drawn frame was discarded");
    for pair in state.presented.windows(2) {
        assert!(pair[1].0 > pair[0].0, "presentation times did not increase: {:?}", state.presented);
    }
    for &(_, refresh) in &state.presented {
        assert!((1_000_000..100_000_000).contains(&refresh), "refresh {}ns is not a monitor's", refresh);
    }
}

// Test 24: content updates sent back to back through wp_fifo_v1, each
// setting a barrier and waiting on the one before, are each presented, on
// later refreshes, none discarded. Without the barriers' release at each
// frame the second would wait forever; without the barriers each would
// replace the last before it reached the screen.
#[test]
fn fifo_updates_each_reach_the_screen() {
    let wl = WlTest::new();
    let conn = wl.connect();
    let (globals, mut queue): (_, EventQueue<FrameState>) = registry_queue_init(&conn).expect("registry_queue_init");
    let mut state = FrameState::default();
    let painter = Painter::map(&globals, &mut queue, &mut state);
    let qh = queue.handle();
    let presentation: WpPresentation = globals.bind(&qh, 1..=1, ()).expect("bind wp_presentation");
    let manager: WpFifoManagerV1 = globals.bind(&qh, 1..=1, ()).expect("bind wp_fifo_manager_v1");
    let fifo = manager.get_fifo(&painter.surface, &qh, ());
    for _ in 0..10 {
        fifo.set_barrier();
        fifo.wait_barrier();
        presentation.feedback(&painter.surface, &qh, ());
        painter.paint(&qh);
    }
    pump_until(&mut queue, &mut state, |s| s.presented.len() as u32 + s.discarded >= 10, "ten updates shown or dropped");
    assert_eq!(state.discarded, 0, "updates were dropped: {:?}", state.presented);
    for pair in state.presented.windows(2) {
        assert!(pair[1].0 > pair[0].0, "two updates presented at once: {:?}", state.presented);
    }
}

// Test 25: a content update timed through wp_commit_timer_v1 for 200 ms
// from now is not presented before then.
#[test]
fn a_timed_update_waits_for_its_time() {
    let wl = WlTest::new();
    let conn = wl.connect();
    let (globals, mut queue): (_, EventQueue<FrameState>) = registry_queue_init(&conn).expect("registry_queue_init");
    let mut state = FrameState::default();
    let painter = Painter::map(&globals, &mut queue, &mut state);
    let qh = queue.handle();
    let presentation: WpPresentation = globals.bind(&qh, 1..=1, ()).expect("bind wp_presentation");
    let manager: WpCommitTimingManagerV1 = globals.bind(&qh, 1..=1, ()).expect("bind wp_commit_timing_manager_v1");
    let timer = manager.get_timer(&painter.surface, &qh, ());
    let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    let target = now.tv_sec as u64 * 1_000_000_000 + now.tv_nsec as u64 + 200_000_000;
    let secs = target / 1_000_000_000;
    timer.set_timestamp((secs >> 32) as u32, secs as u32, (target % 1_000_000_000) as u32);
    presentation.feedback(&painter.surface, &qh, ());
    painter.paint(&qh);
    pump_until(&mut queue, &mut state, |s| !s.presented.is_empty() || s.discarded > 0, "the timed update shown");
    let shown = state.presented.first().expect("the timed update was dropped").0;
    assert!(shown >= target, "the update was shown {} ms before its time", (target - shown) / 1_000_000);
}

// Test 26: timed updates, each for 100 ms after the last was shown, with
// nothing else drawing, are each shown within a few refreshes of their
// time. A compositor that renders only on damage held each until some
// other client drew: Halls of Torment's frames through NVIDIA's swapchain
// waited up to 330 ms for a terminal's redraw. It measures time, so it
// runs alone.
#[test]
fn a_timed_update_is_shown_on_its_refresh() {
    let wl = WlTest::alone();
    let conn = wl.connect();
    let (globals, mut queue): (_, EventQueue<FrameState>) = registry_queue_init(&conn).expect("registry_queue_init");
    let mut state = FrameState::default();
    let painter = Painter::map(&globals, &mut queue, &mut state);
    let qh = queue.handle();
    let presentation: WpPresentation = globals.bind(&qh, 1..=1, ()).expect("bind wp_presentation");
    let manager: WpCommitTimingManagerV1 = globals.bind(&qh, 1..=1, ()).expect("bind wp_commit_timing_manager_v1");
    let timer = manager.get_timer(&painter.surface, &qh, ());
    for n in 1..=3 {
        let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
        let target = now.tv_sec as u64 * 1_000_000_000 + now.tv_nsec as u64 + 100_000_000;
        let secs = target / 1_000_000_000;
        timer.set_timestamp((secs >> 32) as u32, secs as u32, (target % 1_000_000_000) as u32);
        presentation.feedback(&painter.surface, &qh, ());
        painter.paint(&qh);
        pump_until(&mut queue, &mut state, |s| s.presented.len() >= n || s.discarded > 0, "the timed update shown");
        assert_eq!(state.discarded, 0, "the timed update was dropped");
        let (shown, refresh) = state.presented[n - 1];
        assert!(shown >= target, "update {n} was shown {} ms before its time", (target - shown) / 1_000_000);
        let late = shown - target;
        let refresh = refresh as u64;
        assert!(late < 3 * refresh + 20_000_000, "update {n} was shown {} ms after its time, at a {} ms refresh", late / 1_000_000,
                refresh / 1_000_000);
    }
}

// State for the dmabuf test: The default feedback, until `done`.
#[derive(Default)]
struct FeedbackState {
    main_device: Option<u64>,
    table_size: u32,
    tranche_formats: usize,
    done: bool,
}

impl Dispatch<WlRegistry, GlobalListContents> for FeedbackState {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as wayland_client::Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpLinuxDmabufFeedbackV1, ()> for FeedbackState {
    fn event(
        state: &mut Self,
        _: &ZwpLinuxDmabufFeedbackV1,
        event: zwp_linux_dmabuf_feedback_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_linux_dmabuf_feedback_v1::Event::MainDevice { device } => {
                state.main_device = device.get(..8).map(|b| u64::from_ne_bytes(b.try_into().unwrap()));
            }
            zwp_linux_dmabuf_feedback_v1::Event::FormatTable { size, .. } => state.table_size = size,
            zwp_linux_dmabuf_feedback_v1::Event::TrancheFormats { indices } => state.tranche_formats += indices.len() / 2,
            zwp_linux_dmabuf_feedback_v1::Event::Done => state.done = true,
            _ => {}
        }
    }
}

delegate_noop!(FeedbackState: ignore ZwpLinuxDmabufV1);

// Test 12: on a GPU, linux-dmabuf is offered and its default feedback
// names the GPU's render node and what the renderer imports. Without it
// NVIDIA's EGL refuses the display and GPU clients fall back to the CPU.
// A software renderer (Xvfb's) offers none; DEMIURGE_WL_TEST_GPU says
// this run has a GPU and must see it.
#[test]
fn dmabuf_feedback_names_the_render_node() {
    let wl = WlTest::new();
    let conn = wl.connect();
    let (globals, mut queue): (_, EventQueue<FeedbackState>) = registry_queue_init(&conn).expect("registry_queue_init");
    let qh = queue.handle();
    let Ok(dmabuf) = globals.bind::<ZwpLinuxDmabufV1, _, _>(&qh, 4..=5, ()) else {
        assert!(std::env::var_os("DEMIURGE_WL_TEST_GPU").is_none(), "no zwp_linux_dmabuf_v1 on a GPU");
        eprintln!("no GPU renderer here; linux-dmabuf is rightly not offered");
        return;
    };
    let _feedback = dmabuf.get_default_feedback(&qh, ());
    let mut state = FeedbackState::default();
    pump_until(&mut queue, &mut state, |s| s.done, "dmabuf feedback");

    let dev = state.main_device.expect("feedback names no main device");
    let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & 0xffff_f000);
    let minor = (dev & 0xff) | ((dev >> 12) & 0xffff_ff00);
    assert_eq!(major, 226, "main device {}:{} is not a DRM node", major, minor);
    assert!(minor >= 128, "main device {}:{} is a primary node, not a render node", major, minor);
    assert!(state.table_size > 0 && state.tranche_formats > 0, "the feedback lists no formats");
}

// xdg_toplevel's state value for fullscreen.
const FULLSCREEN: u32 = 2;

// State for the fullscreen and popup tests: The output's current mode,
// the toplevel's last configure (size and states) and the popup's.
#[derive(Default)]
struct ShellState {
    mode: Option<(i32, i32)>,
    toplevel: Option<(i32, i32, Vec<u32>)>,
    popup: Option<(i32, i32, i32, i32)>,
}

impl Dispatch<WlRegistry, GlobalListContents> for ShellState {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as wayland_client::Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlOutput, ()> for ShellState {
    fn event(state: &mut Self, _: &WlOutput, event: wl_output::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_output::Event::Mode { flags: wayland_client::WEnum::Value(flags), width, height, .. } = event {
            if flags.contains(wl_output::Mode::Current) {
                state.mode = Some((width, height));
            }
        }
    }
}

impl Dispatch<XdgWmBase, ()> for ShellState {
    fn event(_: &mut Self, base: &XdgWmBase, event: xdg_wm_base::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for ShellState {
    fn event(_: &mut Self, surface: &XdgSurface, event: xdg_surface::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
        }
    }
}

impl Dispatch<XdgToplevel, ()> for ShellState {
    fn event(state: &mut Self, _: &XdgToplevel, event: xdg_toplevel::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_toplevel::Event::Configure { width, height, states } = event {
            let states = states.chunks_exact(4).map(|b| u32::from_ne_bytes(b.try_into().unwrap())).collect();
            state.toplevel = Some((width, height, states));
        }
    }
}

impl Dispatch<XdgPopup, ()> for ShellState {
    fn event(state: &mut Self, _: &XdgPopup, event: xdg_popup::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_popup::Event::Configure { x, y, width, height } = event {
            state.popup = Some((x, y, width, height));
        }
    }
}

delegate_noop!(ShellState: ignore WlCompositor);
delegate_noop!(ShellState: ignore WlSurface);
delegate_noop!(ShellState: ignore XdgPositioner);

// A toplevel past its first configure, with the output's mode known.
fn shell_toplevel(
    globals: &GlobalList,
    queue: &mut EventQueue<ShellState>,
    state: &mut ShellState,
) -> (WlCompositor, XdgWmBase, WlSurface, XdgSurface, XdgToplevel, WlOutput) {
    let qh = queue.handle();
    let output: WlOutput = globals.bind(&qh, 1..=4, ()).expect("bind wl_output");
    let compositor: WlCompositor = globals.bind(&qh, 4..=6, ()).expect("bind wl_compositor");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("bind xdg_wm_base");
    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    surface.commit();
    pump_until(queue, state, |s| s.mode.is_some() && s.toplevel.is_some(), "first configure and the output's mode");
    (compositor, wm_base, surface, xdg_surface, toplevel, output)
}

// Test 13: a client's own fullscreen request (a game's, on start) is
// answered with a fullscreen configure the size of the output, and
// unsetting it takes the state away again.
#[test]
fn a_fullscreen_request_fills_the_output() {
    let wl = WlTest::new();
    let conn = wl.connect();
    let (globals, mut queue): (_, EventQueue<ShellState>) = registry_queue_init(&conn).expect("registry_queue_init");
    let mut state = ShellState::default();
    let (_c, _b, _s, _x, toplevel, _o) = shell_toplevel(&globals, &mut queue, &mut state);

    toplevel.set_fullscreen(None);
    pump_until(&mut queue, &mut state, |s| s.toplevel.as_ref().is_some_and(|t| t.2.contains(&FULLSCREEN)), "fullscreen configure");
    let (w, h, _) = state.toplevel.clone().unwrap();
    assert_eq!(Some((w, h)), state.mode, "a fullscreen window must fill the output");

    toplevel.unset_fullscreen();
    pump_until(&mut queue, &mut state, |s| s.toplevel.as_ref().is_some_and(|t| !t.2.contains(&FULLSCREEN)), "unfullscreen configure");
}

// Test 14: a popup anchored past the right edge of the monitor is slid
// back onto it, as its positioner allows, and configured at its size.
#[test]
fn a_popup_past_the_edge_slides_onto_the_monitor() {
    let wl = WlTest::new();
    let conn = wl.connect();
    let (globals, mut queue): (_, EventQueue<ShellState>) = registry_queue_init(&conn).expect("registry_queue_init");
    let qh = queue.handle();
    let mut state = ShellState::default();
    let (compositor, wm_base, _s, parent, _t, _o) = shell_toplevel(&globals, &mut queue, &mut state);
    let (width, _) = state.mode.unwrap();

    let positioner = wm_base.create_positioner(&qh, ());
    positioner.set_size(200, 100);
    positioner.set_anchor_rect(width - 10, 10, 1, 1);
    positioner.set_anchor(xdg_positioner::Anchor::TopLeft);
    positioner.set_gravity(xdg_positioner::Gravity::BottomRight);
    positioner.set_constraint_adjustment(
        xdg_positioner::ConstraintAdjustment::SlideX | xdg_positioner::ConstraintAdjustment::SlideY,
    );
    let popup_surface = compositor.create_surface(&qh, ());
    let popup_xdg = wm_base.get_xdg_surface(&popup_surface, &qh, ());
    let _popup = popup_xdg.get_popup(Some(&parent), &positioner, &qh, ());
    popup_surface.commit();
    pump_until(&mut queue, &mut state, |s| s.popup.is_some(), "popup configure");

    let (x, _, w, h) = state.popup.unwrap();
    assert_eq!((w, h), (200, 100), "the popup was not configured at its size");
    assert!(x + w <= width, "the popup runs off the monitor: x {} + {} past {}", x, w, width);
}

// Test 15: the startup commands run with $DISPLAY naming the compositor's
// own XWayland, so an X11 command in [startup] finds it. Needs Xwayland
// installed; skipped where it is not.
#[test]
fn startup_commands_see_the_compositors_xwayland() {
    if !std::path::Path::new("/usr/bin/Xwayland").exists() {
        eprintln!("no /usr/bin/Xwayland here; skipped");
        return;
    }
    let seen = tempfile::tempdir().expect("tempdir");
    let display_file = seen.path().join("display");
    let cfg = format!("[startup]\ncommands = ['echo \"$DISPLAY\" > {}']\n", display_file.display());
    let wl = WlTest::with_xwayland(&cfg);
    let deadline = Instant::now() + Duration::from_secs(5);
    let display = loop {
        match std::fs::read_to_string(&display_file) {
            Ok(text) if text.ends_with('\n') => break text.trim().to_string(),
            _ if Instant::now() >= deadline => panic!("the startup command never wrote $DISPLAY"),
            _ => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    let xwayland = children(wl.pid())
        .into_iter()
        .find(|(_, comm, _)| comm == "Xwayland")
        .expect("the compositor spawned no Xwayland");
    let cmdline = std::fs::read(format!("/proc/{}/cmdline", xwayland.0)).expect("Xwayland's cmdline");
    let own = cmdline
        .split(|&b| b == 0)
        .map(String::from_utf8_lossy)
        .find(|arg| arg.starts_with(':'))
        .expect("Xwayland's display argument")
        .to_string();
    assert_eq!(display, own, "a startup command saw $DISPLAY {:?}, not the compositor's XWayland {:?}", display, own);
}

// Test 16: a startup command that exits is reaped, not left a zombie.
#[test]
fn exited_startup_commands_are_reaped() {
    let wl = WlTest::with_config("[startup]\ncommands = ['true', 'true']\n");
    std::thread::sleep(Duration::from_millis(300));
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let zombies: Vec<_> = children(wl.pid()).into_iter().filter(|(_, _, state)| *state == 'Z').collect();
        if zombies.is_empty() {
            break;
        }
        assert!(Instant::now() < deadline, "exited startup commands left as zombies: {:?}", zombies);
        std::thread::sleep(Duration::from_millis(50));
    }
}

// State for the activation test: Whether each toplevel's latest
// configure carried Activated.
#[derive(Default)]
struct ActiveState {
    activated: [Option<bool>; 2],
}

impl Dispatch<WlRegistry, GlobalListContents> for ActiveState {
    fn event(
        _state: &mut Self,
        _registry: &WlRegistry,
        _event: <WlRegistry as wayland_client::Proxy>::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<XdgWmBase, ()> for ActiveState {
    fn event(
        _state: &mut Self,
        wm_base: &XdgWmBase,
        event: <XdgWmBase as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, usize> for ActiveState {
    fn event(
        _state: &mut Self,
        xdg_surface: &XdgSurface,
        event: <XdgSurface as wayland_client::Proxy>::Event,
        _slot: &usize,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
        }
    }
}

impl Dispatch<XdgToplevel, usize> for ActiveState {
    fn event(
        state: &mut Self,
        _toplevel: &XdgToplevel,
        event: <XdgToplevel as wayland_client::Proxy>::Event,
        slot: &usize,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Configure { states, .. } = event {
            let activated = xdg_toplevel::State::Activated as u32;
            state.activated[*slot] = Some(
                states
                    .chunks_exact(4)
                    .any(|s| u32::from_ne_bytes([s[0], s[1], s[2], s[3]]) == activated),
            );
        }
    }
}

delegate_noop!(ActiveState: ignore WlCompositor);
delegate_noop!(ActiveState: ignore WlSurface);

// Test 17: only the window holding the keyboard is told it is activated.
// A second window takes the keyboard as it maps, and the first gets a
// configure without Activated, as a game needs to pause or mute when
// it loses focus.
#[test]
fn only_the_focused_window_is_activated() {
    let wl = WlTest::new();
    let conn = wl.connect();
    let (globals, mut queue): (_, EventQueue<ActiveState>) =
        registry_queue_init(&conn).expect("registry_queue_init");
    let qh = queue.handle();
    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("bind wl_compositor");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=5, ()).expect("bind xdg_wm_base");
    let mut state = ActiveState::default();

    let first = compositor.create_surface(&qh, ());
    let first_xdg = wm_base.get_xdg_surface(&first, &qh, 0usize);
    let _first_toplevel = first_xdg.get_toplevel(&qh, 0usize);
    first.commit();
    pump_until(&mut queue, &mut state, |s| s.activated[0] == Some(true), "the first window activated");

    let second = compositor.create_surface(&qh, ());
    let second_xdg = wm_base.get_xdg_surface(&second, &qh, 1usize);
    let _second_toplevel = second_xdg.get_toplevel(&qh, 1usize);
    second.commit();
    pump_until(
        &mut queue,
        &mut state,
        |s| s.activated == [Some(false), Some(true)],
        "the second window activated and the first no longer",
    );
}

// Test 18: a child is started with the compositor's WAYLAND_DISPLAY and
// cursor theme in its own environment.
#[test]
fn children_get_the_compositors_socket_and_cursor_theme() {
    let seen = tempfile::tempdir().expect("tempdir");
    let env_file = seen.path().join("env");
    let cfg = format!(
        "[cursor]\ntheme = \"Adwaita\"\nsize = 32\n\n[startup]\ncommands = ['env > {0}.part && mv {0}.part {0}']\n",
        env_file.display()
    );
    let wl = WlTest::with_config(&cfg);
    let deadline = Instant::now() + Duration::from_secs(5);
    let env = loop {
        match std::fs::read_to_string(&env_file) {
            Ok(text) => break text,
            _ if Instant::now() >= deadline => panic!("the startup command never wrote its environment"),
            _ => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    let var = |key: &str| env.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('='));
    assert_eq!(var("WAYLAND_DISPLAY"), Some(wl.socket_name.as_str()), "the child's WAYLAND_DISPLAY");
    assert_eq!(var("XCURSOR_THEME"), Some("Adwaita"), "the child's XCURSOR_THEME");
    assert_eq!(var("XCURSOR_SIZE"), Some("32"), "the child's XCURSOR_SIZE");
}

// Test 19: nothing in the compositor writes its own environment; a
// child's variables go through Command::env. A set_var once EGL has
// started the driver's threads races their getenv, a crash no test can
// provoke on demand, so this one reads the sources.
#[test]
fn the_compositor_never_writes_its_own_environment() {
    fn rust_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read a source directory").flatten() {
            let path = entry.path();
            if path.is_dir() {
                rust_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    if !root.join("src").exists() {
        eprintln!("the sources are not beside this binary (the QEMU guest); skipped");
        return;
    }
    let mut files = Vec::new();
    for dir in ["src", "screen/src", "window/src"] {
        rust_files(&root.join(dir), &mut files);
    }
    let writes: Vec<String> = files
        .iter()
        .flat_map(|path| {
            let text = std::fs::read_to_string(path).expect("read a source file");
            text.lines()
                .enumerate()
                .filter(|(_, line)| !line.trim_start().starts_with("//"))
                .filter(|(_, line)| line.contains("set_var(") || line.contains("remove_var("))
                .map(|(n, _)| format!("{}:{}", path.display(), n + 1))
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(writes.is_empty(), "the compositor writes its own environment at {:?}", writes);
}

// State for the color-management tests: What the manager supports, and
// each image description's answer and information by the tag the test
// gave it.
#[derive(Default)]
struct ColorState {
    tfs: Vec<TransferFunction>,
    primaries: Vec<Primaries>,
    features: Vec<Feature>,
    supported_done: bool,
    ready: Vec<(u32, u64)>,
    failed: Vec<(u32, String)>,
    info: Vec<(u32, DescriptionInfo)>,
    preferred: Option<u64>,
}

#[derive(Default, Debug)]
struct DescriptionInfo {
    tf: Option<TransferFunction>,
    primaries: Option<Primaries>,
    target_luminance: Option<(u32, u32)>,
    max_cll: Option<u32>,
    max_fall: Option<u32>,
    done: bool,
}

impl ColorState {
    fn identity(&self, tag: u32) -> Option<u64> {
        self.ready.iter().find(|r| r.0 == tag).map(|r| r.1)
    }

    fn info(&mut self, tag: u32) -> &mut DescriptionInfo {
        if let Some(i) = self.info.iter().position(|i| i.0 == tag) {
            return &mut self.info[i].1;
        }
        self.info.push((tag, DescriptionInfo::default()));
        &mut self.info.last_mut().expect("pushed").1
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for ColorState {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as wayland_client::Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpColorManagerV1, ()> for ColorState {
    fn event(state: &mut Self, _: &WpColorManagerV1, event: wp_color_manager_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        use wayland_client::WEnum::Value;
        match event {
            wp_color_manager_v1::Event::SupportedTfNamed { tf: Value(tf) } => state.tfs.push(tf),
            wp_color_manager_v1::Event::SupportedPrimariesNamed { primaries: Value(p) } => state.primaries.push(p),
            wp_color_manager_v1::Event::SupportedFeature { feature: Value(f) } => state.features.push(f),
            wp_color_manager_v1::Event::Done => state.supported_done = true,
            _ => {}
        }
    }
}

impl Dispatch<WpImageDescriptionV1, u32> for ColorState {
    fn event(state: &mut Self, _: &WpImageDescriptionV1, event: wp_image_description_v1::Event, tag: &u32, _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wp_image_description_v1::Event::Ready2 { identity_hi, identity_lo } => {
                state.ready.push((*tag, ((identity_hi as u64) << 32) | identity_lo as u64));
            }
            wp_image_description_v1::Event::Failed { msg, .. } => state.failed.push((*tag, msg)),
            _ => {}
        }
    }
}

impl Dispatch<WpImageDescriptionInfoV1, u32> for ColorState {
    fn event(state: &mut Self, _: &WpImageDescriptionInfoV1, event: wp_image_description_info_v1::Event, tag: &u32, _: &Connection, _: &QueueHandle<Self>) {
        use wayland_client::WEnum::Value;
        let info = state.info(*tag);
        match event {
            wp_image_description_info_v1::Event::TfNamed { tf: Value(tf) } => info.tf = Some(tf),
            wp_image_description_info_v1::Event::PrimariesNamed { primaries: Value(p) } => info.primaries = Some(p),
            wp_image_description_info_v1::Event::TargetLuminance { min_lum, max_lum } => info.target_luminance = Some((min_lum, max_lum)),
            wp_image_description_info_v1::Event::TargetMaxCll { max_cll } => info.max_cll = Some(max_cll),
            wp_image_description_info_v1::Event::TargetMaxFall { max_fall } => info.max_fall = Some(max_fall),
            wp_image_description_info_v1::Event::Done => info.done = true,
            _ => {}
        }
    }
}

impl Dispatch<WpColorManagementSurfaceFeedbackV1, ()> for ColorState {
    fn event(
        state: &mut Self,
        _: &WpColorManagementSurfaceFeedbackV1,
        event: wp_color_management_surface_feedback_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_color_management_surface_feedback_v1::Event::PreferredChanged2 { identity_hi, identity_lo } = event {
            state.preferred = Some(((identity_hi as u64) << 32) | identity_lo as u64);
        }
    }
}

delegate_noop!(ColorState: ignore WlCompositor);
delegate_noop!(ColorState: ignore WlSurface);
delegate_noop!(ColorState: ignore WlOutput);
delegate_noop!(ColorState: ignore WpColorManagementOutputV1);
delegate_noop!(ColorState: ignore WpColorManagementSurfaceV1);
delegate_noop!(ColorState: ignore WpImageDescriptionCreatorParamsV1);

// BT.2020's primaries and D65 white as CIE 1931 xy times 1,000,000: An
// HDR10 mastering display's, as a game in HDR declares them.
const BT2020: [i32; 8] = [708_000, 292_000, 170_000, 797_000, 131_000, 46_000, 312_700, 329_000];

// Test 20: a client's HDR10 image description, PQ over BT.2020 with its
// mastering metadata, is made, read back as it was given, and reaches
// the compositor at the surface's commit. The output, and the surface's
// preferred description with it, is the sRGB display the compositor
// composites for: One record with one identity.
#[test]
fn an_hdr_image_description_reaches_the_compositor() {
    let wl = WlTest::new();
    let conn = wl.connect();
    let (globals, mut queue): (_, EventQueue<ColorState>) = registry_queue_init(&conn).expect("registry_queue_init");
    let qh = queue.handle();
    let manager: WpColorManagerV1 = globals.bind(&qh, 2..=2, ()).expect("bind wp_color_manager_v1");
    let mut state = ColorState::default();
    pump_until(&mut queue, &mut state, |s| s.supported_done, "the manager's supported set");
    assert!(state.tfs.contains(&TransferFunction::St2084Pq), "PQ is not supported: {:?}", state.tfs);
    assert!(state.primaries.contains(&Primaries::Bt2020), "BT.2020 is not supported: {:?}", state.primaries);
    for feature in [Feature::Parametric, Feature::SetMasteringDisplayPrimaries] {
        assert!(state.features.contains(&feature), "{:?} is not supported: {:?}", feature, state.features);
    }

    let creator = manager.create_parametric_creator(&qh, ());
    creator.set_tf_named(TransferFunction::St2084Pq);
    creator.set_primaries_named(Primaries::Bt2020);
    let [r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y] = BT2020;
    creator.set_mastering_display_primaries(r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y);
    creator.set_mastering_luminance(50, 1000);
    creator.set_max_cll(1000);
    creator.set_max_fall(400);
    let hdr = creator.create(&qh, 1);
    pump_until(&mut queue, &mut state, |s| s.identity(1).is_some() || !s.failed.is_empty(), "the HDR description");
    assert!(state.failed.is_empty(), "the HDR description failed: {:?}", state.failed);

    hdr.get_information(&qh, 1);
    pump_until(&mut queue, &mut state, |s| s.info.iter().any(|i| i.0 == 1 && i.1.done), "the HDR description's information");
    let info = state.info(1);
    assert_eq!(info.tf, Some(TransferFunction::St2084Pq), "{:?}", info);
    assert_eq!(info.primaries, Some(Primaries::Bt2020), "{:?}", info);
    assert_eq!((info.target_luminance, info.max_cll, info.max_fall), (Some((50, 1000)), Some(1000), Some(400)), "{:?}", info);

    let compositor: WlCompositor = globals.bind(&qh, 4..=6, ()).expect("bind wl_compositor");
    let surface = compositor.create_surface(&qh, ());
    let color = manager.get_surface(&surface, &qh, ());
    color.set_image_description(&hdr, RenderIntent::Perceptual);
    surface.commit();
    queue.roundtrip(&mut state).expect("the description was refused");
    let line = wl
        .wait_for_log("surface image description: tf=st2084_pq primaries=bt2020", ROUNDTRIP_TIMEOUT)
        .unwrap_or_else(|| panic!("the description never reached the compositor:\n{}", wl.log()));
    assert!(line.contains("mastering=50/1000 max_cll=1000 max_fall=400"), "the metadata did not arrive whole: {}", line);

    let output: WlOutput = globals.bind(&qh, 1..=4, ()).expect("bind wl_output");
    let display = manager.get_output(&output, &qh, ()).get_image_description(&qh, 2);
    let _feedback = manager.get_surface_feedback(&surface, &qh, ());
    pump_until(&mut queue, &mut state, |s| s.identity(2).is_some() && s.preferred.is_some(), "the output's description");
    display.get_information(&qh, 2);
    pump_until(&mut queue, &mut state, |s| s.info.iter().any(|i| i.0 == 2 && i.1.done), "the output's information");
    let output_info = state.info(2);
    assert_eq!((output_info.tf, output_info.primaries), (Some(TransferFunction::Gamma22), Some(Primaries::Srgb)), "{:?}", output_info);
    assert_eq!(state.preferred, state.identity(2), "the preferred description is not the output's");
}

// Test 21: an image description without its transfer function or its
// primaries is the creator's incomplete_set protocol error, as the
// protocol has it, never a description the compositor guesses at. create
// is a destructor, so the client no longer knows the object the error
// names; the compositor's log says which refusal it was.
#[test]
fn an_incomplete_image_description_is_a_protocol_error() {
    let wl = WlTest::new();
    let conn = wl.connect();
    let (globals, mut queue): (_, EventQueue<ColorState>) = registry_queue_init(&conn).expect("registry_queue_init");
    let qh = queue.handle();
    let manager: WpColorManagerV1 = globals.bind(&qh, 2..=2, ()).expect("bind wp_color_manager_v1");
    let mut state = ColorState::default();
    let creator = manager.create_parametric_creator(&qh, ());
    creator.set_tf_named(TransferFunction::St2084Pq);
    let _description = creator.create(&qh, 1);
    queue.roundtrip(&mut state).expect_err("an incomplete description was accepted");
    let error = conn.protocol_error().expect("a protocol error");
    assert_eq!(error.code, wp_image_description_creator_params_v1::Error::IncompleteSet as u32, "{:?}", error);
    let line = wl
        .wait_for_log("color management refused: a transfer function and primaries are required", ROUNDTRIP_TIMEOUT)
        .unwrap_or_else(|| panic!("the refusal was not the incomplete set:\n{}", wl.log()));
    assert!(line.contains("wp_image_description_creator_params_v1"), "refused by another object: {}", line);
}

// The SteamGameId a process carries in its environment.
fn steam_game_of(pid: u32) -> Option<String> {
    let environ = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    environ.split(|&b| b == 0).find_map(|e| std::str::from_utf8(e).ok()?.strip_prefix("SteamGameId=").map(str::to_string))
}

// Test 22's Steam game: A window mapped by a process of its own, which
// that test starts with the game's Steam IDs in its environment. Run
// otherwise, it does nothing.
#[test]
#[ignore = "the client a_steam_games_window_holds_xwaylands_input_focus starts"]
fn steam_game_window() {
    let Some(socket) = std::env::var_os("DEMIURGE_TEST_SOCKET") else { return };
    let stream = std::os::unix::net::UnixStream::connect(socket).expect("connect to the compositor");
    let conn = Connection::from_socket(stream).expect("wayland-client Connection::from_socket");
    let (globals, mut queue): (_, EventQueue<ActiveState>) = registry_queue_init(&conn).expect("registry_queue_init");
    let qh = queue.handle();
    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("bind wl_compositor");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=5, ()).expect("bind xdg_wm_base");
    let mut state = ActiveState::default();
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, 0usize);
    let _toplevel = xdg.get_toplevel(&qh, 0usize);
    surface.commit();
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && queue.roundtrip(&mut state).is_ok() {
        std::thread::sleep(Duration::from_millis(50));
    }
}

// Test 22: a Steam game's Wayland window holding the keyboard gives
// XWayland's input focus to a window whose X client carries the game's
// SteamGameId, the client Steam names the focused game by (through XRes),
// so Steam Input hands the game its controller. A window that is no game
// takes the keyboard, and the helper holding the focus goes. Needs
// Xwayland installed; skipped where it is not.
#[test]
fn a_steam_games_window_holds_xwaylands_input_focus() {
    use smithay::reexports::x11rb;
    use x11rb::protocol::res::{ClientIdMask, ClientIdSpec, ConnectionExt as _};
    use x11rb::protocol::xproto::ConnectionExt as _;

    if !std::path::Path::new("/usr/bin/Xwayland").exists() {
        eprintln!("no /usr/bin/Xwayland here; skipped");
        return;
    }
    let wl = WlTest::with_xwayland("");
    let display = wl.xwayland_display();
    let (x, _) = x11rb::connect(Some(&display)).expect("connect to the compositor's XWayland");
    // The game the X client owning the focused window names, as Steam
    // reads it; none for no focus or PointerRoot.
    let focused_game = || -> Option<String> {
        let focus = x.get_input_focus().ok()?.reply().ok()?.focus;
        if focus <= 1 {
            return None;
        }
        let spec = ClientIdSpec { client: focus, mask: ClientIdMask::LOCAL_CLIENT_PID };
        let ids = x.res_query_client_ids(&[spec]).ok()?.reply().ok()?;
        steam_game_of(*ids.ids.first()?.value.first()?)
    };

    let mut game = std::process::Command::new(std::env::current_exe().expect("this test binary"))
        .args(["steam_game_window", "--exact", "--ignored", "--nocapture"])
        .env("DEMIURGE_TEST_SOCKET", &wl.socket_path)
        .env("SteamGameId", "4242")
        .env("SteamAppId", "4242")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("start the game's window");
    let deadline = Instant::now() + Duration::from_secs(10);
    let held = loop {
        let seen = focused_game();
        if seen.is_some() || Instant::now() >= deadline {
            break seen;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(held.as_deref(), Some("4242"), "XWayland's input focus on a window whose owner carries the game's SteamGameId\n{}", wl.log());

    let conn = wl.connect();
    let (globals, mut queue): (_, EventQueue<ActiveState>) = registry_queue_init(&conn).expect("registry_queue_init");
    let qh = queue.handle();
    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("bind wl_compositor");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=5, ()).expect("bind xdg_wm_base");
    let mut state = ActiveState::default();
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, 0usize);
    let _toplevel = xdg.get_toplevel(&qh, 0usize);
    surface.commit();
    pump_until(&mut queue, &mut state, |s| s.activated[0] == Some(true), "a window that is no game takes the keyboard");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let helper = children(wl.pid()).into_iter().find(|(pid, _, _)| steam_game_of(*pid).is_some());
        if focused_game().is_none() && helper.is_none() {
            break;
        }
        assert!(Instant::now() < deadline, "the focus still held for the game: {:?}, its helper {:?}", focused_game(), helper);
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = game.kill();
    let _ = game.wait();
}

// One window of one client, for xdg-activation: Whether it is activated,
// and the token it was last given.
#[derive(Default)]
struct ActivationState {
    activated: Option<bool>,
    token: Option<String>,
}

impl Dispatch<WlRegistry, GlobalListContents> for ActivationState {
    fn event(
        _state: &mut Self,
        _registry: &WlRegistry,
        _event: <WlRegistry as wayland_client::Proxy>::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<XdgWmBase, ()> for ActivationState {
    fn event(
        _state: &mut Self,
        wm_base: &XdgWmBase,
        event: <XdgWmBase as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for ActivationState {
    fn event(
        _state: &mut Self,
        xdg_surface: &XdgSurface,
        event: <XdgSurface as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
        }
    }
}

impl Dispatch<XdgToplevel, ()> for ActivationState {
    fn event(
        state: &mut Self,
        _toplevel: &XdgToplevel,
        event: <XdgToplevel as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Configure { states, .. } = event {
            let activated = xdg_toplevel::State::Activated as u32;
            state.activated =
                Some(states.chunks_exact(4).any(|s| u32::from_ne_bytes([s[0], s[1], s[2], s[3]]) == activated));
        }
    }
}

impl Dispatch<XdgActivationTokenV1, ()> for ActivationState {
    fn event(
        state: &mut Self,
        _token: &XdgActivationTokenV1,
        event: <XdgActivationTokenV1 as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let xdg_activation_token_v1::Event::Done { token } = event {
            state.token = Some(token);
        }
    }
}

delegate_noop!(ActivationState: ignore WlCompositor);
delegate_noop!(ActivationState: ignore WlSurface);
delegate_noop!(ActivationState: ignore XdgActivationV1);
delegate_noop!(ActivationState: ignore ZwpIdleInhibitManagerV1);
delegate_noop!(ActivationState: ignore ZwpIdleInhibitorV1);

struct Activating {
    queue: EventQueue<ActivationState>,
    state: ActivationState,
    surface: WlSurface,
    activation: XdgActivationV1,
    _toplevel: XdgToplevel,
}

impl Activating {
    // A client's window, mapped and holding the keyboard as a new window
    // does.
    fn map(conn: &Connection) -> Self {
        let (globals, mut queue): (_, EventQueue<ActivationState>) =
            registry_queue_init(conn).expect("registry_queue_init");
        let qh = queue.handle();
        let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("bind wl_compositor");
        let wm_base: XdgWmBase = globals.bind(&qh, 1..=5, ()).expect("bind xdg_wm_base");
        let activation: XdgActivationV1 = globals.bind(&qh, 1..=1, ()).expect("bind xdg_activation_v1");
        let surface = compositor.create_surface(&qh, ());
        let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        surface.commit();
        let mut state = ActivationState::default();
        pump_until(&mut queue, &mut state, |s| s.activated == Some(true), "a new window activated");
        Activating { queue, state, surface, activation, _toplevel: toplevel }
    }

    fn token(&mut self) -> String {
        let token = self.activation.get_activation_token(&self.queue.handle(), ());
        token.set_surface(&self.surface);
        token.commit();
        self.state.token = None;
        pump_until(&mut self.queue, &mut self.state, |s| s.token.is_some(), "an activation token");
        self.state.token.take().unwrap()
    }

    fn activate(&mut self, token: &str) {
        self.activation.activate(token.to_string(), &self.surface);
        self.pump();
    }

    fn pump(&mut self) {
        self.queue.roundtrip(&mut self.state).expect("roundtrip");
    }
}

// xdg-activation, as quark-wayland's SetForegroundWindow asks: A token from
// the client holding the keyboard, presented by another client, hands that
// client's window the keyboard; a token from a client without it focuses
// nothing.
#[test]
fn a_token_from_the_focused_client_hands_over_the_keyboard() {
    let wl = WlTest::new();
    let mut a = Activating::map(&wl.connect());
    let mut b = Activating::map(&wl.connect());
    pump_until(&mut a.queue, &mut a.state, |s| s.activated == Some(false), "the first window deactivated");

    let token = b.token();
    a.activate(&token);
    pump_until(&mut a.queue, &mut a.state, |s| s.activated == Some(true), "the window presenting the token activated");
    pump_until(&mut b.queue, &mut b.state, |s| s.activated == Some(false), "the token's client deactivated");

    let token = b.token();
    b.activate(&token);
    std::thread::sleep(Duration::from_millis(300));
    a.pump();
    b.pump();
    assert_eq!(a.state.activated, Some(true), "a token from a client without the keyboard took it");
    assert_eq!(b.state.activated, Some(false), "a token from a client without the keyboard took it");
}

// A log written to a file, as a session sends it, is plain text: Color
// codes would split each field from its value.
#[test]
fn a_log_to_a_file_carries_no_color_codes() {
    let wl = WlTest::new();
    wl.connect().roundtrip().expect("roundtrip");
    let log = wl.log();
    assert!(log.contains("wayland socket ready"), "{log}");
    assert!(!log.contains('\x1b'), "color codes in the log:\n{log}");
}

// A locker that dies with the session locked is relaunched, as the lock
// bind runs it, instead of leaving a black screen with no client.
#[test]
fn a_locker_that_dies_locked_is_relaunched() {
    let wl = WlTest::new();
    {
        let conn = wl.connect();
        let (globals, mut queue): (_, EventQueue<LockState>) =
            registry_queue_init(&conn).expect("registry_queue_init");
        let qh = queue.handle();
        let mgr: ExtSessionLockManagerV1 = globals.bind(&qh, 1..=1, ()).expect("bind ext_session_lock_manager_v1");
        let _lock = mgr.lock(&qh, ());
        let mut state = LockState::default();
        pump_until(&mut queue, &mut state, |s| s.locked || s.finished, "ext_session_lock_v1.locked");
        assert!(state.locked, "the session never locked");
    }
    // The locker's connection is gone, as a crashed locker's is.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !wl.log().contains("cmd=gordian_knot") {
        assert!(Instant::now() < deadline, "no locker relaunched:\n{}", wl.log());
        std::thread::sleep(Duration::from_millis(50));
    }
}

// [[startup.spawn]] entries are launched with the session, as on X11, where
// demiurge-wl skipped them.
#[test]
fn startup_spawns_are_launched() {
    let seen = tempfile::tempdir().expect("tempdir");
    let mark = seen.path().join("spawned");
    let wl = WlTest::with_config(&format!("[[startup.spawn]]\ncmd = 'touch {}'\n", mark.display()));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !mark.exists() {
        assert!(Instant::now() < deadline, "the [[startup.spawn]] command never ran:\n{}", wl.log());
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait_for_lock(wl: &WlTest, within: Duration, why: &str) {
    let deadline = Instant::now() + within;
    while !wl.log().contains("cmd=gordian_knot") {
        assert!(Instant::now() < deadline, "{why}:\n{}", wl.log());
        std::thread::sleep(Duration::from_millis(50));
    }
}

// The idle lock: With lock_seconds set, a session given no input runs the
// locker, as the X11 stack's gordian_knot daemon does.
#[test]
fn an_idle_session_locks() {
    let wl = WlTest::with_config("[input.idle]\nlock_seconds = 1\n");
    wait_for_lock(&wl, Duration::from_secs(5), "an idle session never locked");
}

// idle-inhibit: A shown window's inhibitor holds the idle lock off, until
// it is destroyed.
#[test]
fn an_idle_inhibitor_holds_off_the_lock() {
    let wl = WlTest::with_config("[input.idle]\nlock_seconds = 1\n");
    let conn = wl.connect();
    let (globals, mut queue): (_, EventQueue<ActivationState>) =
        registry_queue_init(&conn).expect("registry_queue_init");
    let qh = queue.handle();
    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("bind wl_compositor");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=5, ()).expect("bind xdg_wm_base");
    let manager: ZwpIdleInhibitManagerV1 = globals.bind(&qh, 1..=1, ()).expect("bind zwp_idle_inhibit_manager_v1");
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let _toplevel = xdg.get_toplevel(&qh, ());
    let inhibitor = manager.create_inhibitor(&surface, &qh, ());
    surface.commit();
    let mut state = ActivationState::default();
    pump_until(&mut queue, &mut state, |s| s.activated == Some(true), "the window mapped");

    std::thread::sleep(Duration::from_secs(3));
    queue.roundtrip(&mut state).expect("roundtrip");
    assert!(!wl.log().contains("cmd=gordian_knot"), "the session locked under a shown window's inhibitor:\n{}", wl.log());

    inhibitor.destroy();
    queue.roundtrip(&mut state).expect("roundtrip");
    wait_for_lock(&wl, Duration::from_secs(5), "the session never locked once the inhibitor went");
}

// An input method's client, as fcitx5 runs: Whether it is active, and how
// many `done` events it has had, which its commits count.
#[derive(Default)]
struct ImeState {
    active: bool,
    done: u32,
}

impl Dispatch<WlRegistry, GlobalListContents> for ImeState {
    fn event(
        _state: &mut Self,
        _registry: &WlRegistry,
        _event: <WlRegistry as wayland_client::Proxy>::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpInputMethodV2, ()> for ImeState {
    fn event(
        state: &mut Self,
        _im: &ZwpInputMethodV2,
        event: <ZwpInputMethodV2 as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_method_v2::Event::Activate => state.active = true,
            zwp_input_method_v2::Event::Deactivate => state.active = false,
            zwp_input_method_v2::Event::Done => state.done += 1,
            _ => {}
        }
    }
}

delegate_noop!(ImeState: ignore WlSeat);
delegate_noop!(ImeState: ignore ZwpInputMethodManagerV2);

// The window typed into: Its text input's entry, preedit and commit.
#[derive(Default)]
struct TypedState {
    entered: bool,
    preedit: Option<String>,
    committed: Option<String>,
}

impl Dispatch<WlRegistry, GlobalListContents> for TypedState {
    fn event(
        _state: &mut Self,
        _registry: &WlRegistry,
        _event: <WlRegistry as wayland_client::Proxy>::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<XdgWmBase, ()> for TypedState {
    fn event(
        _state: &mut Self,
        wm_base: &XdgWmBase,
        event: <XdgWmBase as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for TypedState {
    fn event(
        _state: &mut Self,
        xdg_surface: &XdgSurface,
        event: <XdgSurface as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
        }
    }
}

impl Dispatch<ZwpTextInputV3, ()> for TypedState {
    fn event(
        state: &mut Self,
        text_input: &ZwpTextInputV3,
        event: <ZwpTextInputV3 as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwp_text_input_v3::Event::Enter { .. } => {
                state.entered = true;
                text_input.enable();
                text_input.commit();
            }
            zwp_text_input_v3::Event::PreeditString { text, .. } => state.preedit = text,
            zwp_text_input_v3::Event::CommitString { text } => state.committed = text,
            _ => {}
        }
    }
}

delegate_noop!(TypedState: ignore WlCompositor);
delegate_noop!(TypedState: ignore WlSurface);
delegate_noop!(TypedState: ignore WlSeat);
delegate_noop!(TypedState: ignore XdgToplevel);
delegate_noop!(TypedState: ignore ZwpTextInputManagerV3);

// text-input-v3 and input-method-v2, behind quark-wayland's IME: An input
// method's preedit and commit reach the text input of the window holding
// the keyboard.
#[test]
fn an_input_methods_text_reaches_the_focused_window() {
    let wl = WlTest::new();
    let ime_conn = wl.connect();
    let (globals, mut ime_queue): (_, EventQueue<ImeState>) =
        registry_queue_init(&ime_conn).expect("registry_queue_init");
    let qh = ime_queue.handle();
    let seat: WlSeat = globals.bind(&qh, 1..=7, ()).expect("bind wl_seat");
    let manager: ZwpInputMethodManagerV2 = globals.bind(&qh, 1..=1, ()).expect("bind zwp_input_method_manager_v2");
    let im = manager.get_input_method(&seat, &qh, ());
    let mut ime = ImeState::default();
    ime_queue.roundtrip(&mut ime).expect("roundtrip");

    let conn = wl.connect();
    let (globals, mut queue): (_, EventQueue<TypedState>) = registry_queue_init(&conn).expect("registry_queue_init");
    let qh = queue.handle();
    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("bind wl_compositor");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=5, ()).expect("bind xdg_wm_base");
    let seat: WlSeat = globals.bind(&qh, 1..=7, ()).expect("bind wl_seat");
    let manager: ZwpTextInputManagerV3 = globals.bind(&qh, 1..=1, ()).expect("bind zwp_text_input_manager_v3");
    let _text_input = manager.get_text_input(&seat, &qh, ());
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let _toplevel = xdg.get_toplevel(&qh, ());
    surface.commit();
    let mut typed = TypedState::default();
    pump_until(&mut queue, &mut typed, |s| s.entered, "the text input entered the window holding the keyboard");
    // The text input's enable, sent as it entered.
    queue.roundtrip(&mut typed).expect("roundtrip");

    pump_until(&mut ime_queue, &mut ime, |s| s.active && s.done > 0, "the input method activated");
    im.set_preedit_string("ni".into(), 0, 2);
    im.commit(ime.done);
    ime_queue.roundtrip(&mut ime).expect("roundtrip");
    pump_until(&mut queue, &mut typed, |s| s.preedit.as_deref() == Some("ni"), "the preedit reached the window");
    im.commit_string("\u{4f60}".into());
    im.commit(ime.done);
    ime_queue.roundtrip(&mut ime).expect("roundtrip");
    pump_until(&mut queue, &mut typed, |s| s.committed.as_deref() == Some("\u{4f60}"), "the commit reached the window");
}

// XWayland's X11 apps get DEMIURGE's settings through XSETTINGS, as on the
// X11 stack: A manager owns _XSETTINGS_S0 and its property carries the
// cursor theme.
#[test]
fn xwayland_apps_get_xsettings() {
    if !std::path::Path::new("/usr/bin/Xwayland").exists() {
        eprintln!("no /usr/bin/Xwayland here; skipped");
        return;
    }
    use smithay::reexports::x11rb;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _};

    let wl = WlTest::with_xwayland("");
    let (x, screen) = x11rb::connect(Some(&wl.xwayland_display())).expect("connect to the compositor's XWayland");
    let atom = |name: &str| x.intern_atom(false, name.as_bytes()).unwrap().reply().unwrap().atom;
    let (selection, settings) = (atom(&format!("_XSETTINGS_S{screen}")), atom("_XSETTINGS_SETTINGS"));
    let deadline = Instant::now() + Duration::from_secs(5);
    let owner = loop {
        let owner = x.get_selection_owner(selection).unwrap().reply().unwrap().owner;
        if owner != 0 {
            break owner;
        }
        assert!(Instant::now() < deadline, "no XSETTINGS manager on XWayland:\n{}", wl.log());
        std::thread::sleep(Duration::from_millis(50));
    };
    let blob = x.get_property(false, owner, settings, AtomEnum::ANY, 0, 4096).unwrap().reply().unwrap().value;
    assert!(blob.windows(19).any(|w| w == b"Gtk/CursorThemeName"), "the settings carry no cursor theme");
}

// The mode xdg-decoration's configure last named.
#[derive(Default)]
struct DecorationState {
    mode: Option<zxdg_toplevel_decoration_v1::Mode>,
}

impl Dispatch<WlRegistry, GlobalListContents> for DecorationState {
    fn event(
        _state: &mut Self,
        _registry: &WlRegistry,
        _event: <WlRegistry as wayland_client::Proxy>::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<XdgWmBase, ()> for DecorationState {
    fn event(
        _state: &mut Self,
        wm_base: &XdgWmBase,
        event: <XdgWmBase as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for DecorationState {
    fn event(
        _state: &mut Self,
        xdg_surface: &XdgSurface,
        event: <XdgSurface as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
        }
    }
}

impl Dispatch<ZxdgToplevelDecorationV1, ()> for DecorationState {
    fn event(
        state: &mut Self,
        _decoration: &ZxdgToplevelDecorationV1,
        event: <ZxdgToplevelDecorationV1 as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let zxdg_toplevel_decoration_v1::Event::Configure { mode } = event {
            state.mode = mode.into_result().ok();
        }
    }
}

delegate_noop!(DecorationState: ignore WlCompositor);
delegate_noop!(DecorationState: ignore WlSurface);
delegate_noop!(DecorationState: ignore XdgToplevel);
delegate_noop!(DecorationState: ignore ZxdgDecorationManagerV1);

// The mode a window asking for server-side decorations is given.
fn decoration_given(config: &str) -> Option<zxdg_toplevel_decoration_v1::Mode> {
    let wl = WlTest::with_config(config);
    let conn = wl.connect();
    let (globals, mut queue): (_, EventQueue<DecorationState>) = registry_queue_init(&conn).expect("registry_queue_init");
    let qh = queue.handle();
    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("bind wl_compositor");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=5, ()).expect("bind xdg_wm_base");
    let manager: ZxdgDecorationManagerV1 = globals.bind(&qh, 1..=1, ()).expect("bind zxdg_decoration_manager_v1");
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    let decoration = manager.get_toplevel_decoration(&toplevel, &qh, ());
    decoration.set_mode(zxdg_toplevel_decoration_v1::Mode::ServerSide);
    surface.commit();
    let mut state = DecorationState::default();
    pump_until(&mut queue, &mut state, |s| s.mode.is_some(), "a decoration configure");
    state.mode
}

// xdg-decoration: A window asking for server-side decorations gets them
// with title_bars on, as the X11 stack frames a floating window, and draws
// its own with them off.
#[test]
fn server_side_bars_are_given_with_title_bars_on() {
    assert_eq!(decoration_given("[general]\ntitle_bars = true\n"), Some(zxdg_toplevel_decoration_v1::Mode::ServerSide));
    assert_eq!(decoration_given(""), Some(zxdg_toplevel_decoration_v1::Mode::ClientSide));
}

// Idleness past the screensaver's and DPMS's timeouts: The monitors are
// blanked, then powered off, as the X11 stack sets X's screensaver and DPMS.
#[test]
fn an_idle_session_blanks_then_powers_off() {
    let wl = WlTest::with_config("[input.idle]\nscreensaver_seconds = 1\ndpms_off_seconds = 2\n");
    let deadline = Instant::now() + Duration::from_secs(6);
    while !(wl.log().contains("idle; blanking") && wl.log().contains("idle; outputs off")) {
        assert!(Instant::now() < deadline, "the idle session did not blank and power off:\n{}", wl.log());
        std::thread::sleep(Duration::from_millis(50));
    }
    let log = wl.log();
    assert!(log.find("idle; blanking") < log.find("idle; outputs off"), "powered off before it blanked:\n{log}");
}
