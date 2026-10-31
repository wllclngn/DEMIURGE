// demiurge under the harness against the session the tests run in: In a
// runtime directory of its own the WM reaches no user manager, so the
// tests' own is never handed the test server's display, nor its session
// target started and stopped from under it.

use std::process::Command;
use std::time::{Duration, Instant};

mod common;
use common::wm::{minimal_config, WmTest};

fn systemctl(args: &[&str]) -> Option<String> {
    let out = Command::new("systemctl").arg("--user").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[test]
fn a_wm_under_the_harness_leaves_the_tests_own_session_alone() {
    // Where no user manager runs there is no session to take over.
    let Some(before) = systemctl(&["show-environment"]) else {
        return;
    };
    let target_was_active = systemctl(&["is-active", "--quiet", demiurge_core::session::X11_TARGET]).is_some();
    let mut wm = WmTest::new();
    wm.start_demiurge(minimal_config());
    let handed = format!("DISPLAY={}", wm.display);
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut taken = false;
    while !taken && Instant::now() < deadline {
        taken = systemctl(&["show-environment"]).is_some_and(|env| env.lines().any(|l| l == handed));
        std::thread::sleep(Duration::from_millis(100));
    }
    let display = wm.display.clone();
    drop(wm);
    if taken {
        // The session as it was, before failing.
        match before.lines().find(|l| l.starts_with("DISPLAY=")) {
            Some(display) => systemctl(&["set-environment", display]),
            None => systemctl(&["unset-environment", "DISPLAY"]),
        };
        if target_was_active {
            systemctl(&["start", demiurge_core::session::X11_TARGET]);
        }
        panic!("the test WM handed its display ({display}) to the user manager of the session running the tests");
    }
}
