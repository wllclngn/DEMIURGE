use cherrypie::backend;
use cherrypie::config;
use cherrypie::daemon;

const VERSION: &str = env!("CARGO_PKG_VERSION");

enum Command {
    Daemon { config: Option<String>, dry_run: bool },
    Help,
    Version,
}

fn parse_args() -> Command {
    let args: Vec<String> = std::env::args().collect();
    let mut config = None;
    let mut dry_run = false;
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
            other => {
                eprintln!("unknown argument: {}", other);
                std::process::exit(1);
            }
        }
        i += 1;
    }

    Command::Daemon { config, dry_run }
}

fn print_help() {
    println!("cherrypie {} - window matching daemon", VERSION);
    println!();
    println!("USAGE:");
    println!("    cherrypie [OPTIONS]");
    println!();
    println!("OPTIONS:");
    println!("    -c, --config <PATH>    Config file (default: ~/.config/cherrypie/config.toml)");
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
        Command::Daemon { config, dry_run } => {
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

            if !paths.config_file.exists() {
                eprintln!(
                    "[cherrypie] config not found: {}",
                    paths.config_file.display()
                );
                eprintln!("[cherrypie] create it and add rules, then restart");
                std::process::exit(1);
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

            daemon::run(wm, &paths.config_file, dry_run, signal_fd);
        }
    }
}
