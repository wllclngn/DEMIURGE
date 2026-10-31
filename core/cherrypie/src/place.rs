// Where a rule puts a window, independent of how the window is reached: The
// standalone X11 backend and the DEMIURGE API both place through here, so a
// rule means the same thing under either.

use crate::rules::{CompiledRule, DimensionVal, MonitorTarget, NamedPosition, PositionTarget, SizeTarget};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorGeometry {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl MonitorGeometry {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.width as i32 && y >= self.y && y < self.y + self.height as i32
    }
}

pub fn resolve_dim(val: DimensionVal, total: i32) -> i32 {
    match val {
        DimensionVal::Pixels(px) => px,
        // Round, not truncate: 70% of 720 is 503.999... in f64.
        DimensionVal::Percent(pct) => (total as f64 * pct).round() as i32,
    }
}

// The monitor a rule names by index or output name, if it names one that
// exists.
pub fn named_monitor<'a>(rule: &CompiledRule, monitors: &'a [MonitorGeometry]) -> Option<&'a MonitorGeometry> {
    match rule.monitor.as_ref()? {
        MonitorTarget::Index(idx) => monitors.get(*idx as usize),
        MonitorTarget::Name(name) => monitors.iter().find(|m| m.name == *name),
    }
}

pub fn size(sz: &SizeTarget, area: &MonitorGeometry) -> (u32, u32) {
    match sz {
        SizeTarget::Absolute(w, h) => (*w, *h),
        SizeTarget::Flexible(wv, hv) => {
            let w = resolve_dim(*wv, area.width as i32).max(1) as u32;
            let h = resolve_dim(*hv, area.height as i32).max(1) as u32;
            (w, h)
        }
    }
}

pub fn position(pos: &PositionTarget, area: &MonitorGeometry, win_size: (u32, u32)) -> (i32, i32) {
    let (mx, my) = (area.x, area.y);
    let (mw, mh) = (area.width as i32, area.height as i32);
    let (ww, wh) = (win_size.0 as i32, win_size.1 as i32);
    match pos {
        PositionTarget::Absolute(x, y) => (*x, *y),
        PositionTarget::Named(anchor) => match anchor {
            NamedPosition::Center => (mx + (mw - ww) / 2, my + (mh - wh) / 2),
            NamedPosition::TopLeft => (mx, my),
            NamedPosition::TopRight => (mx + mw - ww, my),
            NamedPosition::BottomLeft => (mx, my + mh - wh),
            NamedPosition::BottomRight => (mx + mw - ww, my + mh - wh),
            NamedPosition::Left => (mx, my + (mh - wh) / 2),
            NamedPosition::Right => (mx + mw - ww, my + (mh - wh) / 2),
            NamedPosition::Top => (mx + (mw - ww) / 2, my),
            NamedPosition::Bottom => (mx + (mw - ww) / 2, my + mh - wh),
        },
        PositionTarget::Flexible(xv, yv) => (resolve_dim(*xv, mw) + mx, resolve_dim(*yv, mh) + my),
    }
}

// The geometry `rule` gives a window of size `current` inside `area`, or
// None when the rule sets neither size nor position. A rule that sets only a
// size keeps the window's position; one that sets only a position keeps its
// size.
pub fn geometry(
    rule: &CompiledRule,
    area: &MonitorGeometry,
    current: (i32, i32, u32, u32),
) -> Option<(i32, i32, u32, u32)> {
    if rule.size.is_none() && rule.position.is_none() {
        return None;
    }
    let (w, h) = rule.size.as_ref().map_or((current.2, current.3), |sz| size(sz, area));
    let (x, y) = rule.position.as_ref().map_or((current.0, current.1), |pos| position(pos, area, (w, h)));
    Some((x, y, w, h))
}
