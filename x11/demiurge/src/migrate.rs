// The one-time move of [[startup.spawn]] tags into cherrypie rules: Which tag
// a window lands on is cherrypie's to answer, and DEMIURGE's spawn entries
// keep only the launching.

use std::fs;
use std::path::Path;

use crate::config::{self, Config};

// Append a cherrypie rule (class and tag) for each spawn entry that names a
// tag, then drop those tags from DEMIURGE's config file. Returns how many
// moved. Nothing is removed unless the rewritten config still loads and the
// rules were written; the file is rewritten in place, so a symlinked config
// stays a symlink.
pub fn spawn_tags(cfg: &Config, demiurge_config: &Path, cherrypie_config: &Path) -> Result<usize, String> {
    let tagged: Vec<(String, String)> = cfg
        .startup
        .spawn
        .iter()
        .filter_map(|sp| sp.tag.as_ref().map(|t| (sp.class_match(), t.clone())))
        .collect();
    if tagged.is_empty() {
        return Ok(0);
    }

    let text = fs::read_to_string(demiurge_config).map_err(|e| format!("read: {e}"))?;
    let stripped = strip_spawn_tags(&text);
    let check = demiurge_config.with_extension("toml.migrating");
    fs::write(&check, &stripped).map_err(|e| format!("write check copy: {e}"))?;
    let loads = config::load(&config::Paths::with_config(check.clone()));
    let _ = fs::remove_file(&check);
    loads.map_err(|e| format!("the config without tags would not load: {e}"))?;

    let date = cherrypie::learn::today();
    for (class, tag) in &tagged {
        cherrypie::learn::append(cherrypie_config, &cherrypie::learn::tag_rule_text(class, tag, &date))
            .map_err(|e| format!("cherrypie config: {e}"))?;
    }
    fs::write(demiurge_config, stripped).map_err(|e| format!("rewrite: {e}"))?;
    Ok(tagged.len())
}

// The config text with every `tag = ...` line inside a [[startup.spawn]]
// table removed; every other line, comments included, is kept verbatim.
pub fn strip_spawn_tags(text: &str) -> String {
    let mut in_spawn = false;
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let t = line.trim_start();
        if t.starts_with('[') {
            in_spawn = t.starts_with("[[startup.spawn]]");
        }
        let is_tag = t.strip_prefix("tag").is_some_and(|rest| rest.trim_start().starts_with('='));
        if !(in_spawn && is_tag) {
            out.push_str(line);
        }
    }
    out
}
