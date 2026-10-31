// Torrentius (X11): The server-agnostic primitives in
// demiurge_core::torrentius, and the root's capture to a PNG from the X11
// client crate, under the names the window manager's call sites use.

#[allow(unused_imports)]
pub use demiurge_core::torrentius::{
    PanelColors, PanelRow, draw_panel, hex_to_pixel, make_font_options, new_surface,
    panel_height_for, parse_hex,
};
pub use x11_client::capture::capture_root_to_png;
