// Keybind compilation: Keysyms, modifier predicates, tag indexing and
// skipped entries.

use demiurge_core::config::Keybind;
use demiurge_core::Action;
use demiurge_wl::keys::compile;

fn kb(mods: &[&str], key: &str, action: &str, args: Option<&str>) -> Keybind {
    Keybind {
        mods: mods.iter().map(|s| s.to_string()).collect(),
        key: key.to_string(),
        action: action.to_string(),
        args: args.map(|s| s.to_string()),
    }
}

#[test]
fn compiles_known_bindings() {
    let binds = compile(&[
        kb(&["Super"], "Return", "spawn", Some("kitty")),
        kb(&["Super"], "q", "close_window", None),
        kb(&["Super", "Shift"], "1", "move_to_tag", Some("1")),
    ]);
    assert_eq!(binds.len(), 3);
    // Super+Shift+1 -> move_to_tag(0), exact modifier predicate.
    assert!(binds[2].sup && binds[2].shift && !binds[2].alt && !binds[2].ctrl);
    assert_eq!(binds[2].keysym, 0x0031);
    match binds[2].action {
        Action::MoveToTag(t) => assert_eq!(t, 0),
        _ => panic!("expected MoveToTag"),
    }
}

#[test]
fn return_keysym_and_mods() {
    let binds = compile(&[kb(&["Super"], "Return", "spawn", Some("kitty"))]);
    assert_eq!(binds.len(), 1);
    assert_eq!(binds[0].keysym, 0xff0d);
    assert!(binds[0].sup);
    assert!(!binds[0].alt && !binds[0].ctrl && !binds[0].shift);
    assert!(matches!(binds[0].action, Action::Spawn(_)));
}

#[test]
fn view_tag_is_zero_indexed() {
    let binds = compile(&[kb(&["Super"], "2", "view_tag", Some("2"))]);
    assert_eq!(binds.len(), 1);
    match binds[0].action {
        Action::ViewTag(t) => assert_eq!(t, 1),
        _ => panic!("expected ViewTag"),
    }
}

#[test]
fn unknown_key_is_skipped() {
    let binds = compile(&[
        kb(&["Super"], "NoSuchKey", "quit", None),
        kb(&["Super"], "q", "quit", None),
    ]);
    assert_eq!(binds.len(), 1);
    assert_eq!(binds[0].keysym, 0x0071);
}

#[test]
fn unknown_action_is_skipped() {
    let binds = compile(&[kb(&["Super"], "q", "bogus_action", None)]);
    assert!(binds.is_empty());
}

#[test]
fn hex_keysym_parses() {
    let binds = compile(&[kb(&[], "0xff09", "mru_next", None)]);
    assert_eq!(binds.len(), 1);
    assert_eq!(binds[0].keysym, 0xff09);
}
