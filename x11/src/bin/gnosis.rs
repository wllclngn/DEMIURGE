// GNOSIS -- DEMIURGE's file manager. Replaces pcmanfm.
//
// Keyboard-first, no GTK/Qt, renders with torrentius (Cairo + Pango).
//
// Usage:
//   gnosis [dir]    open at <dir> (default: $HOME, or cwd if HOME unset)
//   gnosis -h       help
//
// Navigation: j/k or Down/Up move, l/Right/Enter descend or open,
// h/Left/Backspace go up, g/G top/bottom, '.' toggle hidden, q/Esc quit.

use std::path::PathBuf;
use std::process::ExitCode;

use demiurge_x11::config;
use demiurge_x11::gnosis::app::App;

fn main() -> ExitCode {
    let mut start: Option<PathBuf> = None;
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "-h" | "--help" => {
                print_help();
                return ExitCode::SUCCESS;
            }
            other => start = Some(PathBuf::from(other)),
        }
    }

    let start = start.unwrap_or_else(default_dir);
    let cfg = config::Paths::init()
        .and_then(|p| config::load(&p))
        .unwrap_or_default();

    match App::run(&cfg, start) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[gnosis] {}", e);
            ExitCode::FAILURE
        }
    }
}

fn default_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")))
}

fn print_help() {
    println!(
        "GNOSIS -- DEMIURGE file manager\n\
         \n\
         usage: gnosis [dir] [-h]\n\
         \n\
         j/k or Down/Up   move selection\n\
         l/Right/Enter    descend into dir / open file (xdg-open)\n\
         h/Left/Backspace go to parent\n\
         g / G            top / bottom\n\
         /                type-to-jump filter\n\
         b                bookmark picker\n\
         y / p            yank / paste (copy)\n\
         x / Delete       move to trash\n\
         .                toggle hidden files\n\
         q / Esc          quit"
    );
}
