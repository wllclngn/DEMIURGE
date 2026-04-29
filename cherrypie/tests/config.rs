use std::fs;
use std::path::PathBuf;

use cherrypie::config;

fn temp_config(content: &str) -> (tempfile::TempDir, config::Paths) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, content).unwrap();
    let paths = config::Paths::with_config(path);
    (dir, paths)
}

// BASIC PARSING

#[test]
fn parse_single_rule() {
    let (_dir, paths) = temp_config(
        r#"
        [[rule]]
        class = "kitty"
        workspace = 1
        maximize = true
        "#,
    );

    let cfg = config::load(&paths).unwrap();
    assert_eq!(cfg.rule.len(), 1);
    assert_eq!(cfg.rule[0].class.as_deref(), Some("kitty"));
    assert_eq!(cfg.rule[0].workspace, Some(1));
    assert_eq!(cfg.rule[0].maximize, Some(true));
    assert!(cfg.rule[0].title.is_none());
    assert!(cfg.rule[0].position.is_none());
}

#[test]
fn parse_multiple_rules() {
    let (_dir, paths) = temp_config(
        r#"
        [[rule]]
        class = "kitty"
        workspace = 1

        [[rule]]
        class = "chromium"
        workspace = 2
        position = [0, 0]
        size = [1920, 1080]

        [[rule]]
        title = ".*GIMP.*"
        pin = true
        opacity = 0.95
        "#,
    );

    let cfg = config::load(&paths).unwrap();
    assert_eq!(cfg.rule.len(), 3);
}

#[test]
fn empty_rules_array() {
    let (_dir, paths) = temp_config("rule = []");
    let cfg = config::load(&paths).unwrap();
    assert_eq!(cfg.rule.len(), 0);
}

// NEW MATCHERS

#[test]
fn parse_process_matcher() {
    let (_dir, paths) = temp_config(
        r#"
        [[rule]]
        process = "montauk"
        monitor = "Z"
        maximize = true
        "#,
    );

    let cfg = config::load(&paths).unwrap();
    assert_eq!(cfg.rule[0].process.as_deref(), Some("montauk"));
}

#[test]
fn parse_type_matcher() {
    let (_dir, paths) = temp_config(
        r#"
        [[rule]]
        type = "dialog"
        position = "center"
        above = true
        "#,
    );

    let cfg = config::load(&paths).unwrap();
    assert_eq!(cfg.rule[0].window_type.as_deref(), Some("dialog"));
    assert_eq!(cfg.rule[0].above, Some(true));
}

// POSITION VARIANTS

#[test]
fn parse_position_absolute() {
    let (_dir, paths) = temp_config(
        r#"
        [[rule]]
        class = "test"
        position = [100, 200]
        "#,
    );

    let cfg = config::load(&paths).unwrap();
    match cfg.rule[0].position {
        Some(config::PositionValue::Absolute(coords)) => {
            assert_eq!(coords, [100, 200]);
        }
        _ => panic!("expected Absolute position"),
    }
}

#[test]
fn parse_position_named() {
    for name in &[
        "center",
        "top-left",
        "top-right",
        "bottom-left",
        "bottom-right",
        "left",
        "right",
        "top",
        "bottom",
    ] {
        let (_dir, paths) = temp_config(&format!(
            r#"
            [[rule]]
            class = "test"
            position = "{}"
            "#,
            name
        ));

        let cfg = config::load(&paths).unwrap();
        match &cfg.rule[0].position {
            Some(config::PositionValue::Named(n)) => assert_eq!(n, name),
            _ => panic!("expected Named position for '{}'", name),
        }
    }
}

#[test]
fn parse_position_percentage() {
    let (_dir, paths) = temp_config(
        r#"
        [[rule]]
        class = "test"
        position = ["25%", "50%"]
        "#,
    );

    let cfg = config::load(&paths).unwrap();
    match &cfg.rule[0].position {
        Some(config::PositionValue::Flexible(parts)) => {
            assert_eq!(parts[0], "25%");
            assert_eq!(parts[1], "50%");
        }
        _ => panic!("expected Flexible position"),
    }
}

#[test]
fn reject_invalid_position_name() {
    let (_dir, paths) = temp_config(
        r#"
        [[rule]]
        class = "test"
        position = "middle-ish"
        "#,
    );

    let err = config::load(&paths).unwrap_err();
    assert!(err.contains("invalid position"), "got: {}", err);
}

#[test]
fn reject_invalid_position_percentage() {
    let (_dir, paths) = temp_config(
        r#"
        [[rule]]
        class = "test"
        position = ["abc%", "50%"]
        "#,
    );

    let err = config::load(&paths).unwrap_err();
    assert!(err.contains("invalid") || err.contains("percentage"), "got: {}", err);
}

// SIZE VARIANTS

#[test]
fn parse_size_absolute() {
    let (_dir, paths) = temp_config(
        r#"
        [[rule]]
        class = "test"
        size = [1920, 1080]
        "#,
    );

    let cfg = config::load(&paths).unwrap();
    match cfg.rule[0].size {
        Some(config::SizeValue::Absolute(dims)) => assert_eq!(dims, [1920, 1080]),
        _ => panic!("expected Absolute size"),
    }
}

#[test]
fn parse_size_percentage() {
    let (_dir, paths) = temp_config(
        r#"
        [[rule]]
        class = "test"
        size = ["50%", "100%"]
        "#,
    );

    let cfg = config::load(&paths).unwrap();
    match &cfg.rule[0].size {
        Some(config::SizeValue::Flexible(parts)) => {
            assert_eq!(parts[0], "50%");
            assert_eq!(parts[1], "100%");
        }
        _ => panic!("expected Flexible size"),
    }
}

// MONITOR VARIANTS

#[test]
fn parse_monitor_by_index() {
    let (_dir, paths) = temp_config(
        r#"
        [[rule]]
        class = "test"
        monitor = 2
        "#,
    );

    let cfg = config::load(&paths).unwrap();
    match cfg.rule[0].monitor {
        Some(config::MonitorValue::Index(i)) => assert_eq!(i, 2),
        _ => panic!("expected Index monitor"),
    }
}

#[test]
fn parse_monitor_by_name() {
    let (_dir, paths) = temp_config(
        r#"
        [[rule]]
        class = "test"
        monitor = "HDMI-1"
        "#,
    );

    let cfg = config::load(&paths).unwrap();
    match &cfg.rule[0].monitor {
        Some(config::MonitorValue::Name(n)) => assert_eq!(n, "HDMI-1"),
        _ => panic!("expected Name monitor"),
    }
}

// NEW ACTIONS

#[test]
fn parse_all_new_actions() {
    let (_dir, paths) = temp_config(
        r#"
        [[rule]]
        class = "test"
        fullscreen = true
        above = true
        below = false
        decorate = false
        focus = true
        minimize = false
        shade = true
        "#,
    );

    let cfg = config::load(&paths).unwrap();
    let r = &cfg.rule[0];
    assert_eq!(r.fullscreen, Some(true));
    assert_eq!(r.above, Some(true));
    assert_eq!(r.below, Some(false));
    assert_eq!(r.decorate, Some(false));
    assert_eq!(r.focus, Some(true));
    assert_eq!(r.minimize, Some(false));
    assert_eq!(r.shade, Some(true));
}

// VALIDATION

#[test]
fn reject_rule_without_matcher() {
    let (_dir, paths) = temp_config(
        r#"
        [[rule]]
        workspace = 1
        maximize = true
        "#,
    );

    let err = config::load(&paths).unwrap_err();
    assert!(err.contains("no matcher"), "got: {}", err);
}

#[test]
fn process_alone_is_valid_matcher() {
    let (_dir, paths) = temp_config(
        r#"
        [[rule]]
        process = "firefox"
        workspace = 2
        "#,
    );

    config::load(&paths).unwrap(); // should not error
}

#[test]
fn type_alone_is_valid_matcher() {
    let (_dir, paths) = temp_config(
        r#"
        [[rule]]
        type = "dialog"
        above = true
        "#,
    );

    config::load(&paths).unwrap(); // should not error
}

#[test]
fn reject_missing_file() {
    let paths = config::Paths::with_config(PathBuf::from("/tmp/cherrypie-nonexistent.toml"));
    let err = config::load(&paths).unwrap_err();
    assert!(!err.is_empty());
}

#[test]
fn reject_invalid_toml() {
    let (_dir, paths) = temp_config("this is not valid toml [[[");
    let err = config::load(&paths).unwrap_err();
    assert!(!err.is_empty());
}

// FULL EXAMPLE (the user's target config)

#[test]
fn parse_full_example_config() {
    let (_dir, paths) = temp_config(
        r#"
        [[rule]]
        class = "kitty"
        workspace = 1
        maximize = true

        [[rule]]
        class = "kitty"
        title = ".*montauk.*"
        monitor = "Z"
        workspace = 3
        maximize = true

        [[rule]]
        class = "chromium"
        monitor = "X"
        position = "center"
        size = ["80%", "90%"]

        [[rule]]
        class = "pavucontrol"
        position = "top-right"
        size = [400, 600]

        [[rule]]
        class = "thunar"
        monitor = "Y"
        position = "left"
        size = ["50%", "100%"]

        [[rule]]
        process = "montauk"
        monitor = "Z"
        maximize = true

        [[rule]]
        type = "dialog"
        position = "center"
        above = true

        [[rule]]
        class = "firefox"
        monitor = "X"
        workspace = 2
        position = [0, 0]
        size = [1920, 1080]
        fullscreen = true
        focus = true
        decorate = false
        above = true
        opacity = 0.95
        "#,
    );

    let cfg = config::load(&paths).unwrap();
    assert_eq!(cfg.rule.len(), 8);
}
