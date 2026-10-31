// What both binaries' entry points share: The config with its accent
// resolved, each mode's privilege settled first, and exit codes.

use std::process::ExitCode;

use demiurge_core::config;

use crate::LockError;
use crate::privsep::{self, Ident};

pub fn load_config() -> Result<config::Config, String> {
    let paths = config::Paths::init()?;
    let mut cfg = config::load(&paths)?;
    // An empty [gordian_knot] accent inherits the shared [appearance] accent.
    if cfg.gordian_knot.accent.is_empty() {
        cfg.gordian_knot.accent = cfg.appearance.accent.clone();
    }
    Ok(cfg)
}

pub fn check_config() -> ExitCode {
    match load_config() {
        Ok(_) => {
            println!("gordian_knot: config ok");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("gordian_knot: config error: {}", e);
            ExitCode::FAILURE
        }
    }
}

pub fn as_user(ident: &Ident) {
    if let Err(e) = privsep::drop_to_user(ident) {
        eprintln!("[gordian_knot] {}", e);
        std::process::exit(1);
    }
}

pub fn as_holder() {
    if let Err(e) = privsep::become_root() {
        eprintln!("[gordian_knot] {}", e);
        std::process::exit(1);
    }
    privsep::harden_holder();
}

pub fn report(result: Result<(), LockError>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("[gordian_knot] {}", e);
            1
        }
    }
}

pub fn code(c: i32) -> ExitCode {
    if c == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

// The value after a flag taking two fds or ids, or the usage error.
pub fn two_args<A: std::str::FromStr, B: std::str::FromStr>(args: &[String], i: usize, flag: &str, usage: &str) -> (A, B) {
    let a = args.get(i + 1).and_then(|s| s.parse().ok());
    let b = args.get(i + 2).and_then(|s| s.parse().ok());
    match (a, b) {
        (Some(a), Some(b)) => (a, b),
        _ => {
            eprintln!("[gordian_knot] {} takes {}", flag, usage);
            std::process::exit(1);
        }
    }
}
