//! Sigmoid transition math.
//!
//! Dusk is canonical: day -> night over DUSK_DURATION centered on sunset.
//! Dawn is its inverse: night -> day over DAWN_DURATION centered on sunrise.
//! Manual overrides use the same sigmoid over [0, duration].

use crate::{
    DAWN_DURATION, DAWN_OFFSET, DUSK_DURATION, DUSK_OFFSET, SIGMOID_STEEPNESS, TEMP_DAY_CLEAR, TEMP_DAY_DARK,
    TEMP_NIGHT,
};
use crate::solar;

const SECONDS_PER_DAY: i64 = 86400;

fn sigmoid_raw(x: f64, steepness: f64) -> f64 {
    1.0 / (1.0 + (-steepness * x).exp())
}

pub fn sigmoid_norm(x: f64, steepness: f64) -> f64 {
    let raw = sigmoid_raw(x, steepness);
    let low = sigmoid_raw(-1.0, steepness);
    let high = sigmoid_raw(1.0, steepness);
    (raw - low) / (high - low)
}

pub fn calculate_solar_temp(
    minutes_from_sunrise: f64,
    minutes_to_sunset: f64,
    cloud_factor: f64,
) -> i32 {
    let day_temp = (TEMP_DAY_CLEAR as f64
        - cloud_factor * (TEMP_DAY_CLEAR - TEMP_DAY_DARK) as f64) as i32;
    let night_temp = TEMP_NIGHT;

    let dawn_half = DAWN_DURATION / 2.0;
    let dusk_half = DUSK_DURATION / 2.0;

    // Dawn: night -> day (inverse of dusk, midpoint offset after sunrise)
    let dawn_shifted = minutes_from_sunrise - DAWN_OFFSET;
    if dawn_shifted.abs() < dawn_half {
        let x = dawn_shifted / dawn_half; // [-1, 1]
        let factor = sigmoid_norm(x, SIGMOID_STEEPNESS);
        return (night_temp as f64 + (day_temp - night_temp) as f64 * factor) as i32;
    }

    // Dusk: day -> night (canonical, midpoint offset before sunset)
    let dusk_shifted = minutes_to_sunset - DUSK_OFFSET;
    if dusk_shifted.abs() < dusk_half {
        let x = dusk_shifted / dusk_half; // [1, -1]
        let factor = sigmoid_norm(x, SIGMOID_STEEPNESS);
        return (night_temp as f64 + (day_temp - night_temp) as f64 * factor) as i32;
    }

    // Daytime (between windows)
    if dawn_shifted >= dawn_half && dusk_shifted >= dusk_half {
        return day_temp;
    }

    // Night
    night_temp
}

pub fn calculate_manual_temp(
    start_temp: i32,
    target_temp: i32,
    start_time: i64,
    duration_min: i32,
    now: i64,
) -> i32 {
    if duration_min <= 0 {
        return target_temp;
    }

    let elapsed_min = (now - start_time) as f64 / 60.0;

    if elapsed_min >= duration_min as f64 {
        return target_temp;
    }

    // Map [0, duration] -> [-1, 1]
    let x = 2.0 * (elapsed_min / duration_min as f64) - 1.0;
    let factor = sigmoid_norm(x, SIGMOID_STEEPNESS);
    (start_temp as f64 + (target_temp - start_temp) as f64 * factor) as i32
}

/// Earliest time the solar curve could plausibly cross the manual target.
/// For darkening overrides (`darkened=true`), the next dusk window start.
/// For brightening, the next dawn window start. Used to gate the catch-up
/// check so the daemon doesn't recompute solar_temperature every tick during
/// a multi-hour hold.
pub fn next_solar_window_start(now: i64, lat: f64, lon: f64, darkened: bool) -> i64 {
    let st = match solar::sunrise_sunset(now, lat, lon) {
        Some(st) => st,
        None => return now + SECONDS_PER_DAY, // polar fallback
    };

    let dawn_window_start = st.sunrise - ((DAWN_DURATION / 2.0 - DAWN_OFFSET) * 60.0) as i64;
    let dusk_window_start = st.sunset - ((DUSK_DURATION / 2.0 + DUSK_OFFSET) * 60.0) as i64;

    let want = if darkened { dusk_window_start } else { dawn_window_start };
    if want > now {
        return want;
    }

    // Today's window has passed -- use tomorrow's.
    let tomorrow = now + SECONDS_PER_DAY;
    match solar::sunrise_sunset(tomorrow, lat, lon) {
        Some(st2) => {
            if darkened {
                st2.sunset - ((DUSK_DURATION / 2.0 + DUSK_OFFSET) * 60.0) as i64
            } else {
                st2.sunrise - ((DAWN_DURATION / 2.0 - DAWN_OFFSET) * 60.0) as i64
            }
        }
        None => now + SECONDS_PER_DAY,
    }
}

