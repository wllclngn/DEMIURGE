// The one-time move of [[startup.spawn]] tags into cherrypie rules.

use std::fs;

use demiurge_x11::config::{self, Paths};
use demiurge_x11::migrate::{spawn_tags, strip_spawn_tags};

const CONFIG: &str = r#"# my desktop
[general]
tags = ["X", "Y", "Z"]

[[startup.spawn]]
cmd = "kitty"
tag = "X"

# the monitor, on its own tag
[[startup.spawn]]
cmd = "kitty --class montauk-term -e montauk"
tag = "Z"   # keep it out of the way
class = "montauk-term"

[[keybind]]
mods = ["Super"]
key = "t"
action = "spawn"
args = "kitty"
"#;

#[test]
fn only_spawn_tags_are_stripped() {
    let out = strip_spawn_tags(CONFIG);
    assert!(!out.contains("tag = \"X\"") && !out.contains("tag = \"Z\""));
    assert!(out.contains("tags = [\"X\", \"Y\", \"Z\"]"), "[general] tags must stay");
    assert!(out.contains("# the monitor, on its own tag"));
    assert!(out.contains("class = \"montauk-term\""));
    assert!(out.contains("key = \"t\""));
}

#[test]
fn tags_move_into_cherrypie_rules_once() {
    let dir = tempfile::tempdir().unwrap();
    let demiurge = dir.path().join("demiurge.toml");
    let cherrypie = dir.path().join("cherrypie.toml");
    fs::write(&demiurge, CONFIG).unwrap();
    let cfg = config::load(&Paths::with_config(demiurge.clone())).unwrap();

    assert_eq!(spawn_tags(&cfg, &demiurge, &cherrypie), Ok(2));

    let rules = fs::read_to_string(&cherrypie).unwrap();
    assert!(rules.contains("class = '^kitty$'\nworkspace = \"X\""), "{rules}");
    assert!(rules.contains("class = '^montauk\\-term$'\nworkspace = \"Z\""), "{rules}");
    let compiled = cherrypie::config::load(&cherrypie::config::Paths::with_config(cherrypie.clone()))
        .map(|c| c.rule.len());
    assert_eq!(compiled.ok(), Some(2), "the moved rules must parse");

    let rewritten = config::load(&Paths::with_config(demiurge.clone())).unwrap();
    assert!(rewritten.startup.spawn.iter().all(|sp| sp.tag.is_none()));
    assert_eq!(rewritten.startup.spawn.len(), 2, "the spawns themselves stay");

    assert_eq!(spawn_tags(&rewritten, &demiurge, &cherrypie), Ok(0));
    assert_eq!(fs::read_to_string(&cherrypie).unwrap(), rules, "a second run appended again");
}

#[test]
fn a_spawn_without_a_tag_loads() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, "[general]\ntags = [\"X\"]\n\n[[startup.spawn]]\ncmd = \"kitty\"\n").unwrap();
    let cfg = config::load(&Paths::with_config(path)).unwrap();
    assert_eq!(cfg.startup.spawn[0].tag, None);
}
