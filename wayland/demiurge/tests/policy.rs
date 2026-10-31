// demiurge-wl as the client of cherrypie's window-policy API, against a
// scripted cherrypie in this test, as x11/tests/policy.rs runs DEMIURGE's
// X11 stack: A toplevel's first configure waits for the answer and
// carries the geometry it gives; the answer's tag and focus are honored,
// and a window it says nothing of focusing is focused;
// an overdue answer falls back to DEMIURGE's default; the monitors come
// first on a new link; a closed window is reported; an X11 window on
// XWayland is asked about before it maps. cherrypie's socket is keyed by
// XWayland's display, so these need Xwayland installed and skip where it
// is not.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cherrypie::api::{display_tag, NewWindow, Placement, Rect, Request, Response, Tag};
use common::WlTest;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols::xdg::shell::client::xdg_surface::{self, XdgSurface};
use wayland_protocols::xdg::shell::client::xdg_toplevel::{self, XdgToplevel};
use wayland_protocols::xdg::shell::client::xdg_wm_base::{self, XdgWmBase};

const WAIT: Duration = Duration::from_secs(5);

// A cherrypie stand-in: Records every request and when it came, and
// answers POST /windows with `answer` (None: never answer).
struct FakeCherrypie {
    requests: Arc<Mutex<Vec<(Request, Instant)>>>,
    answered: Arc<Mutex<Option<Instant>>>,
}

impl FakeCherrypie {
    fn start(runtime: &Path, display: &str, answer: Option<Placement>) -> Self {
        let path = runtime.join(format!("cherrypie-{}.sock", display_tag(display)));
        let listener = UnixListener::bind(&path).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let answered = Arc::new(Mutex::new(None));
        let (seen, when) = (requests.clone(), answered.clone());
        std::thread::spawn(move || {
            let Ok((stream, _)) = listener.accept() else { return };
            let mut writer = stream.try_clone().unwrap();
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                let request: Request = serde_json::from_str(&line).unwrap();
                let response = match (request.op.as_str(), &answer) {
                    ("POST", Some(p)) => Some(Response::new(request.id, 200, p.clone())),
                    ("POST", None) => None,
                    _ => Some(Response::new(request.id, 204, ())),
                };
                let post = request.op == "POST";
                seen.lock().unwrap().push((request, Instant::now()));
                if let Some(r) = response {
                    if post {
                        *when.lock().unwrap() = Some(Instant::now());
                    }
                    let _ = writer.write_all(r.line().as_bytes());
                }
            }
        });
        Self { requests, answered }
    }

    fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().iter().map(|(r, _)| r.clone()).collect()
    }

    // The first request with `op` on a path starting `path`, and when it
    // came, once it has.
    fn wait_for(&self, op: &str, path: &str) -> Option<(Request, Instant)> {
        let deadline = Instant::now() + WAIT;
        loop {
            let found = self.requests.lock().unwrap().iter().find(|(r, _)| r.op == op && r.path.starts_with(path)).cloned();
            if found.is_some() || Instant::now() >= deadline {
                return found;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    // The window a POST /windows described.
    fn asked(&self) -> NewWindow {
        let (request, _) = self.wait_for("POST", "/windows").expect("cherrypie was never asked");
        serde_json::from_value(request.body).expect("POST /windows carries a NewWindow")
    }
}

// The compositor, with XWayland where `xwayland` asks for it (None where
// Xwayland is not installed), and the scripted cherrypie on the
// compositor's own socket name, where the Wayland cherrypie serves.
fn start(answer: Option<Placement>, xwayland: bool) -> Option<(WlTest, FakeCherrypie)> {
    if xwayland && !Path::new("/usr/bin/Xwayland").exists() {
        eprintln!("no /usr/bin/Xwayland here; skipped");
        return None;
    }
    let wl = if xwayland { WlTest::with_xwayland("") } else { WlTest::new() };
    let display = wl.socket_path.file_name().expect("a socket name").to_string_lossy().into_owned();
    let fake = FakeCherrypie::start(wl.runtime_dir(), &display, answer);
    Some((wl, fake))
}

// The compositor's log line holding `needle`, its color codes left out.
fn logged(wl: &WlTest, needle: &str) -> String {
    let line = wl.wait_for_log(needle, WAIT).unwrap_or_else(|| panic!("never logged '{needle}':\n{}", wl.log()));
    let mut plain = String::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            chars.by_ref().find(|&c| c == 'm');
        } else {
            plain.push(c);
        }
    }
    plain
}

// A client's view of its window: When its first configure came, with the
// size its toplevel configure gave, and whether any configure has said the
// window is the active one.
#[derive(Default)]
struct Window {
    configured: Option<Instant>,
    size: Option<(i32, i32)>,
    activated: bool,
}

impl Dispatch<WlRegistry, GlobalListContents> for Window {
    fn event(_: &mut Self, _: &WlRegistry, _: <WlRegistry as wayland_client::Proxy>::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<XdgWmBase, ()> for Window {
    fn event(_: &mut Self, wm_base: &XdgWmBase, event: xdg_wm_base::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for Window {
    fn event(state: &mut Self, xdg: &XdgSurface, event: xdg_surface::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg.ack_configure(serial);
            state.configured.get_or_insert_with(Instant::now);
        }
    }
}

impl Dispatch<XdgToplevel, ()> for Window {
    fn event(state: &mut Self, _: &XdgToplevel, event: xdg_toplevel::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_toplevel::Event::Configure { width, height, states } = event {
            if state.configured.is_none() {
                state.size = Some((width, height));
            }
            let activated = xdg_toplevel::State::Activated as u32;
            state.activated |= states.chunks_exact(4).any(|s| u32::from_ne_bytes([s[0], s[1], s[2], s[3]]) == activated);
        }
    }
}

delegate_noop!(Window: ignore WlCompositor);
delegate_noop!(Window: ignore WlSurface);

// A toplevel with an app_id and a title, its initial commit made.
struct Client {
    queue: EventQueue<Window>,
    state: Window,
    toplevel: XdgToplevel,
    xdg: XdgSurface,
    surface: WlSurface,
    conn: Connection,
}

fn open(wl: &WlTest, app_id: &str) -> Client {
    let conn = wl.connect();
    let (globals, queue) = registry_queue_init::<Window>(&conn).expect("registry_queue_init");
    let qh = queue.handle();
    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("bind wl_compositor");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=5, ()).expect("bind xdg_wm_base");
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_app_id(app_id.into());
    toplevel.set_title(format!("{app_id} window"));
    surface.commit();
    conn.flush().expect("flush");
    Client { queue, state: Window::default(), toplevel, xdg, surface, conn }
}

impl Client {
    // When the first configure came, once it has.
    fn configured(&mut self) -> Option<Instant> {
        let deadline = Instant::now() + WAIT;
        while self.state.configured.is_none() && Instant::now() < deadline {
            self.queue.roundtrip(&mut self.state).expect("roundtrip");
            std::thread::sleep(Duration::from_millis(10));
        }
        self.state.configured
    }

    // Dispatch for `time`, taking every configure that comes.
    fn settle(&mut self, time: Duration) {
        let deadline = Instant::now() + time;
        while Instant::now() < deadline {
            self.queue.roundtrip(&mut self.state).expect("roundtrip");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn close(self) {
        self.toplevel.destroy();
        self.xdg.destroy();
        self.surface.destroy();
        self.conn.flush().expect("flush");
    }
}

#[test]
fn a_toplevels_first_configure_waits_for_cherrypie_and_carries_its_geometry() {
    let at = Rect { x: 100, y: 200, w: 500, h: 300 };
    let Some((wl, fake)) = start(Some(Placement { geometry: Some(at), ..Placement::default() }), false) else {
        return;
    };
    let mut client = open(&wl, "policy.placed");
    let configured = client.configured().unwrap_or_else(|| panic!("no first configure:\n{}", wl.log()));
    let answered = fake.answered.lock().unwrap().expect("cherrypie never answered");
    assert!(answered <= configured, "the first configure came before cherrypie's answer");
    assert_eq!(client.state.size, Some((500, 300)), "the first configure carries cherrypie's size");

    let ops: Vec<(String, String)> = fake.requests().into_iter().map(|r| (r.op, r.path)).collect();
    assert_eq!(ops.first(), Some(&("PUT".into(), "/monitors".into())), "monitors first on a new link");
    let window = fake.asked();
    assert_eq!(
        (window.class.as_str(), window.title.as_str(), window.window_type.as_str(), window.transient),
        ("policy.placed", "policy.placed window", "normal", false),
        "the window as its client set it up before the initial commit"
    );
}

#[test]
fn the_answers_tag_is_honored() {
    // The default tags are X, Y and Z: "Z" is tag 2, out of view.
    let answer = Placement { tag: Some(Tag::Name("Z".into())), ..Placement::default() };
    let Some((wl, _fake)) = start(Some(answer), false) else {
        return;
    };
    let mut client = open(&wl, "policy.elsewhere");
    client.configured().unwrap_or_else(|| panic!("no first configure:\n{}", wl.log()));
    let mapped = logged(&wl, "toplevel mapped");
    assert!(mapped.contains("tag=2"), "managed on cherrypie's tag Z: {mapped}");
}

#[test]
fn a_window_cherrypie_says_not_to_focus_stays_inactive() {
    let Some((wl, _fake)) = start(Some(Placement { focus: Some(false), ..Placement::default() }), false) else {
        return;
    };
    let mut client = open(&wl, "policy.unfocused");
    client.configured().unwrap_or_else(|| panic!("no first configure:\n{}", wl.log()));
    client.settle(Duration::from_millis(300));
    assert!(!client.state.activated, "activated on the tag in view though cherrypie said not to focus it");
}

// The same window with no word on focus is focused, as by default.
#[test]
fn a_window_placed_with_no_word_on_focus_is_activated() {
    let Some((wl, _fake)) = start(Some(Placement::default()), false) else {
        return;
    };
    let mut client = open(&wl, "policy.focused");
    client.configured().unwrap_or_else(|| panic!("no first configure:\n{}", wl.log()));
    client.settle(Duration::from_millis(300));
    assert!(client.state.activated, "a window placed with no word on focus was not activated:\n{}", wl.log());
}

#[test]
fn an_overdue_answer_falls_back_to_the_default() {
    let Some((wl, fake)) = start(None, false) else {
        return;
    };
    let mut client = open(&wl, "policy.silent");
    let configured = client.configured().unwrap_or_else(|| panic!("no first configure:\n{}", wl.log()));
    let (_, asked) = fake.wait_for("POST", "/windows").expect("cherrypie was never asked");
    assert!(configured.duration_since(asked) >= Duration::from_millis(80), "configured before the answer was due");
    let mapped = logged(&wl, "toplevel mapped");
    assert!(mapped.contains("tag=0"), "managed on the focused monitor's tag: {mapped}");
}

#[test]
fn a_closed_toplevel_is_reported() {
    let Some((wl, fake)) = start(Some(Placement::default()), false) else {
        return;
    };
    let mut client = open(&wl, "policy.closed");
    client.configured().unwrap_or_else(|| panic!("no first configure:\n{}", wl.log()));
    let window = fake.asked();
    client.close();
    let (gone, _) = fake.wait_for("DELETE", "/windows/").unwrap_or_else(|| panic!("no DELETE:\n{}", wl.log()));
    assert_eq!(gone.path, format!("/windows/{}", window.window));
}

#[test]
fn an_x11_window_is_asked_about_before_it_maps() {
    use smithay::reexports::x11rb;
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, CreateWindowAux, PropMode, WindowClass};
    use x11rb::wrapper::ConnectionExt as _;

    let answer = Placement { tag: Some(Tag::Index(1)), ..Placement::default() };
    let Some((wl, fake)) = start(Some(answer), true) else {
        return;
    };
    let (x, screen) = x11rb::connect(Some(&wl.xwayland_display())).expect("connect to the compositor's XWayland");
    let root = x.setup().roots[screen].root;
    let win = x.generate_id().unwrap();
    x.create_window(0, win, root, 0, 0, 300, 200, 0, WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new()).unwrap();
    x.change_property8(PropMode::REPLACE, win, AtomEnum::WM_CLASS, AtomEnum::STRING, b"policyx\0PolicyX\0").unwrap();
    x.change_property8(PropMode::REPLACE, win, AtomEnum::WM_NAME, AtomEnum::STRING, b"Policy X").unwrap();
    x.map_window(win).unwrap();
    x.flush().unwrap();

    let window = fake.asked();
    assert_eq!((window.class.as_str(), window.title.as_str()), ("PolicyX", "Policy X"), "the X11 window as its client named it");
    assert_eq!(window.requested, Rect { x: 0, y: 0, w: 300, h: 200 }, "the size the X11 window asked for");
    let mapped = logged(&wl, "toplevel mapped");
    assert!(mapped.contains("tag=1"), "managed on cherrypie's tag 1: {mapped}");
}
