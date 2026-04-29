use cherrypie::config::Config;
use cherrypie::rules;

fn make_config(toml_str: &str) -> Config {
    toml::from_str(toml_str).unwrap()
}

// CLASS MATCHING

#[test]
fn exact_class_match() {
    let cfg = make_config(r#"
        [[rule]]
        class = "^kitty$"
        workspace = 1
    "#);
    let compiled = rules::compile(&cfg).unwrap();

    assert!(compiled[0].matches("kitty", "", "", "", ""));
    assert!(!compiled[0].matches("kitty-terminal", "", "", "", ""));
    assert!(!compiled[0].matches("xkitty", "", "", "", ""));
}

#[test]
fn regex_class_match() {
    let cfg = make_config(r#"
        [[rule]]
        class = "chrom.*"
        workspace = 2
    "#);
    let compiled = rules::compile(&cfg).unwrap();

    assert!(compiled[0].matches("chromium", "", "", "", ""));
    assert!(compiled[0].matches("chromium-browser", "", "", "", ""));
    assert!(!compiled[0].matches("firefox", "", "", "", ""));
}

// TITLE MATCHING

#[test]
fn title_regex_match() {
    let cfg = make_config(r#"
        [[rule]]
        title = ".*GIMP.*"
        pin = true
    "#);
    let compiled = rules::compile(&cfg).unwrap();

    assert!(compiled[0].matches("", "GIMP 2.10", "", "", ""));
    assert!(!compiled[0].matches("", "gimp", "", "", ""));
}

#[test]
fn case_insensitive_regex() {
    let cfg = make_config(r#"
        [[rule]]
        title = "(?i)gimp"
        pin = true
    "#);
    let compiled = rules::compile(&cfg).unwrap();

    assert!(compiled[0].matches("", "GIMP", "", "", ""));
    assert!(compiled[0].matches("", "gimp", "", "", ""));
}

// ROLE MATCHING

#[test]
fn role_match() {
    let cfg = make_config(r#"
        [[rule]]
        role = "browser"
        workspace = 2
    "#);
    let compiled = rules::compile(&cfg).unwrap();

    assert!(compiled[0].matches("", "", "browser", "", ""));
    assert!(!compiled[0].matches("", "", "editor", "", ""));
}

// PROCESS MATCHING

#[test]
fn process_match() {
    let cfg = make_config(r#"
        [[rule]]
        process = "montauk"
        maximize = true
    "#);
    let compiled = rules::compile(&cfg).unwrap();

    assert!(compiled[0].matches("", "", "", "montauk", ""));
    assert!(!compiled[0].matches("", "", "", "firefox", ""));
}

#[test]
fn process_regex_match() {
    let cfg = make_config(r#"
        [[rule]]
        process = "python3?\\.?"
        workspace = 3
    "#);
    let compiled = rules::compile(&cfg).unwrap();

    assert!(compiled[0].matches("", "", "", "python3", ""));
    assert!(compiled[0].matches("", "", "", "python", ""));
    assert!(!compiled[0].matches("", "", "", "ruby", ""));
}

// WINDOW TYPE MATCHING

#[test]
fn type_match() {
    let cfg = make_config(r#"
        [[rule]]
        type = "dialog"
        position = "center"
    "#);
    let compiled = rules::compile(&cfg).unwrap();

    assert!(compiled[0].matches("", "", "", "", "dialog"));
    assert!(compiled[0].matches("", "", "", "", "DIALOG")); // case insensitive
    assert!(!compiled[0].matches("", "", "", "", "normal"));
}

// COMBINED MATCHERS

#[test]
fn combined_matchers_all_must_match() {
    let cfg = make_config(r#"
        [[rule]]
        class = "firefox"
        title = ".*YouTube.*"
        workspace = 3
    "#);
    let compiled = rules::compile(&cfg).unwrap();

    assert!(compiled[0].matches("firefox", "YouTube - Firefox", "", "", ""));
    assert!(!compiled[0].matches("firefox", "Google - Firefox", "", "", ""));
    assert!(!compiled[0].matches("chromium", "YouTube", "", "", ""));
}

#[test]
fn class_and_process_combined() {
    let cfg = make_config(r#"
        [[rule]]
        class = "kitty"
        process = "montauk"
        workspace = 3
    "#);
    let compiled = rules::compile(&cfg).unwrap();

    // Both must match
    assert!(compiled[0].matches("kitty", "", "", "montauk", ""));
    // Only class
    assert!(!compiled[0].matches("kitty", "", "", "htop", ""));
    // Only process
    assert!(!compiled[0].matches("alacritty", "", "", "montauk", ""));
}

// NONE MATCHERS ARE PERMISSIVE

#[test]
fn none_matchers_are_permissive() {
    let cfg = make_config(r#"
        [[rule]]
        class = "kitty"
        workspace = 1
    "#);
    let compiled = rules::compile(&cfg).unwrap();

    assert!(compiled[0].matches("kitty", "any title", "any role", "any process", "normal"));
}

// MULTIPLE RULES

#[test]
fn multiple_rules_independent() {
    let cfg = make_config(r#"
        [[rule]]
        class = "kitty"
        workspace = 1

        [[rule]]
        class = "firefox"
        workspace = 2
    "#);
    let compiled = rules::compile(&cfg).unwrap();

    assert!(compiled[0].matches("kitty", "", "", "", ""));
    assert!(!compiled[0].matches("firefox", "", "", "", ""));
    assert!(compiled[1].matches("firefox", "", "", "", ""));
    assert!(!compiled[1].matches("kitty", "", "", "", ""));
}

// INVALID REGEX

#[test]
fn invalid_regex_rejected() {
    let cfg = make_config(r#"
        [[rule]]
        class = "[invalid"
        workspace = 1
    "#);
    match rules::compile(&cfg) {
        Err(e) => assert!(e.contains("bad regex"), "expected 'bad regex', got: {}", e),
        Ok(_) => panic!("expected error for invalid regex"),
    }
}

#[test]
fn invalid_process_regex_rejected() {
    let cfg = make_config(r#"
        [[rule]]
        process = "(unclosed"
        workspace = 1
    "#);
    assert!(rules::compile(&cfg).is_err());
}

// ACTIONS PRESERVED

#[test]
fn all_actions_preserved() {
    let cfg = make_config(r#"
        [[rule]]
        class = "test"
        workspace = 5
        maximize = true
        fullscreen = true
        pin = true
        minimize = false
        shade = true
        above = true
        below = false
        decorate = false
        focus = true
        opacity = 0.75
        position = [10, 20]
        size = [640, 480]
    "#);
    let compiled = rules::compile(&cfg).unwrap();
    let r = &compiled[0];

    assert_eq!(r.workspace, Some(5));
    assert_eq!(r.maximize, Some(true));
    assert_eq!(r.fullscreen, Some(true));
    assert_eq!(r.pin, Some(true));
    assert_eq!(r.minimize, Some(false));
    assert_eq!(r.shade, Some(true));
    assert_eq!(r.above, Some(true));
    assert_eq!(r.below, Some(false));
    assert_eq!(r.decorate, Some(false));
    assert_eq!(r.focus, Some(true));
    assert_eq!(r.opacity, Some(0.75));
}

// POSITION COMPILATION

#[test]
fn compile_named_position() {
    let cfg = make_config(r#"
        [[rule]]
        class = "test"
        position = "center"
    "#);
    let compiled = rules::compile(&cfg).unwrap();
    assert!(matches!(
        compiled[0].position,
        Some(rules::PositionTarget::Named(rules::NamedPosition::Center))
    ));
}

#[test]
fn compile_absolute_position() {
    let cfg = make_config(r#"
        [[rule]]
        class = "test"
        position = [100, 200]
    "#);
    let compiled = rules::compile(&cfg).unwrap();
    assert!(matches!(
        compiled[0].position,
        Some(rules::PositionTarget::Absolute(100, 200))
    ));
}

#[test]
fn compile_percentage_position() {
    let cfg = make_config(r#"
        [[rule]]
        class = "test"
        position = ["25%", "50%"]
    "#);
    let compiled = rules::compile(&cfg).unwrap();
    match &compiled[0].position {
        Some(rules::PositionTarget::Flexible(x, y)) => {
            assert!(matches!(x, rules::DimensionVal::Percent(p) if (*p - 0.25).abs() < 0.001));
            assert!(matches!(y, rules::DimensionVal::Percent(p) if (*p - 0.50).abs() < 0.001));
        }
        _ => panic!("expected Flexible position"),
    }
}

// SIZE COMPILATION

#[test]
fn compile_percentage_size() {
    let cfg = make_config(r#"
        [[rule]]
        class = "test"
        size = ["80%", "90%"]
    "#);
    let compiled = rules::compile(&cfg).unwrap();
    match &compiled[0].size {
        Some(rules::SizeTarget::Flexible(w, h)) => {
            assert!(matches!(w, rules::DimensionVal::Percent(p) if (*p - 0.80).abs() < 0.001));
            assert!(matches!(h, rules::DimensionVal::Percent(p) if (*p - 0.90).abs() < 0.001));
        }
        _ => panic!("expected Flexible size"),
    }
}

// MONITOR COMPILATION

#[test]
fn compile_monitor_by_name() {
    let cfg = make_config(r#"
        [[rule]]
        class = "test"
        monitor = "Z"
    "#);
    let compiled = rules::compile(&cfg).unwrap();
    assert!(matches!(
        &compiled[0].monitor,
        Some(rules::MonitorTarget::Name(n)) if n == "Z"
    ));
}

#[test]
fn compile_monitor_by_index() {
    let cfg = make_config(r#"
        [[rule]]
        class = "test"
        monitor = 1
    "#);
    let compiled = rules::compile(&cfg).unwrap();
    assert!(matches!(
        compiled[0].monitor,
        Some(rules::MonitorTarget::Index(1))
    ));
}

// EMPTY

#[test]
fn compile_empty_rules() {
    let cfg = make_config("rule = []");
    let compiled = rules::compile(&cfg).unwrap();
    assert!(compiled.is_empty());
}
