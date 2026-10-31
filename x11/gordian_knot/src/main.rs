// GORDIAN KNOT for the X11 stack -- the screen locker, idle daemon and
// greetd greeter. Installed setuid root; every mode settles its privilege
// first:
//
//   (default)   lock this session: The VT lock, which X sees no keys
//               through, and with $DISPLAY set the in-X lock only if no VT
//               can be taken
//   --x11 / --vt  force one path
//   --daemon    idle + lock-before-suspend watcher (runs as the user)
//   --login     greetd greeter drawn inside cage, the TTY greeter
//               without a Wayland display (runs as the user)
//   --ui FD WIN / --ui-fb FD FBFD
//               internal: the lock's renderer, exec'd by the holder
//
// The X11 and VT holders become root fully and never set NO_NEW_PRIVS
// (PAM's unix_chkpwd needs its setuid bit); see core/gordian_knot's
// privsep.rs.

mod daemon;
mod decoder;
mod inhibit;
mod window;
mod x11_lock;

use std::env;
use std::process::ExitCode;

use gordian_knot_core::cli::{self, code, report};
use gordian_knot_core::ui::{self, Framebuffer, Output};
use gordian_knot_core::{LockError, greeter, login, privsep, screen, vt};

enum Mode {
    Auto,
    ForceVt,
    ForceX11,
    Daemon,
    Login,
    CheckConfig,
    Ui(i32, u32),
    UiFb(i32, i32),
    Help,
    Version,
}

fn parse_args() -> (Mode, bool) {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut mode = Mode::Auto;
    let mut no_sandbox = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--vt" => mode = Mode::ForceVt,
            "--x11" => mode = Mode::ForceX11,
            "--daemon" => mode = Mode::Daemon,
            "--login" => mode = Mode::Login,
            "--check-config" => mode = Mode::CheckConfig,
            "--no-sandbox" => no_sandbox = true,
            "--ui" => {
                let (fd, win) = cli::two_args(&args, i, "--ui", "FD WINDOW");
                mode = Mode::Ui(fd, win);
                i += 2;
            }
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
        "usage: gordian_knot [--vt | --x11 | --daemon | --login | --check-config] [--no-sandbox] [-v] [-h]\n\
         \n\
         Default:        Lock this session: --vt, falling back to --x11 with\n\
                         $DISPLAY set if no VT can be taken\n\
         --vt            VT lock: The panels on a fresh console's framebuffer\n\
                         (a text prompt without one), switching locked\n\
         --x11           X11 in-session lock\n\
         --daemon        Idle + lock-before-suspend watcher\n\
         --login         greetd greeter, in cage or on the TTY (drives PAM via $GREETD_SOCK)\n\
         --check-config  Parse ~/.config/demiurge/config.toml and exit\n\
         --no-sandbox    Skip the renderer's seccomp + landlock (debug only)"
    );
}

fn main() -> ExitCode {
    let (mode, no_sandbox) = parse_args();
    let ident = privsep::invoking_user();
    // Resolved before any credential change: Afterwards /proc/self belongs
    // to root (a setuid exec leaves the process non-dumpable).
    let self_bin = env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "gordian_knot".into());

    if matches!(mode, Mode::Auto | Mode::ForceX11 | Mode::ForceVt) {
        cli::as_holder();
    } else if !matches!(mode, Mode::Ui(..) | Mode::UiFb(..)) {
        cli::as_user(&ident);
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
        Mode::Ui(fd, win) => {
            let open = || window::LockWindow::open(win).map(|w| Box::new(w) as Box<dyn Output>);
            return code(ui::run(fd, open, !no_sandbox));
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

    // Should a VT lock's holder die, its warden locks again in X when there
    // is a display, on a VT when there is not.
    let x = env::var_os("DISPLAY").is_some();
    let relock = if x { c"--x11" } else { c"--vt" };
    let sandbox = !no_sandbox;
    code(match mode {
        Mode::ForceVt => report(vt::lock(&gk.prompt, &ident, sandbox, relock)),
        Mode::ForceX11 => report(x11_lock::lock(gk, &ident, sandbox)),
        Mode::Auto if x => match vt::lock(&gk.prompt, &ident, sandbox, relock) {
            Err(LockError::Unavailable(e)) => {
                eprintln!("[gordian_knot] VT lock unavailable ({}); locking in X", e);
                report(x11_lock::lock(gk, &ident, sandbox))
            }
            other => report(other),
        },
        Mode::Auto => report(vt::lock(&gk.prompt, &ident, sandbox, relock)),
        Mode::Login => greeter::run(login::LoginConfig::new(gk, login::X11_SESSION), screen::Theme::new(gk)),
        Mode::Daemon => {
            // [input.idle].lock_seconds is the source of truth; the legacy
            // [gordian_knot].idle_timeout_seconds applies when it is 0.
            let threshold = if cfg.input.idle.lock_seconds > 0 {
                cfg.input.idle.lock_seconds as u64
            } else {
                gk.idle_timeout_seconds
            };
            daemon::run(threshold, &self_bin)
        }
        Mode::Help | Mode::Version | Mode::CheckConfig | Mode::Ui(..) | Mode::UiFb(..) => unreachable!(),
    })
}
