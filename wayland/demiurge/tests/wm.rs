// The headless Wayland Wm: Tag views, layouts, quit, focus-relative
// actions and a real xdg_toplevel moved between tags and zoomed.

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
use demiurge_core::config::{Config, Keybind};
use smithay::backend::input::{ButtonState, KeyState};
use smithay::input::keyboard::XkbConfig;
use smithay::output::{Mode, PhysicalProperties, Subpixel};
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
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel as server_toplevel;

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
    headless_wm_with(Config::default())
}

fn headless_wm_with(cfg: Config) -> (Display<Wm>, Wm) {
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
            serial_number: String::new(),
        },
    );
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
    // refocus_active_tag calls set_keyboard_focus, which needs a
    // keyboard on the seat.
    wm.seat
        .add_keyboard(XkbConfig::default(), 200, 25)
        .expect("add_keyboard");
    // Give it a real monitor so the active-tag / work-area math has
    // non-degenerate geometry.
    let mode = Mode { size: (1280, 800).into(), refresh: 60_000 };
    output.change_current_state(Some(mode), None, None, Some((0, 0).into()));
    wm.set_monitors(&[output]);
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

// A request the worker makes of its toplevel.
enum Request {
    Maximize,
    Unmaximize,
    Title(String),
    // Its window geometry, 0,0 and this size, committed.
    Geometry(i32, i32),
}

// A real xdg_toplevel on a worker thread, mapped against the headless Wm
// (the server is pumped on the test thread). Dropping it stops the worker,
// which destroys the toplevel.
struct Toplevel {
    requests: mpsc::Sender<Option<Request>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Toplevel {
    fn map(display: &mut Display<Wm>, wm: &mut Wm) -> Self {
        Self::map_as(display, wm, None)
    }

    // Mapped with `app_id` set before its first commit, as a client names
    // itself.
    fn map_as(display: &mut Display<Wm>, wm: &mut Wm, app_id: Option<&str>) -> Self {
        let app_id = app_id.map(str::to_string);
        let before = wm.clients.len();
        let mut dh = display.handle();
        let (server_end, client_end) = UnixStream::pair().expect("socketpair");
        dh.insert_client(server_end, std::sync::Arc::new(ClientState::default()))
            .expect("insert_client");

        let (requests, inbox) = mpsc::channel::<Option<Request>>();
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
            if let Some(app_id) = app_id {
                toplevel.set_app_id(app_id);
            }
            surface.commit();
            let _ = conn.flush();
            // Keep the protocol objects alive until told to stop.
            while let Ok(Some(request)) = inbox.recv() {
                match request {
                    Request::Maximize => toplevel.set_maximized(),
                    Request::Unmaximize => toplevel.unset_maximized(),
                    Request::Title(title) => toplevel.set_title(title),
                    Request::Geometry(w, h) => {
                        xsurf.set_window_geometry(0, 0, w, h);
                        surface.commit();
                    }
                }
                let _ = conn.flush();
            }
            drop((toplevel, xsurf, surface, wm_base, compositor, queue, conn));
        });
        let top = Self { requests, worker: Some(worker) };
        // The initial commit places the toplevel; with no cherrypie to ask,
        // a ManagedClient is pushed at once.
        assert!(pump_until(display, wm, |wm| wm.clients.len() > before), "toplevel never mapped");
        top
    }

    fn ask(&self, request: Request) {
        self.requests.send(Some(request)).ok();
    }
}

impl Drop for Toplevel {
    fn drop(&mut self) {
        self.requests.send(None).ok();
        if let Some(worker) = self.worker.take() {
            worker.join().ok();
        }
    }
}

fn pump_until(display: &mut Display<Wm>, wm: &mut Wm, mut done: impl FnMut(&Wm) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done(wm) {
        let _ = display.dispatch_clients(wm);
        let _ = display.flush_clients();
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    true
}

// Exercise the focus-relative tag actions on a genuinely managed client.
#[test]
fn move_to_tag_hides_client_and_refocus_follows() {
    let (mut display, mut wm) = headless_wm();
    let _top = Toplevel::map(&mut display, &mut wm);

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
}

fn geometry(wm: &Wm) -> (i32, i32, u32, u32) {
    let c = &wm.clients[0];
    (c.x, c.y, c.w, c.h)
}

fn maximized(wm: &Wm) -> bool {
    wm.clients[0].toplevel.xdg().expect("an xdg toplevel").with_pending_state(|s| s.states.contains(server_toplevel::State::Maximized))
}

// Zoom fills the work area below the bar and tells the client it is
// maximized; the client's own maximize request is the same zoom.
#[test]
fn zoom_fills_the_work_area_and_maximize_requests_zoom() {
    let (mut display, mut wm) = headless_wm();
    let top = Toplevel::map(&mut display, &mut wm);
    let bar = wm.bar_height;
    let work_area = (0, bar as i32, 1280, 800 - bar);
    let before = geometry(&wm);

    wm.handle_action(&Action::ZoomIn);
    assert_eq!(geometry(&wm), work_area);
    assert!(maximized(&wm));
    wm.handle_action(&Action::ZoomOut);
    assert_eq!(geometry(&wm), before);
    assert!(!maximized(&wm));

    top.ask(Request::Maximize);
    assert!(pump_until(&mut display, &mut wm, |wm| wm.clients[0].saved_zoom.is_some()), "set_maximized did not zoom");
    assert_eq!(geometry(&wm), work_area);
    top.ask(Request::Unmaximize);
    assert!(pump_until(&mut display, &mut wm, |wm| wm.clients[0].saved_zoom.is_none()), "unset_maximized did not zoom out");
    assert_eq!(geometry(&wm), before);

    // A tiled client floats out for the zoom and goes back into the tile.
    wm.handle_action(&Action::ToggleLayout);
    assert_eq!(wm.layouts[0], Layout::Tile);
    let tile = geometry(&wm);
    wm.handle_action(&Action::ZoomIn);
    assert!(wm.clients[0].floating);
    wm.handle_action(&Action::ZoomOut);
    assert!(!wm.clients[0].floating);
    assert_eq!(geometry(&wm), tile);
}

// Keys by their evdev codes, pressed through the compositor's keyboard
// filter as a keyboard would.
const KEY_ESC: u32 = 1;
const KEY_TAB: u32 = 15;
const KEY_ENTER: u32 = 28;
const KEY_LEFTMETA: u32 = 125;

fn evdev(c: char) -> u32 {
    const ROWS: [(&str, u32); 4] = [("1234567890", 2), ("qwertyuiop", 16), ("asdfghjkl", 30), ("zxcvbnm", 44)];
    match c {
        ' ' => 57,
        '-' => 12,
        '/' => 53,
        _ => ROWS
            .iter()
            .find_map(|(row, first)| row.find(c).map(|i| first + i as u32))
            .unwrap_or_else(|| panic!("no key for {c:?}")),
    }
}

fn chord(wm: &mut Wm, codes: &[u32]) {
    for &code in codes {
        wm.key((code + 8).into(), KeyState::Pressed, 0);
    }
    for &code in codes.iter().rev() {
        wm.key((code + 8).into(), KeyState::Released, 0);
    }
}

fn type_text(wm: &mut Wm, text: &str) {
    for c in text.chars() {
        chord(wm, &[evdev(c)]);
    }
}

fn run_prompt_bound(wm: &mut Wm) {
    wm.bindings = demiurge_wl::keys::compile(&[Keybind {
        mods: vec!["Super".into()],
        key: "r".into(),
        action: "run_prompt".into(),
        args: None,
    }]);
}

// Super+R opens the run prompt and every key goes to it, Super+R again
// only typing an r; Tab takes the completion and Return runs the command.
#[test]
fn the_run_prompt_runs_what_is_typed() {
    let (_d, mut wm) = headless_wm();
    run_prompt_bound(&mut wm);
    let mark = format!("/tmp/demiurge-wl-prompt-{}", std::process::id());
    let _ = std::fs::remove_file(&mark);

    chord(&mut wm, &[KEY_LEFTMETA, evdev('r')]);
    assert!(wm.prompt.is_some(), "Super+R opened no prompt");
    chord(&mut wm, &[KEY_LEFTMETA, evdev('r')]);
    assert_eq!(wm.prompt.as_ref().unwrap().input, "r");
    chord(&mut wm, &[0x0e]); // BackSpace

    type_text(&mut wm, "touc");
    chord(&mut wm, &[KEY_TAB]);
    assert_eq!(wm.prompt.as_ref().unwrap().input, "touch");
    type_text(&mut wm, &format!(" {mark}"));
    chord(&mut wm, &[KEY_ENTER]);
    assert!(wm.prompt.is_none(), "Return left the prompt open");

    let deadline = Instant::now() + Duration::from_secs(5);
    while !std::path::Path::new(&mark).exists() {
        assert!(Instant::now() < deadline, "the command typed never ran");
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = std::fs::remove_file(&mark);
}

// Escape closes the prompt with nothing run, and the binds work again.
#[test]
fn escape_closes_the_run_prompt() {
    let (_d, mut wm) = headless_wm();
    run_prompt_bound(&mut wm);
    chord(&mut wm, &[KEY_LEFTMETA, evdev('r')]);
    type_text(&mut wm, "x");
    chord(&mut wm, &[KEY_ESC]);
    assert!(wm.prompt.is_none(), "Escape left the prompt open");
    chord(&mut wm, &[KEY_LEFTMETA, evdev('r')]);
    let prompt = wm.prompt.as_ref().expect("Super+R after Escape opened no prompt");
    assert_eq!(prompt.input, "");
}

// The bar's middle: Nothing for an untitled window, then the focused
// window's title, and a notification over it.
#[test]
fn the_bar_shows_the_focused_title_and_a_notification_over_it() {
    let (mut display, mut wm) = headless_wm();
    let top = Toplevel::map(&mut display, &mut wm);
    assert_eq!(wm.bar_center(), None, "an untitled window put something in the bar");

    top.ask(Request::Title("kitty".into()));
    assert!(
        pump_until(&mut display, &mut wm, |wm| wm.bar_center() == Some(("kitty".into(), false))),
        "the focused window's title never reached the bar: {:?}",
        wm.bar_center()
    );

    let text = "NOTIFICATION: System Volume, 50%.";
    wm.notify(text.into());
    assert_eq!(wm.bar_center(), Some((text.into(), true)));
}

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

fn pointer_at(wm: &mut Wm, x: i32, y: i32) {
    wm.pointer_moved((x as f64, y as f64).into(), None, 0);
}

fn click(wm: &mut Wm, button: u32, pressed: bool) {
    wm.pointer_button(button, if pressed { ButtonState::Pressed } else { ButtonState::Released }, 0);
}

// A cherrypie stand-in on the policy socket: Every request it is sent.
struct Recorder {
    requests: std::sync::Arc<std::sync::Mutex<Vec<cherrypie::api::Request>>>,
    path: std::path::PathBuf,
}

impl Recorder {
    fn link(wm: &mut Wm) -> Self {
        use std::io::BufRead;
        let display = format!("drag-test-{}", std::process::id());
        let path = cherrypie::api::socket_path(&display);
        let _ = std::fs::remove_file(&path);
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind the policy socket");
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = requests.clone();
        std::thread::spawn(move || {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            for line in std::io::BufReader::new(stream).lines().map_while(Result::ok) {
                if let Ok(request) = serde_json::from_str(&line) {
                    seen.lock().unwrap().push(request);
                }
            }
        });
        wm.policy = cherrypie::client::Policy::new(&display);
        wm.policy.link();
        Recorder { requests, path }
    }

    // The bodies of the requests sent to `path`, once there are `n`.
    fn sent(&self, op: &str, path: &str, n: usize) -> Vec<serde_json::Value> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let found: Vec<_> =
                self.requests.lock().unwrap().iter().filter(|r| r.op == op && r.path == path).map(|r| r.body.clone()).collect();
            if found.len() >= n || Instant::now() >= deadline {
                return found;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

// Super and the left button move the window under the pointer out of its
// tile, Super and the right button resize it by its corner, and where the
// drag left it goes to cherrypie's window memory as the X11 stack sends it.
// A press without Super is the window's.
#[test]
fn super_drag_moves_and_resizes_a_window_and_tells_cherrypie() {
    let (mut display, mut wm) = headless_wm();
    let _top = Toplevel::map(&mut display, &mut wm);
    // Linked once the window is up: A window mapping would wait for its
    // answer.
    let cherrypie = Recorder::link(&mut wm);
    wm.seat.add_pointer();
    wm.handle_action(&Action::ToggleLayout);
    assert_eq!(wm.layouts[0], Layout::Tile);
    let (x, y, w, h) = geometry(&wm);
    let id = wm.clients[0].id;

    pointer_at(&mut wm, x + 20, y + 20);
    click(&mut wm, BTN_LEFT, true);
    pointer_at(&mut wm, x + 120, y + 80);
    click(&mut wm, BTN_LEFT, false);
    assert_eq!(geometry(&wm), (x, y, w, h), "a press without Super moved the window");

    pointer_at(&mut wm, x + 20, y + 20);
    wm.key((KEY_LEFTMETA + 8).into(), KeyState::Pressed, 0);
    click(&mut wm, BTN_LEFT, true);
    pointer_at(&mut wm, x + 120, y + 80);
    click(&mut wm, BTN_LEFT, false);
    assert!(wm.clients[0].floating, "the window dragged stayed in its tile");
    assert_eq!(geometry(&wm), (x + 100, y + 60, w, h));

    click(&mut wm, BTN_RIGHT, true);
    pointer_at(&mut wm, x + 170, y + 110);
    click(&mut wm, BTN_RIGHT, false);
    wm.key((KEY_LEFTMETA + 8).into(), KeyState::Released, 0);
    assert_eq!(geometry(&wm), (x + 100, y + 60, w + 50, h + 30));

    let rect = |body: &serde_json::Value| {
        let r = &body["rect"];
        (r["x"].as_i64(), r["y"].as_i64(), r["w"].as_u64(), r["h"].as_u64())
    };
    let sent = cherrypie.sent("PUT", &format!("/windows/{}/geometry", id as u32), 2);
    let rects: Vec<_> = sent.iter().map(rect).collect();
    let (x, y, w, h) = (x as i64, y as i64, w as u64, h as u64);
    assert_eq!(rects, vec![(Some(x + 100), Some(y + 60), Some(w), Some(h)), (Some(x + 100), Some(y + 60), Some(w + 50), Some(h + 30))]);
}

// A tagged [[startup.spawn]] sends the first window of its class to its
// tag, as on X11; the next window of that class goes where any window goes.
#[test]
fn a_tagged_spawns_first_window_lands_on_its_tag() {
    let (mut display, mut wm) = headless_wm();
    wm.pending_spawns = vec![("montauk-term".into(), 2)];
    let _first = Toplevel::map_as(&mut display, &mut wm, Some("montauk-term"));
    assert_eq!(wm.clients[0].tag, 2, "the spawn's window missed its tag");
    assert!(wm.pending_spawns.is_empty(), "the spawn was not used up");
    let _second = Toplevel::map_as(&mut display, &mut wm, Some("montauk-term"));
    assert_eq!(wm.clients[1].tag, wm.active_tags[0]);
}

const KEY_DOWN: u32 = 108;

// Minimize hides a window on its tag, out of the tiles and focus. The tag's
// key again opens the tag menu, the minimized window marked; Escape closes
// it, and Down then Return brings the window back, focused.
#[test]
fn a_minimized_window_comes_back_from_the_tag_menu() {
    let (mut display, mut wm) = headless_wm();
    let _a = Toplevel::map(&mut display, &mut wm);
    let _b = Toplevel::map(&mut display, &mut wm);
    wm.handle_action(&Action::ToggleLayout);
    assert_eq!(wm.layouts[0], Layout::Tile);
    let (a, b) = (wm.clients[0].id, wm.clients[1].id);
    wm.focus_client(b);
    let half = geometry(&wm);

    wm.handle_action(&Action::Minimize);
    assert!(wm.clients[1].minimized, "the focused window was not minimized");
    assert_eq!(wm.focused, Some(a), "focus stayed on the minimized window");
    assert!(geometry(&wm).2 > half.2, "the other window did not take the minimized one's tile");

    wm.handle_action(&Action::ViewTag(0));
    let menu = &wm.tag_menu.as_ref().expect("the tag's key again opened no menu").menu;
    let listed: Vec<_> = menu.entries.iter().map(|e| (e.id, e.minimized)).collect();
    assert_eq!(listed, vec![(a, false), (b, true)]);
    assert_eq!(menu.selected, Some(0));
    chord(&mut wm, &[KEY_ESC]);
    assert!(wm.tag_menu.is_none(), "Escape left the menu open");

    wm.handle_action(&Action::ViewTag(0));
    chord(&mut wm, &[KEY_DOWN]);
    chord(&mut wm, &[KEY_ENTER]);
    assert!(wm.tag_menu.is_none(), "Return left the menu open");
    assert!(!wm.clients[1].minimized, "the window picked stayed minimized");
    assert_eq!(wm.focused, Some(b), "the window picked was not focused");
}

// A floating window sizes itself: Its window geometry becomes its size in
// the Wm, the first centering it on its monitor below the bar, so that
// Super+drag finds it where it is shown. A later size keeps its place.
#[test]
fn a_floating_window_takes_its_own_size_centered() {
    let (mut display, mut wm) = headless_wm();
    let top = Toplevel::map(&mut display, &mut wm);
    assert_eq!(wm.layouts[0], Layout::Floating);
    top.ask(Request::Geometry(400, 300));
    assert!(pump_until(&mut display, &mut wm, |wm| geometry(wm).2 == 400), "the window's size never reached the Wm");
    let bar = wm.bar_height as i32;
    assert_eq!(geometry(&wm), (440, bar + (800 - bar - 300) / 2, 400, 300));
    top.ask(Request::Geometry(500, 200));
    assert!(pump_until(&mut display, &mut wm, |wm| geometry(wm).2 == 500), "a later size never reached the Wm");
    assert_eq!((geometry(&wm).0, geometry(&wm).3), (440, 200), "a later size moved the window");
}

// Title bars: With title_bars on, a floating window in server-side mode
// carries its bar inside its size, its client told the room below the bar
// and drawn there. A left press on the zoom light zooms it, the bar kept,
// and again zooms it back; on the minimize light minimizes it.
#[test]
fn a_titled_window_carries_its_bar_and_its_lights_act() {
    use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
    let mut cfg = Config::default();
    cfg.general.title_bars = true;
    let (mut display, mut wm) = headless_wm_with(cfg);
    wm.seat.add_pointer();
    let top = Toplevel::map(&mut display, &mut wm);
    // Server-side, as xdg-decoration's handler sets it for a window asking.
    wm.clients[0].toplevel.xdg().unwrap().with_pending_state(|s| s.decoration_mode = Some(Mode::ServerSide));
    top.ask(Request::Geometry(400, 300));
    assert!(pump_until(&mut display, &mut wm, |wm| geometry(wm).2 == 400), "the window's size never reached the Wm");
    let (x, y, w, h) = geometry(&wm);
    assert_eq!((w, h), (400, 330), "the bar is not inside the window's size");
    assert_eq!(wm.clients[0].surface_origin().y, y + 30, "the client is not drawn below its bar");

    pointer_at(&mut wm, x + 58, y + 15);
    click(&mut wm, BTN_LEFT, true);
    click(&mut wm, BTN_LEFT, false);
    assert!(wm.clients[0].saved_zoom.is_some(), "the zoom light did not zoom the window");
    let bar = wm.bar_height;
    let room = wm.clients[0].toplevel.xdg().unwrap().with_pending_state(|s| s.size);
    assert_eq!(room, Some((1280, (800 - bar - 30) as i32).into()), "the zoomed client was not told the room below its bar");

    let (zx, zy, _, _) = geometry(&wm);
    pointer_at(&mut wm, zx + 58, zy + 15);
    click(&mut wm, BTN_LEFT, true);
    click(&mut wm, BTN_LEFT, false);
    assert!(wm.clients[0].saved_zoom.is_none(), "the zoom light did not zoom the window back");
    assert_eq!(geometry(&wm), (x, y, w, h));

    pointer_at(&mut wm, x + 38, y + 15);
    click(&mut wm, BTN_LEFT, true);
    click(&mut wm, BTN_LEFT, false);
    assert!(wm.clients[0].minimized, "the minimize light did not minimize the window");
}

const KEY_LEFTALT: u32 = 56;

// Alt+Tab as on X11: While Alt is held, each Tab focuses the next window,
// most recently used first, the ring staying as it was; letting Alt go
// puts the window chosen at the ring's front.
#[test]
fn alt_tab_walks_the_ring_and_commits_on_release() {
    let (mut display, mut wm) = headless_wm();
    wm.bindings = demiurge_wl::keys::compile(&[Keybind {
        mods: vec!["Alt".into()],
        key: "Tab".into(),
        action: "mru_next".into(),
        args: None,
    }]);
    let _a = Toplevel::map(&mut display, &mut wm);
    let _b = Toplevel::map(&mut display, &mut wm);
    let _c = Toplevel::map(&mut display, &mut wm);
    let (a, b, c) = (wm.clients[0].id, wm.clients[1].id, wm.clients[2].id);
    wm.focus_client(a);
    wm.focus_client(b);
    wm.focus_client(c);
    assert_eq!(wm.mru.as_slice(), &[c, b, a]);

    wm.key((KEY_LEFTALT + 8).into(), KeyState::Pressed, 0);
    chord(&mut wm, &[KEY_TAB]);
    assert_eq!(wm.focused, Some(b));
    chord(&mut wm, &[KEY_TAB]);
    assert_eq!(wm.focused, Some(a));
    assert_eq!(wm.mru.as_slice(), &[c, b, a], "the ring moved while Alt was held");
    assert!(wm.cycle.is_some());

    wm.key((KEY_LEFTALT + 8).into(), KeyState::Released, 0);
    assert!(wm.cycle.is_none(), "letting Alt go left the cycle on");
    assert_eq!(wm.mru.as_slice(), &[a, c, b], "the window chosen is not the ring's front");
    assert_eq!(wm.focused, Some(a));
}
