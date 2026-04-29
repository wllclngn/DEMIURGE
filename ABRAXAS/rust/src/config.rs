//! Configuration, override state, and path resolution.
//!
//! INI parser for [location] section. JSON override and weather cache via serde.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::PathBuf;

use crate::{WEATHER_REFRESH_SEC, now_epoch};

/// Resolved filesystem paths
#[derive(Clone)]
pub struct Paths {
    pub config_file: PathBuf,
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

        Ok(Self {
            config_file: config_dir.join("config.ini"),
            cache_file: config_dir.join("weather_cache.json"),
            override_file: config_dir.join("override.json"),
            zipdb_file: config_dir.join("us_zipcodes.bin"),
            pid_file: config_dir.join("daemon.pid"),
        })
    }
}

/// Geographic location
pub struct Location {
    pub lat: f64,
    pub lon: f64,
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

/// Load location from INI config
pub fn load_location(paths: &Paths) -> Option<Location> {
    let content = fs::read_to_string(&paths.config_file).ok()?;

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

    match (lat, lon) {
        (Some(lat), Some(lon)) => Some(Location { lat, lon }),
        _ => None,
    }
}

/// Save location to INI config
pub fn save_location(paths: &Paths, lat: f64, lon: f64) -> Result<(), io::Error> {
    let content = format!("[location]\nlatitude = {:.6}\nlongitude = {:.6}\n", lat, lon);
    fs::write(&paths.config_file, content)
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
