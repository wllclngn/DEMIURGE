use serde::Deserialize;
use std::fs;
use std::path::PathBuf;

pub struct Paths {
    pub config_file: PathBuf,
}

impl Paths {
    pub fn init() -> Result<Self, String> {
        let home = std::env::var("HOME").map_err(|_| "HOME not set".to_string())?;
        let config_dir = PathBuf::from(&home).join(".config").join("demiurge");
        fs::create_dir_all(&config_dir).map_err(|e| format!("config dir: {}", e))?;

        Ok(Self {
            config_file: config_dir.join("config.toml"),
        })
    }

    pub fn with_config(path: PathBuf) -> Self {
        Self { config_file: path }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub general: General,
    #[serde(default)]
    pub bar: Bar,
    #[serde(default)]
    pub startup: Startup,
    #[serde(default)]
    pub cursor: Cursor,
    #[serde(default)]
    pub font: Font,
    #[serde(default, rename = "keybind")]
    pub keybinds: Vec<Keybind>,
    // Used by the GORDIAN KNOT binary, not DEMIURGE itself. Dead-code
    // analysis runs per-binary and flags it here.
    #[allow(dead_code)]
    #[serde(default)]
    pub gordian_knot: GordianKnot,
}

#[derive(Debug, Deserialize)]
pub struct General {
    #[serde(default = "default_tags")]
    pub tags: Vec<String>,
    #[serde(default = "default_layout")]
    pub default_layout: String,
    #[serde(default = "default_master_ratio")]
    pub master_ratio: f64,
}

impl Default for General {
    fn default() -> Self {
        Self {
            tags: default_tags(),
            default_layout: default_layout(),
            master_ratio: default_master_ratio(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Bar {
    #[serde(default = "default_bar_height")]
    pub height: u32,
    #[serde(default = "default_font")]
    pub font: String,
    #[serde(default = "default_bar_bg")]
    pub bg: String,
    #[serde(default = "default_bar_fg")]
    pub fg: String,
    #[serde(default = "default_clock_format")]
    pub clock_format: String,
    #[serde(default = "default_tag_focused_bg")]
    pub tag_focused_bg: String,
    #[serde(default = "default_tag_focused_fg")]
    pub tag_focused_fg: String,
    #[serde(default = "default_tag_occupied_fg")]
    pub tag_occupied_fg: String,
    #[serde(default = "default_tag_empty_fg")]
    pub tag_empty_fg: String,
    #[serde(default = "default_notification_fg")]
    pub notification_fg: String,
}

impl Default for Bar {
    fn default() -> Self {
        Self {
            height: default_bar_height(),
            font: default_font(),
            bg: default_bar_bg(),
            fg: default_bar_fg(),
            clock_format: default_clock_format(),
            tag_focused_bg: default_tag_focused_bg(),
            tag_focused_fg: default_tag_focused_fg(),
            tag_occupied_fg: default_tag_occupied_fg(),
            tag_empty_fg: default_tag_empty_fg(),
            notification_fg: default_notification_fg(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct Startup {
    #[serde(default)]
    pub commands: Vec<String>,
    #[serde(default)]
    pub spawn: Vec<TaggedSpawn>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TaggedSpawn {
    pub cmd: String,
    pub tag: String,
    #[serde(default)]
    pub class: Option<String>,
}

impl TaggedSpawn {
    // WM_CLASS match: explicit config field, or first whitespace-separated
    // token of the command line.
    pub fn class_match(&self) -> String {
        self.class.clone().unwrap_or_else(|| {
            self.cmd
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_string()
        })
    }
}

// Consumed by the GORDIAN KNOT binary. DEMIURGE itself doesn't read these,
// so per-binary dead-code analysis would otherwise flag every field.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct GordianKnot {
    #[serde(default = "default_gk_bg")]
    pub bg: String,
    #[serde(default = "default_gk_fg")]
    pub fg: String,
    #[serde(default = "default_gk_accent")]
    pub accent: String,
    #[serde(default = "default_gk_font")]
    pub font: String,
    #[serde(default = "default_gk_idle_timeout")]
    pub idle_timeout_seconds: u64,
    #[serde(default = "default_gk_prompt")]
    pub prompt: String,
}

impl Default for GordianKnot {
    fn default() -> Self {
        Self {
            bg: default_gk_bg(),
            fg: default_gk_fg(),
            accent: default_gk_accent(),
            font: default_gk_font(),
            idle_timeout_seconds: default_gk_idle_timeout(),
            prompt: default_gk_prompt(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Cursor {
    #[serde(default = "default_cursor_theme")]
    pub theme: String,
    #[serde(default = "default_cursor_size")]
    pub size: u32,
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            theme: default_cursor_theme(),
            size: default_cursor_size(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Font {
    #[serde(default = "default_font_family")]
    pub default: String,
}

impl Default for Font {
    fn default() -> Self {
        Self {
            default: default_font_family(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Keybind {
    #[serde(default)]
    pub mods: Vec<String>,
    pub key: String,
    pub action: String,
    #[serde(default)]
    pub args: Option<String>,
}

// Defaults
fn default_tags() -> Vec<String> {
    vec!["X".into(), "Y".into(), "Z".into()]
}
fn default_layout() -> String {
    "floating".into()
}
fn default_master_ratio() -> f64 {
    0.5
}
fn default_bar_height() -> u32 {
    30
}
fn default_font() -> String {
    "monospace 9".into()
}
fn default_bar_bg() -> String {
    "#121212".into()
}
fn default_bar_fg() -> String {
    "#888888".into()
}
fn default_clock_format() -> String {
    "%a %b %d %Y   %I:%M:%S %p %Z".into()
}
fn default_tag_focused_bg() -> String {
    "#4A4881".into()
}
fn default_tag_focused_fg() -> String {
    "#ffffff".into()
}
fn default_tag_occupied_fg() -> String {
    "#888888".into()
}
fn default_tag_empty_fg() -> String {
    "#555555".into()
}
fn default_notification_fg() -> String {
    "#e5a93d".into()
}
fn default_cursor_theme() -> String {
    "default".into()
}
fn default_cursor_size() -> u32 {
    24
}
fn default_font_family() -> String {
    "Noto Sans 9".into()
}
fn default_gk_bg() -> String {
    "#121212".into()
}
fn default_gk_fg() -> String {
    "#c8c8c8".into()
}
fn default_gk_accent() -> String {
    "#e5a93d".into()
}
fn default_gk_font() -> String {
    "monospace 12".into()
}
fn default_gk_idle_timeout() -> u64 {
    600
}
fn default_gk_prompt() -> String {
    "PASSWORD".into()
}

pub fn load(paths: &Paths) -> Result<Config, String> {
    let content = match fs::read_to_string(&paths.config_file) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Config::default());
        }
        Err(e) => return Err(format!("{}: {}", paths.config_file.display(), e)),
    };

    let config: Config =
        toml::from_str(&content).map_err(|e| format!("{}: {}", paths.config_file.display(), e))?;

    if config.general.tags.is_empty() {
        return Err("general.tags must not be empty".into());
    }

    let r = config.general.master_ratio;
    if r <= 0.0 || r >= 1.0 {
        return Err(format!("general.master_ratio must be between 0.0 and 1.0, got {}", r));
    }

    match config.general.default_layout.as_str() {
        "floating" | "tile" | "monocle" => {}
        other => return Err(format!("general.default_layout: unknown layout '{}'", other)),
    }

    for (i, sp) in config.startup.spawn.iter().enumerate() {
        if !config.general.tags.iter().any(|t| t == &sp.tag) {
            return Err(format!(
                "startup.spawn[{}]: tag '{}' not in general.tags",
                i, sp.tag
            ));
        }
        if sp.cmd.trim().is_empty() {
            return Err(format!("startup.spawn[{}]: cmd must not be empty", i));
        }
    }

    for (i, kb) in config.keybinds.iter().enumerate() {
        for m in &kb.mods {
            match m.as_str() {
                "Super" | "Alt" | "Control" | "Shift" => {}
                _ => return Err(format!("keybind[{}]: unknown modifier '{}'", i, m)),
            }
        }
        validate_action(&kb.action, i)?;
    }

    Ok(config)
}

fn validate_action(action: &str, idx: usize) -> Result<(), String> {
    match action {
        "spawn" | "close_window" | "quit" | "mru_next" | "mru_prev"
        | "mru_next_global" | "mru_prev_global" | "view_tag"
        | "view_prev_tag" | "view_next_tag" | "move_to_tag" | "toggle_above"
        | "toggle_fullscreen" | "toggle_layout" | "run_prompt" | "screenshot"
        | "lock" | "volume_up" | "volume_down" | "volume_mute" | "volume_mic_mute"
        | "brightness_up" | "brightness_down" | "media_play_pause" | "media_next"
        | "media_prev" => Ok(()),
        _ => Err(format!("keybind[{}]: unknown action '{}'", idx, action)),
    }
}
