// What the lock and the greeter show: The background, and the SYSTEM panel
// (host, kernel, date, time, uptime) centered above the USER panel, whose
// rows each gives.

use demiurge_core::config::GordianKnot as Cfg;
use demiurge_core::torrentius::{self, PanelColors, PanelRow};
use crate::sysinfo;

pub struct Theme {
    pub bg: String,
    pub fg: String,
    pub accent: String,
    pub font: String,
    pub prompt: String,
}

impl Theme {
    // GordianKnot config doesn't implement Clone (the toml-loaded
    // structs in demiurge-core skip it). Pre-extract the few fields
    // the UI uses into owned strings so its state doesn't need to
    // borrow from `cfg` for its lifetime.
    pub fn new(cfg: &Cfg) -> Theme {
        Theme {
            bg: cfg.bg.clone(),
            fg: cfg.fg.clone(),
            accent: cfg.accent.clone(),
            font: cfg.font.clone(),
            prompt: cfg.prompt.clone(),
        }
    }
}

/// Draw the screen, `w` by `h`, into `cr`, the USER panel showing `user`.
pub fn draw(cr: &cairo::Context, w: u32, h: u32, theme: &Theme, user: &[PanelRow]) -> Result<(), String> {
    let opts = torrentius::make_font_options()?;
    cr.set_font_options(&opts);

    let bg = torrentius::parse_hex(&theme.bg);
    let fg = torrentius::parse_hex(&theme.fg);
    let accent = torrentius::parse_hex(&theme.accent);

    cr.set_source_rgb(bg.0, bg.1, bg.2);
    cr.rectangle(0.0, 0.0, w as f64, h as f64);
    let _ = cr.fill();

    let layout = pangocairo::functions::create_layout(cr);
    let font_desc = pango::FontDescription::from_string(&theme.font);
    layout.set_font_description(Some(&font_desc));
    let pango_ctx = layout.context();
    pangocairo::functions::context_set_font_options(&pango_ctx, Some(&opts));

    let colors = PanelColors {
        bg,
        fg,
        accent,
        border: fg,
    };

    // SYSTEM panel
    let panel_w = 480.0_f64.min((w as f64) * 0.45);
    let sys_rows = [
        PanelRow {
            label: "HOSTNAME",
            value: &sysinfo::hostname(),
        },
        PanelRow {
            label: "KERNEL",
            value: &sysinfo::kernel_release(),
        },
        PanelRow {
            label: "DATE",
            value: &sysinfo::current_date(),
        },
        PanelRow {
            label: "TIME",
            value: &sysinfo::current_time(),
        },
        PanelRow {
            label: "UPTIME",
            value: &sysinfo::uptime_formatted(),
        },
    ];
    let sys_h = torrentius::panel_height_for(&layout, sys_rows.len());
    let center_x = (w as f64) / 2.0 - panel_w / 2.0;
    let sys_y = (h as f64) / 2.0 - sys_h - 40.0;
    torrentius::draw_panel(
        cr,
        &layout,
        center_x,
        sys_y,
        panel_w,
        sys_h,
        "SYSTEM",
        &sys_rows,
        &colors,
    );

    // USER panel
    let user_h = torrentius::panel_height_for(&layout, user.len());
    let user_y = sys_y + sys_h + 30.0;
    torrentius::draw_panel(
        cr,
        &layout,
        center_x,
        user_y,
        panel_w,
        user_h,
        "USER",
        user,
        &colors,
    );
    Ok(())
}
