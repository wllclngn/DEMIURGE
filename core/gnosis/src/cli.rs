// GNOSIS's command line, the same on both stacks:
//   gnosis [dir]    open at <dir> (default: $HOME, or cwd if HOME unset)
//   gnosis -h       help

use std::path::PathBuf;
use std::process::ExitCode;

use demiurge_core::config::{self, Config};

/// The config and the directory to open, or the code to exit with at once
/// (after the help).
pub fn parse() -> Result<(Config, PathBuf), ExitCode> {
    let mut start: Option<PathBuf> = None;
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "-h" | "--help" => {
                print_help();
                return Err(ExitCode::SUCCESS);
            }
            other => start = Some(PathBuf::from(other)),
        }
    }
    let start = start.unwrap_or_else(default_dir);
    let cfg = config::Paths::init()
        .and_then(|p| config::load(&p))
        .unwrap_or_default();
    Ok((cfg, start))
}

/// The exit code for how GNOSIS's run ended.
pub fn report(result: Result<(), String>) -> ExitCode {
    match result {
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
         s                sidebar of bookmarks and drives\n\
         y / p            yank / paste (copy)\n\
         x / Delete       move to trash\n\
         .                toggle hidden files\n\
         q / Esc          quit"
    );
}
