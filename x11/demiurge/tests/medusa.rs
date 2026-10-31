// MEDUSA end to end: The real binary in Xephyr over a red window at a known
// place. A region dragged over the window and saved with Ctrl+S is a PNG of
// that window's pixels, and `medusa full` writes the whole screen.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;

mod common;
use common::wm::WmTest;

const WAIT: Duration = Duration::from_secs(5);
const RED: u32 = 0xff0000;

// MEDUSA's X11 binary, which `cargo test --workspace` builds beside this
// test's own target directory: Another package's binary has no
// CARGO_BIN_EXE_ here.
fn medusa_bin() -> PathBuf {
    let exe = std::env::current_exe().expect("the test's own path");
    let bin = exe.parent().and_then(|deps| deps.parent()).expect("a target directory").join("medusa");
    assert!(bin.exists(), "{} is not built: run `cargo test --workspace` or `cargo build -p medusa-x11`", bin.display());
    bin
}

// A red window 200 x 100 at (100, 100), mapped with no window manager.
fn red_window(wm: &WmTest) {
    let win = wm.conn.generate_id().unwrap();
    let depth = wm.conn.setup().roots[0].root_depth;
    wm.conn
        .create_window(
            depth,
            win,
            wm.root,
            100,
            100,
            200,
            100,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new().background_pixel(RED),
        )
        .unwrap();
    wm.conn.map_window(win).unwrap();
    wm.conn.flush().unwrap();
    std::thread::sleep(Duration::from_millis(200));
}

fn medusa(wm: &WmTest, home: &Path, args: &[&str]) -> std::process::Child {
    std::process::Command::new(medusa_bin())
        .args(args)
        .args(["-p", &home.to_string_lossy(), "--no-copy"])
        .env("DISPLAY", &wm.display)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .spawn()
        .unwrap()
}

// True once something else holds the keyboard, as MEDUSA's overlay does.
fn keyboard_grabbed(wm: &WmTest) -> bool {
    let status = wm
        .conn
        .grab_keyboard(false, wm.root, x11rb::CURRENT_TIME, GrabMode::ASYNC, GrabMode::ASYNC)
        .unwrap()
        .reply()
        .unwrap()
        .status;
    if status == GrabStatus::SUCCESS {
        wm.conn.ungrab_keyboard(x11rb::CURRENT_TIME).unwrap();
        wm.conn.flush().unwrap();
    }
    status == GrabStatus::ALREADY_GRABBED
}

fn exited(child: &mut std::process::Child) -> bool {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            return status.success();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    false
}

// The one screenshot MEDUSA wrote into `dir`.
fn screenshot(dir: &Path) -> cairo::ImageSurface {
    let shots: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "png"))
        .collect();
    assert_eq!(shots.len(), 1, "MEDUSA wrote {:?}", shots);
    cairo::ImageSurface::create_from_png(&mut std::fs::File::open(&shots[0]).unwrap()).unwrap()
}

fn pixel(surface: &mut cairo::ImageSurface, x: usize, y: usize) -> u32 {
    let stride = surface.stride() as usize;
    let data = surface.data().unwrap();
    u32::from_le_bytes(data[y * stride + x * 4..y * stride + x * 4 + 4].try_into().unwrap()) & 0xffffff
}

#[test]
fn a_region_saved_with_ctrl_s_is_a_png_of_the_pixels_under_it() {
    let wm = WmTest::new();
    red_window(&wm);
    let home = tempfile::tempdir().unwrap();
    let mut child = medusa(&wm, home.path(), &[]);
    assert!(wm.wait_for(keyboard_grabbed, WAIT), "MEDUSA's overlay never took the keyboard");

    // A drag from (120, 110) to (220, 160), inside the window.
    wm.fake(MOTION_NOTIFY_EVENT, 0, 120, 110);
    wm.fake(BUTTON_PRESS_EVENT, 1, 0, 0);
    wm.fake(MOTION_NOTIFY_EVENT, 0, 170, 140);
    wm.fake(MOTION_NOTIFY_EVENT, 0, 220, 160);
    wm.fake(BUTTON_RELEASE_EVENT, 1, 0, 0);
    let (ctrl, s) = (wm.keycode(0xffe3), wm.keycode(0x73));
    wm.fake(KEY_PRESS_EVENT, ctrl, 0, 0);
    wm.fake(KEY_PRESS_EVENT, s, 0, 0);
    wm.fake(KEY_RELEASE_EVENT, s, 0, 0);
    wm.fake(KEY_RELEASE_EVENT, ctrl, 0, 0);
    assert!(exited(&mut child), "MEDUSA did not exit on Ctrl+S");

    let mut shot = screenshot(home.path());
    assert_eq!((shot.width(), shot.height()), (100, 50));
    for (x, y) in [(0, 0), (99, 0), (50, 25), (0, 49), (99, 49)] {
        assert_eq!(pixel(&mut shot, x, y), RED, "({}, {}) of the region is not the window's", x, y);
    }
}

#[test]
fn medusa_full_writes_the_whole_screen() {
    let wm = WmTest::new();
    red_window(&wm);
    let home = tempfile::tempdir().unwrap();
    let mut child = medusa(&wm, home.path(), &["full"]);
    assert!(exited(&mut child), "medusa full did not finish");

    let mut shot = screenshot(home.path());
    assert_eq!((shot.width(), shot.height()), (1280, 720));
    assert_eq!(pixel(&mut shot, 150, 150), RED, "the window is not where the screen has it");
    assert_ne!(pixel(&mut shot, 50, 50), RED, "the root shows as the window");
}
