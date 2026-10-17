// GORDIAN KNOT -- X11 / VT / Wayland screen locker, idle daemon and greetd
// greeter. Installed setuid root; every mode settles its privilege first:
//
//   (default)   lock this session: X11 when $DISPLAY is set, falling back
//               to the VT lock only if nothing was locked; Wayland when
//               $WAYLAND_DISPLAY is set; else the VT lock
//   --x11 / --vt  force one path
//   --wl        Wayland ext-session-lock (runs as the user)
//   --daemon    idle + lock-before-suspend watcher (runs as the user)
//   --login     greetd greeter (runs as the user)
//   --ui FD WIN internal: the X11 lock's renderer, exec'd by the holder
//
// The X11 and VT holders become root fully and never set NO_NEW_PRIVS
// (PAM's unix_chkpwd needs its setuid bit); see gordian_knot/privsep.rs.

use std::env;
use std::process::ExitCode;

use demiurge_x11::config;
use demiurge_x11::gordian_knot::privsep::{self, Ident};
use demiurge_x11::gordian_knot::x11_lock::LockError;
use demiurge_x11::gordian_knot::{daemon, login, ui, vt, wl_lock, x11_lock};

enum Mode {
    Auto,
    ForceVt,
    ForceX11,
    ForceWl,
    Daemon,
    Login,
    CheckConfig,
    Ui(i32, u32),
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
            "--wl" => mode = Mode::ForceWl,
            "--daemon" => mode = Mode::Daemon,
            "--login" => mode = Mode::Login,
            "--check-config" => mode = Mode::CheckConfig,
            "--no-sandbox" => no_sandbox = true,
            "--ui" => {
                let fd = args.get(i + 1).and_then(|s| s.parse().ok());
                let win = args.get(i + 2).and_then(|s| s.parse().ok());
                match (fd, win) {
                    (Some(fd), Some(win)) => mode = Mode::Ui(fd, win),
                    _ => {
                        eprintln!("[gordian_knot] --ui takes FD WINDOW");
                        std::process::exit(1);
                    }
                }
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
        "usage: gordian_knot [--vt | --x11 | --wl | --daemon | --login | --check-config] [--no-sandbox] [-v] [-h]\n\
         \n\
         Default:        Lock this session: --wl if $WAYLAND_DISPLAY is set, else\n\
                         --x11 (with the VT lock as fallback if nothing locked),\n\
                         else --vt\n\
         --vt            VT lock (text mode on a fresh TTY, switching locked)\n\
         --x11           X11 in-session lock\n\
         --wl            Wayland in-session lock via ext-session-lock-v1\n\
         --daemon        Idle + lock-before-suspend watcher\n\
         --login         greetd greeter mode (drives PAM via $GREETD_SOCK)\n\
         --check-config  Parse ~/.config/demiurge/config.toml and exit\n\
         --no-sandbox    Skip the X11 renderer's seccomp + landlock (debug only)"
    );
}

fn load_config() -> Result<config::Config, String> {
    let paths = config::Paths::init()?;
    let mut cfg = config::load(&paths)?;
    // An empty [gordian_knot] accent inherits the shared [appearance] accent.
    if cfg.gordian_knot.accent.is_empty() {
        cfg.gordian_knot.accent = cfg.appearance.accent.clone();
    }
    Ok(cfg)
}

fn as_user(ident: &Ident) {
    if let Err(e) = privsep::drop_to_user(ident) {
        eprintln!("[gordian_knot] {}", e);
        std::process::exit(1);
    }
}

fn as_holder() {
    if let Err(e) = privsep::become_root() {
        eprintln!("[gordian_knot] {}", e);
        std::process::exit(1);
    }
    privsep::harden_holder();
}

fn main() -> ExitCode {
    let (mode, no_sandbox) = parse_args();
    let ident = privsep::invoking_user();
    // Resolved before any credential change: Afterwards /proc/self belongs
    // to root (a setuid exec leaves the process non-dumpable).
    let self_bin = env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "gordian_knot".into());

    let auto_wl = matches!(mode, Mode::Auto) && env::var_os("WAYLAND_DISPLAY").is_some();
    let holder = !auto_wl && matches!(mode, Mode::Auto | Mode::ForceX11 | Mode::ForceVt);
    if holder {
        as_holder();
    } else if !matches!(mode, Mode::Ui(..)) {
        as_user(&ident);
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
            return code(ui::run(fd, win, !no_sandbox));
        }
        Mode::CheckConfig => {
            return match load_config() {
                Ok(_) => {
                    println!("gordian_knot: config ok");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("gordian_knot: config error: {}", e);
                    ExitCode::FAILURE
                }
            };
        }
        _ => {}
    }

    let cfg = match load_config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[gordian_knot] config: {}", e);
            return ExitCode::FAILURE;
        }
    };
    let gk = &cfg.gordian_knot;

    code(match mode {
        Mode::ForceVt => vt::lock(&gk.prompt, &ident),
        Mode::ForceX11 => match x11_lock::lock(gk, &ident, !no_sandbox) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("[gordian_knot] {}", e);
                1
            }
        },
        Mode::ForceWl => run_wl(gk, &ident.name),
        Mode::Auto if auto_wl => run_wl(gk, &ident.name),
        Mode::Auto if env::var_os("DISPLAY").is_some() => match x11_lock::lock(gk, &ident, !no_sandbox) {
            Ok(()) => 0,
            Err(LockError::Unavailable(e)) => {
                eprintln!("[gordian_knot] X11 lock unavailable ({}); falling back to the VT lock", e);
                vt::lock(&gk.prompt, &ident)
            }
            Err(e) => {
                eprintln!("[gordian_knot] {}", e);
                1
            }
        },
        Mode::Auto => vt::lock(&gk.prompt, &ident),
        Mode::Login => login::run(&login::LoginConfig {
            session_cmd: gk.session_cmd.clone(),
            session_env: gk.session_env.clone(),
        }),
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
        Mode::Help | Mode::Version | Mode::CheckConfig | Mode::Ui(..) => unreachable!(),
    })
}

fn run_wl(gk: &config::GordianKnot, user: &str) -> i32 {
    match wl_lock::run(gk, user) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("[gordian_knot] wl lock: {}", e);
            1
        }
    }
}

fn code(c: i32) -> ExitCode {
    if c == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
