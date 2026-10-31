// The bar's notifications for the volume, brightness and media binds: Each
// runs its command, then reads the value back as the text the bar shows in
// place of the focused window's title for NOTIFICATION_TTL_MS. Both stacks
// share them; each blocks for as long as its commands run.

pub const NOTIFICATION_TTL_MS: u64 = 1500;

/// `wpctl set-volume` on the default sink by `delta` ("5%+", "5%-").
pub fn volume_delta(delta: &str) -> String {
    let _ = std::process::Command::new("wpctl")
        .args(["set-volume", "@DEFAULT_AUDIO_SINK@", delta])
        .status();
    let value = read_wpctl_volume("@DEFAULT_AUDIO_SINK@").unwrap_or_else(|| "unknown".into());
    format!("NOTIFICATION: System Volume, {}.", value)
}

/// Mute toggled on the default sink, or on the default source for the
/// microphone.
pub fn toggle_mute(is_mic: bool) -> String {
    let target = if is_mic { "@DEFAULT_AUDIO_SOURCE@" } else { "@DEFAULT_AUDIO_SINK@" };
    let _ = std::process::Command::new("wpctl").args(["set-mute", target, "toggle"]).status();
    let value = read_wpctl_volume(target).unwrap_or_else(|| "unknown".into());
    let label = if is_mic { "Microphone" } else { "System Volume" };
    format!("NOTIFICATION: {}, {}.", label, value)
}

/// `brightnessctl set` by `delta` ("5%+", "5%-").
pub fn brightness_delta(delta: &str) -> String {
    let _ = std::process::Command::new("brightnessctl").args(["set", delta]).status();
    let value = read_brightnessctl().unwrap_or_else(|| "unknown".into());
    format!("NOTIFICATION: Brightness, {}.", value)
}

/// `playerctl` with `cmd` ("play-pause", "next", "previous").
pub fn media_command(cmd: &str) -> String {
    let _ = std::process::Command::new("playerctl").arg(cmd).status();
    match read_media_state() {
        Some((state, Some(title))) => format!("NOTIFICATION: {}, {}.", state, title),
        Some((state, None)) => format!("NOTIFICATION: {}.", state),
        None => "NOTIFICATION: No media player.".into(),
    }
}

// Parse `wpctl get-volume <target>` output:
//   "Volume: 0.50"            -> "50%"
//   "Volume: 0.50 [MUTED]"    -> "MUTED"
// When muted, return "MUTED" regardless of the numeric value.
fn read_wpctl_volume(target: &str) -> Option<String> {
    let out = std::process::Command::new("wpctl")
        .args(["get-volume", target])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    if s.contains("[MUTED]") {
        return Some("MUTED".into());
    }
    let value: f64 = s.split_whitespace().nth(1)?.parse().ok()?;
    Some(format!("{}%", (value * 100.0).round() as u32))
}

// `brightnessctl -m`: "device,class,current,percent,max", as
// "intel_backlight,backlight,600,60%,1000".
fn read_brightnessctl() -> Option<String> {
    let out = std::process::Command::new("brightnessctl").arg("-m").output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().split(',').nth(3).map(str::to_string)
}

// Returns (state, optional track). state is "Playing", "Paused", "Stopped".
// track is "Title — Artist" when metadata is available. None means no
// player is running or playerctl isn't installed.
fn read_media_state() -> Option<(String, Option<String>)> {
    let status = std::process::Command::new("playerctl")
        .arg("status")
        .output()
        .ok()?;
    if !status.status.success() {
        return None;
    }
    let state = String::from_utf8_lossy(&status.stdout).trim().to_string();
    if state.is_empty() || state == "No players found" {
        return None;
    }

    let meta = std::process::Command::new("playerctl")
        .args(["metadata", "--format", "{{title}} — {{artist}}"])
        .output()
        .ok()?;
    let track = String::from_utf8_lossy(&meta.stdout).trim().to_string();
    let track = if track.is_empty() || track == " — " {
        None
    } else {
        Some(track)
    };

    Some((state, track))
}
