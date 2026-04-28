// GORDIAN KNOT -- VT + X11 screen locker.
//
// Dispatcher logic:
//   --vt        force the VT path (text-mode lock on a fresh TTY)
//   --daemon    idle-watcher mode (poll XScreenSaver, trigger lock on idle)
//   (default)   try X11 in-session lock; fall back to VT if the grab fails
//               or $DISPLAY is unset
//
// Privilege model:
//   X11 path:   runs entirely as the invoking user. No setuid needed.
//               PAM delegates shadow access to unix_chkpwd (setuid helper).
//   VT path:    requires setuid-root at install time. Parent holds root
//               to drive VT ioctls; child forks + drops to the invoking
//               user before PAM + UI.
//   Daemon:     runs as the invoking user. Spawns a fresh `gordian_knot`
//               (this binary, without --daemon) when the idle threshold
//               is crossed. The respawned process picks X11 or VT itself.

use std::env;
use std::process::ExitCode;

use demiurge_x11::config;
use demiurge_x11::gordian_knot::{daemon, landlock, privsep, seccomp, vt, x11_lock};

enum Mode {
    Auto,
    ForceVt,
    ForceX11,
    Daemon,
    CheckConfig,
    Help,
    Version,
}

fn parse_args() -> (Mode, bool) {
    let args: Vec<String> = env::args().collect();
    let mut mode = Mode::Auto;
    let mut no_sandbox = false;
    for a in args.iter().skip(1) {
        match a.as_str() {
            "--vt" => mode = Mode::ForceVt,
            "--x11" => mode = Mode::ForceX11,
            "--daemon" => mode = Mode::Daemon,
            "--check-config" => mode = Mode::CheckConfig,
            "--no-sandbox" => no_sandbox = true,
            "-h" | "--help" => mode = Mode::Help,
            "-v" | "--version" => mode = Mode::Version,
            other => {
                eprintln!("[gordian_knot] unknown argument: {}", other);
                std::process::exit(1);
            }
        }
    }
    (mode, no_sandbox)
}

fn print_help() {
    println!(
        "usage: gordian_knot [--vt | --x11 | --daemon | --check-config] [--no-sandbox] [-v] [-h]\n\
         \n\
         Default:        X11 in-session lock (fallback to VT if grab fails)\n\
         --vt            Force VT locker (text-mode on a fresh TTY)\n\
         --x11           Force X11 in-session lock (error if not available)\n\
         --daemon        Idle-watcher mode (spawns gordian_knot on idle)\n\
         --check-config  Parse ~/.config/demiurge/config.toml and exit\n\
         --no-sandbox    Skip seccomp + landlock (debug only)"
    );
}

fn load_config() -> Result<config::Config, String> {
    let paths = config::Paths::init()?;
    config::load(&paths)
}

fn current_username() -> String {
    unsafe {
        let uid = libc::getuid();
        let pw = libc::getpwuid(uid);
        if pw.is_null() {
            return String::from("user");
        }
        std::ffi::CStr::from_ptr((*pw).pw_name)
            .to_string_lossy()
            .into_owned()
    }
}

fn install_sandbox(no_sandbox: bool) {
    if no_sandbox {
        eprintln!("[gordian_knot] --no-sandbox: seccomp + landlock skipped");
        return;
    }
    if !landlock::install_sandbox() {
        eprintln!("[gordian_knot] landlock unavailable (kernel <5.13?); continuing");
    }
    if !seccomp::install_filter() {
        eprintln!("[gordian_knot] seccomp install failed; continuing without filter");
    }
}

// X11 path. No setuid needed. Runs as user. PAM + UI + sandbox all in
// this process.
fn run_x11(cfg: &config::GordianKnot, user: &str, no_sandbox: bool) -> i32 {
    install_sandbox(no_sandbox);
    match x11_lock::lock(cfg, user) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("[gordian_knot] x11 lock: {}", e);
            1
        }
    }
}

// VT path. Parent stays root, does VT ioctls. Child drops to user, runs
// sandbox + PAM. Parent waits + releases VT on child exit.
fn run_vt(cfg: &config::GordianKnot, user: &str, no_sandbox: bool) -> i32 {
    let _ = cfg; // VT UI is text-only; only the user name + prompt are read.

    let mut session = match vt::acquire() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[gordian_knot] VT acquire: {}", e);
            return 1;
        }
    };

    let prompt = cfg.prompt.clone();
    let user_owned = user.to_string();
    let tty_fd = session.tty_fd;

    let pid = match privsep::fork_child(move || {
        if let Err(e) = privsep::drop_to_real_user() {
            eprintln!("[gordian_knot] privsep drop: {}", e);
            return 1;
        }
        install_sandbox(no_sandbox);
        vt::prompt_loop(tty_fd, &user_owned, &prompt)
    }) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[gordian_knot] fork: {}", e);
            vt::release(&mut session);
            return 1;
        }
    };

    let code = privsep::wait_child(pid);
    vt::release(&mut session);
    if code == 0 {
        0
    } else {
        1
    }
}

fn main() -> ExitCode {
    let (mode, no_sandbox) = parse_args();

    match mode {
        Mode::Help => {
            print_help();
            return ExitCode::SUCCESS;
        }
        Mode::Version => {
            println!("gordian_knot {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
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

    let user = current_username();

    let code = match mode {
        Mode::ForceVt => run_vt(&cfg.gordian_knot, &user, no_sandbox),
        Mode::ForceX11 => run_x11(&cfg.gordian_knot, &user, no_sandbox),
        Mode::Daemon => {
            // Path to our own binary so the daemon can respawn us for each
            // lock session. argv[0] is what the kernel was asked to exec.
            let self_bin = env::args().next().unwrap_or_else(|| "gordian_knot".into());
            daemon::run(&cfg.gordian_knot, &self_bin)
        }
        Mode::Auto => {
            if env::var_os("DISPLAY").is_some() {
                let rc = run_x11(&cfg.gordian_knot, &user, no_sandbox);
                if rc == 0 {
                    0
                } else {
                    // X11 grab failed or errored -- try VT as fallback.
                    eprintln!("[gordian_knot] falling back to VT lock");
                    run_vt(&cfg.gordian_knot, &user, no_sandbox)
                }
            } else {
                run_vt(&cfg.gordian_knot, &user, no_sandbox)
            }
        }
        _ => unreachable!(),
    };

    match code {
        0 => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}
