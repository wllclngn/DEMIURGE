use std::fs;
use std::path::PathBuf;

// We test config parsing directly since it's pure TOML/serde
// (no X11 connection needed)

fn parse_config(content: &str) -> Result<(), String> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, content).unwrap();

    let paths = demiurge_x11::config::Paths::with_config(path);
    demiurge_x11::config::load(&paths).map(|_| ())
}

#[test]
fn minimal_config() {
    let toml = r##"
[general]
tags = ["1"]
"##;
    assert!(parse_config(toml).is_ok());
}

#[test]
fn full_config() {
    let toml = r##"
[general]
tags = ["X", "Y", "Z"]
default_layout = "floating"

[bar]
height = 30
font = "monospace 9"
bg = "#121212"
fg = "#888888"

[startup]
commands = ["kitty"]

[[keybind]]
mods = ["Super"]
key = "Return"
action = "spawn"
args = "kitty"

[[keybind]]
mods = ["Alt"]
key = "F4"
action = "close_window"

[[keybind]]
mods = ["Super"]
key = "1"
action = "view_tag"
args = "1"

[[keybind]]
mods = ["Super", "Shift"]
key = "1"
action = "move_to_tag"
args = "1"
"##;
    assert!(parse_config(toml).is_ok());
}

#[test]
fn defaults_applied() {
    let toml = r##"
[general]
tags = ["A"]
"##;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, toml).unwrap();

    let paths = demiurge_x11::config::Paths::with_config(path);
    let cfg = demiurge_x11::config::load(&paths).unwrap();

    assert_eq!(cfg.general.default_layout, "floating");
    assert_eq!(cfg.bar.height, 30);
    assert!(cfg.startup.commands.is_empty());
    assert!(cfg.keybinds.is_empty());
}

#[test]
fn empty_tags_rejected() {
    let toml = r##"
[general]
tags = []
"##;
    let err = parse_config(toml).unwrap_err();
    assert!(err.contains("tags must not be empty"));
}

#[test]
fn unknown_modifier_rejected() {
    let toml = r##"
[general]
tags = ["1"]

[[keybind]]
mods = ["Hyper"]
key = "a"
action = "quit"
"##;
    let err = parse_config(toml).unwrap_err();
    assert!(err.contains("unknown modifier"));
}

#[test]
fn unknown_action_rejected() {
    let toml = r##"
[general]
tags = ["1"]

[[keybind]]
key = "a"
action = "explode"
"##;
    let err = parse_config(toml).unwrap_err();
    assert!(err.contains("unknown action"));
}

#[test]
fn all_actions_valid() {
    let actions = [
        ("spawn", Some("kitty")),
        ("close_window", None),
        ("quit", None),
        ("mru_next", None),
        ("mru_prev", None),
        ("mru_next_global", None),
        ("mru_prev_global", None),
        ("view_tag", Some("1")),
        ("view_prev_tag", None),
        ("view_next_tag", None),
        ("move_to_tag", Some("2")),
        ("toggle_above", None),
        ("toggle_fullscreen", None),
        ("toggle_layout", None),
        ("run_prompt", None),
        ("volume_up", None),
        ("volume_down", None),
        ("volume_mute", None),
        ("volume_mic_mute", None),
        ("brightness_up", None),
        ("brightness_down", None),
        ("media_play_pause", None),
        ("media_next", None),
        ("media_prev", None),
    ];

    for (action, args) in &actions {
        let args_line = match args {
            Some(a) => format!("args = \"{}\"", a),
            None => String::new(),
        };
        let toml = format!(
            r##"
[general]
tags = ["1"]

[[keybind]]
key = "a"
action = "{}"
{}
"##,
            action, args_line
        );
        assert!(parse_config(&toml).is_ok(), "action '{}' should be valid", action);
    }
}

#[test]
fn all_modifiers_valid() {
    for modifier in &["Super", "Alt", "Control", "Shift"] {
        let toml = format!(
            r##"
[general]
tags = ["1"]

[[keybind]]
mods = ["{}"]
key = "a"
action = "quit"
"##,
            modifier
        );
        assert!(parse_config(&toml).is_ok(), "modifier '{}' should be valid", modifier);
    }
}

#[test]
fn no_keybinds_ok() {
    let toml = r##"
[general]
tags = ["1", "2"]
"##;
    assert!(parse_config(toml).is_ok());
}

#[test]
fn missing_config_file_uses_defaults() {
    let paths = demiurge_x11::config::Paths::with_config(PathBuf::from("/nonexistent/config.toml"));
    let cfg = demiurge_x11::config::load(&paths).expect("missing config should fall back to defaults");
    assert_eq!(cfg.general.tags, vec!["X", "Y", "Z"]);
    assert_eq!(cfg.general.default_layout, "floating");
    assert!(cfg.keybinds.is_empty());
}

#[test]
fn master_ratio_default() {
    let toml = r##"
[general]
tags = ["1"]
"##;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, toml).unwrap();

    let paths = demiurge_x11::config::Paths::with_config(path);
    let cfg = demiurge_x11::config::load(&paths).unwrap();
    assert!((cfg.general.master_ratio - 0.5).abs() < f64::EPSILON);
}

#[test]
fn master_ratio_custom() {
    let toml = r##"
[general]
tags = ["1"]
master_ratio = 0.65
"##;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, toml).unwrap();

    let paths = demiurge_x11::config::Paths::with_config(path);
    let cfg = demiurge_x11::config::load(&paths).unwrap();
    assert!((cfg.general.master_ratio - 0.65).abs() < f64::EPSILON);
}

#[test]
fn master_ratio_zero_rejected() {
    let toml = r##"
[general]
tags = ["1"]
master_ratio = 0.0
"##;
    let err = parse_config(toml).unwrap_err();
    assert!(err.contains("master_ratio"));
}

#[test]
fn master_ratio_one_rejected() {
    let toml = r##"
[general]
tags = ["1"]
master_ratio = 1.0
"##;
    let err = parse_config(toml).unwrap_err();
    assert!(err.contains("master_ratio"));
}

#[test]
fn default_layout_valid_values() {
    for layout in &["floating", "tile", "monocle"] {
        let toml = format!(
            r##"
[general]
tags = ["1"]
default_layout = "{}"
"##,
            layout
        );
        assert!(parse_config(&toml).is_ok(), "layout '{}' should be valid", layout);
    }
}

#[test]
fn default_layout_invalid_rejected() {
    let toml = r##"
[general]
tags = ["1"]
default_layout = "spiral"
"##;
    let err = parse_config(toml).unwrap_err();
    assert!(err.contains("unknown layout"));
}

#[test]
fn tagged_spawn_parses() {
    let toml = r##"
[general]
tags = ["X", "Y", "Z"]

[[startup.spawn]]
cmd = "kitty"
tag = "X"

[[startup.spawn]]
cmd = "kitty --class montauk-term -e montauk"
tag = "Z"
class = "montauk-term"
"##;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, toml).unwrap();

    let paths = demiurge_x11::config::Paths::with_config(path);
    let cfg = demiurge_x11::config::load(&paths).unwrap();

    assert_eq!(cfg.startup.spawn.len(), 2);
    assert_eq!(cfg.startup.spawn[0].cmd, "kitty");
    assert_eq!(cfg.startup.spawn[0].tag, "X");
    assert_eq!(cfg.startup.spawn[0].class_match(), "kitty");
    assert_eq!(cfg.startup.spawn[1].class_match(), "montauk-term");
}

#[test]
fn tagged_spawn_rejects_unknown_tag() {
    let toml = r##"
[general]
tags = ["X", "Y"]

[[startup.spawn]]
cmd = "kitty"
tag = "Q"
"##;
    let err = parse_config(toml).unwrap_err();
    assert!(err.contains("not in general.tags"));
}

#[test]
fn tagged_spawn_rejects_empty_cmd() {
    let toml = r##"
[general]
tags = ["X"]

[[startup.spawn]]
cmd = ""
tag = "X"
"##;
    let err = parse_config(toml).unwrap_err();
    assert!(err.contains("cmd must not be empty"));
}

#[test]
fn input_keyboard_repeat_defaults_to_zero() {
    let toml = r##"[general]
tags = ["X"]
"##;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, toml).unwrap();
    let paths = demiurge_x11::config::Paths::with_config(path);
    let cfg = demiurge_x11::config::load(&paths).unwrap();
    assert_eq!(cfg.input.keyboard.repeat_delay, 0);
    assert_eq!(cfg.input.keyboard.repeat_rate, 0);
    assert_eq!(cfg.input.keyboard.layout, "");
    assert_eq!(cfg.input.keyboard.variant, "");
    assert!(cfg.input.keyboard.options.is_empty());
}

#[test]
fn input_keyboard_full_block_parses() {
    let toml = r##"[general]
tags = ["X"]

[input.keyboard]
repeat_delay = 185
repeat_rate = 30
layout = "us"
variant = "dvorak"
options = ["caps:escape", "ctrl:nocaps"]
"##;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, toml).unwrap();
    let paths = demiurge_x11::config::Paths::with_config(path);
    let cfg = demiurge_x11::config::load(&paths).unwrap();
    assert_eq!(cfg.input.keyboard.repeat_delay, 185);
    assert_eq!(cfg.input.keyboard.repeat_rate, 30);
    assert_eq!(cfg.input.keyboard.layout, "us");
    assert_eq!(cfg.input.keyboard.variant, "dvorak");
    assert_eq!(
        cfg.input.keyboard.options,
        vec!["caps:escape".to_string(), "ctrl:nocaps".to_string()]
    );
}

#[test]
fn input_idle_defaults_to_zero() {
    let toml = r##"[general]
tags = ["X"]
"##;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, toml).unwrap();
    let paths = demiurge_x11::config::Paths::with_config(path);
    let cfg = demiurge_x11::config::load(&paths).unwrap();
    assert_eq!(cfg.input.idle.lock_seconds, 0);
    assert_eq!(cfg.input.idle.screensaver_seconds, 0);
    assert_eq!(cfg.input.idle.dpms_standby_seconds, 0);
    assert_eq!(cfg.input.idle.dpms_suspend_seconds, 0);
    assert_eq!(cfg.input.idle.dpms_off_seconds, 0);
}

#[test]
fn input_idle_full_block_parses() {
    let toml = r##"[general]
tags = ["X"]

[input.idle]
lock_seconds = 600
screensaver_seconds = 300
dpms_standby_seconds = 300
dpms_suspend_seconds = 600
dpms_off_seconds = 900
"##;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, toml).unwrap();
    let paths = demiurge_x11::config::Paths::with_config(path);
    let cfg = demiurge_x11::config::load(&paths).unwrap();
    assert_eq!(cfg.input.idle.lock_seconds, 600);
    assert_eq!(cfg.input.idle.screensaver_seconds, 300);
    assert_eq!(cfg.input.idle.dpms_standby_seconds, 300);
    assert_eq!(cfg.input.idle.dpms_suspend_seconds, 600);
    assert_eq!(cfg.input.idle.dpms_off_seconds, 900);
}

#[test]
fn input_bell_all_none_when_omitted() {
    let toml = r##"[general]
tags = ["X"]
"##;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, toml).unwrap();
    let paths = demiurge_x11::config::Paths::with_config(path);
    let cfg = demiurge_x11::config::load(&paths).unwrap();
    assert!(cfg.input.bell.enabled.is_none());
    assert!(cfg.input.bell.volume.is_none());
    assert!(cfg.input.bell.pitch_hz.is_none());
    assert!(cfg.input.bell.duration_ms.is_none());
}

#[test]
fn input_bell_disable_sets_enabled_some_false() {
    let toml = r##"[general]
tags = ["X"]

[input.bell]
enabled = false
"##;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, toml).unwrap();
    let paths = demiurge_x11::config::Paths::with_config(path);
    let cfg = demiurge_x11::config::load(&paths).unwrap();
    assert_eq!(cfg.input.bell.enabled, Some(false));
}

#[test]
fn cursor_auto_hide_defaults() {
    let toml = r##"[general]
tags = ["X"]
"##;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, toml).unwrap();
    let paths = demiurge_x11::config::Paths::with_config(path);
    let cfg = demiurge_x11::config::load(&paths).unwrap();
    assert!(!cfg.cursor.auto_hide);
    assert_eq!(cfg.cursor.auto_hide_seconds, 5);
}

#[test]
fn cursor_auto_hide_explicit() {
    let toml = r##"[general]
tags = ["X"]

[cursor]
auto_hide = true
auto_hide_seconds = 0
"##;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, toml).unwrap();
    let paths = demiurge_x11::config::Paths::with_config(path);
    let cfg = demiurge_x11::config::load(&paths).unwrap();
    assert!(cfg.cursor.auto_hide);
    assert_eq!(cfg.cursor.auto_hide_seconds, 0);
}
