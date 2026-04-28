// Wm: the central state object. Mirrors x11/src/wm.rs's Wm struct
// in role -- it owns the protocol surface, holds the client list,
// drives focus/visibility, and is the receiver for delegated
// protocol events. The contents differ because the protocols differ:
// where the X11 Wm holds a RustConnection + Atoms + Bar + monitors,
// the Wayland Wm holds CompositorState + XdgShellState + ShmState +
// SeatState + Seat.
//
// In this v0 the Wm is structurally minimal: it implements every
// handler smithay needs to dispatch protocol events for compositor /
// xdg-shell / shm / seat / data-device, and it carries the seat
// handle so the input loop can drive keyboard focus. Tags, MRU, the
// bar, layouts, and the rest of the per-monitor model land in
// follow-on slices that mirror the x11 versions.

use std::os::unix::io::OwnedFd;

use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::Client;
use smithay::reexports::wayland_server::protocol::wl_seat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::Serial;
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    CompositorClientState, CompositorHandler, CompositorState,
};
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::selection::data_device::{
    ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::{
    delegate_compositor, delegate_data_device, delegate_seat, delegate_shm, delegate_xdg_shell,
};

use crate::client::ClientState;

pub struct Wm {
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    pub seat_state: SeatState<Self>,
    pub data_device_state: DataDeviceState,
    pub seat: Seat<Self>,
}

impl CompositorHandler for Wm {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client
            .get_data::<ClientState>()
            .expect("ClientState attached at insert_client")
            .compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        // Tells smithay to advance the buffer state machine on the
        // committed surface tree (acks any pending wl_buffer attaches,
        // etc.). Without this, the buffer associated with a commit is
        // never made available to the renderer.
        smithay::backend::renderer::utils::on_commit_buffer_handler::<Self>(surface);
    }
}

impl XdgShellHandler for Wm {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        // Mark the toplevel as activated so its initial render uses
        // active-window styling. Floating placement at (0, 0) for v0;
        // tile/monocle layouts come in the layout.rs slice.
        surface.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Activated);
        });
        surface.send_configure();
        tracing::info!("new toplevel mapped");
    }

    fn new_popup(&mut self, _surface: PopupSurface, _positioner: PositionerState) {
        // Popups land here once any client uses them (tooltips, menus).
        // No-op for v0; we'll wire popup tracking in a later slice.
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {}

    fn reposition_request(
        &mut self,
        _surface: PopupSurface,
        _positioner: PositionerState,
        _token: u32,
    ) {
    }
}

impl SelectionHandler for Wm {
    type SelectionUserData = ();
}

impl DataDeviceHandler for Wm {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl ClientDndGrabHandler for Wm {}
impl ServerDndGrabHandler for Wm {
    fn send(&mut self, _mime_type: String, _fd: OwnedFd, _seat: Seat<Self>) {}
}

impl ShmHandler for Wm {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl SeatHandler for Wm {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, _seat: &Seat<Self>, _focused: Option<&WlSurface>) {
        // Where x11/src/wm.rs::focus_window does ewmh::set_active_window
        // + MRU promotion, the Wayland equivalent will go here. v0 stub.
    }

    fn cursor_image(
        &mut self,
        _seat: &Seat<Self>,
        _image: smithay::input::pointer::CursorImageStatus,
    ) {
        // Cursor theme handling lands here once we wire wp_cursor_shape.
    }
}

impl BufferHandler for Wm {
    fn buffer_destroyed(
        &mut self,
        _buffer: &smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer,
    ) {
    }
}

// Macros that wire smithay's auto-generated dispatch for each protocol
// to the trait impls above. One per protocol we serve.
delegate_compositor!(Wm);
delegate_xdg_shell!(Wm);
delegate_shm!(Wm);
delegate_seat!(Wm);
delegate_data_device!(Wm);
