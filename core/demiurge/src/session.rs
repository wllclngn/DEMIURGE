// The graphical session in the user's systemd manager. The session's display
// server hands the manager its displays, then starts its stack's target,
// demiurge-x11.target or demiurge-wayland.target, which binds
// graphical-session.target: The services it wants (that stack's ABRAXAS and
// cherrypie instances) start then, with the displays in their environment,
// and stop as the session ends. Both stacks begin it from their own binary,
// so a session under startx has it as one under a display manager does.
//
// The manager asked is the one behind this process's XDG_RUNTIME_DIR, where
// its clients find the display. A compositor a test runs in a runtime
// directory of its own reaches no manager, and leaves the session it runs
// inside alone.

use std::ffi::OsStr;
use std::process::{Command, Stdio};

pub const X11_TARGET: &str = "demiurge-x11.target";
pub const WAYLAND_TARGET: &str = "demiurge-wayland.target";

/// A session begun in the user manager; ended by `end`, or when dropped.
pub struct Session {
    systemctl: String,
    target: String,
    names: Vec<String>,
    ended: bool,
}

impl Session {
    /// Hand `vars` to the user manager, then start `target`. A refused
    /// start takes the variables back, leaving the manager as it was.
    pub fn begin(target: &str, vars: &[(&str, String)]) -> Result<Session, String> {
        Self::begin_with("systemctl", target, vars)
    }

    /// `begin`, through the program `systemctl` in place of systemctl.
    pub fn begin_with(systemctl: &str, target: &str, vars: &[(&str, String)]) -> Result<Session, String> {
        let names: Vec<String> = vars.iter().map(|(name, _)| name.to_string()).collect();
        let assignments: Vec<String> = vars.iter().map(|(name, value)| format!("{name}={value}")).collect();
        run(systemctl, "set-environment", &assignments)?;
        if let Err(e) = run(systemctl, "start", &["--no-block", target]) {
            let _ = run(systemctl, "unset-environment", &names);
            return Err(e);
        }
        Ok(Session { systemctl: systemctl.to_string(), target: target.to_string(), names, ended: false })
    }

    /// Stop the session target, then take back what `begin` handed over.
    pub fn end(mut self) -> Result<(), String> {
        self.ended = true;
        self.stop()
    }

    fn stop(&self) -> Result<(), String> {
        let stopped = run(&self.systemctl, "stop", &["--no-block", self.target.as_str()]);
        let unset = run(&self.systemctl, "unset-environment", &self.names);
        stopped.and(unset)
    }
}

impl Drop for Session {
    // A session left on an error path ends too, its failure unreported.
    fn drop(&mut self) {
        if !self.ended {
            let _ = self.stop();
        }
    }
}

fn run(systemctl: &str, verb: &str, args: &[impl AsRef<OsStr>]) -> Result<(), String> {
    // The session bus is no way in: A systemctl that falls back to it would
    // reach the manager whatever the runtime directory.
    let out = Command::new(systemctl)
        .args(["--user", verb])
        .args(args)
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("{systemctl}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("systemctl --user {verb}: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}
