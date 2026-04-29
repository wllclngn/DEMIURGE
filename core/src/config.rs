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
    #[serde(default)]
    pub input: Input,
    #[serde(default, rename = "display")]
    pub displays: Vec<Display>,
    #[serde(default, rename = "keybind")]
    pub keybinds: Vec<Keybind>,
    // Used by the GORDIAN KNOT binary, not DEMIURGE itself. Dead-code
    // analysis runs per-binary and flags it here.
    #[allow(dead_code)]
    #[serde(default)]
    pub gordian_knot: GordianKnot,
}

// Per-output display configuration. Each entry matches an output by
// connector name (e.g., "DP-1", "HDMI-A-0") or EDID model substring.
// First match wins; outputs with no matching entry keep their server-
// default mode and no DSR.
//
// Dynamic Super Resolution (DSR) -- the per-output dsr_multiplier --
// renders the framebuffer at a higher resolution than the panel and
// downscales for scanout. On X11, applied via RandR's CRTC transform
// matrix (driver-side scaler). On Wayland, applied via a render-
// target multiplier in demiurge-wl with a downscale pass before
// scanout. Same TOML reaches both implementations.
#[derive(Debug, Clone, Deserialize)]
pub struct Display {
    pub r#match: String,
    #[serde(default)]
    pub mode: String,
    #[serde(default = "default_dsr_multiplier")]
    pub dsr_multiplier: f64,
    #[serde(default)]
    pub dpi: u32,
    #[serde(default = "default_dsr_filter")]
    pub filter: String,
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
    // When true, the cursor hides after auto_hide_seconds of no
    // pointer motion and reappears on motion. Default false: normal
    // cursor behavior. Setting auto_hide_seconds = 0 with auto_hide
    // = true gives "always hidden except while moving" -- the cursor
    // is gone the moment you stop, visible only during active
    // movement. Force-shown during a Super+drag operation.
    #[serde(default)]
    pub auto_hide: bool,
    #[serde(default = "default_cursor_auto_hide_seconds")]
    pub auto_hide_seconds: u32,
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            theme: default_cursor_theme(),
            size: default_cursor_size(),
            auto_hide: false,
            auto_hide_seconds: default_cursor_auto_hide_seconds(),
        }
    }
}

// Settings DEMIURGE owns directly (replaces external xset / setxkbmap
// dependencies in startup.commands). Each field maps 1:1 to an X11
// protocol call applied at Wm::init and re-applied on the events
// that would otherwise clobber it (MappingNotify for keyboard,
// reload_config for everything else).
#[derive(Debug, Default, Deserialize)]
pub struct Input {
    #[serde(default)]
    pub keyboard: InputKeyboard,
    #[serde(default)]
    pub idle: Idle,
    #[serde(default)]
    pub bell: Bell,
}

#[derive(Debug, Deserialize)]
pub struct InputKeyboard {
    // Milliseconds to wait before auto-repeat starts. xset r rate's
    // first arg. 0 disables (the X server uses its compiled default).
    #[serde(default)]
    pub repeat_delay: u32,
    // Auto-repeat rate in repeats per second. xset r rate's second
    // arg. 0 disables.
    #[serde(default)]
    pub repeat_rate: u32,
    // XKB Rules-Model-Layout-Variant-Options. Empty string / empty
    // vec = "leave server default in place" (DEMIURGE doesn't own
    // the layout). When non-empty, applied via XKB
    // GetKeyboardByName at Wm::init and reload_config -- the server
    // compiles internally; no shellout to setxkbmap or xkbcomp.
    //
    // layout: ISO 639 code or compound ("us", "us,de", "fr").
    // variant: optional sub-layout ("dvorak", "colemak").
    // options: ["caps:escape", "ctrl:nocaps", "compose:menu"]. List
    // is pass-through; typos surface as the X server falling back
    // to the previous keymap (visible at next keystroke).
    #[serde(default)]
    pub layout: String,
    #[serde(default)]
    pub variant: String,
    #[serde(default)]
    pub options: Vec<String>,
}

impl Default for InputKeyboard {
    fn default() -> Self {
        Self {
            repeat_delay: 0,
            repeat_rate: 0,
            layout: String::new(),
            variant: String::new(),
            options: Vec::new(),
        }
    }
}

// Idle / power-save / lock thresholds. Single source of truth for the
// "when does the session go dark" timeline -- DEMIURGE applies DPMS
// timeouts to the X server, the X screensaver timeout, and GORDIAN
// KNOT's daemon reads lock_seconds from here too.
//
// All values in seconds. 0 means "leave the server default in place"
// (DEMIURGE doesn't own that timer). Tier ordering during a session:
//   screensaver_seconds <= dpms_standby_seconds <=
//   dpms_suspend_seconds <= dpms_off_seconds, then
//   lock_seconds independently triggers GORDIAN KNOT.
//
// We don't enforce ordering -- the X server will accept any values,
// and a user can set lock < dpms or vice versa depending on their
// preference (lock-before-blank vs. blank-before-lock).
#[derive(Debug, Deserialize)]
pub struct Idle {
    #[serde(default)]
    pub lock_seconds: u32,
    #[serde(default)]
    pub screensaver_seconds: u32,
    #[serde(default)]
    pub dpms_standby_seconds: u32,
    #[serde(default)]
    pub dpms_suspend_seconds: u32,
    #[serde(default)]
    pub dpms_off_seconds: u32,
}

impl Default for Idle {
    fn default() -> Self {
        Self {
            lock_seconds: 0,
            screensaver_seconds: 0,
            dpms_standby_seconds: 0,
            dpms_suspend_seconds: 0,
            dpms_off_seconds: 0,
        }
    }
}

// X11 audible bell. xset b's surface. enabled = false silences the
// terminal BEL on most setups; the alternative is each app that emits
// BEL handling it themselves, which most don't.
#[derive(Debug, Deserialize)]
pub struct Bell {
    // None = leave server default in place. Some(true) = on,
    // Some(false) = silence. We use Option rather than a bool with
    // a default because "the user explicitly didn't configure this"
    // is meaningfully different from "the user wants the default".
    #[serde(default)]
    pub enabled: Option<bool>,
    // Volume 0-100; X server clamps. None = don't touch.
    #[serde(default)]
    pub volume: Option<u8>,
    // Pitch in Hz. None = don't touch.
    #[serde(default)]
    pub pitch_hz: Option<u16>,
    // Duration in milliseconds. None = don't touch.
    #[serde(default)]
    pub duration_ms: Option<u16>,
}

impl Default for Bell {
    fn default() -> Self {
        Self {
            enabled: None,
            volume: None,
            pitch_hz: None,
            duration_ms: None,
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
fn default_cursor_auto_hide_seconds() -> u32 {
    5
}
fn default_dsr_multiplier() -> f64 {
    1.0
}
fn default_dsr_filter() -> String {
    "bilinear".into()
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
