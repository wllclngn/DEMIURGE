// Window memory. Where the user leaves a window, the rule that decides its
// geometry records the spot as a monitor name plus percentages of that
// monitor's work area, so the class opens there next time. cherrypie's own
// learned rules remember freely; a hand-written rule only with
// `remember = true`. The file is edited with toml_edit, so the user's
// comments, ordering and formatting survive.

use toml_edit::{DocumentMut, Item, Table, Value};

use crate::api::{NewWindow, Rect};
use crate::config::Config;
use crate::learn::LEARNED_MARK;
use crate::rules;

// A window's spot as a rule spells it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spot {
    pub monitor: String,
    pub position: [String; 2],
    pub size: [String; 2],
}

impl Spot {
    // `rect` as percentages of `area`, the work area of `monitor`.
    pub fn new(monitor: &str, rect: Rect, area: Rect) -> Self {
        Self {
            monitor: monitor.into(),
            position: [percent(rect.x - area.x, area.w), percent(rect.y - area.y, area.h)],
            size: [percent(rect.w as i32, area.w), percent(rect.h as i32, area.h)],
        }
    }
}

// Two decimals land back on the same pixel for any work area under 10000 px.
fn percent(part: i32, whole: u32) -> String {
    let p = (part as f64 * 10000.0 / whole.max(1) as f64).round() / 100.0 + 0.0;
    format!("{p}%")
}

// `text` with `spot` written into the rule that decides `w`'s geometry: The
// last matching rule that sets a size or position, else the last matching
// rule. None when no rule matches, that rule does not remember, or it
// already holds the spot.
pub fn record(text: &str, w: &NewWindow, spot: &Spot) -> Result<Option<String>, String> {
    let cfg: Config = toml::from_str(text).map_err(|e| e.to_string())?;
    let compiled = rules::compile(&cfg)?;
    let matching: Vec<usize> = compiled
        .iter()
        .enumerate()
        .filter(|(_, r)| r.matches(&w.class, &w.title, &w.role, &w.process, &w.window_type))
        .map(|(i, _)| i)
        .collect();
    let deciding = matching
        .iter()
        .rev()
        .find(|&&i| compiled[i].size.is_some() || compiled[i].position.is_some())
        .or(matching.last());
    let Some(&index) = deciding else {
        return Ok(None);
    };

    let mut doc: DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| e.to_string())?;
    let Some(table) = doc
        .get_mut("rule")
        .and_then(Item::as_array_of_tables_mut)
        .and_then(|rules| rules.get_mut(index))
    else {
        return Ok(None);
    };
    if !remembers(table) {
        return Ok(None);
    }
    set(table, "monitor", Value::from(spot.monitor.as_str()));
    set(table, "position", spot.position.iter().collect());
    set(table, "size", spot.size.iter().collect());
    let out = doc.to_string();
    Ok((out != text).then_some(out))
}

// A rule remembers when it says so; saying nothing, when cherrypie learned
// it, which the comment learn.rs writes above it marks.
fn remembers(table: &Table) -> bool {
    match table.get("remember").and_then(Item::as_bool) {
        Some(on) => on,
        None => table
            .decor()
            .prefix()
            .and_then(|p| p.as_str())
            .is_some_and(|p| p.lines().any(|l| l.starts_with(LEARNED_MARK))),
    }
}

// Replace a value in place, keeping its key and any trailing comment; a key
// the rule lacks is added at its end.
fn set(table: &mut Table, key: &str, mut value: Value) {
    match table.get_mut(key).and_then(Item::as_value_mut) {
        Some(old) => {
            *value.decor_mut() = old.decor().clone();
            *old = value;
        }
        None => {
            table.insert(key, Item::Value(value));
        }
    }
}
