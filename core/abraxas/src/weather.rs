//! NOAA weather API client.
//!
//! Two-step API:
//!   1. GET https://api.weather.gov/points/{lat},{lon}
//!      -> extract properties.forecastHourly URL
//!   2. GET that URL
//!      -> extract first period's shortForecast, temperature, isDaytime
//!
//! Uses curl(1) child process for HTTP -- zero TLS dependencies.
//! When compiled without the "noaa" feature, all functions are no-ops.

use crate::config::WeatherData;
use crate::now_epoch;

#[cfg(feature = "noaa")]
pub fn init() {}

#[cfg(feature = "noaa")]
pub fn cleanup() {}

#[cfg(feature = "noaa")]
pub fn fetch(lat: f64, lon: f64) -> WeatherData {
    match fetch_inner(lat, lon) {
        Ok(wd) => wd,
        Err(_) => WeatherData {
            cloud_cover: 0,
            forecast: "Unknown".to_string(),
            temperature: 0.0,
            is_day: true,
            fetched_at: now_epoch(),
            has_error: true,
        },
    }
}

#[cfg(feature = "noaa")]
fn http_get(url: &str) -> Result<String, Box<dyn std::error::Error>> {
    let output = std::process::Command::new("curl")
        .args([
            "-s", "-f", "-L", "--max-time", "5",
            "-H", "User-Agent: abraxas/7.0 (weather color temp daemon)",
            "-H", "Accept: application/geo+json",
            url,
        ])
        .output()?;

    if !output.status.success() {
        return Err(format!("curl exit {}", output.status).into());
    }

    String::from_utf8(output.stdout).map_err(|e| e.into())
}

#[cfg(feature = "noaa")]
fn fetch_inner(lat: f64, lon: f64) -> Result<WeatherData, Box<dyn std::error::Error>> {
    // Step 1: Get grid point
    let url = format!("https://api.weather.gov/points/{:.4},{:.4}", lat, lon);
    let body = http_get(&url)?;
    let resp: serde_json::Value = serde_json::from_str(&body)?;

    let forecast_url = resp["properties"]["forecastHourly"]
        .as_str()
        .ok_or("No forecastHourly URL")?
        .to_string();

    // Step 2: Get hourly forecast
    let body = http_get(&forecast_url)?;
    let resp: serde_json::Value = serde_json::from_str(&body)?;

    let period = &resp["properties"]["periods"][0];
    if period.is_null() {
        return Err("No forecast periods".into());
    }

    let short_forecast = period["shortForecast"]
        .as_str()
        .unwrap_or("Unknown")
        .to_string();
    let temperature = period["temperature"].as_f64().unwrap_or(0.0);
    let is_day = period["isDaytime"].as_bool().unwrap_or(true);

    let cloud_cover = cloud_cover_from_forecast(&short_forecast);

    Ok(WeatherData {
        cloud_cover,
        forecast: short_forecast,
        temperature,
        is_day,
        fetched_at: now_epoch(),
        has_error: false,
    })
}

#[cfg(feature = "noaa")]
fn cloud_cover_from_forecast(forecast: &str) -> i32 {
    let lower = forecast.to_lowercase();

    // Precipitation always means heavy cloud
    if lower.contains("rain")
        || lower.contains("storm")
        || lower.contains("snow")
        || lower.contains("drizzle")
        || lower.contains("showers")
    {
        return 95;
    }

    if lower.contains("overcast") {
        return 90;
    }

    // Mostly cloudy (before general "cloudy" check)
    if lower.contains("mostly cloudy") {
        return 75;
    }

    if lower.contains("cloudy") {
        return 90;
    }

    if lower.contains("partly") {
        return 50;
    }

    // Mostly sunny/clear (before general "sunny"/"clear")
    if lower.contains("mostly sunny") || lower.contains("mostly clear") {
        return 25;
    }

    if lower.contains("sunny") || lower.contains("clear") {
        return 10;
    }

    0
}

// --- Async weather fetch (non-blocking, io_uring integrated) ---

#[cfg(feature = "noaa")]
#[derive(PartialEq, Eq)]
pub enum FetchPhase {
    Idle,
    ReadingPoints,
    ReadingForecast,
}

#[cfg(feature = "noaa")]
pub enum ReadResult {
    Pending,
    NewPipe,
    Done(Result<WeatherData, Box<dyn std::error::Error>>),
}

#[cfg(feature = "noaa")]
pub struct FetchState {
    pub phase: FetchPhase,
    child: Option<std::process::Child>,
    pub pipe_fd: i32,
    buf: Vec<u8>,
    lat: f64,
    lon: f64,
}

#[cfg(feature = "noaa")]
impl FetchState {
    pub fn new() -> Self {
        Self {
            phase: FetchPhase::Idle,
            child: None,
            pipe_fd: -1,
            buf: Vec::new(),
            lat: 0.0,
            lon: 0.0,
        }
    }

    pub fn needs_poll(&self) -> bool {
        self.pipe_fd >= 0 && self.phase != FetchPhase::Idle
    }

    fn spawn_curl(url: &str) -> Result<(std::process::Child, i32), Box<dyn std::error::Error>> {
        use std::os::unix::io::AsRawFd;
        use std::process::Stdio;

        let child = std::process::Command::new("curl")
            .args([
                "-s", "-f", "-L", "--max-time", "5",
                "-H", "User-Agent: abraxas/7.0 (weather color temp daemon)",
                "-H", "Accept: application/geo+json",
                url,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;

        let fd = child.stdout.as_ref()
            .ok_or("no stdout")?
            .as_raw_fd();

        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 {
            return Err("fcntl F_GETFL failed".into());
        }
        if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err("fcntl O_NONBLOCK failed".into());
        }

        Ok((child, fd))
    }

    pub fn start(&mut self, lat: f64, lon: f64) -> i32 {
        if self.phase != FetchPhase::Idle {
            return -1;
        }

        self.lat = lat;
        self.lon = lon;
        self.buf.clear();

        let url = format!("https://api.weather.gov/points/{:.4},{:.4}", lat, lon);

        match Self::spawn_curl(&url) {
            Ok((child, fd)) => {
                self.child = Some(child);
                self.pipe_fd = fd;
                self.phase = FetchPhase::ReadingPoints;
                fd
            }
            Err(e) => {
                eprintln!("  spawn_curl failed: {}", e);
                -1
            }
        }
    }

    /// Non-blocking drain. Returns Ok(true) for EOF, Ok(false) for EAGAIN.
    fn drain_pipe(&mut self) -> Result<bool, ()> {
        let mut chunk = [0u8; 4096];
        loop {
            let n = unsafe {
                libc::read(
                    self.pipe_fd,
                    chunk.as_mut_ptr() as *mut libc::c_void,
                    chunk.len(),
                )
            };
            if n > 0 {
                self.buf.extend_from_slice(&chunk[..n as usize]);
                continue;
            }
            if n == 0 {
                return Ok(true); // EOF
            }
            let err = unsafe { *libc::__errno_location() };
            if err == libc::EAGAIN || err == libc::EWOULDBLOCK {
                return Ok(false);
            }
            return Err(());
        }
    }

    pub fn read_response(&mut self) -> ReadResult {
        match self.drain_pipe() {
            Ok(false) => return ReadResult::Pending,
            Err(()) => {
                self.abort();
                return ReadResult::Done(Err("pipe read error".into()));
            }
            Ok(true) => {} // EOF -- process below
        }

        // EOF: reap child
        self.pipe_fd = -1;
        let status = match self.child.as_mut() {
            Some(c) => c.wait(),
            None => {
                self.abort();
                return ReadResult::Done(Err("no child".into()));
            }
        };
        self.child = None;

        let ok = match status {
            Ok(s) => s.success() && !self.buf.is_empty(),
            Err(_) => false,
        };

        if !ok {
            self.phase = FetchPhase::Idle;
            return ReadResult::Done(Err("curl failed".into()));
        }

        let body = match String::from_utf8(std::mem::take(&mut self.buf)) {
            Ok(s) => s,
            Err(_) => {
                self.phase = FetchPhase::Idle;
                return ReadResult::Done(Err("invalid utf8".into()));
            }
        };

        match self.phase {
            FetchPhase::ReadingPoints => {
                let resp: serde_json::Value = match serde_json::from_str(&body) {
                    Ok(v) => v,
                    Err(e) => {
                        self.phase = FetchPhase::Idle;
                        return ReadResult::Done(Err(e.into()));
                    }
                };

                let forecast_url = match resp["properties"]["forecastHourly"].as_str() {
                    Some(u) => u.to_string(),
                    None => {
                        self.phase = FetchPhase::Idle;
                        return ReadResult::Done(Err("no forecastHourly URL".into()));
                    }
                };

                match Self::spawn_curl(&forecast_url) {
                    Ok((child, fd)) => {
                        self.child = Some(child);
                        self.pipe_fd = fd;
                        self.phase = FetchPhase::ReadingForecast;
                        ReadResult::NewPipe
                    }
                    Err(e) => {
                        eprintln!("  spawn_curl (forecast) failed: {}", e);
                        self.phase = FetchPhase::Idle;
                        ReadResult::Done(Err(e))
                    }
                }
            }
            FetchPhase::ReadingForecast => {
                self.phase = FetchPhase::Idle;

                let resp: serde_json::Value = match serde_json::from_str(&body) {
                    Ok(v) => v,
                    Err(e) => return ReadResult::Done(Err(e.into())),
                };

                let period = &resp["properties"]["periods"][0];
                if period.is_null() {
                    return ReadResult::Done(Err("no forecast periods".into()));
                }

                let short_forecast = period["shortForecast"]
                    .as_str()
                    .unwrap_or("Unknown")
                    .to_string();
                let temperature = period["temperature"].as_f64().unwrap_or(0.0);
                let is_day = period["isDaytime"].as_bool().unwrap_or(true);
                let cloud_cover = cloud_cover_from_forecast(&short_forecast);

                ReadResult::Done(Ok(WeatherData {
                    cloud_cover,
                    forecast: short_forecast,
                    temperature,
                    is_day,
                    fetched_at: now_epoch(),
                    has_error: false,
                }))
            }
            FetchPhase::Idle => ReadResult::Done(Err("unexpected idle".into())),
        }
    }

    pub fn abort(&mut self) {
        if let Some(ref mut child) = self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.child = None;
        self.pipe_fd = -1;
        self.buf.clear();
        self.phase = FetchPhase::Idle;
    }
}

// Non-NOAA stubs
#[cfg(not(feature = "noaa"))]
pub fn init() {}

#[cfg(not(feature = "noaa"))]
pub fn cleanup() {}

#[cfg(not(feature = "noaa"))]
pub fn fetch(_lat: f64, _lon: f64) -> WeatherData {
    WeatherData {
        cloud_cover: 0,
        forecast: "Disabled (non-USA build)".to_string(),
        temperature: 0.0,
        is_day: true,
        fetched_at: now_epoch(),
        has_error: true,
    }
}

#[cfg(not(feature = "noaa"))]
pub struct FetchState {
    pub pipe_fd: i32,
    pub phase: u8,
}

#[cfg(not(feature = "noaa"))]
impl FetchState {
    pub fn new() -> Self { Self { pipe_fd: -1, phase: 0 } }
    pub fn needs_poll(&self) -> bool { false }
    pub fn start(&mut self, _lat: f64, _lon: f64) -> i32 { -1 }
    pub fn abort(&mut self) {}
}
