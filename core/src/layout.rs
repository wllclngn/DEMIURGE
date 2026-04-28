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
