// Window geometry under a drag, shared by both stacks: Which edges a resize
// moves and where they end up.

const MIN_SIZE: u32 = 50;

// The window edges a resize drags; the opposite edges stay put.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edges {
    pub left: bool,
    pub top: bool,
    pub right: bool,
    pub bottom: bool,
}

impl Edges {
    pub const BOTTOM_RIGHT: Edges = Edges { left: false, top: false, right: true, bottom: true };

    // _NET_WM_MOVERESIZE directions 0-7, clockwise from the top-left corner.
    pub fn from_moveresize(direction: u32) -> Option<Edges> {
        let (left, top, right, bottom) = match direction {
            0 => (true, true, false, false),
            1 => (false, true, false, false),
            2 => (false, true, true, false),
            3 => (false, false, true, false),
            4 => (false, false, true, true),
            5 => (false, false, false, true),
            6 => (true, false, false, true),
            7 => (true, false, false, false),
            _ => return None,
        };
        Some(Edges { left, top, right, bottom })
    }
}

// Geometry after dragging `edges` by (dx, dy) from `orig`. Each dimension
// stays at least MIN_SIZE, the dragged left or top edge moves while the
// opposite edge holds, and the top edge never rises above `min_y` (the bar).
pub fn resize_geometry(
    edges: Edges,
    orig: (i32, i32, u32, u32),
    dx: i32,
    dy: i32,
    min_y: i32,
) -> (i32, i32, u32, u32) {
    let (ox, oy, ow, oh) = (orig.0, orig.1, orig.2 as i32, orig.3 as i32);
    let min = MIN_SIZE as i32;
    let (mut x, mut w) = (ox, ow);
    if edges.right {
        w = (ow + dx).max(min);
    } else if edges.left {
        w = (ow - dx).max(min);
        x = ox + ow - w;
    }
    let (mut y, mut h) = (oy, oh);
    if edges.bottom {
        h = (oh + dy).max(min);
    } else if edges.top {
        let bottom = oy + oh;
        y = (oy + dy).max(min_y).min(bottom - min);
        h = bottom - y;
    }
    (x, y, w as u32, h as u32)
}
