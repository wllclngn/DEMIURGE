// The Wayland cherrypie end to end: The real binary with no X display in
// its environment, serving the API on WAYLAND_DISPLAY's socket, with this
// test as the compositor.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use cherrypie::api::{Monitor, NewWindow, Placement, Rect, Request, Response, Tag};

const WAIT: Duration = Duration::from_secs(5);
const AREA: Rect = Rect { x: 0, y: 24, w: 1280, h: 696 };

struct Daemon {
    child: Child,
    socket: PathBuf,
    _runtime: tempfile::TempDir,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// cherrypie-wl on its own runtime directory, and the backend it reported.
fn start(config: &Path, learn_after: u64) -> (Daemon, String) {
    let runtime = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_cherrypie-wl"))
        .arg("--config")
        .arg(config)
        .args(["--learn-after", &learn_after.to_string()])
        .env_remove("DISPLAY")
        .env("WAYLAND_DISPLAY", "wayland-test")
        .env("XDG_RUNTIME_DIR", runtime.path())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cherrypie-wl");
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if line.contains("daemon started") {
                let _ = tx.send(line);
            }
        }
    });
    let started = rx.recv_timeout(WAIT).expect("cherrypie-wl never reported 'daemon started'");
    let socket = runtime.path().join("cherrypie-wayland_test.sock");
    (Daemon { child, socket, _runtime: runtime }, started)
}

struct Client {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    next: u64,
}

impl Client {
    fn connect(daemon: &Daemon) -> Self {
        let stream = UnixStream::connect(&daemon.socket).expect("cherrypie-wl's socket on WAYLAND_DISPLAY");
        stream.set_read_timeout(Some(WAIT)).unwrap();
        let reader = BufReader::new(stream.try_clone().unwrap());
        let mut c = Self { stream, reader, next: 1 };
        let monitors = vec![Monitor { name: "M".into(), rect: Rect { x: 0, y: 0, w: 1280, h: 720 }, work_area: AREA }];
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

    fn place(&mut self, window: u32, class: &str) -> Placement {
        let w = NewWindow {
            window,
            class: class.into(),
            title: class.into(),
            role: String::new(),
            process: String::new(),
            window_type: "normal".into(),
            transient: false,
            requested: Rect { x: 0, y: 0, w: 300, h: 200 },
            fullscreen: false,
            monitor: "M".into(),
        };
        let r = self.send("POST", "/windows", w);
        assert_eq!(r.status, 200, "{:?}", r.body);
        serde_json::from_value(r.body).unwrap_or_default()
    }
}

#[test]
fn a_rule_places_a_window_with_no_x_display() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    let rule = "[[rule]]\nclass = \"Term\"\nposition = \"center\"\nsize = [\"50%\", \"50%\"]\nworkspace = 2\nfocus = false\n";
    std::fs::write(&config, rule).unwrap();
    let (daemon, started) = start(&config, 30);
    assert!(started.contains("backend: none"), "{started}");
    let mut c = Client::connect(&daemon);

    let p = c.place(1, "Term");
    // 50% of the 1280x696 work area, centered inside it.
    assert_eq!(p.geometry, Some(Rect { x: 320, y: 24 + 174, w: 640, h: 348 }));
    assert_eq!(p.tag, Some(Tag::Index(2)));
    assert_eq!(p.focus, Some(false));
}

#[test]
fn a_window_that_lasts_is_learned_from_the_api_alone() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    let (daemon, _) = start(&config, 1);
    let mut c = Client::connect(&daemon);

    c.place(1, "Lasting");
    c.place(2, "Fleeting");
    assert_eq!(c.send("DELETE", "/windows/2", ()).status, 204);
    let deadline = Instant::now() + WAIT;
    let learned = loop {
        // Nothing else wakes the daemon once its interval runs out.
        c.send("PUT", "/monitors", vec![Monitor { name: "M".into(), rect: Rect { x: 0, y: 0, w: 1280, h: 720 }, work_area: AREA }]);
        let text = std::fs::read_to_string(&config).unwrap_or_default();
        if text.contains("Lasting") || Instant::now() > deadline {
            break text;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    assert!(learned.contains("class = '^Lasting$'"), "no learned rule:\n{learned}");
    assert!(!learned.contains("Fleeting"), "a window closed before its interval was learned:\n{learned}");
}
