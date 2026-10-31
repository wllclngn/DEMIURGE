// MEDUSA -- DEMIURGE's screenshot capture utility, its X11 binary.
//
// UX modeled on flameshot (github.com/flameshot-org/flameshot, GPLv3);
// fresh Rust implementation on DEMIURGE's Cairo + x11rb stack. Credit
// to the flameshot developers for the interaction design. The command
// line is core's (medusa::cli).

use std::process::ExitCode;

use x11rb::connection::Connection;
use x11rb::rust_connection::RustConnection;

mod clipboard;
mod overlay;

use medusa::cli::{self, Mode};
use medusa::overlay::GuiOpts;
use x11_client::capture;

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

// Full-screen capture: grab the root, write it, optionally copy. No
// overlay, no selection -- the `flameshot full` equivalent.
fn capture_full(opts: &GuiOpts) -> Result<(), String> {
    let (conn, screen_num) =
        RustConnection::connect(None).map_err(|e| format!("x11 connect: {}", e))?;
    let root = conn.setup().roots[screen_num].root;
    let shot = capture::capture_root_to_surface(&conn, root)?;
    drop(conn);
    let path = medusa::overlay::save(&shot, &opts.save_dir)?;
    if opts.copy {
        overlay::copy_to_clipboard(&path);
    }
    Ok(())
}
