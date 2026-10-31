// The window-policy API between a window manager or compositor (DEMIURGE,
// the client) and cherrypie (the server): REST-shaped JSON, one object per
// line, over one persistent Unix socket. The client asks; cherrypie answers.
//
//   POST   /windows                NewWindow  -> 200 Placement
//   DELETE /windows/{id}                      -> 204
//   PUT    /windows/{id}/geometry  Moved      -> 204
//   PUT    /monitors               [Monitor]  -> 204
//
// While a client is connected, cherrypie's own X11 actions stand down: The
// client places every window from the answers, so the two never act on the
// same window.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

// The display name as it appears in cherrypie's runtime file names.
pub fn display_tag(display: &str) -> String {
    display.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect()
}

// $XDG_RUNTIME_DIR/cherrypie-<display>.sock
pub fn socket_path(display: &str) -> PathBuf {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(dir).join(format!("cherrypie-{}.sock", display_tag(display)))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub op: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub body: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    pub status: u16,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub body: Value,
}

impl Request {
    pub fn new(id: u64, op: &str, path: &str, body: impl Serialize) -> Self {
        let body = serde_json::to_value(body).unwrap_or(Value::Null);
        Self { id, op: op.into(), path: path.into(), body }
    }

    pub fn line(&self) -> String {
        let mut s = serde_json::to_string(self).unwrap_or_default();
        s.push('\n');
        s
    }
}

impl Response {
    pub fn new(id: u64, status: u16, body: impl Serialize) -> Self {
        let body = serde_json::to_value(body).unwrap_or(Value::Null);
        Self { id, status, body }
    }

    pub fn line(&self) -> String {
        let mut s = serde_json::to_string(self).unwrap_or_default();
        s.push('\n');
        s
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Monitor {
    pub name: String,
    pub rect: Rect,
    // The part of the monitor windows are placed in: Below DEMIURGE's bar.
    pub work_area: Rect,
}

// A window about to map, as the client sees it before mapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewWindow {
    pub window: u32,
    pub class: String,
    pub title: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub process: String,
    // The EWMH type in cherrypie's spelling: "normal", "dialog", ...
    pub window_type: String,
    #[serde(default)]
    pub transient: bool,
    pub requested: Rect,
    #[serde(default)]
    pub fullscreen: bool,
    // The monitor the window would land on by the client's own reckoning.
    pub monitor: String,
}

// Where the user left a window at the end of a move or resize: The monitor
// it sits on and its rect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Moved {
    pub monitor: String,
    pub rect: Rect,
}

// A tag (an EWMH desktop) by its name, as DEMIURGE's [general] tags spell
// it, or by its index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Tag {
    Index(u32),
    Name(String),
}

// The answer to POST /windows. A field left out is the client's to decide.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry: Option<Rect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<Tag>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fullscreen: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub above: Option<bool>,
}
