use cherrypie::backend;
use cherrypie::config;
use cherrypie::daemon;

use std::time::Duration;

const VERSION: &str = env!("CARGO_PKG_VERSION");

enum Command {
    Daemon { config: Option<String>, dry_run: bool, learn_after: Duration },
    Help,
    Version,
}

fn parse_args() -> Command {
    let args: Vec<String> = std::env::args().collect();
    let mut config = None;
    let mut dry_run = false;
    let mut learn_after = daemon::LEARN_AFTER;
    let mut i = 1;

    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => return Command::Help,
            "--version" | "-V" => return Command::Version,
            "--dry-run" => dry_run = true,
            "--config" | "-c" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("--config requires a path");
                    std::process::exit(1);
                }
                config = Some(args[i].clone());
            }
            "--learn-after" => {
                i += 1;
                match args.get(i).and_then(|s| s.parse::<u64>().ok()) {
                    Some(secs) => learn_after = Duration::from_secs(secs),
                    None => {
                        eprintln!("--learn-after requires a number of seconds");
                        std::process::exit(1);
                    }
                }
            }
            other => {
                eprintln!("unknown argument: {}", other);
                std::process::exit(1);
            }
        }
        i += 1;
    }

    Command::Daemon { config, dry_run, learn_after }
}

fn print_help() {
    println!("cherrypie {} - window matching daemon", VERSION);
    println!();
    println!("USAGE:");
    println!("    cherrypie [OPTIONS]");
    println!();
    println!("OPTIONS:");
    println!("    -c, --config <PATH>    Config file (default: ~/.config/cherrypie/config.toml;");
    println!("                           created with a learned rule for each new window class)");
    println!("    --learn-after <SECS>   Write a learned rule only once a window of the class");
    println!("                           has stayed open this long (default: {})",
             daemon::LEARN_AFTER.as_secs());
    println!("    --dry-run              Log matches without applying actions");
    println!("    -h, --help             Show this help");
    println!("    -V, --version          Show version");
}

fn main() {
    match parse_args() {
        Command::Help => {
            print_help();
        }
        Command::Version => {
            println!("cherrypie {}", VERSION);
        }
        Command::Daemon { config, dry_run, learn_after } => {
            let paths = match config {
                Some(path) => config::Paths::with_config(path.into()),
                None => match config::Paths::init() {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("[cherrypie] {}", e);
                        std::process::exit(1);
                    }
                },
            };

            // One daemon per user and display: DEMIURGE's [startup] and the
            // systemd unit can both start one, and two would learn every new
            // window class twice. The lock lives as long as the process.
            if !daemon::claim_instance() {
                eprintln!("[cherrypie] already running on this display; exiting");
                std::process::exit(0);
            }

            // Signal handling before anything else so shutdown works during init
            let signal_fd = daemon::setup_signalfd();

            let wm = match backend::WindowManager::init(signal_fd) {
                Ok(wm) => wm,
                Err(e) => {
                    eprintln!("[cherrypie] {}", e);
                    std::process::exit(1);
                }
            };

            daemon::run(wm, &paths.config_file, dry_run, signal_fd, learn_after);
        }
    }
}
