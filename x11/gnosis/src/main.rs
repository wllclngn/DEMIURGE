// GNOSIS -- DEMIURGE's file manager, its X11 binary. Replaces pcmanfm.
//
// Keyboard-first, no GTK/Qt, renders with torrentius (Cairo + Pango). The
// browser and the command line are core's (gnosis::browser, gnosis::cli).

use std::process::ExitCode;

mod app;

use app::App;
use gnosis::cli;

fn main() -> ExitCode {
    let (cfg, start) = match cli::parse() {
        Ok(parsed) => parsed,
        Err(code) => return code,
    };
    cli::report(App::run(&cfg, start))
}
