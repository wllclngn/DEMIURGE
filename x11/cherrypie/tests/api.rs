// The window-policy API end to end: The real cherrypie binary on a headless
// Xvfb, with this test as the window manager on its socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use cherrypie::api::{Monitor, Moved, NewWindow, Placement, Rect, Request, Response, Tag};
use x11rb::connection::Connection;
use x11rb::rust_connection::RustConnection;

mod common;
use common::*;

const BAR: i32 = 24;

struct Client {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    next: u64,
}

impl Client {
    fn connect(cherrypie: &Cherrypie) -> Self {
        let path = cherrypie.socket();
        assert!(wait_until(|| path.exists()), "no socket at {}", path.display());
        let stream = UnixStream::connect(&path).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let reader = BufReader::new(stream.try_clone().unwrap());
        let mut c = Self { stream, reader, next: 1 };
        let work_area = Rect { x: 0, y: BAR, w: SCREEN_W, h: SCREEN_H - BAR as u32 };
        let monitors = vec![Monitor {
            name: "M".into(),
            rect: Rect { x: 0, y: 0, w: SCREEN_W, h: SCREEN_H },
            work_area,
        }];
        assert_eq!(c.send("PUT", "/monitors", monitors).status, 204);
        c
    }

    fn send(&mut self, op: &str, path: &str, body: impl serde::Serialize) -> Response {
        let id = self.next;
        self.next += 1;
        self.stream.write_all(Request::new(id, op, path, body).line().as_bytes()).unwrap();
        let mut line = String::new();
        self.reader.read_line(&mut line).unwrap();
        let response: Response = serde_json::from_str(&line).unwrap();
        assert_eq!(response.id, id);
        response
    }

    fn place(&mut self, window: NewWindow) -> Placement {
        let r = self.send("POST", "/windows", window);
        assert_eq!(r.status, 200, "{:?}", r.body);
        serde_json::from_value(r.body).unwrap_or_default()
    }
}

fn new_window(window: u32, class: &str, window_type: &str) -> NewWindow {
    NewWindow {
        window,
        class: class.into(),
        title: class.into(),
        role: String::new(),
        process: String::new(),
        window_type: window_type.into(),
        transient: false,
        requested: Rect { x: 0, y: 0, w: 300, h: 200 },
        fullscreen: false,
        monitor: "M".into(),
    }
}

fn setup(config: &str, learn_after: u64) -> (Kill, Cherrypie, tempfile::TempDir) {
    let (xvfb, display) = start_xvfb();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    if !config.is_empty() {
        std::fs::write(&path, config).unwrap();
    }
    let cherrypie = start_cherrypie(&display, &path, learn_after);
    (xvfb, cherrypie, dir)
}

#[test]
fn a_matched_rule_answers_with_geometry_and_tag() {
    let rule = "[[rule]]\nclass = \"Term\"\nposition = \"center\"\nsize = [\"50%\", \"50%\"]\nworkspace = 2\nfocus = false\n";
    let (_x, cherrypie, _dir) = setup(rule, 30);
    let mut c = Client::connect(&cherrypie);

    let p = c.place(new_window(1, "Term", "normal"));
    // 50% of the 1280x696 work area, centered inside it.
    assert_eq!(p.geometry, Some(Rect { x: 320, y: BAR + 174, w: 640, h: 348 }));
    assert_eq!(p.tag, Some(Tag::Index(2)));
    assert_eq!(p.focus, Some(false));
}

#[test]
fn a_rule_names_its_tag() {
    let (_x, cherrypie, _dir) = setup("[[rule]]\nclass = \"Mon\"\nworkspace = \"Z\"\n", 30);
    let mut c = Client::connect(&cherrypie);
    assert_eq!(c.place(new_window(1, "Mon", "normal")).tag, Some(Tag::Name("Z".into())));
}

#[test]
fn an_unmatched_window_gets_the_learned_defaults_in_the_work_area() {
    let (_x, cherrypie, _dir) = setup("", 30);
    let mut c = Client::connect(&cherrypie);

    let p = c.place(new_window(1, "Fresh", "normal"));
    // 60% x 70% of 1280x696, centered.
    assert_eq!(p.geometry, Some(Rect { x: 256, y: BAR + 104, w: 768, h: 487 }));
    assert_eq!(p.focus, Some(true));
}

#[test]
fn a_learned_rule_is_written_only_once_its_window_lasts() {
    let (_x, cherrypie, dir) = setup("", 1);
    let config = dir.path().join("config.toml");
    let mut c = Client::connect(&cherrypie);

    c.place(new_window(1, "Kept", "normal"));
    c.place(new_window(2, "Blink", "normal"));
    assert_eq!(c.send("DELETE", "/windows/2", ()).status, 204);

    assert!(wait_until(|| config_text(&config).contains("'^Kept$'")));
    std::thread::sleep(Duration::from_millis(500));
    assert!(!config_text(&config).contains("Blink"), "a window closed inside the interval was learned");
}

#[test]
fn a_dialog_gets_no_answer_and_no_rule() {
    let (_x, cherrypie, dir) = setup("", 1);
    let mut c = Client::connect(&cherrypie);

    assert_eq!(c.place(new_window(1, "Prompt", "dialog")), Placement::default());
    std::thread::sleep(Duration::from_millis(1500));
    assert!(!config_text(&dir.path().join("config.toml")).contains("Prompt"));
}

#[test]
fn unknown_routes_and_bad_bodies_are_refused() {
    let (_x, cherrypie, _dir) = setup("", 30);
    let mut c = Client::connect(&cherrypie);

    assert_eq!(c.send("GET", "/nothing", ()).status, 404);
    assert_eq!(c.send("POST", "/windows", "not a window").status, 400);
    assert_eq!(c.send("DELETE", "/windows/abc", ()).status, 400);

    let moved = Moved { monitor: "M".into(), rect: Rect { x: 0, y: BAR, w: 300, h: 200 } };
    assert_eq!(c.send("PUT", "/windows/9/geometry", &moved).status, 404, "a window never announced");
    c.place(new_window(1, "Known", "normal"));
    let elsewhere = Moved { monitor: "Nowhere".into(), ..moved };
    assert_eq!(c.send("PUT", "/windows/1/geometry", elsewhere).status, 400);
}

fn moved(x: i32, y: i32, w: u32, h: u32) -> (Moved, Rect) {
    let rect = Rect { x, y, w, h };
    (Moved { monitor: "M".into(), rect }, rect)
}

#[test]
fn a_moved_window_is_remembered_in_its_rule() {
    let rule = "# Learned 2026-10-18 from a new window; edit to taste.\n[[rule]]\nclass = '^Kept$'\nposition = \"center\"\nsize = [\"60%\", \"70%\"]\n";
    let (_x, cherrypie, dir) = setup(rule, 30);
    let config = dir.path().join("config.toml");
    let mut c = Client::connect(&cherrypie);

    c.place(new_window(1, "Kept", "normal"));
    let (body, rect) = moved(128, BAR + 69, 640, 348);
    assert_eq!(c.send("PUT", "/windows/1/geometry", body).status, 204);
    assert!(wait_until(|| config_text(&config).contains("monitor = \"M\"")), "{}", config_text(&config));
    assert!(config_text(&config).starts_with("# Learned 2026-10-18"), "the comment was lost");

    assert!(
        wait_until(|| c.place(new_window(2, "Kept", "normal")).geometry == Some(rect)),
        "the next window did not open where the last one was left"
    );
}

#[test]
fn a_window_moved_before_its_rule_is_written_keeps_the_spot() {
    let (_x, cherrypie, dir) = setup("", 2);
    let config = dir.path().join("config.toml");
    let mut c = Client::connect(&cherrypie);

    c.place(new_window(1, "Fresh", "normal"));
    let (body, rect) = moved(0, BAR, 320, 174);
    assert_eq!(c.send("PUT", "/windows/1/geometry", body).status, 204);
    assert_eq!(c.place(new_window(2, "Fresh", "normal")).geometry, Some(rect), "the pending rule kept the defaults");

    assert!(wait_until(|| config_text(&config).contains("'^Fresh$'")));
    let text = config_text(&config);
    assert!(text.contains("position = [\"0%\", \"0%\"]") && text.contains("monitor = \"M\""), "{text}");
}

// The X11 backend stands down while a client is connected and picks up again
// once it leaves.
#[test]
fn standalone_actions_stop_while_a_client_is_connected() {
    let rule = "[[rule]]\nclass = \"Moved\"\nposition = [100, 100]\n";
    let (_x, cherrypie, _dir) = setup(rule, 30);
    let (conn, screen) = RustConnection::connect(Some(&cherrypie.display)).unwrap();
    let root = conn.setup().roots[screen].root;
    let list = intern(&conn, "_NET_CLIENT_LIST");

    let c = Client::connect(&cherrypie);
    let first = client_window(&conn, root, "Moved", None);
    publish_clients(&conn, root, list, &[first]);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(geometry(&conn, first), (0, 0, 300, 200), "moved while a client was connected");

    drop(c);
    std::thread::sleep(Duration::from_millis(300));
    let second = client_window(&conn, root, "Moved", None);
    publish_clients(&conn, root, list, &[first, second]);
    assert!(wait_until(|| geometry(&conn, second) == (100, 100, 300, 200)), "standalone did not resume");
}
