// Text input, behind quark-wayland's IME: text-input-v3 for the window
// typed into and input-method-v2 for an input method such as fcitx5.
// smithay gives the text input to the surface holding the keyboard and
// relays the input method's preedit and commits to it. The input method's
// candidate popups are drawn under the text cursor of the window they
// serve.

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Rectangle};
use smithay::wayland::input_method::{InputMethodHandler, PopupSurface};

use crate::wm::Wm;

impl InputMethodHandler for Wm {
    fn new_popup(&mut self, surface: PopupSurface) {
        self.ime_popups.push(surface);
        self.queue_redraw();
    }

    fn dismiss_popup(&mut self, surface: PopupSurface) {
        self.ime_popups.retain(|p| p.wl_surface() != surface.wl_surface());
        self.queue_redraw();
    }

    fn popup_repositioned(&mut self, _surface: PopupSurface) {
        self.queue_redraw();
    }

    // The window a popup serves, where its surface is on the screen.
    fn parent_geometry(&self, parent: &WlSurface) -> Rectangle<i32, Logical> {
        self.clients
            .iter()
            .find(|c| c.toplevel.wl_surface().as_ref() == Some(parent))
            .map(|c| Rectangle::new(c.surface_origin(), (c.w as i32, c.h as i32).into()))
            .unwrap_or_default()
    }
}

impl Wm {
    /// Where an input method's popup goes on the screen: Its window's
    /// place, then the text cursor's in it, below the cursor.
    pub(crate) fn ime_popup_place(&self, popup: &PopupSurface) -> Option<Point<i32, Logical>> {
        let parent = popup.get_parent()?;
        Some(parent.location.loc + popup.location() + Point::from((0, popup.text_input_rectangle().size.h)))
    }
}

smithay::delegate_input_method_manager!(Wm);
smithay::delegate_text_input_manager!(Wm);
