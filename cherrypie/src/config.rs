use serde::Deserialize;
use std::fs;
use std::io;
use std::path::PathBuf;

pub struct Paths {
    pub config_file: PathBuf,
}

impl Paths {
    pub fn init() -> Result<Self, io::Error> {
        let home = std::env::var("HOME").map_err(|_| {
            io::Error::new(io::ErrorKind::NotFound, "HOME not set")
        })?;
        let config_dir = PathBuf::from(&home).join(".config").join("cherrypie");
        fs::create_dir_all(&config_dir)?;

        Ok(Self {
            config_file: config_dir.join("config.toml"),
        })
    }

    pub fn with_config(path: PathBuf) -> Self {
        Self { config_file: path }
    }
}

// Position can be:
//   "center", "top-left", "top-right", "bottom-left", "bottom-right",
//   "left", "right", "top", "bottom"           -> Named anchor
//   [100, 200]                                  -> Absolute pixels
//   ["25%", "50%"]                              -> Percentage of monitor
//   ["100", "200"]                              -> Absolute as strings
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum PositionValue {
    Named(String),
    Absolute([i32; 2]),
    Flexible([String; 2]),
}

// Size can be:
//   [800, 600]                                  -> Absolute pixels
//   ["50%", "100%"]                             -> Percentage of monitor
//   ["800", "600"]                              -> Absolute as strings
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum SizeValue {
    Absolute([u32; 2]),
    Flexible([String; 2]),
}

// Monitor can be:
//   0, 1, 2                                     -> By index
//   "Z", "HDMI-1", "DP-2"                      -> By output name
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum MonitorValue {
    Index(u32),
    Name(String),
}

#[derive(Debug, Deserialize)]
pub struct Rule {
    // Matchers
    pub class: Option<String>,
    pub title: Option<String>,
    pub role: Option<String>,
    pub process: Option<String>,
    #[serde(rename = "type")]
    pub window_type: Option<String>,

    // Actions
    pub workspace: Option<u32>,
    pub monitor: Option<MonitorValue>,
    pub position: Option<PositionValue>,
    pub size: Option<SizeValue>,
    pub maximize: Option<bool>,
    pub fullscreen: Option<bool>,
    pub pin: Option<bool>,
    pub minimize: Option<bool>,
    pub shade: Option<bool>,
    pub above: Option<bool>,
    pub below: Option<bool>,
    pub decorate: Option<bool>,
    pub focus: Option<bool>,
    pub opacity: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct Config {
    pub rule: Vec<Rule>,
}

pub fn load(paths: &Paths) -> Result<Config, String> {
    let content = fs::read_to_string(&paths.config_file).map_err(|e| {
        format!("{}: {}", paths.config_file.display(), e)
    })?;

    let config: Config = toml::from_str(&content).map_err(|e| {
        format!("{}: {}", paths.config_file.display(), e)
    })?;

    for (i, rule) in config.rule.iter().enumerate() {
        if rule.class.is_none()
            && rule.title.is_none()
            && rule.role.is_none()
            && rule.process.is_none()
            && rule.window_type.is_none()
        {
            return Err(format!(
                "rule[{}]: no matcher (need class, title, role, process, or type)",
                i
            ));
        }

        if let Some(ref pos) = rule.position {
            validate_position(pos, i)?;
        }
        if let Some(ref sz) = rule.size {
            validate_size(sz, i)?;
        }
    }

    Ok(config)
}

const NAMED_POSITIONS: &[&str] = &[
    "center",
    "top-left",
    "top-right",
    "bottom-left",
    "bottom-right",
    "left",
    "right",
    "top",
    "bottom",
];

fn validate_position(pos: &PositionValue, rule_idx: usize) -> Result<(), String> {
    match pos {
        PositionValue::Named(name) => {
            if !NAMED_POSITIONS.contains(&name.as_str()) {
                return Err(format!(
                    "rule[{}]: invalid position '{}' (expected one of: {})",
                    rule_idx,
                    name,
                    NAMED_POSITIONS.join(", ")
                ));
            }
        }
        PositionValue::Absolute(_) => {}
        PositionValue::Flexible(parts) => {
            for (j, part) in parts.iter().enumerate() {
                validate_dimension_string(part, rule_idx, "position", j)?;
            }
        }
    }
    Ok(())
}

fn validate_size(sz: &SizeValue, rule_idx: usize) -> Result<(), String> {
    match sz {
        SizeValue::Absolute(_) => {}
        SizeValue::Flexible(parts) => {
            for (j, part) in parts.iter().enumerate() {
                validate_dimension_string(part, rule_idx, "size", j)?;
            }
        }
    }
    Ok(())
}

fn validate_dimension_string(
    s: &str,
    rule_idx: usize,
    field: &str,
    axis: usize,
) -> Result<(), String> {
    let axis_name = if axis == 0 { "x/width" } else { "y/height" };
    if let Some(pct) = s.strip_suffix('%') {
        pct.parse::<f64>().map_err(|_| {
            format!("rule[{}]: invalid {} {} percentage '{}'", rule_idx, field, axis_name, s)
        })?;
    } else {
        s.parse::<i64>().map_err(|_| {
            format!("rule[{}]: invalid {} {} value '{}'", rule_idx, field, axis_name, s)
        })?;
    }
    Ok(())
}
