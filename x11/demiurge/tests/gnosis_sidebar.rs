// GNOSIS's sidebar end to end: The real binary in Xephyr, its system bus a
// private one with the fake udisks2. `s` opens the sidebar, Enter on an
// unmounted drive mounts it, `u` unmounts it, and Enter on a mounted drive
// opens it without another mount.

use std::time::{Duration, Instant};

use x11rb::protocol::xproto::*;

mod common;
use common::udisks::{fake_udisks, private_bus, ROOT};
use common::wm::{minimal_config, WmTest};

const WAIT: Duration = Duration::from_secs(5);

// GNOSIS's X11 binary, which `cargo test --workspace` builds beside this
// test's own target directory: Another package's binary has no
// CARGO_BIN_EXE_ here.
fn gnosis_bin() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("the test's own path");
    let bin = exe.parent().and_then(|deps| deps.parent()).expect("a target directory").join("gnosis");
    assert!(bin.exists(), "{} is not built: run `cargo test --workspace` or `cargo build -p gnosis-x11`", bin.display());
    bin
}

fn press(wm: &WmTest, keysym: u32) {
    let key = wm.keycode(keysym);
    wm.fake(KEY_PRESS_EVENT, key, 0, 0);
    wm.fake(KEY_RELEASE_EVENT, key, 0, 0);
    std::thread::sleep(Duration::from_millis(60));
}

#[test]
fn the_sidebar_mounts_opens_and_unmounts_a_drive() {
    let bus = private_bus();
    let media = tempfile::tempdir().unwrap();
    let target: &'static str = Box::leak(media.path().to_string_lossy().into_owned().into_boxed_str());
    let (_server, calls) = fake_udisks(&bus, target);
    let home = tempfile::tempdir().unwrap();

    let mut wm = WmTest::new();
    wm.start_demiurge(minimal_config());
    let mut gnosis = std::process::Command::new(gnosis_bin())
        .arg(home.path())
        .env("DISPLAY", &wm.display)
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env("DBUS_SYSTEM_BUS_ADDRESS", &bus.address)
        .spawn()
        .unwrap();
    assert!(wm.wait_for(|t| !t.client_list().is_empty() && t.active_window().is_some(), WAIT));
    std::thread::sleep(Duration::from_millis(300));

    // Bookmarks Home and Root, then the drives by label: USB STICK, data.
    press(&wm, 0x73); // s
    press(&wm, 0xff54); // Down: Root
    press(&wm, 0xff54); // Down: USB STICK
    press(&wm, 0xff0d); // Enter: mount and open
    let stick = format!("{ROOT}/block_devices/sdb1");
    let saw = |method: &str| {
        let deadline = Instant::now() + WAIT;
        while Instant::now() < deadline {
            if calls.lock().unwrap().iter().any(|(m, o, _)| m == method && *o == stick) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    };
    assert!(saw("Mount"), "Enter on the stick asked for no mount");

    press(&wm, 0xff09); // Tab: back into the sidebar, still on the stick
    press(&wm, 0x75); // u
    assert!(saw("Unmount"), "u asked for no unmount");

    press(&wm, 0xff09); // Tab
    press(&wm, 0xff54); // Down: data, mounted at /mnt/data
    press(&wm, 0xff0d); // Enter: open, no mount
    std::thread::sleep(Duration::from_millis(400));

    let _ = gnosis.kill();
    let _ = gnosis.wait();
    assert_eq!(
        calls.lock().unwrap().clone(),
        vec![("Mount".into(), stick.clone(), true), ("Unmount".into(), stick, true)],
        "a mounted drive was mounted again, or a call asked for user interaction"
    );
}
