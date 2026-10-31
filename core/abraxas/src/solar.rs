//! NOAA sun position and sunrise/sunset calculations.
//!
//! Port of the C23 NOAA solar equations.
//! Julian day -> Julian century -> geometric mean longitude/anomaly ->
//! equation of center -> apparent longitude -> declination -> hour angle.

use std::f64::consts::PI;

fn deg2rad(d: f64) -> f64 {
    d * PI / 180.0
}

fn rad2deg(r: f64) -> f64 {
    r * 180.0 / PI
}

/// Sun position result
pub struct SunPosition {
    pub elevation: f64,
}

/// Sunrise/sunset times
pub struct SunTimes {
    pub sunrise: i64,
    pub sunset: i64,
}

/// Timezone offset in hours from UTC
fn get_tz_offset_hours() -> f64 {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let t = unsafe { libc::time(std::ptr::null_mut()) };
    unsafe { libc::localtime_r(&t, &mut tm) };
    tm.tm_gmtoff as f64 / 3600.0
}

/// Julian Day from broken-down time
fn julian_day(year: i32, month: i32, day: i32, hour_frac: f64) -> f64 {
    let (y, m) = if month <= 2 {
        (year - 1, month + 12)
    } else {
        (year, month)
    };

    let a = y / 100;
    let b = 2 - a + a / 4;

    let jd = (365.25 * (y + 4716) as f64) as i32 as f64
        + (30.6001 * (m + 1) as f64) as i32 as f64
        + day as f64
        + b as f64
        - 1524.5;
    jd + hour_frac / 24.0
}

/// Shared NOAA solar parameters from Julian century
#[allow(dead_code)]
struct SolarParams {
    l0: f64,          // geometric mean longitude (deg)
    m: f64,           // geometric mean anomaly (deg)
    e: f64,           // eccentricity of orbit
    sun_declin: f64,  // solar declination (deg)
    eq_time: f64,     // equation of time (minutes)
    obliq_corr: f64,  // corrected obliquity (deg)
}

fn compute_solar_params(jc: f64) -> SolarParams {
    let l0 = (280.46646 + jc * (36000.76983 + 0.0003032 * jc)) % 360.0;
    let m = 357.52911 + jc * (35999.05029 - 0.0001537 * jc);
    let m_rad = deg2rad(m);
    let e = 0.016708634 - jc * (0.000042037 + 0.0000001267 * jc);

    // Sun's equation of center
    let c = m_rad.sin() * (1.914602 - jc * (0.004817 + 0.000014 * jc))
        + (2.0 * m_rad).sin() * (0.019993 - 0.000101 * jc)
        + (3.0 * m_rad).sin() * 0.000289;

    // Sun's true and apparent longitude
    let sun_lon = l0 + c;
    let omega = 125.04 - 1934.136 * jc;
    let sun_apparent_lon = sun_lon - 0.00569 - 0.00478 * deg2rad(omega).sin();

    // Mean obliquity and correction
    let obliq_mean = 23.0
        + (26.0 + (21.448 - jc * (46.815 + jc * (0.00059 - jc * 0.001813))) / 60.0) / 60.0;
    let obliq_corr = obliq_mean + 0.00256 * deg2rad(omega).cos();
    let obliq_corr_rad = deg2rad(obliq_corr);

    // Solar declination
    let sun_declin =
        rad2deg((obliq_corr_rad.sin() * deg2rad(sun_apparent_lon).sin()).asin());

    // Equation of time
    let var_y = (obliq_corr_rad / 2.0).tan().powi(2);
    let eq_time = 4.0
        * rad2deg(
            var_y * (2.0 * deg2rad(l0)).sin()
                - 2.0 * e * m_rad.sin()
                + 4.0 * e * var_y * m_rad.sin() * (2.0 * deg2rad(l0)).cos()
                - 0.5 * var_y * var_y * (4.0 * deg2rad(l0)).sin()
                - 1.25 * e * e * (2.0 * m_rad).sin(),
        );

    SolarParams {
        l0,
        m,
        e,
        sun_declin,
        eq_time,
        obliq_corr,
    }
}

/// Calculate sun position (elevation angle) at a given time and location
pub fn position(when: i64, lat: f64, lon: f64) -> SunPosition {
    let mut lt: libc::tm = unsafe { std::mem::zeroed() };
    let t = when;
    unsafe { libc::localtime_r(&t, &mut lt) };

    let hour_frac = lt.tm_hour as f64 + lt.tm_min as f64 / 60.0 + lt.tm_sec as f64 / 3600.0;
    let jd = julian_day(lt.tm_year + 1900, lt.tm_mon + 1, lt.tm_mday, hour_frac);
    let jc = (jd - 2451545.0) / 36525.0;

    let sp = compute_solar_params(jc);

    // True solar time
    let tz_offset = get_tz_offset_hours();
    let time_offset = sp.eq_time + 4.0 * lon - 60.0 * tz_offset;
    let tst = lt.tm_hour as f64 * 60.0 + lt.tm_min as f64 + lt.tm_sec as f64 / 60.0 + time_offset;

    // Hour angle
    let mut hour_angle = tst / 4.0 - 180.0;
    if hour_angle < -180.0 {
        hour_angle += 360.0;
    }

    // Zenith and elevation
    let lat_rad = deg2rad(lat);
    let declin_rad = deg2rad(sp.sun_declin);
    let ha_rad = deg2rad(hour_angle);

    let cos_zenith =
        (lat_rad.sin() * declin_rad.sin() + lat_rad.cos() * declin_rad.cos() * ha_rad.cos())
            .clamp(-1.0, 1.0);

    let zenith = rad2deg(cos_zenith.acos());

    SunPosition {
        elevation: 90.0 - zenith,
    }
}

/// Calculate sunrise and sunset times for a given day and location
pub fn sunrise_sunset(when: i64, lat: f64, lon: f64) -> Option<SunTimes> {
    let mut lt: libc::tm = unsafe { std::mem::zeroed() };
    let t = when;
    unsafe { libc::localtime_r(&t, &mut lt) };

    // Use noon of the given day
    let jd = julian_day(lt.tm_year + 1900, lt.tm_mon + 1, lt.tm_mday, 12.0);
    let jc = (jd - 2451545.0) / 36525.0;

    let sp = compute_solar_params(jc);

    // Hour angle for sunrise/sunset (zenith 90.833 degrees)
    let zenith = 90.833_f64;
    let lat_rad = deg2rad(lat);
    let declin_rad = deg2rad(sp.sun_declin);

    let cos_ha =
        deg2rad(zenith).cos() / (lat_rad.cos() * declin_rad.cos()) - lat_rad.tan() * declin_rad.tan();

    // Polar region check
    if cos_ha < -1.0 || cos_ha > 1.0 {
        return None;
    }

    let ha = rad2deg(cos_ha.acos());
    let tz_offset = get_tz_offset_hours();

    let sunrise_min = 720.0 - 4.0 * (lon + ha) - sp.eq_time + tz_offset * 60.0;
    let sunset_min = 720.0 - 4.0 * (lon - ha) - sp.eq_time + tz_offset * 60.0;

    // Base midnight of the given day
    let mut base: libc::tm = unsafe { std::mem::zeroed() };
    base.tm_year = lt.tm_year;
    base.tm_mon = lt.tm_mon;
    base.tm_mday = lt.tm_mday;
    base.tm_isdst = -1;
    let midnight = unsafe { libc::mktime(&mut base) } as i64;

    Some(SunTimes {
        sunrise: midnight + (sunrise_min * 60.0) as i64,
        sunset: midnight + (sunset_min * 60.0) as i64,
    })
}
