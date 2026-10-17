// MEDUSA -- DEMIURGE's screenshot capture utility.
//
// UX modeled on flameshot (github.com/flameshot-org/flameshot, GPLv3);
// fresh Rust implementation on DEMIURGE's Cairo + x11rb stack. Credit
// to the flameshot developers for the interaction design.
//
// Usage:
//   medusa            interactive region capture (default; == `gui`)
//   medusa gui        same
//   medusa full       capture the whole screen, no overlay
//   medusa -p <dir>   write screenshots to <dir> (default $HOME)
//   medusa --no-copy  don't push the result to the clipboard
//   medusa -h         help

use std::path::PathBuf;
use std::process::ExitCode;

use x11rb::connection::Connection;
use x11rb::rust_connection::RustConnection;

use demiurge_x11::config;
use demiurge_x11::medusa::overlay::{self, GuiOpts};
use demiurge_x11::torrentius;

enum Mode {
    Gui,
    Full,
    Help,
}

fn main() -> ExitCode {
    let mut mode = Mode::Gui;
    let mut save_dir: Option<PathBuf> = None;
    let mut copy = true;

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "gui" => mode = Mode::Gui,
            "full" => mode = Mode::Full,
            "-p" | "--path" => {
                save_dir = args.next().map(PathBuf::from);
                if save_dir.is_none() {
                    eprintln!("[medusa] {} needs a directory argument", a);
                    return ExitCode::FAILURE;
                }
            }
            "--no-copy" => copy = false,
            "-h" | "--help" => mode = Mode::Help,
            other => {
                eprintln!("[medusa] unknown argument: {}", other);
                return ExitCode::FAILURE;
            }
        }
    }

    if matches!(mode, Mode::Help) {
        print_help();
        return ExitCode::SUCCESS;
    }

    let save_dir = save_dir.unwrap_or_else(default_save_dir);

    // Pull the shared accent from [appearance]. Best-effort: if the
    // config can't load, fall back to Config::default() (which carries
    // the DEMIURGE amber default), so MEDUSA still runs with a sane
    // selection colour outside a configured session.
    let cfg = config::Paths::init()
        .and_then(|p| config::load(&p))
        .unwrap_or_default();
    let accent = torrentius::parse_hex(&cfg.appearance.accent);

    let opts = GuiOpts {
        save_dir,
        copy,
        accent,
    };

    let result = match mode {
        Mode::Gui => overlay::run(&opts),
        Mode::Full => capture_full(&opts),
        Mode::Help => unreachable!(),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[medusa] {}", e);
            ExitCode::FAILURE
        }
    }
}

// Full-screen capture: grab the root, write it, optionally copy. No
// overlay, no selection -- the `flameshot full` equivalent.
fn capture_full(opts: &GuiOpts) -> Result<(), String> {
    let (conn, screen_num) =
        RustConnection::connect(None).map_err(|e| format!("x11 connect: {}", e))?;
    let root = conn.setup().roots[screen_num].root;
    let shot = torrentius::capture_root_to_surface(&conn, root)?;
    let path = opts.save_dir.join(format!("medusa-{}.png", overlay::timestamp_pub()));
    torrentius::surface_to_png(&shot, &path)?;
    eprintln!("[medusa] saved {}", path.display());
    if opts.copy {
        overlay::copy_to_clipboard_pub(&path);
    }
    Ok(())
}

fn default_save_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn print_help() {
    println!(
        "MEDUSA -- DEMIURGE screenshot capture\n\
         \n\
         usage: medusa [gui | full] [-p <dir>] [--no-copy] [-h]\n\
         \n\
         gui          Interactive region capture (default)\n\
         full         Capture the whole screen immediately\n\
         -p, --path   Save directory (default: $HOME)\n\
         --no-copy    Don't copy the result to the clipboard\n\
         -h, --help   This help\n\
         \n\
         In gui mode: drag to select, Enter to copy+save,\n\
         Ctrl+S to save only, Esc to cancel."
    );
}
