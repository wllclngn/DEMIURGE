// GORDIAN KNOT for the Wayland stack -- the screen locker and greetd
// greeter, with nothing of X11 in it. demiurge-wl runs it for the lock bind
// and its idle timer:
//
//   (default)   lock this session through ext-session-lock-v1 (runs as
//               the user)
//   --vt        the VT lock, setuid root as on X11
//   --login     greetd greeter drawn inside cage, the TTY greeter
//               without a Wayland display (runs as the user)
//   --ui-fb FD FBFD
//               internal: the VT lock's renderer, exec'd by the holder

mod wl_lock;

use std::env;
use std::process::ExitCode;

use gordian_knot_core::cli::{self, code, report};
use gordian_knot_core::ui::{self, Framebuffer, Output};
use gordian_knot_core::login::{self, LoginConfig};
use gordian_knot_core::{greeter, privsep, screen, vt};

enum Mode {
    Lock,
    ForceVt,
    Login,
    CheckConfig,
    UiFb(i32, i32),
    Help,
    Version,
}

fn parse_args() -> (Mode, bool) {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut mode = Mode::Lock;
    let mut no_sandbox = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--wl" => mode = Mode::Lock,
            "--vt" => mode = Mode::ForceVt,
            "--login" => mode = Mode::Login,
            "--check-config" => mode = Mode::CheckConfig,
            "--no-sandbox" => no_sandbox = true,
            "--ui-fb" => {
                let (fd, fb) = cli::two_args(&args, i, "--ui-fb", "FD FBFD");
                mode = Mode::UiFb(fd, fb);
                i += 2;
            }
            "-h" | "--help" => mode = Mode::Help,
            "-v" | "--version" => mode = Mode::Version,
            other => {
                eprintln!("[gordian_knot] unknown argument: {}", other);
                std::process::exit(1);
            }
        }
        i += 1;
    }
    (mode, no_sandbox)
}

fn print_help() {
    println!(
        "usage: gordian_knot [--wl | --vt | --login | --check-config] [--no-sandbox] [-v] [-h]\n\
         \n\
         Default, --wl:  Lock this session through ext-session-lock-v1\n\
         --vt            VT lock: The panels on a fresh console's framebuffer\n\
                         (a text prompt without one), switching locked\n\
         --login         greetd greeter, in cage or on the TTY (drives PAM via $GREETD_SOCK)\n\
         --check-config  Parse ~/.config/demiurge/config.toml and exit\n\
         --no-sandbox    Skip the renderer's seccomp + landlock (debug only)"
    );
}

fn main() -> ExitCode {
    let (mode, no_sandbox) = parse_args();
    let ident = privsep::invoking_user();
    match mode {
        Mode::ForceVt => cli::as_holder(),
        Mode::UiFb(..) => {}
        _ => cli::as_user(&ident),
    }

    match mode {
        Mode::Help => {
            print_help();
            return ExitCode::SUCCESS;
        }
        Mode::Version => {
            println!("gordian_knot {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Mode::UiFb(fd, fb) => {
            let open = || Framebuffer::open(fb).map(|f| Box::new(f) as Box<dyn Output>);
            return code(ui::run(fd, open, !no_sandbox));
        }
        Mode::CheckConfig => return cli::check_config(),
        _ => {}
    }

    let cfg = match cli::load_config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[gordian_knot] config: {}", e);
            return ExitCode::FAILURE;
        }
    };
    let gk = &cfg.gordian_knot;
    code(match mode {
        // Should the VT lock's holder die, its warden locks a VT again.
        Mode::ForceVt => report(vt::lock(&gk.prompt, &ident, !no_sandbox, c"--vt")),
        Mode::Lock => match wl_lock::run(gk, &ident.name) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("[gordian_knot] wl lock: {}", e);
                1
            }
        },
        Mode::Login => greeter::run(LoginConfig::new(gk, login::WAYLAND_SESSION), screen::Theme::new(gk)),
        Mode::Help | Mode::Version | Mode::CheckConfig | Mode::UiFb(..) => unreachable!(),
    })
}
