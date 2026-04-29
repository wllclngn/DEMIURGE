use regex::Regex;

use crate::config::{Config, MonitorValue, PositionValue, Rule, SizeValue};

pub struct CompiledRule {
    // Matchers
    pub class: Option<Regex>,
    pub title: Option<Regex>,
    pub role: Option<Regex>,
    pub process: Option<Regex>,
    pub window_type: Option<String>,

    // Actions
    pub workspace: Option<u32>,
    pub monitor: Option<MonitorTarget>,
    pub position: Option<PositionTarget>,
    pub size: Option<SizeTarget>,
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

#[derive(Debug, Clone)]
pub enum MonitorTarget {
    Index(u32),
    Name(String),
}

#[derive(Debug, Clone)]
pub enum PositionTarget {
    Absolute(i32, i32),
    Named(NamedPosition),
    Flexible(DimensionVal, DimensionVal),
}

#[derive(Debug, Clone, Copy)]
pub enum NamedPosition {
    Center,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Debug, Clone)]
pub enum SizeTarget {
    Absolute(u32, u32),
    Flexible(DimensionVal, DimensionVal),
}

#[derive(Debug, Clone, Copy)]
pub enum DimensionVal {
    Pixels(i32),
    Percent(f64),
}

impl CompiledRule {
    fn compile(rule: &Rule) -> Result<Self, String> {
        let compile_pat = |pat: &Option<String>| -> Result<Option<Regex>, String> {
            match pat {
                Some(s) => Regex::new(s)
                    .map(Some)
                    .map_err(|e| format!("bad regex '{}': {}", s, e)),
                None => Ok(None),
            }
        };

        Ok(Self {
            class: compile_pat(&rule.class)?,
            title: compile_pat(&rule.title)?,
            role: compile_pat(&rule.role)?,
            process: compile_pat(&rule.process)?,
            window_type: rule.window_type.clone(),

            workspace: rule.workspace,
            monitor: rule.monitor.as_ref().map(compile_monitor),
            position: rule.position.as_ref().map(compile_position).transpose()?,
            size: rule.size.as_ref().map(compile_size).transpose()?,
            maximize: rule.maximize,
            fullscreen: rule.fullscreen,
            pin: rule.pin,
            minimize: rule.minimize,
            shade: rule.shade,
            above: rule.above,
            below: rule.below,
            decorate: rule.decorate,
            focus: rule.focus,
            opacity: rule.opacity,
        })
    }

    pub fn matches(
        &self,
        class: &str,
        title: &str,
        role: &str,
        process: &str,
        window_type: &str,
    ) -> bool {
        let class_ok = self.class.as_ref().is_none_or(|re| re.is_match(class));
        let title_ok = self.title.as_ref().is_none_or(|re| re.is_match(title));
        let role_ok = self.role.as_ref().is_none_or(|re| re.is_match(role));
        let process_ok = self.process.as_ref().is_none_or(|re| re.is_match(process));
        let type_ok = self
            .window_type
            .as_ref()
            .is_none_or(|t| t.eq_ignore_ascii_case(window_type));
        class_ok && title_ok && role_ok && process_ok && type_ok
    }
}

fn compile_monitor(val: &MonitorValue) -> MonitorTarget {
    match val {
        MonitorValue::Index(i) => MonitorTarget::Index(*i),
        MonitorValue::Name(n) => MonitorTarget::Name(n.clone()),
    }
}

fn compile_position(val: &PositionValue) -> Result<PositionTarget, String> {
    match val {
        PositionValue::Named(name) => {
            let named = match name.as_str() {
                "center" => NamedPosition::Center,
                "top-left" => NamedPosition::TopLeft,
                "top-right" => NamedPosition::TopRight,
                "bottom-left" => NamedPosition::BottomLeft,
                "bottom-right" => NamedPosition::BottomRight,
                "left" => NamedPosition::Left,
                "right" => NamedPosition::Right,
                "top" => NamedPosition::Top,
                "bottom" => NamedPosition::Bottom,
                _ => return Err(format!("unknown position '{}'", name)),
            };
            Ok(PositionTarget::Named(named))
        }
        PositionValue::Absolute(coords) => Ok(PositionTarget::Absolute(coords[0], coords[1])),
        PositionValue::Flexible(parts) => {
            let x = parse_dimension(&parts[0])?;
            let y = parse_dimension(&parts[1])?;
            Ok(PositionTarget::Flexible(x, y))
        }
    }
}

fn compile_size(val: &SizeValue) -> Result<SizeTarget, String> {
    match val {
        SizeValue::Absolute(dims) => Ok(SizeTarget::Absolute(dims[0], dims[1])),
        SizeValue::Flexible(parts) => {
            let w = parse_dimension(&parts[0])?;
            let h = parse_dimension(&parts[1])?;
            Ok(SizeTarget::Flexible(w, h))
        }
    }
}

fn parse_dimension(s: &str) -> Result<DimensionVal, String> {
    if let Some(pct) = s.strip_suffix('%') {
        let val: f64 = pct
            .parse()
            .map_err(|_| format!("invalid percentage '{}'", s))?;
        Ok(DimensionVal::Percent(val / 100.0))
    } else {
        let val: i32 = s.parse().map_err(|_| format!("invalid dimension '{}'", s))?;
        Ok(DimensionVal::Pixels(val))
    }
}

pub fn compile(config: &Config) -> Result<Vec<CompiledRule>, String> {
    config
        .rule
        .iter()
        .enumerate()
        .map(|(i, r)| CompiledRule::compile(r).map_err(|e| format!("rule[{}]: {}", i, e)))
        .collect()
}
