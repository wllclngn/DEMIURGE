// Tests for the pure-logic surfaces of gordian_knot.
//
// What's testable without a PAM stack, X server, or real VT:
//   - config [gordian_knot] section parses with defaults
//   - config overrides are applied
//   - sysinfo::format_uptime produces the expected "0D 3H 46M 12S" form
//   - screenshot: torrentius hex parsers already covered in tests/torrentius.rs
//
//   - the Secret buffer, the key-action table, the PAM child's wire
//     format and the UI child's state datagram
//
// Everything that grabs input, takes a VT, runs PAM, runs setuid or
// sandboxes itself is tested only inside the QEMU guest
// (tests/qemu/guest.py test), never on the host.

use std::fs;

use demiurge_x11::config;
use demiurge_x11::gordian_knot::keyinput::{action, KeyAction};
use demiurge_x11::gordian_knot::pam::{self, Attempt, AuthError};
use demiurge_x11::gordian_knot::secret::Secret;
use demiurge_x11::gordian_knot::{sysinfo, ui};

#[test]
fn secret_pushes_pops_characters_and_clears() {
    let mut s = Secret::new().unwrap();
    assert!(s.is_empty());
    assert!(s.push(b"pa"));
    assert!(s.push("ß€".as_bytes()));
    assert_eq!(s.char_count(), 4);
    s.pop_char();
    assert_eq!(s.bytes(), "paß".as_bytes());
    s.pop_char();
    assert_eq!(s.bytes(), b"pa");
    // NUL-terminated in place for PAM.
    let c = unsafe { std::ffi::CStr::from_ptr(s.as_c_ptr()) };
    assert_eq!(c.to_bytes(), b"pa");
    s.clear();
    assert!(s.is_empty());
    assert_eq!(unsafe { *s.as_c_ptr() }, 0);
}

#[test]
fn secret_refuses_what_does_not_fit() {
    let mut s = Secret::new().unwrap();
    assert!(s.push(&[b'x'; 4000]));
    assert!(!s.push(&[b'y'; 200]), "overflow must be refused, not truncated");
    assert_eq!(s.bytes().len(), 4000);
}

#[test]
fn key_actions() {
    use xkbcommon::xkb::keysyms as ks;
    assert_eq!(action(ks::KEY_Return, "\r", false), KeyAction::Submit);
    assert_eq!(action(ks::KEY_KP_Enter, "\r", false), KeyAction::Submit);
    assert_eq!(action(ks::KEY_BackSpace, "\x08", false), KeyAction::Backspace);
    assert_eq!(action(ks::KEY_Escape, "\x1b", false), KeyAction::Clear);
    assert_eq!(action(ks::KEY_u, "\x15", true), KeyAction::Clear, "Ctrl+U clears");
    assert_eq!(action(ks::KEY_a, "\x01", true), KeyAction::Ignore, "no other Ctrl chord types");
    assert_eq!(action(ks::KEY_KP_4, "4", false), KeyAction::Append("4".into()));
    assert_eq!(action(ks::KEY_A, "A", false), KeyAction::Append("A".into()));
    assert_eq!(action(ks::KEY_eacute, "é", false), KeyAction::Append("é".into()));
    assert_eq!(action(ks::KEY_Shift_L, "", false), KeyAction::Ignore);
}

#[test]
fn pam_wire_format_round_trips() {
    for result in [
        Ok(()),
        Err(AuthError::WrongCredentials),
        Err(AuthError::MaxTries),
        Err(AuthError::Other("helper\tsaid\nso".into())),
    ] {
        let a = Attempt {
            result: result.clone(),
            messages: vec!["The account is locked due to 3 failed logins.".into()],
        };
        let back = pam::decode(&pam::encode(&a)).expect("decodes");
        assert_eq!(back.messages, a.messages);
        match (&back.result, &result) {
            (Err(AuthError::Other(x)), Err(AuthError::Other(_))) => assert_eq!(x, "helper said so"),
            (x, y) => assert_eq!(x, y),
        }
    }
    assert!(pam::decode("").is_none(), "a helper that died reports nothing");
    assert!(pam::decode("M\tonly a message\n").is_none());
}

#[test]
fn ui_state_datagram_round_trips() {
    let msg = ui::encode_state(12, true, "Authentication failed");
    assert_eq!(ui::parse_state(&msg), Some((12, true, "Authentication failed".into())));
    assert_eq!(ui::parse_state(b"P"), None);
}

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
    // accent now defaults to empty = "inherit [appearance].accent".
    // The gordian_knot binary fills it from cfg.appearance.accent right
    // after load; the shared accent default is the amber.
    assert_eq!(cfg.gordian_knot.accent, "");
    assert_eq!(cfg.appearance.accent, "#e5a93d");
    assert_eq!(cfg.gordian_knot.font, "monospace 12");
    assert_eq!(cfg.gordian_knot.idle_timeout_seconds, 600);
    assert_eq!(cfg.gordian_knot.prompt, "PASSWORD");
    // --login's post-auth exec. greetd's own config.toml only names the
    // greeter, so if this default is wrong the machine authenticates and
    // then starts nothing.
    assert_eq!(
        cfg.gordian_knot.session_cmd,
        vec!["startx".to_string(), "/usr/local/bin/demiurge".to_string()]
    );
    assert_eq!(
        cfg.gordian_knot.session_env,
        vec![
            "XDG_SESSION_TYPE=x11".to_string(),
            "OZONE_PLATFORM=x11".to_string()
        ]
    );
}

#[test]
fn login_session_command_is_configurable() {
    // Pointing the greeter at the Wayland session instead of the X11
    // daily-driver is a config edit, not a rebuild.
    let cfg = parse_config(
        r##"
[general]
tags = ["1"]

[gordian_knot]
session_cmd = ["/usr/local/bin/demiurge-wl-tty"]
session_env = ["XDG_SESSION_TYPE=wayland"]
"##,
    );
    assert_eq!(
        cfg.gordian_knot.session_cmd,
        vec!["/usr/local/bin/demiurge-wl-tty".to_string()]
    );
    assert_eq!(
        cfg.gordian_knot.session_env,
        vec!["XDG_SESSION_TYPE=wayland".to_string()]
    );
}

#[test]
fn appearance_accent_is_the_shared_default() {
    // [appearance].accent is the desktop's shared highlight. When no
    // config sets it, it falls back to the DEMIURGE amber; the bar
    // focused tag, lock / login accent, and MEDUSA selection all
    // inherit it.
    let cfg = parse_config(
        r##"
[general]
tags = ["1"]
"##,
    );
    assert_eq!(cfg.appearance.accent, "#e5a93d");
    assert_eq!(cfg.appearance.gtk_theme, "");
    assert_eq!(cfg.appearance.icon_theme, "");
    // Bar focused-tag bg also defaults empty -> inherits the accent.
    assert_eq!(cfg.bar.tag_focused_bg, "");
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
