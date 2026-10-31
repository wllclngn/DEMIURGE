// demiurge-core's session against a stand-in systemctl that logs each call:
// The displays handed to the manager and the target started, then the
// target stopped and the displays taken back, the session bus never passed
// on to it.

use std::path::PathBuf;

use demiurge_core::session::{Session, X11_TARGET as TARGET};

struct Stub {
    dir: PathBuf,
}

impl Stub {
    // A systemctl logging its arguments a line per call, "+bus" marking a
    // call that was given the session bus, and refusing the verb `refuse` as
    // the manager refuses a unit it does not have.
    fn new(name: &str, refuse: &str) -> Stub {
        let dir = std::env::temp_dir().join(format!("demiurge-session-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let log = dir.join("calls");
        let program = format!(
            "#!/usr/bin/env python3\n\
             import os, sys\n\
             with open({log:?}, 'a') as f:\n    \
                 f.write(' '.join(sys.argv[1:]) + (' +bus' if 'DBUS_SESSION_BUS_ADDRESS' in os.environ else '') + '\\n')\n\
             if sys.argv[2:3] == [{refuse:?}]:\n    \
                 sys.stderr.write('Unit {TARGET} not found.\\n')\n    \
                 sys.exit(5)\n",
            log = log.to_str().expect("utf-8 path"),
        );
        let path = dir.join("systemctl");
        std::fs::write(&path, program).expect("write the stand-in");
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        std::fs::set_permissions(&path, permissions).expect("chmod");
        Stub { dir }
    }

    fn program(&self) -> String {
        self.dir.join("systemctl").to_str().expect("utf-8 path").to_string()
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("calls")).unwrap_or_default().lines().map(str::to_string).collect()
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn displays() -> Vec<(&'static str, String)> {
    vec![("WAYLAND_DISPLAY", "wayland-7".to_string()), ("DISPLAY", ":7".to_string())]
}

#[test]
fn a_session_hands_the_manager_its_displays_and_takes_them_back() {
    let stub = Stub::new("ends", "");
    let session = Session::begin_with(&stub.program(), TARGET, &displays()).expect("begin");
    assert_eq!(
        stub.calls(),
        ["--user set-environment WAYLAND_DISPLAY=wayland-7 DISPLAY=:7".to_string(), format!("--user start --no-block {TARGET}")]
    );
    session.end().expect("end");
    assert_eq!(
        stub.calls()[2..],
        [format!("--user stop --no-block {TARGET}"), "--user unset-environment WAYLAND_DISPLAY DISPLAY".to_string()]
    );
}

#[test]
fn a_refused_start_takes_the_displays_back() {
    let stub = Stub::new("refused", "start");
    let e = Session::begin_with(&stub.program(), TARGET, &displays()).err().expect("a refused start fails the session");
    assert!(e.contains("not found"), "the refusal is reported: {e}");
    assert_eq!(
        stub.calls(),
        [
            "--user set-environment WAYLAND_DISPLAY=wayland-7 DISPLAY=:7".to_string(),
            format!("--user start --no-block {TARGET}"),
            "--user unset-environment WAYLAND_DISPLAY DISPLAY".to_string(),
        ]
    );
}

#[test]
fn a_session_dropped_on_an_error_path_ends_too() {
    let stub = Stub::new("dropped", "");
    drop(Session::begin_with(&stub.program(), TARGET, &displays()).expect("begin"));
    assert_eq!(
        stub.calls()[2..],
        [format!("--user stop --no-block {TARGET}"), "--user unset-environment WAYLAND_DISPLAY DISPLAY".to_string()]
    );
}
