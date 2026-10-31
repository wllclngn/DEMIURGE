// xdg-activation-v1: A client holding the keyboard hands a token to
// another, which presents it to have one of its windows focused, as
// quark-wayland's SetForegroundWindow does. A token is good only if the
// client asking for it held the keyboard as it asked, and only once; any
// other token focuses nothing. A minimized window comes back.

use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::xdg_activation::{
    XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
};

use crate::wm::Wm;

impl Wm {
    // The client whose surface holds the keyboard.
    fn keyboard_client(&self) -> Option<ClientId> {
        let dh = &self.runtime.as_ref()?.display;
        let focus = self.seat.get_keyboard()?.current_focus()?;
        let surface = focus.wl_surface()?;
        dh.get_client(surface.id()).ok().map(|c| c.id())
    }
}

impl XdgActivationHandler for Wm {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.runtime.as_mut().expect("xdg-activation is served only with a screen").activation
    }

    fn token_created(&mut self, _: XdgActivationToken, data: XdgActivationTokenData) -> bool {
        data.client_id.is_some() && data.client_id == self.keyboard_client()
    }

    fn request_activation(&mut self, token: XdgActivationToken, _: XdgActivationTokenData, surface: WlSurface) {
        self.activation_state().remove_token(&token);
        if self.locked() {
            return;
        }
        let Some((id, tag)) = self
            .clients
            .iter()
            .find(|c| c.toplevel.wl_surface().as_ref() == Some(&surface))
            .map(|c| (c.id, c.tag))
        else {
            return;
        };
        if !self.active_tags.contains(&tag) {
            self.view_tag(tag);
        }
        self.restore(id);
        self.focus_client(id);
        self.queue_redraw();
    }
}

smithay::delegate_xdg_activation!(Wm);
