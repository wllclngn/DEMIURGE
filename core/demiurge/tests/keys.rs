use demiurge_core::Action;
use demiurge_core::keys::{name_to_keysym, parse_action};

#[test]
fn an_action_and_its_args_parse() {
    assert!(matches!(parse_action("view_tag", &Some("2".into()), 0), Ok(Action::ViewTag(1))));
    assert!(matches!(parse_action("spawn", &Some("kitty".into()), 0), Ok(Action::Spawn(cmd)) if cmd == "kitty"));
    assert!(parse_action("view_tag", &None, 3).unwrap_err().contains("keybind[3]"));
    assert!(parse_action("no_such_action", &None, 0).is_err());
}

#[test]
fn key_names_are_x11_keysyms() {
    assert_eq!(name_to_keysym("Return"), Some(0xff0d));
    assert_eq!(name_to_keysym("Tab"), Some(0xff09));
    assert_eq!(name_to_keysym("F12"), Some(0xffc9));
    assert_eq!(name_to_keysym("a"), Some(0x61));
    assert_eq!(name_to_keysym("NoSuchKey"), None);
}
