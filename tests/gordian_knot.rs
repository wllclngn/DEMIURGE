// Tests for the pure-logic surfaces of gordian_knot.
//
// What's testable without a PAM stack, X server, or real VT:
//   - config [gordian_knot] section parses with defaults
//   - config overrides are applied
//   - sysinfo::format_uptime produces the expected "0D 3H 46M 12S" form
//   - screenshot: torrentius hex parsers already covered in tests/torrentius.rs
//
// What's NOT testable in CI / unit form:
//   - pam::authenticate  (needs a live PAM stack, real user, real shadow)
//   - x11_lock::lock     (needs an X server + grabs)
//   - vt::acquire/release(needs root + /dev/console)
//   - privsep::fork_child (changes process state; would fork the test runner)
//   - seccomp/landlock   (would kill-restrict the test runner)
//   - daemon::run        (blocks forever on screensaver query)
//   - inhibit::Watcher   (needs a D-Bus session + real logind)
// Those paths are tagged "manual verify" in COMMIT_MESSAGE.txt.

use std::fs;

use demiurge::config;
use demiurge::gordian_knot::sysinfo;

fn parse_config(content: &str) -> config::Config {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, content).unwrap();
    let paths = config::Paths::with_config(path);
    config::load(&paths).expect("config parses")
}

#[test]
fn gordian_knot_section_defaults_when_missing() {
    let cfg = parse_config(
        r##"
[general]
tags = ["1"]
"##,
    );
    assert_eq!(cfg.gordian_knot.bg, "#121212");
    assert_eq!(cfg.gordian_knot.fg, "#c8c8c8");
    assert_eq!(cfg.gordian_knot.accent, "#e5a93d");
    assert_eq!(cfg.gordian_knot.font, "monospace 12");
    assert_eq!(cfg.gordian_knot.idle_timeout_seconds, 600);
    assert_eq!(cfg.gordian_knot.prompt, "PASSWORD");
}

#[test]
fn gordian_knot_overrides_take_effect() {
    let cfg = parse_config(
        r##"
[general]
tags = ["1"]

[gordian_knot]
bg = "#000000"
fg = "#ffffff"
accent = "#ff00ff"
font = "Iosevka 14"
idle_timeout_seconds = 300
prompt = "KEY"
"##,
    );
    assert_eq!(cfg.gordian_knot.bg, "#000000");
    assert_eq!(cfg.gordian_knot.fg, "#ffffff");
    assert_eq!(cfg.gordian_knot.accent, "#ff00ff");
    assert_eq!(cfg.gordian_knot.font, "Iosevka 14");
    assert_eq!(cfg.gordian_knot.idle_timeout_seconds, 300);
    assert_eq!(cfg.gordian_knot.prompt, "KEY");
}

#[test]
fn gordian_knot_partial_override_uses_defaults_for_rest() {
    let cfg = parse_config(
        r##"
[general]
tags = ["1"]

[gordian_knot]
idle_timeout_seconds = 120
"##,
    );
    assert_eq!(cfg.gordian_knot.idle_timeout_seconds, 120);
    // Untouched keys fall back to defaults via #[serde(default = ...)].
    assert_eq!(cfg.gordian_knot.bg, "#121212");
    assert_eq!(cfg.gordian_knot.font, "monospace 12");
}

#[test]
fn uptime_zero_formats_as_all_zeros() {
    assert_eq!(sysinfo::format_uptime(0), "0D 0H 0M 0S");
}

#[test]
fn uptime_under_a_minute_shows_seconds_only() {
    assert_eq!(sysinfo::format_uptime(47), "0D 0H 0M 47S");
}

#[test]
fn uptime_minutes_and_seconds() {
    assert_eq!(sysinfo::format_uptime(125), "0D 0H 2M 5S");
}

#[test]
fn uptime_hours_minutes_seconds() {
    assert_eq!(sysinfo::format_uptime(3_600 * 3 + 60 * 46 + 12), "0D 3H 46M 12S");
}

#[test]
fn uptime_days_handled() {
    // 2 days, 0 hours, 0 minutes, 0 seconds.
    assert_eq!(sysinfo::format_uptime(86_400 * 2), "2D 0H 0M 0S");
}

#[test]
fn uptime_mixed_large() {
    // 5 days, 12 hours, 34 minutes, 56 seconds.
    let s = 86_400 * 5 + 3_600 * 12 + 60 * 34 + 56;
    assert_eq!(sysinfo::format_uptime(s), "5D 12H 34M 56S");
}

#[test]
fn uptime_boundary_at_seconds_wrap() {
    // 59s -> 0 min; 60s -> 1 min; 61s -> 1 min 1 sec.
    assert_eq!(sysinfo::format_uptime(59), "0D 0H 0M 59S");
    assert_eq!(sysinfo::format_uptime(60), "0D 0H 1M 0S");
    assert_eq!(sysinfo::format_uptime(61), "0D 0H 1M 1S");
}

// Note: hostname() and kernel_release() call into libc/uname and can't
// have stable assertions. They're exercised at build time (the
// format_uptime tests catch the same code path via the common module).
#[test]
fn hostname_returns_non_empty() {
    let h = sysinfo::hostname();
    assert!(!h.is_empty(), "hostname should not be empty on any sane system");
}

#[test]
fn kernel_release_returns_non_empty() {
    let k = sysinfo::kernel_release();
    assert!(!k.is_empty(), "uname release should not be empty");
}
