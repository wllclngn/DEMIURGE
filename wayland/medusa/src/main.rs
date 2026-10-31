// MEDUSA -- DEMIURGE's screenshot capture utility, its Wayland binary.
//
// UX modeled on flameshot (github.com/flameshot-org/flameshot, GPLv3);
// fresh Rust implementation on DEMIURGE's Cairo stack, drawn into wl_shm
// over the compositor's own frames. Credit to the flameshot developers for
// the interaction design. The command line is core's (medusa::cli).

use std::process::ExitCode;

mod clipboard;
mod overlay;

use medusa::cli::{self, Mode};
use medusa::overlay::GuiOpts;
use wayland_client::Connection;

fn main() -> ExitCode {
    let (mode, opts) = match cli::parse() {
        Ok(parsed) => parsed,
        Err(code) => return code,
    };
    cli::report(match mode {
        Mode::Gui => overlay::run(&opts),
        Mode::Full => capture_full(&opts),
    })
}

// Full-screen capture: every monitor's frame, written, optionally copied.
// No overlay, no selection -- the `flameshot full` equivalent.
fn capture_full(opts: &GuiOpts) -> Result<(), String> {
    let conn = Connection::connect_to_env().map_err(|e| format!("wayland connect: {}", e))?;
    let (shot, _) = wl_client::capture::desktop(&conn)?;
    drop(conn);
    let path = medusa::overlay::save(&shot, &opts.save_dir)?;
    if opts.copy {
        clipboard::copy_to_clipboard(&path);
    }
    Ok(())
}
