// Moving and resizing with the pointer, as the X11 stack does: Super and the
// left button move the window under the pointer, Super and the right button
// resize it by its bottom-right corner, and a client's own title bar or
// edge asks the same through xdg-shell's move and resize, or an X11
// window's _NET_WM_MOVERESIZE. While a drag lasts the pointer is the
// drag's, as X11's pointer grab makes it. A tiled window dragged floats out
// of its tile, and where it is left goes to cherrypie's window memory.

use demiurge_core::Layout;
use demiurge_core::drag::{Edges, resize_geometry};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::utils::{Logical, Point};

use crate::wm::{Toplevel, Wm};

pub const BTN_LEFT: u32 = 0x110;
pub const BTN_RIGHT: u32 = 0x111;

pub enum DragKind {
    Move,
    Resize(Edges),
}

pub struct Drag {
    kind: DragKind,
    id: u64,
    start: Point<f64, Logical>,
    orig: (i32, i32, u32, u32),
    // A client's own drag: The press went to it, so the release does too.
    pub(crate) forwarded: bool,
}

impl Wm {
    // The window whose rectangle holds `at`, front to back, as the X11 stack
    // takes the frame under the pointer.
    fn window_at(&self, at: Point<f64, Logical>) -> Option<u64> {
        let (x, y) = (at.x.floor() as i32, at.y.floor() as i32);
        self.stacking()
            .into_iter()
            .find(|c| x >= c.x && y >= c.y && x < c.x + c.w as i32 && y < c.y + c.h as i32)
            .map(|c| c.id)
    }

    /// A press that starts a drag: Super with the left or right button over
    /// a window. True when it did, and the press is the drag's.
    pub(crate) fn super_drag(&mut self, button: u32) -> bool {
        let held = self.seat.get_keyboard().is_some_and(|k| k.modifier_state().logo);
        if !held || self.locked() || (button != BTN_LEFT && button != BTN_RIGHT) {
            return false;
        }
        let Some(id) = self.window_at(self.pointer_location) else {
            return false;
        };
        let kind = if button == BTN_LEFT { DragKind::Move } else { DragKind::Resize(Edges::BOTTOM_RIGHT) };
        self.start_drag(id, kind, false)
    }

    /// A client's own move or resize of `toplevel`, honored while a button
    /// is held.
    pub(crate) fn client_drag(&mut self, toplevel: &Toplevel, kind: DragKind) {
        let held = self.seat.get_pointer().is_some_and(|p| p.is_grabbed());
        if !held || self.drag.is_some() || self.locked() {
            return;
        }
        if let Some(id) = self.clients.iter().find(|c| &c.toplevel == toplevel).map(|c| c.id) {
            self.start_drag(id, kind, true);
        }
    }

    /// A drag of window `id` from where the pointer is; none for a
    /// fullscreen window.
    pub(crate) fn start_drag(&mut self, id: u64, kind: DragKind, forwarded: bool) -> bool {
        let Some(i) = self.clients.iter().position(|c| c.id == id) else {
            return false;
        };
        if self.clients[i].fullscreen {
            return false;
        }
        self.focus_client(id);
        let c = &self.clients[i];
        if !c.floating && self.layouts[c.tag] != Layout::Floating {
            self.clients[i].floating = true;
            self.arrange();
        }
        let c = &self.clients[i];
        self.drag = Some(Drag { kind, id, start: self.pointer_location, orig: (c.x, c.y, c.w, c.h), forwarded });
        true
    }

    /// The pointer at `at` during a drag: The window moved or resized by as
    /// much as the pointer moved, its top kept below the bar.
    pub(crate) fn drag_to(&mut self, at: Point<f64, Logical>) {
        let Some(drag) = &self.drag else {
            return;
        };
        let Some(i) = self.clients.iter().position(|c| c.id == drag.id) else {
            self.drag = None;
            return;
        };
        let (dx, dy) = ((at.x - drag.start.x) as i32, (at.y - drag.start.y) as i32);
        let bar = self.bar_height as i32;
        let (x, y, w, h) = match drag.kind {
            DragKind::Move => (drag.orig.0 + dx, (drag.orig.1 + dy).max(bar), drag.orig.2, drag.orig.3),
            DragKind::Resize(edges) => resize_geometry(edges, drag.orig, dx, dy, bar),
        };
        if (x, y, w, h) != (self.clients[i].x, self.clients[i].y, self.clients[i].w, self.clients[i].h) {
            self.set_geometry(i, x, y, w, h);
        }
    }

    /// A button let go during a drag: A window that moved or resized leaves
    /// its zoom, and where it was left goes to cherrypie's window memory.
    pub(crate) fn end_drag(&mut self) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        let Some(i) = self.clients.iter().position(|c| c.id == drag.id) else {
            return;
        };
        let c = &self.clients[i];
        let now = (c.x, c.y, c.w, c.h);
        if c.fullscreen || now == drag.orig {
            return;
        }
        if self.clients[i].saved_zoom.take().is_some() {
            self.set_geometry_state(i, now.0, now.1, now.2, now.3, xdg_toplevel::State::Maximized, false);
        }
        let moved = cherrypie::api::Moved {
            monitor: self.client_monitor(i).name.clone(),
            rect: cherrypie::api::Rect { x: now.0, y: now.1, w: now.2, h: now.3 },
        };
        self.policy.send("PUT", &format!("/windows/{}/geometry", drag.id as u32), moved);
    }
}
