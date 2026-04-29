//! ABRAXAS - Dynamic color temperature daemon (Rust implementation)
//!
//! Commands:
//!   --daemon         Run as daemon (default)
//!   --status         Show current status
//!   --set-location   Set location (ZIP or lat,lon)
//!   --refresh        Force weather refresh
//!   --set TEMP [MIN] Manual override to TEMP over MIN minutes
//!   --resume         Clear manual override
//!   --reset          Restore gamma and exit
//!   --help           Show usage

mod config;
mod daemon;
mod gamma;
mod landlock;
mod seccomp;
mod sigmoid;
mod solar;
mod uring;
mod weather;
mod zipdb;

use std::process;

/// Temperature bounds (Kelvin)
pub const TEMP_MIN: i32 = 1000;
pub const TEMP_MAX: i32 = 25000;

/// Temperature targets
pub const TEMP_DAY_CLEAR: i32 = 6500;
pub const TEMP_DAY_DARK: i32 = 4500;
pub const TEMP_NIGHT: i32 = 2900;

/// Cloud threshold (% cover that triggers dark mode)
pub const CLOUD_THRESHOLD: i32 = 60;

/// Cloud transition: sigmoid duration in minutes
pub const CLOUD_TRANSITION_MIN: f64 = 10.0;

/// Timing
pub const WEATHER_REFRESH_SEC: i64 = 900; // 15 minutes
pub const TEMP_UPDATE_SEC: i64 = 60; // 1 minute

/// Transition windows (minutes)
pub const DAWN_DURATION: f64 = 90.0;
pub const DUSK_DURATION: f64 = 180.0;

/// Dawn offset: shift sigmoid midpoint this many minutes after sunrise
pub const DAWN_OFFSET: f64 = 30.0;

/// Dusk offset: shift sigmoid midpoint this many minutes before sunset
pub const DUSK_OFFSET: f64 = 30.0;

/// Sigmoid steepness for transitions
pub const SIGMOID_STEEPNESS: f64 = 8.0;

enum Command {
    Daemon,
    Status,
    SetLocation(String),
    Refresh,
    Set { temp: i32, duration: i32 },
    Resume,
    Reset,
    Benchmark,
}

fn print_usage() {
    eprintln!("abraxas - Dynamic color temperature daemon");
    eprintln!();
    eprintln!("Usage: abraxas [COMMAND]");
    eprintln!();
    eprintln!("  --daemon              Run daemon (default)");
    eprintln!("  --status              Show current status");
    eprintln!("  --set-location LOC    Set location (ZIP code or LAT,LON)");
    eprintln!("  --refresh             Force weather refresh");
    eprintln!("  --set TEMP [MINUTES]  Override to TEMP over MINUTES (default 3)");
    eprintln!("  --resume              Clear override, resume solar control");
    eprintln!("  --reset               Restore gamma and exit");
    eprintln!("  --benchmark           Run nanosecond benchmark");
    eprintln!("  --help                Show this help");
}

fn parse_args() -> Command {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        return Command::Daemon;
    }

    match args[1].as_str() {
        "--daemon" | "daemon" => Command::Daemon,
        "--status" | "status" => Command::Status,
        "--set-location" | "set-location" => {
            if args.len() < 3 {
                eprintln!("--set-location requires a location argument");
                eprintln!("  Example: abraxas --set-location 60614");
                eprintln!("  Example: abraxas --set-location 41.88,-87.63");
                process::exit(1);
            }
            Command::SetLocation(args[2].clone())
        }
        "--refresh" | "refresh" => Command::Refresh,
        "--set" | "set" => {
            if args.len() < 3 {
                eprintln!("--set requires a temperature argument");
                eprintln!("  Example: abraxas --set 3500 30");
                process::exit(1);
            }
            let temp: i32 = match args[2].parse() {
                Ok(v) => v,
                Err(_) => {
                    eprintln!("Invalid temperature: {}", args[2]);
                    process::exit(1);
                }
            };
            let duration: i32 = if args.len() >= 4 {
                match args[3].parse() {
                    Ok(v) => v,
                    Err(_) => {
                        eprintln!("Invalid duration: {}", args[3]);
                        process::exit(1);
                    }
                }
            } else {
                3
            };
            Command::Set { temp, duration }
        }
        "--resume" | "resume" => Command::Resume,
        "--reset" | "reset" => Command::Reset,
        "--benchmark" | "benchmark" => Command::Benchmark,
        "--help" | "-h" | "help" => {
            print_usage();
            process::exit(0);
        }
        other => {
            eprintln!("Unknown command: {}", other);
            print_usage();
            process::exit(1);
        }
    }
}

fn main() {
    let command = parse_args();

    let paths = match config::Paths::init() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Failed to initialize paths: {e}");
            process::exit(1);
        }
    };

    // Commands that don't need location
    match &command {
        Command::Reset => {
            cmd_reset(&paths);
            return;
        }
        Command::Resume => {
            cmd_resume(&paths);
            return;
        }
        Command::Benchmark => {
            cmd_benchmark(&paths);
            return;
        }
        Command::SetLocation(location) => {
            process::exit(cmd_set_location(location, &paths));
        }
        Command::Set { temp, duration } => {
            process::exit(cmd_set_temp(*temp, *duration, &paths));
        }
        _ => {}
    }

    // Remaining commands need location
    let loc = match config::load_location(&paths) {
        Some(loc) => loc,
        None => {
            eprintln!("No location configured. Use --set-location first.");
            eprintln!("  Example: abraxas --set-location 60614");
            eprintln!("  Example: abraxas --set-location 41.88,-87.63");
            process::exit(1);
        }
    };

    weather::init();

    let result = match command {
        Command::Status => {
            cmd_status(loc.lat, loc.lon, &paths);
            0
        }
        Command::Refresh => cmd_refresh(loc.lat, loc.lon, &paths),
        Command::Set { temp, duration } => cmd_set_temp(temp, duration, &paths),
        Command::Daemon => {
            daemon::run(loc, &paths);
            0
        }
        _ => unreachable!(),
    };

    weather::cleanup();
    process::exit(result);
}

fn cmd_status(lat: f64, lon: f64, paths: &config::Paths) {
    println!("ABRAXAS v8.6.0 [Rust]\n");
    println!("Location: {:.4}, {:.4}\n", lat, lon);

    let now = chrono_now();
    let st = solar::sunrise_sunset(now, lat, lon);
    let sp = solar::position(now, lat, lon);

    let local = local_time(now);
    println!(
        "Date: {:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        local.year, local.month, local.day, local.hour, local.min, local.sec
    );

    if let Some(ref times) = st {
        let sr = local_time(times.sunrise);
        let ss = local_time(times.sunset);
        println!("Sunrise: {:02}:{:02}", sr.hour, sr.min);
        println!("Sunset: {:02}:{:02}", ss.hour, ss.min);
    } else {
        println!("Sunrise/Sunset: N/A (polar region)");
    }
    println!("Sun elevation: {:.1} degrees\n", sp.elevation);

    // Weather
    let weather = config::load_weather_cache(paths);
    if let Some(ref w) = weather {
        if !w.has_error {
            println!("Weather: {}", w.forecast);
            println!("Cloud cover: {}%", w.cloud_cover);

            let ft = local_time(w.fetched_at);
            println!(
                "Last updated: {:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                ft.year, ft.month, ft.day, ft.hour, ft.min, ft.sec
            );
        } else {
            println!("Weather: Not available");
        }
    } else {
        println!("Weather: Not available");
    }
    println!();

    // Override status
    let ovr = config::load_override(paths);
    if let Some(ref o) = ovr {
        if o.active {
            let elapsed_min = (now - o.issued_at) as f64 / 60.0;
            println!("Mode: MANUAL OVERRIDE");
            if elapsed_min >= o.duration_minutes as f64 {
                println!(
                    "Holding at: {}K (until solar reaches {}K)",
                    o.target_temp, o.target_temp
                );
            } else {
                println!("Target: {}K over {} min", o.target_temp, o.duration_minutes);
            }

            let it = local_time(o.issued_at);
            println!(
                "Issued: {:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                it.year, it.month, it.day, it.hour, it.min, it.sec
            );
            return;
        }
    }

    let cloud_dark = weather
        .as_ref()
        .map(|w| !w.has_error && w.cloud_cover >= CLOUD_THRESHOLD)
        .unwrap_or(false);
    let cloud_factor = if cloud_dark { 1.0 } else { 0.0 };

    let (min_from_sunrise, min_to_sunset) = if let Some(ref times) = st {
        (
            (now - times.sunrise) as f64 / 60.0,
            (times.sunset - now) as f64 / 60.0,
        )
    } else {
        (0.0, 0.0)
    };

    let temp = sigmoid::calculate_solar_temp(min_from_sunrise, min_to_sunset, cloud_factor);

    println!("Mode: {}", if cloud_dark { "DARK" } else { "CLEAR" });
    println!("Target temperature: {}K", temp);
}

fn cmd_set_location(loc_str: &str, paths: &config::Paths) -> i32 {
    if loc_str.contains(',') {
        let parts: Vec<&str> = loc_str.split(',').collect();
        if parts.len() != 2 {
            eprintln!("Invalid format. Use: LAT,LON (e.g., 41.88,-87.63)");
            return 1;
        }
        let lat: f64 = match parts[0].parse() {
            Ok(v) => v,
            Err(_) => {
                eprintln!("Invalid format. Use: LAT,LON (e.g., 41.88,-87.63)");
                return 1;
            }
        };
        let lon: f64 = match parts[1].parse() {
            Ok(v) => v,
            Err(_) => {
                eprintln!("Invalid format. Use: LAT,LON (e.g., 41.88,-87.63)");
                return 1;
            }
        };

        if config::save_location(paths, lat, lon).is_err() {
            eprintln!("Failed to save config");
            return 1;
        }
        println!("Location set to: {:.4}, {:.4}", lat, lon);
        return 0;
    }

    // ZIP code
    if loc_str.len() != 5 || !loc_str.chars().all(|c| c.is_ascii_digit()) {
        eprintln!("Invalid ZIP code. Must be 5 digits.");
        return 1;
    }

    println!("Looking up ZIP code {}...", loc_str);
    match zipdb::lookup(&paths.zipdb_file, loc_str) {
        Some((lat, lon)) => {
            println!("Found: {} -> {:.4}, {:.4}", loc_str, lat, lon);
            if config::save_location(paths, lat as f64, lon as f64).is_err() {
                eprintln!("Failed to save config");
                return 1;
            }
            println!("Location set to: {:.4}, {:.4}", lat, lon);
            0
        }
        None => {
            eprintln!("ZIP code {} not found in database.", loc_str);
            1
        }
    }
}

fn cmd_refresh(lat: f64, lon: f64, paths: &config::Paths) -> i32 {
    println!("Fetching weather...");
    let wd = weather::fetch(lat, lon);

    if wd.has_error {
        eprintln!("Weather fetch failed");
        return 1;
    }

    let _ = config::save_weather_cache(paths, &wd);
    println!("Weather: {}", wd.forecast);
    println!("Cloud cover: {}%", wd.cloud_cover);
    0
}

fn cmd_set_temp(target_temp: i32, duration_min: i32, paths: &config::Paths) -> i32 {
    if target_temp < TEMP_MIN || target_temp > TEMP_MAX {
        eprintln!("Temperature must be between {}K and {}K.", TEMP_MIN, TEMP_MAX);
        return 1;
    }

    let ovr = config::OverrideState {
        active: true,
        target_temp,
        duration_minutes: duration_min,
        issued_at: now_epoch(),
        start_temp: 0, // daemon fills this
    };

    if config::save_override(paths, &ovr).is_err() {
        eprintln!("Failed to write override");
        return 1;
    }

    if duration_min > 0 {
        println!("Override: -> {}K over {} min (sigmoid)", target_temp, duration_min);
    } else {
        println!("Override: -> {}K (instant)", target_temp);
    }

    if config::check_daemon_alive(paths) {
        println!("Daemon will process on next tick (up to 60s).");
    } else {
        eprintln!("[warn] Daemon is not running. Override saved but won't apply until daemon starts.");
    }
    0
}

fn cmd_resume(paths: &config::Paths) {
    let ovr = config::OverrideState {
        active: false,
        target_temp: 0,
        duration_minutes: 0,
        issued_at: 0,
        start_temp: 0,
    };
    let _ = config::save_override(paths, &ovr);

    if config::check_daemon_alive(paths) {
        println!("Resume sent. Daemon will return to solar control.");
    } else {
        eprintln!("[warn] Daemon is not running. Resume saved but won't apply until daemon starts.");
    }
}

fn cmd_reset(paths: &config::Paths) {
    config::clear_override(paths);

    if let Ok(mut state) = gamma::init() {
        let _ = state.restore();
    }

    println!("Screen temperature reset.");
}

fn cmd_benchmark(paths: &config::Paths) {
    println!("ABRAXAS v8.6.0 [Rust] -- Kernel-grade benchmark");
    println!("Clock: CLOCK_MONOTONIC_RAW (hardware TSC)\n");

    fn bench_ns() -> u64 {
        let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC_RAW, &mut ts) };
        ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
    }

    const N: u64 = 1000;

    // config_load_location
    let start = bench_ns();
    for _ in 0..N {
        let _ = config::load_location(paths);
    }
    let elapsed = bench_ns() - start;
    println!("  config_load_location()     {:>8} us  ({} ns/call, {} calls)",
        elapsed / 1000, elapsed / N, N);

    // Get location for solar calcs
    let loc = config::load_location(paths);
    let (lat, lon) = loc.map(|l| (l.lat, l.lon)).unwrap_or((41.88, -87.63));
    let now = now_epoch();

    // solar_sunrise_sunset
    let start = bench_ns();
    for _ in 0..N {
        let _ = solar::sunrise_sunset(now, lat, lon);
    }
    let elapsed = bench_ns() - start;
    println!("  solar_sunrise_sunset()     {:>8} us  ({} ns/call, {} calls)",
        elapsed / 1000, elapsed / N, N);

    // solar_position
    let start = bench_ns();
    for _ in 0..N {
        let _ = solar::position(now, lat, lon);
    }
    let elapsed = bench_ns() - start;
    println!("  solar_position()           {:>8} us  ({} ns/call, {} calls)",
        elapsed / 1000, elapsed / N, N);

    // calculate_solar_temp
    let start = bench_ns();
    for _ in 0..N {
        std::hint::black_box(sigmoid::calculate_solar_temp(
            std::hint::black_box(120.0),
            std::hint::black_box(300.0),
            std::hint::black_box(0.0),
        ));
    }
    let elapsed = bench_ns() - start;
    println!("  calculate_solar_temp()     {:>8} us  ({} ns/call, {} calls)",
        elapsed / 1000, elapsed / N, N);

    // sigmoid_norm
    let start = bench_ns();
    for _ in 0..N {
        std::hint::black_box(sigmoid::sigmoid_norm(
            std::hint::black_box(0.5),
            std::hint::black_box(SIGMOID_STEEPNESS),
        ));
    }
    let elapsed = bench_ns() - start;
    println!("  sigmoid_norm()             {:>8} us  ({} ns/call, {} calls)",
        elapsed / 1000, elapsed / N, N);

    // config_load_override
    let start = bench_ns();
    for _ in 0..N {
        let _ = config::load_override(paths);
    }
    let elapsed = bench_ns() - start;
    println!("  config_load_override()     {:>8} us  ({} ns/call, {} calls)",
        elapsed / 1000, elapsed / N, N);

    // config_load_weather_cache
    let start = bench_ns();
    for _ in 0..N {
        let _ = config::load_weather_cache(paths);
    }
    let elapsed = bench_ns() - start;
    println!("  config_load_weather_cache(){:>8} us  ({} ns/call, {} calls)",
        elapsed / 1000, elapsed / N, N);

    // io_uring setup + teardown
    println!();
    println!("Kernel facilities:");
    let start = bench_ns();
    let ring = uring::AbraxasRing::init(8);
    let elapsed = bench_ns() - start;
    if ring.is_some() {
        println!("  io_uring_setup()           {:>8} us  (one-time)", elapsed / 1000);
        drop(ring);
    } else {
        println!("  io_uring_setup()           UNAVAILABLE");
    }
}

// Time helpers

pub fn now_epoch() -> i64 {
    unsafe { libc::time(std::ptr::null_mut()) as i64 }
}

fn chrono_now() -> i64 {
    now_epoch()
}

struct LocalTime {
    year: i32,
    month: i32,
    day: i32,
    hour: i32,
    min: i32,
    sec: i32,
}

fn local_time(epoch: i64) -> LocalTime {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let t = epoch;
    unsafe { libc::localtime_r(&t, &mut tm) };
    LocalTime {
        year: tm.tm_year + 1900,
        month: tm.tm_mon + 1,
        day: tm.tm_mday,
        hour: tm.tm_hour,
        min: tm.tm_min,
        sec: tm.tm_sec,
    }
}
