use std::fs;

use cherrypie::config;
use cherrypie::learn;
use cherrypie::rules::{DimensionVal, NamedPosition, PositionTarget, SizeTarget};

fn matches_class(rule: &cherrypie::rules::CompiledRule, class: &str) -> bool {
    rule.matches(class, "title", "", "proc", "normal")
}

#[test]
fn learned_rule_matches_its_class_exactly() {
    let rule = learn::compile(&learn::rule_text("Gimp", "2026-10-16")).unwrap();
    assert!(matches_class(&rule, "Gimp"));
    assert!(!matches_class(&rule, "Gimp-2.10"));
    assert!(!matches_class(&rule, "XGimp"));
}

#[test]
fn learned_rule_escapes_regex_metacharacters() {
    let rule = learn::compile(&learn::rule_text("org.gnome.Nautilus", "2026-10-16")).unwrap();
    assert!(matches_class(&rule, "org.gnome.Nautilus"));
    assert!(!matches_class(&rule, "orgXgnomeXNautilus"));

    let quoted = learn::compile(&learn::rule_text("it's (beta)", "2026-10-16")).unwrap();
    assert!(matches_class(&quoted, "it's (beta)"));
}

#[test]
fn learned_rule_carries_live_defaults() {
    let rule = learn::compile(&learn::rule_text("kitty", "2026-10-16")).unwrap();
    assert!(matches!(rule.position, Some(PositionTarget::Named(NamedPosition::Center))));
    match rule.size {
        Some(SizeTarget::Flexible(DimensionVal::Percent(w), DimensionVal::Percent(h))) => {
            assert!((w - 0.60).abs() < 1e-9 && (h - 0.70).abs() < 1e-9);
        }
        other => panic!("unexpected size {:?}", other),
    }
    assert_eq!(rule.focus, Some(true));
}

#[test]
fn append_creates_the_file_with_a_header() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sub").join("config.toml");
    learn::append(&path, &learn::rule_text("kitty", "2026-10-16")).unwrap();
    let body = fs::read_to_string(&path).unwrap();
    assert!(body.starts_with("# cherrypie rules."));
    let cfg = config::load(&config::Paths::with_config(path)).unwrap();
    assert_eq!(cfg.rule.len(), 1);
}

#[test]
fn append_preserves_user_text_and_adds_a_missing_newline() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let user = "# mine\n[[rule]]\nclass = \"kitty\"\nposition = \"center\"";
    fs::write(&path, user).unwrap();

    learn::append(&path, &learn::rule_text("Gimp", "2026-10-16")).unwrap();
    learn::append(&path, &learn::rule_text("firefox", "2026-10-16")).unwrap();

    let body = fs::read_to_string(&path).unwrap();
    assert!(body.starts_with(user));
    assert!(!body.contains("# cherrypie rules."), "header only goes on a new file");
    let cfg = config::load(&config::Paths::with_config(path)).unwrap();
    assert_eq!(cfg.rule.len(), 3);
}
