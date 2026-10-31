// Layout kind enumeration. The actual arrange function (which calls
// each protocol's window-positioning API -- ConfigureWindow on X11,
// xdg_toplevel.set_geometry / wlr_layer_shell on Wayland) lives in
// each implementation crate; the enum + cycle/parse helpers shared
// here keep the user-facing model identical across protocols.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Floating,
    Tile,
    Monocle,
}

impl Layout {
    pub fn next(self) -> Layout {
        match self {
            Layout::Floating => Layout::Tile,
            Layout::Tile => Layout::Monocle,
            Layout::Monocle => Layout::Floating,
        }
    }

    pub fn from_str(s: &str) -> Layout {
        match s {
            "tile" => Layout::Tile,
            "monocle" => Layout::Monocle,
            _ => Layout::Floating,
        }
    }
}

// What zooming a window put aside: The geometry to return to, and whether
// the zoom floated it out of a tiled layout, in which case zooming out puts
// it back in its tile instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Zoom {
    pub geometry: (i32, i32, u32, u32),
    pub floated: bool,
}

impl Zoom {
    /// Zooming a window at `geometry`: One tiled by its tag's `layout`,
    /// not floating of its own, floats out to zoom.
    pub fn new(geometry: (i32, i32, u32, u32), floating: bool, layout: Layout) -> Self {
        Self { geometry, floated: !floating && layout != Layout::Floating }
    }
}

/// Where `n` tiled windows go in the work area (x, y, w, h): The master
/// at the left, `master_ratio` of the width, then the stack top to bottom,
/// the last slice taking what the division leaves. One window takes it
/// all.
pub fn tile(area: (i32, i32, u32, u32), n: usize, master_ratio: f64) -> Vec<(i32, i32, u32, u32)> {
    let (x, y, w, h) = area;
    if n <= 1 {
        return if n == 1 { vec![area] } else { Vec::new() };
    }
    let master_w = (w as f64 * master_ratio) as u32;
    let stack_w = w - master_w;
    let stack_count = n - 1;
    let slice_h = h / stack_count as u32;
    let mut out = Vec::with_capacity(n);
    out.push((x, y, master_w, h));
    for j in 0..stack_count {
        let sh = if j == stack_count - 1 { h - j as u32 * slice_h } else { slice_h };
        out.push((x + master_w as i32, y + (j as u32 * slice_h) as i32, stack_w, sh));
    }
    out
}
