//! Configuration, override state, and path resolution.
//!
//! TOML config with a [location] section, written on first run from the
//! system timezone's coordinates. JSON override and weather cache via serde.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::{WEATHER_REFRESH_SEC, now_epoch};

const ZONE_TABLES: &[&str] = &[
    "/usr/share/zoneinfo/zone1970.tab",
    "/usr/share/zoneinfo/zone.tab",
];

/// Resolved filesystem paths
#[derive(Clone)]
pub struct Paths {
    pub config_file: PathBuf,
    pub legacy_ini: PathBuf,
    pub cache_file: PathBuf,
    pub override_file: PathBuf,
    pub zipdb_file: PathBuf,
    pub pid_file: PathBuf,
}

impl Paths {
    pub fn init() -> Result<Self, io::Error> {
        let home = std::env::var("HOME").map_err(|_| {
            io::Error::new(io::ErrorKind::NotFound, "HOME not set")
        })?;

        let config_dir = PathBuf::from(&home).join(".config").join("abraxas");
        fs::create_dir_all(&config_dir)?;
        Ok(Self::in_dir(&config_dir))
    }

    pub fn in_dir(config_dir: &Path) -> Self {
        Self {
            config_file: config_dir.join("config.toml"),
            legacy_ini: config_dir.join("config.ini"),
            cache_file: config_dir.join("weather_cache.json"),
            override_file: config_dir.join("override.json"),
            zipdb_file: config_dir.join("us_zipcodes.bin"),
            pid_file: config_dir.join("daemon.pid"),
        }
    }
}

/// Geographic location
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Location {
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug)]
pub enum ConfigError {
    Missing,
    Invalid(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Missing => f.write_str("no config file"),
            ConfigError::Invalid(s) => f.write_str(s),
        }
    }
}

#[derive(Deserialize)]
struct ConfigToml {
    location: LocationToml,
}

#[derive(Deserialize)]
struct LocationToml {
    latitude: f64,
    longitude: f64,
}

/// Cached weather data
pub struct WeatherData {
    pub cloud_cover: i32,
    pub forecast: String,
    pub temperature: f64,
    pub is_day: bool,
    pub fetched_at: i64,
    pub has_error: bool,
}

/// Manual override state
#[derive(Serialize, Deserialize)]
pub struct OverrideState {
    pub active: bool,
    pub target_temp: i32,
    pub duration_minutes: i32,
    pub issued_at: i64,
    pub start_temp: i32,
}

/// Load location from the TOML config.
pub fn load_location(paths: &Paths) -> Result<Location, ConfigError> {
    let content = match fs::read_to_string(&paths.config_file) {
        Ok(c) => c,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(ConfigError::Missing),
        Err(e) => {
            return Err(ConfigError::Invalid(format!(
                "{}: {}",
                paths.config_file.display(),
                e
            )))
        }
    };
    parse_location_toml(&content)
        .map_err(|e| ConfigError::Invalid(format!("{}: {}", paths.config_file.display(), e)))
}

fn parse_location_toml(content: &str) -> Result<Location, String> {
    let cfg: ConfigToml = toml::from_str(content).map_err(|e| e.to_string())?;
    let loc = Location {
        lat: cfg.location.latitude,
        lon: cfg.location.longitude,
    };
    validate_location(loc)?;
    Ok(loc)
}

fn validate_location(loc: Location) -> Result<(), String> {
    if !loc.lat.is_finite() || !(-90.0..=90.0).contains(&loc.lat) {
        return Err(format!("latitude {} outside -90..90", loc.lat));
    }
    if !loc.lon.is_finite() || !(-180.0..=180.0).contains(&loc.lon) {
        return Err(format!("longitude {} outside -180..180", loc.lon));
    }
    Ok(())
}

/// Load the location, writing the config first when none exists: From a
/// legacy config.ini if one is present, else from the system timezone's
/// reference coordinates. A config that exists but does not parse is an
/// error, never overwritten.
pub fn load_or_create_location(paths: &Paths) -> Result<Location, ConfigError> {
    match load_location(paths) {
        Err(ConfigError::Missing) => {}
        other => return other,
    }

    if let Some(loc) = load_legacy_ini(&paths.legacy_ini) {
        write_config(paths, loc, "Migrated from config.ini.")
            .map_err(|e| ConfigError::Invalid(format!("{}: {}", paths.config_file.display(), e)))?;
        let _ = fs::remove_file(&paths.legacy_ini);
        eprintln!(
            "[config] migrated {} -> {}",
            paths.legacy_ini.display(),
            paths.config_file.display()
        );
        return Ok(loc);
    }

    let zone = system_timezone().ok_or_else(|| {
        ConfigError::Invalid("no config, and the system timezone could not be resolved".into())
    })?;
    let loc = ZONE_TABLES
        .iter()
        .find_map(|t| fs::read_to_string(t).ok().and_then(|s| zone_coordinates(&s, &zone)))
        .ok_or_else(|| {
            ConfigError::Invalid(format!("no config, and timezone {} has no coordinates", zone))
        })?;
    let note = format!(
        "Location seeded from the system timezone ({}), the zone's reference city.",
        zone
    );
    write_config(paths, loc, &note)
        .map_err(|e| ConfigError::Invalid(format!("{}: {}", paths.config_file.display(), e)))?;
    eprintln!(
        "[config] wrote {} from timezone {}: {:.4}, {:.4}",
        paths.config_file.display(),
        zone,
        loc.lat,
        loc.lon
    );
    Ok(loc)
}

fn write_config(paths: &Paths, loc: Location, note: &str) -> Result<(), io::Error> {
    let content = format!(
        "# ABRAXAS configuration.\n\
         # {}\n\
         # Edit the coordinates, or run `abraxas --set-location ZIP` or\n\
         # `abraxas --set-location LAT,LON`.\n\
         \n\
         [location]\n\
         latitude = {:.6}\n\
         longitude = {:.6}\n",
        note, loc.lat, loc.lon
    );
    fs::write(&paths.config_file, content)
}

/// The IANA zone name: $TZ when it names one, else the /etc/localtime link.
fn system_timezone() -> Option<String> {
    if let Ok(tz) = std::env::var("TZ") {
        let tz = tz.trim_start_matches(':');
        if !tz.is_empty() && !tz.starts_with('/') {
            return Some(tz.to_string());
        }
    }
    let target = fs::read_link("/etc/localtime").ok()?;
    zone_from_path(&target.to_string_lossy())
}

pub fn zone_from_path(path: &str) -> Option<String> {
    let (_, zone) = path.split_once("zoneinfo/")?;
    (!zone.is_empty()).then(|| zone.to_string())
}

/// Coordinates for `zone` from a zone1970.tab / zone.tab body.
pub fn zone_coordinates(table: &str, zone: &str) -> Option<Location> {
    table
        .lines()
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| {
            let mut cols = l.split('\t');
            let coords = cols.nth(1)?;
            (cols.next()? == zone).then(|| parse_iso6709(coords)).flatten()
        })
}

/// ISO 6709 as the tz tables write it: ±DDMM±DDDMM or ±DDMMSS±DDDMMSS.
pub fn parse_iso6709(s: &str) -> Option<Location> {
    let split = s.get(1..)?.find(['+', '-'])? + 1;
    let lat = parse_angle(&s[..split], 2)?;
    let lon = parse_angle(&s[split..], 3)?;
    let loc = Location { lat, lon };
    validate_location(loc).ok()?;
    Some(loc)
}

fn parse_angle(s: &str, deg_digits: usize) -> Option<f64> {
    let sign = match s.as_bytes().first()? {
        b'+' => 1.0,
        b'-' => -1.0,
        _ => return None,
    };
    let digits = &s[1..];
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let field = |from: usize| -> Option<f64> { digits.get(from..from + 2)?.parse().ok() };
    let deg: f64 = digits.get(..deg_digits)?.parse().ok()?;
    let (min, sec) = match digits.len() - deg_digits {
        2 => (field(deg_digits)?, 0.0),
        4 => (field(deg_digits)?, field(deg_digits + 2)?),
        _ => return None,
    };
    Some(sign * (deg + min / 60.0 + sec / 3600.0))
}

/// The pre-TOML config.ini [location] section, read once to migrate it.
fn load_legacy_ini(path: &Path) -> Option<Location> {
    let content = fs::read_to_string(path).ok()?;

    let mut lat: Option<f64> = None;
    let mut lon: Option<f64> = None;
    let mut in_location = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }

        if trimmed.starts_with('[') {
            in_location = trimmed == "[location]";
            continue;
        }

        if !in_location {
            continue;
        }

        if let Some((key, value)) = trimmed.split_once('=') {
            let key = key.trim();
            let value = value.trim();
            match key {
                "latitude" => lat = value.parse().ok(),
                "longitude" => lon = value.parse().ok(),
                _ => {}
            }
        }
    }

    let loc = Location { lat: lat?, lon: lon? };
    validate_location(loc).ok()?;
    Some(loc)
}

/// Save location to the TOML config (`--set-location`).
pub fn save_location(paths: &Paths, lat: f64, lon: f64) -> Result<(), io::Error> {
    let loc = Location { lat, lon };
    validate_location(loc).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    write_config(paths, loc, "Set by abraxas --set-location.")?;
    let _ = fs::remove_file(&paths.legacy_ini);
    Ok(())
}

/// Load override state from JSON
pub fn load_override(paths: &Paths) -> Option<OverrideState> {
    let content = fs::read_to_string(&paths.override_file).ok()?;
    if content.len() > 4096 {
        return None;
    }
    serde_json::from_str(&content).ok()
}

/// Save override state to JSON
pub fn save_override(paths: &Paths, ovr: &OverrideState) -> Result<(), io::Error> {
    let json = serde_json::to_string_pretty(ovr)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    fs::write(&paths.override_file, json)
}

/// Clear override file
pub fn clear_override(paths: &Paths) {
    let _ = fs::remove_file(&paths.override_file);
}

/// JSON structure for weather cache (serde)
#[derive(Serialize, Deserialize)]
struct WeatherCacheJson {
    cloud_cover: i32,
    #[serde(default)]
    forecast: String,
    #[serde(default)]
    temperature: f64,
    #[serde(default)]
    is_day: bool,
    #[serde(default)]
    fetched_at: i64,
    #[serde(default)]
    error: Option<String>,
}

/// Load weather cache from JSON
pub fn load_weather_cache(paths: &Paths) -> Option<WeatherData> {
    let content = fs::read_to_string(&paths.cache_file).ok()?;
    if content.len() > 8192 {
        return None;
    }

    let cached: WeatherCacheJson = serde_json::from_str(&content).ok()?;

    let has_error = cached.error.is_some() || cached.fetched_at == 0;

    Some(WeatherData {
        cloud_cover: cached.cloud_cover,
        forecast: cached.forecast,
        temperature: cached.temperature,
        is_day: cached.is_day,
        fetched_at: cached.fetched_at,
        has_error,
    })
}

/// Save weather cache to JSON
pub fn save_weather_cache(paths: &Paths, wd: &WeatherData) -> Result<(), io::Error> {
    let cached = if wd.has_error {
        WeatherCacheJson {
            cloud_cover: 0,
            forecast: String::new(),
            temperature: 0.0,
            is_day: true,
            fetched_at: wd.fetched_at,
            error: Some("fetch failed".to_string()),
        }
    } else {
        WeatherCacheJson {
            cloud_cover: wd.cloud_cover,
            forecast: wd.forecast.clone(),
            temperature: wd.temperature,
            is_day: wd.is_day,
            fetched_at: wd.fetched_at,
            error: None,
        }
    };

    let json = serde_json::to_string_pretty(&cached)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    fs::write(&paths.cache_file, json)
}

/// Check if weather cache needs refresh
pub fn weather_needs_refresh(wd: &WeatherData) -> bool {
    if wd.has_error || wd.fetched_at == 0 {
        return true;
    }
    let now = now_epoch();
    (now - wd.fetched_at) > WEATHER_REFRESH_SEC
}

/// Check if daemon process is alive via PID file
pub fn check_daemon_alive(paths: &Paths) -> bool {
    let content = match fs::read_to_string(&paths.pid_file) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let pid: i32 = match content.trim().parse() {
        Ok(p) => p,
        Err(_) => return false,
    };
    if pid <= 0 {
        return false;
    }
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Write daemon PID to PID file
pub fn write_pid(paths: &Paths) -> Result<(), io::Error> {
    let pid = unsafe { libc::getpid() };
    fs::write(&paths.pid_file, format!("{}\n", pid))
}

/// Remove daemon PID file
pub fn remove_pid(paths: &Paths) {
    let _ = fs::remove_file(&paths.pid_file);
}
