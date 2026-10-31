// DEMIURGE as the client of cherrypie's window-policy API, against a scripted
// cherrypie in this test: A new window maps once, already placed; an overdue
// answer falls back to DEMIURGE's default; the answer's tag and focus are
// honored; monitors, dragged windows and closed windows are reported.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cherrypie::api::{display_tag, Monitor, Moved, Placement, Rect, Request, Response, Tag};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::protocol::Event;
use x11rb::wrapper::ConnectionExt as _;

mod common;
use common::wm::{minimal_config, WmTest};

const WAIT: Duration = Duration::from_secs(2);

// A cherrypie stand-in: Records every request and answers POST /windows
// with `answer` (None: never answer).
struct FakeCherrypie {
    requests: Arc<Mutex<Vec<Request>>>,
}

impl FakeCherrypie {
    fn start(runtime: &std::path::Path, display: &str, answer: Option<Placement>) -> Self {
        let path = runtime.join(format!("cherrypie-{}.sock", display_tag(display)));
        let listener = UnixListener::bind(&path).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
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
                seen.lock().unwrap().push(request);
                if let Some(r) = response {
                    let _ = writer.write_all(r.line().as_bytes());
                }
            }
        });
        Self { requests }
    }

    fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}

fn start(answer: Option<Placement>) -> (WmTest, FakeCherrypie, tempfile::TempDir) {
    let runtime = tempfile::tempdir().unwrap();
    let mut wm = WmTest::new();
    let fake = FakeCherrypie::start(runtime.path(), &wm.display, answer);
    wm.start_demiurge_env(minimal_config(), &[("XDG_RUNTIME_DIR", runtime.path())]);
    (wm, fake, runtime)
}

fn new_window(wm: &WmTest, class: &str) -> Window {
    let win = wm.conn.generate_id().unwrap();
    let depth = wm.conn.setup().roots[0].root_depth;
    wm.conn
        .create_window(
            depth,
            win,
            wm.root,
            0,
            0,
            300,
            200,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new()
                .background_pixel(wm.conn.setup().roots[0].white_pixel)
                .event_mask(EventMask::STRUCTURE_NOTIFY),
        )
        .unwrap();
    let wm_class = format!("{}\0{}\0", class.to_lowercase(), class);
    wm.conn
        .change_property8(PropMode::REPLACE, win, AtomEnum::WM_CLASS, AtomEnum::STRING, wm_class.as_bytes())
        .unwrap();
    wm.conn
        .change_property8(PropMode::REPLACE, win, AtomEnum::WM_NAME, AtomEnum::STRING, class.as_bytes())
        .unwrap();
    wm.conn.map_window(win).unwrap();
    wm.conn.flush().unwrap();
    win
}

// The window's geometry at the moment it mapped, from its own events: A
// ConfigureNotify carries each change, so the last one before MapNotify is
// what the window first appeared at.
fn geometry_at_map(wm: &WmTest, win: Window) -> Option<(i16, i16, u16, u16)> {
    let deadline = Instant::now() + WAIT;
    let mut last = (0, 0, 300, 200);
    while Instant::now() < deadline {
        while let Some(ev) = wm.conn.poll_for_event().unwrap() {
            match ev {
                Event::ConfigureNotify(c) if c.window == win => last = (c.x, c.y, c.width, c.height),
                Event::MapNotify(m) if m.window == win => return Some(last),
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

fn placement(geometry: Option<Rect>, tag: Option<Tag>, focus: Option<bool>) -> Placement {
    Placement { geometry, tag, focus, ..Placement::default() }
}

#[test]
fn a_window_maps_once_already_where_cherrypie_put_it() {
    let at = Rect { x: 100, y: 200, w: 500, h: 300 };
    let (wm, fake, _rt) = start(Some(placement(Some(at), None, None)));
    let win = new_window(&wm, "Placed");

    assert_eq!(geometry_at_map(&wm, win), Some((100, 200, 500, 300)));
    let ops: Vec<(String, String)> = fake.requests().into_iter().map(|r| (r.op, r.path)).collect();
    assert_eq!(ops.first(), Some(&("PUT".into(), "/monitors".into())), "monitors first on a new link");
    assert!(ops.contains(&("POST".into(), "/windows".into())));
}

#[test]
fn an_overdue_answer_falls_back_to_the_default() {
    let (wm, _fake, _rt) = start(None);
    let asked = Instant::now();
    let win = new_window(&wm, "Silent");

    // DEMIURGE's own default: Centered in the 1280x696 work area.
    assert_eq!(geometry_at_map(&wm, win), Some((490, 24 + 248, 300, 200)));
    assert!(asked.elapsed() >= Duration::from_millis(100), "mapped before the answer was due");
}

#[test]
fn the_answers_tag_and_focus_are_honored() {
    // minimal_config's tags are A, B, C: "B" is tag 1.
    let (wm, _fake, _rt) = start(Some(placement(None, Some(Tag::Name("B".into())), None)));
    let win = new_window(&wm, "Elsewhere");
    assert!(wm.wait_for(|t| t.wm_desktop(win) == Some(1), WAIT));
    assert_ne!(wm.map_state(win), Some(MapState::VIEWABLE), "a window on a hidden tag mapped");

    let (wm, _fake, _rt) = start(Some(placement(None, None, Some(false))));
    let first = new_window(&wm, "First");
    assert!(wm.wait_for(|t| t.map_state(first) == Some(MapState::VIEWABLE), WAIT));
    let before = wm.active_window();
    let quiet = new_window(&wm, "Quiet");
    assert!(wm.wait_for(|t| t.map_state(quiet) == Some(MapState::VIEWABLE), WAIT));
    assert_eq!(wm.active_window(), before, "focus = false took focus");
}

// A Super+drag ends with the window's monitor and rect sent to cherrypie for
// its window memory; a Super+click that moves nothing sends nothing.
#[test]
fn a_drag_reports_where_the_window_was_left() {
    let at = Rect { x: 100, y: 200, w: 300, h: 200 };
    let (wm, fake, _rt) = start(Some(placement(Some(at), None, None)));
    let win = new_window(&wm, "Dragged");
    assert_eq!(geometry_at_map(&wm, win), Some((100, 200, 300, 200)));

    let super_l = wm.keycode(0xffeb);
    wm.fake(MOTION_NOTIFY_EVENT, 0, 250, 300);
    wm.fake(KEY_PRESS_EVENT, super_l, 0, 0);
    wm.fake(BUTTON_PRESS_EVENT, 1, 0, 0);
    wm.fake(BUTTON_RELEASE_EVENT, 1, 0, 0);
    wm.fake(BUTTON_PRESS_EVENT, 1, 0, 0);
    std::thread::sleep(Duration::from_millis(100));
    wm.fake(MOTION_NOTIFY_EVENT, 0, 350, 350);
    std::thread::sleep(Duration::from_millis(100));
    wm.fake(BUTTON_RELEASE_EVENT, 1, 0, 0);
    wm.fake(KEY_RELEASE_EVENT, super_l, 0, 0);

    let path = format!("/windows/{win}/geometry");
    let reports = || -> Vec<Request> { fake.requests().into_iter().filter(|r| r.op == "PUT" && r.path == path).collect() };
    assert!(wm.wait_for(|_| !reports().is_empty(), WAIT), "no geometry report");
    std::thread::sleep(Duration::from_millis(200));
    let reports = reports();
    assert_eq!(reports.len(), 1, "the click without a move was reported too");

    let monitors: Vec<Monitor> = fake
        .requests()
        .into_iter()
        .find(|r| r.path == "/monitors")
        .map(|r| serde_json::from_value(r.body).unwrap())
        .unwrap();
    let moved: Moved = serde_json::from_value(reports[0].body.clone()).unwrap();
    assert_eq!(moved, Moved { monitor: monitors[0].name.clone(), rect: Rect { x: 200, y: 250, w: 300, h: 200 } });
}

#[test]
fn closed_windows_are_reported() {
    let (wm, fake, _rt) = start(Some(Placement::default()));
    let win = new_window(&wm, "Gone");
    assert!(wm.wait_for(|t| t.client_list().contains(&win), WAIT));
    wm.conn.destroy_window(win).unwrap();
    wm.conn.flush().unwrap();
    let path = format!("/windows/{win}");
    assert!(wm.wait_for(|_| fake.requests().iter().any(|r| r.op == "DELETE" && r.path == path), WAIT));
}
