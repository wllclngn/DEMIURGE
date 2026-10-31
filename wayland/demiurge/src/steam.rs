// Steam Input for Wayland games. Steam takes the focused game from
// XWayland's input focus, and names it by the environment of the X client
// that owns the focused window, its SteamGameId, found through XRes. A
// Wayland window never holds X input focus, so while a Steam game's
// Wayland window holds the keyboard Steam keeps the controller on its
// desktop layout and the game's virtual pad stays silent. A helper of this
// binary, started with the game's Steam IDs, holds an override-redirect
// window off-screen with X input focus for as long as the game's window
// holds the keyboard. Its stdin is a pipe from the compositor, so it goes
// when the focus leaves or the compositor does.

use std::io::Read;
use std::process::{Child, Command, ExitCode, Stdio};

use smithay::reexports::x11rb;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt as _, CreateWindowAux, InputFocus, WindowClass};

/// The argument that runs the binary as the helper.
pub const HELPER: &str = "--steam-focus";

/// The variables Steam sets for a game, by which it names it.
const STEAM_IDS: [&str; 4] = ["SteamGameId", "SteamAppId", "SteamOverlayGameId", "STEAM_COMPAT_APP_ID"];

/// The Steam IDs in a process's environment; None for one Steam did not
/// start as a game.
fn steam_ids(pid: i32) -> Option<Vec<(String, String)>> {
    let environ = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    let ids: Vec<(String, String)> = environ
        .split(|&b| b == 0)
        .filter_map(|entry| {
            let (key, value) = std::str::from_utf8(entry).ok()?.split_once('=')?;
            STEAM_IDS.contains(&key).then(|| (key.to_string(), value.to_string()))
        })
        .collect();
    ids.iter().any(|(k, v)| k == "SteamGameId" && !v.is_empty() && v != "0").then_some(ids)
}

/// The helper holding X input focus, and the game it holds it for.
#[derive(Default)]
pub struct SteamFocus {
    held: Option<(i32, Child)>,
}

impl SteamFocus {
    /// The keyboard went to a Wayland window of process `pid`, or to an X11
    /// window or nothing (None): A Steam game's window gets a helper with
    /// its Steam IDs, started with the compositor's children's `env`
    /// (XWayland's DISPLAY among them); any other focus ends the helper.
    pub fn focused(&mut self, pid: Option<i32>, env: &[(&'static str, String)]) {
        if pid.is_some() && self.held.as_ref().map(|(p, _)| *p) == pid {
            return;
        }
        self.release();
        let Some(pid) = pid else { return };
        let Some(ids) = steam_ids(pid) else { return };
        if !env.iter().any(|(k, _)| *k == "DISPLAY") {
            return;
        }
        let exe = match std::env::current_exe() {
            Ok(exe) => exe,
            Err(e) => {
                tracing::warn!(error = %e, "steam focus: no path to this binary, so no helper");
                return;
            }
        };
        let mut cmd = Command::new(exe);
        cmd.arg(HELPER).stdin(Stdio::piped()).stdout(Stdio::null());
        for key in STEAM_IDS {
            cmd.env_remove(key);
        }
        cmd.envs(env.iter().map(|(k, v)| (*k, v.as_str()))).envs(ids.iter().map(|(k, v)| (k, v)));
        match cmd.spawn() {
            Ok(child) => {
                let game = ids.iter().find(|(k, _)| k == "SteamGameId").map_or("", |(_, v)| v.as_str());
                tracing::info!(pid, game, "steam focus: X input focus held for a Steam game's window");
                self.held = Some((pid, child));
            }
            Err(e) => tracing::warn!(error = %e, "steam focus: helper spawn failed"),
        }
    }

    /// The helper's pipe closed, so it exits; reaped off the event loop.
    fn release(&mut self) {
        if let Some((_, mut child)) = self.held.take() {
            drop(child.stdin.take());
            std::thread::spawn(move || child.wait());
        }
    }
}

/// The helper: An override-redirect window off-screen, mapped and given X
/// input focus, held until stdin closes.
pub fn helper() -> ExitCode {
    match hold() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("demiurge steam focus: {e}");
            ExitCode::FAILURE
        }
    }
}

fn hold() -> Result<(), Box<dyn std::error::Error>> {
    let (conn, screen) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen].root;
    let window = conn.generate_id()?;
    conn.create_window(
        x11rb::COPY_DEPTH_FROM_PARENT,
        window,
        root,
        -10,
        -10,
        1,
        1,
        0,
        WindowClass::INPUT_OUTPUT,
        x11rb::COPY_FROM_PARENT,
        &CreateWindowAux::new().override_redirect(1),
    )?;
    conn.map_window(window)?;
    // A round trip, so the window is viewable before it takes the focus.
    conn.get_input_focus()?.reply()?;
    conn.set_input_focus(InputFocus::PARENT, window, x11rb::CURRENT_TIME)?;
    conn.flush()?;
    let _ = std::io::stdin().read_to_end(&mut Vec::new());
    Ok(())
}
