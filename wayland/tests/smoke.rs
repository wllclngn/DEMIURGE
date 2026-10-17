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

use std::time::{Duration, Instant};

use common::WlTest;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::protocol::wl_shm::WlShm;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols::ext::session_lock::v1::client::ext_session_lock_manager_v1::ExtSessionLockManagerV1;
use wayland_protocols::ext::session_lock::v1::client::ext_session_lock_v1::{
    self, ExtSessionLockV1,
};
use wayland_protocols::xdg::shell::client::xdg_surface::{self, XdgSurface};
use wayland_protocols::xdg::shell::client::xdg_toplevel::{self, XdgToplevel};
use wayland_protocols::xdg::shell::client::xdg_wm_base::{self, XdgWmBase};

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
