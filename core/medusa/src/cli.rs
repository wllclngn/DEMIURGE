// MEDUSA's command line, the same on both stacks:
//   medusa            interactive region capture (default; == `gui`)
//   medusa gui        same
//   medusa full       capture the whole screen, no overlay
//   medusa -p <dir>   write screenshots to <dir> (default $HOME)
//   medusa --no-copy  don't push the result to the clipboard
//   medusa -h         help

use std::path::PathBuf;
use std::process::ExitCode;

use demiurge_core::config;
use demiurge_core::torrentius;

use crate::overlay::GuiOpts;

pub enum Mode {
    Gui,
    Full,
}

/// The mode and options the arguments ask for, or the code to exit with at
/// once (after the help, or a bad argument).
pub fn parse() -> Result<(Mode, GuiOpts), ExitCode> {
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
                    return Err(ExitCode::FAILURE);
                }
            }
            "--no-copy" => copy = false,
            "-h" | "--help" => {
                print_help();
                return Err(ExitCode::SUCCESS);
            }
            other => {
                eprintln!("[medusa] unknown argument: {}", other);
                return Err(ExitCode::FAILURE);
            }
        }
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

    Ok((mode, GuiOpts { save_dir, copy, accent }))
}

/// The exit code for how MEDUSA's run ended.
pub fn report(result: Result<(), String>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[medusa] {}", e);
            ExitCode::FAILURE
        }
    }
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
