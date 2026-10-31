// Title bars under demiurge-wl: The X11 frames' bar (demiurge_core::titlebar)
// for every xdg toplevel that asks xdg-decoration for server-side
// decorations, with `[general] title_bars` on, on floating windows only. A
// window's place and size are its outer rectangle, bar included, as an X11
// frame's are; the client is told the size below the bar and drawn there.
// A press on the bar focuses the window, and its lights close, minimize or
// zoom it as on X11, or the bar starts a move.

use demiurge_core::Layout;
use demiurge_core::titlebar::{self, Light};
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
use smithay::wayland::shell::xdg::ToplevelSurface;
use smithay::wayland::shell::xdg::decoration::XdgDecorationHandler;

use crate::drag::{BTN_LEFT, DragKind};
use crate::wm::{Toplevel, Wm};

impl XdgDecorationHandler for Wm {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        self.decorate(toplevel, Mode::ServerSide);
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: Mode) {
        self.decorate(toplevel, mode);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        self.decorate(toplevel, Mode::ServerSide);
    }
}

smithay::delegate_xdg_decoration!(Wm);

impl Wm {
    // The mode a toplevel asked for, server-side only with title bars on.
    // A managed window takes its bar or loses it where it is; one not yet
    // configured hears the mode with its first configure.
    fn decorate(&mut self, toplevel: ToplevelSurface, asked: Mode) {
        let mode = if self.title_bars && asked == Mode::ServerSide { Mode::ServerSide } else { Mode::ClientSide };
        toplevel.with_pending_state(|state| state.decoration_mode = Some(mode));
        let managed = self.clients.iter().position(|c| c.toplevel == Toplevel::Xdg(toplevel.clone()));
        match managed {
            Some(i) => {
                let c = &self.clients[i];
                let (x, y, w, h) = (c.x, c.y, c.w, c.h);
                self.set_geometry(i, x, y, w, h);
            }
            None if toplevel.is_initial_configure_sent() => {
                toplevel.send_configure();
            }
            None => {}
        }
        self.queue_redraw();
    }

    /// Whether window `i` shows a title bar: Title bars on, floating and not
    /// fullscreen, and an xdg toplevel in server-side mode.
    pub(crate) fn titled(&self, i: usize) -> bool {
        let c = &self.clients[i];
        self.title_bars
            && !c.fullscreen
            && (c.floating || self.layouts[c.tag] == Layout::Floating)
            && c.toplevel.xdg().is_some_and(|t| t.with_pending_state(|s| s.decoration_mode) == Some(Mode::ServerSide))
    }

    /// A press on a window's title bar: The window focused, then a left
    /// press on a light closes, minimizes or zooms it, and anywhere else
    /// starts a move. True when the press was the bar's.
    pub(crate) fn title_press(&mut self, button: u32) -> bool {
        let (x, y) = (self.pointer_location.x.floor() as i32, self.pointer_location.y.floor() as i32);
        let Some(c) = self
            .stacking()
            .into_iter()
            .find(|c| x >= c.x && x < c.x + c.w as i32 && y >= c.y && y < c.y + c.h as i32)
        else {
            return false;
        };
        if y >= c.y + c.decor as i32 {
            return false;
        }
        let (id, zoomed) = (c.id, c.saved_zoom.is_some());
        let (bx, by) = ((x - c.x) as f64, (y - c.y) as f64);
        self.focus_client(id);
        if button != BTN_LEFT {
            return true;
        }
        match titlebar::light_at(bx, by) {
            Some(Light::Close) => {
                if let Some(c) = self.clients.iter().find(|c| c.id == id) {
                    c.toplevel.close();
                }
            }
            Some(Light::Minimize) => self.minimize(id),
            Some(Light::Zoom) if zoomed => self.zoom_out(id),
            Some(Light::Zoom) => self.zoom_in(id),
            None => {
                self.start_drag(id, DragKind::Move, false);
            }
        }
        true
    }
}
