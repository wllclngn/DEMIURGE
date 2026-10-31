// XWayland: Its spawn and X11Wm wiring, and its X11 windows under
// management.
//
// An X11 client's window is managed when it asks to be mapped, once placed
// (policy.rs), as an xdg toplevel is at its initial commit, and from then
// on tiles, stacks, focuses and
// goes fullscreen like one. Dialogs, transients, popups and fixed-size
// windows float, centered on their monitor. Override-redirect windows
// (menus, tooltips) are drawn in front of everything but never managed.
// An unmapped window is unmanaged; mapped again, it is managed again.
//
// The clipboard is bridged both ways: An X11 client's copy is offered to
// Wayland clients, and a Wayland client's copy to X11 clients. An X11
// client may take or read the clipboard only while one of its XWayland's
// windows has the keyboard, as a Wayland client may only while focused.

use std::os::unix::io::OwnedFd;

use smithay::input::SeatHandler;
use smithay::input::dnd::{DndFocus, DndGrabHandler};
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::reexports::x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt as _};
use smithay::reexports::x11rb::rust_connection::RustConnection;
use smithay::utils::{Logical, Rectangle};
use smithay::wayland::selection::SelectionTarget;
use smithay::wayland::selection::data_device::{
    clear_data_device_selection, current_data_device_selection_userdata, request_data_device_client_selection,
    set_data_device_selection,
};
use smithay::wayland::xwayland_shell::XWaylandShellHandler;
use smithay::xwayland::xwm::{Reorder, ResizeEdge, WmWindowProperty, WmWindowType, X11Window, XwmId};
use smithay::xwayland::{X11Surface, X11Wm, XWayland, XWaylandEvent, XwmHandler};

use cherrypie::api::Placement;
use demiurge_core::Layout;

use crate::focus::Focus;
use crate::wm::{Toplevel, Wm};

// The compositor's own connection to XWayland, for what smithay's window
// manager does not read: The _NET_WM_STATE a window sets on itself before
// it maps, which is how a game asks to start fullscreen. smithay takes
// that state only from the requests a mapped window sends, and replaces
// the property with its own once it first changes the window's state.
pub struct X11Reader {
    conn: RustConnection,
    net_wm_state: Atom,
    fullscreen: Atom,
    net_wm_icon: Atom,
}

impl X11Reader {
    pub fn connect(display: u32) -> Result<Self, String> {
        let (conn, _) = RustConnection::connect(Some(&format!(":{}", display))).map_err(|e| e.to_string())?;
        let atom = |name: &str| -> Result<Atom, String> {
            let cookie = conn.intern_atom(false, name.as_bytes()).map_err(|e| e.to_string())?;
            Ok(cookie.reply().map_err(|e| e.to_string())?.atom)
        };
        let net_wm_state = atom("_NET_WM_STATE")?;
        let fullscreen = atom("_NET_WM_STATE_FULLSCREEN")?;
        let net_wm_icon = atom("_NET_WM_ICON")?;
        Ok(Self { conn, net_wm_state, fullscreen, net_wm_icon })
    }

    // `window`'s _NET_WM_ICON: Its sizes one after another, each a width, a
    // height and that many ARGB pixels. The biggest, premultiplied for Cairo.
    pub fn icon(&self, window: X11Window) -> Option<demiurge_core::tag_menu::Icon> {
        let reply = self
            .conn
            .get_property(false, window, self.net_wm_icon, AtomEnum::CARDINAL, 0, 1 << 20)
            .ok()?
            .reply()
            .ok()?;
        let values: Vec<u32> = reply.value32()?.collect();
        let mut best: Option<(usize, usize, usize)> = None;
        let mut i = 0;
        while i + 2 <= values.len() {
            let (w, h) = (values[i] as usize, values[i + 1] as usize);
            if w == 0 || h == 0 || i + 2 + w * h > values.len() {
                break;
            }
            if best.is_none_or(|(bw, _, _)| w > bw) {
                best = Some((w, h, i + 2));
            }
            i += 2 + w * h;
        }
        let (w, h, start) = best?;
        let mut data = Vec::with_capacity(w * h * 4);
        for &argb in &values[start..start + w * h] {
            let a = argb >> 24;
            let premultiplied = |c: u32| ((c * a + 127) / 255) as u8;
            data.extend_from_slice(&[
                premultiplied(argb & 0xff),
                premultiplied((argb >> 8) & 0xff),
                premultiplied((argb >> 16) & 0xff),
                a as u8,
            ]);
        }
        Some(demiurge_core::tag_menu::Icon { width: w as i32, height: h as i32, data })
    }

    // Whether `window` has asked for fullscreen on its own _NET_WM_STATE.
    fn asks_fullscreen(&self, window: X11Window) -> bool {
        self.conn
            .get_property(false, window, self.net_wm_state, AtomEnum::ATOM, 0, 64)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .and_then(|reply| reply.value32().map(|mut atoms| atoms.any(|a| a == self.fullscreen)))
            .unwrap_or(false)
    }
}

/// Spawn XWayland and wire it into `loop_handle`, returning its display
/// number for the children's `$DISPLAY`. On XWaylandEvent::Ready,
/// invokes `set_xwm(state, wm, display)` so the caller can attach the
/// X11Wm to its state.
///
/// Returns Ok(None) when XWayland is disabled or the binary is
/// missing -- the compositor must still come up. `DEMIURGE_WL_NO_XWAYLAND=1`
/// short-circuits before any spawn happens (used by the smoke harness
/// so tests don't depend on /usr/bin/Xwayland).
pub fn spawn_xwayland<S>(
    dh: &DisplayHandle,
    loop_handle: calloop::LoopHandle<'static, S>,
    set_xwm: impl Fn(&mut S, X11Wm, u32) + 'static,
) -> Result<Option<u32>, String>
where
    S: XwmHandler + XWaylandShellHandler + SeatHandler + DndGrabHandler + 'static,
    S::PointerFocus: DndFocus<S>,
    S::TouchFocus: DndFocus<S>,
{
    if std::env::var_os("DEMIURGE_WL_NO_XWAYLAND").is_some() {
        tracing::info!("DEMIURGE_WL_NO_XWAYLAND set; skipping XWayland spawn");
        return Ok(None);
    }

    let (xwayland, xwayland_client) = match XWayland::spawn(
        dh,
        None,
        std::iter::empty::<(String, String)>(),
        true,
        std::process::Stdio::null(),
        std::process::Stdio::null(),
        |_| {},
    ) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "XWayland::spawn failed; continuing without X11 support",
            );
            return Ok(None);
        }
    };

    // The display is taken at spawn, so $DISPLAY is right before XWayland
    // is ready: The startup commands that follow are given it, and an X11
    // client that connects early waits on the socket until XWayland
    // accepts.
    let display = xwayland.display_number();

    let inserter = loop_handle.clone();
    let dh = dh.clone();
    loop_handle
        .insert_source(xwayland, move |event, _, state| match event {
            XWaylandEvent::Ready {
                x11_socket,
                display_number,
            } => {
                tracing::info!(display_number, "XWayland ready");
                match X11Wm::start_wm(
                    inserter.clone(),
                    &dh,
                    x11_socket,
                    xwayland_client.clone(),
                ) {
                    Ok(wm) => set_xwm(state, wm, display_number),
                    Err(e) => tracing::error!(
                        error = %e,
                        "X11Wm::start_wm failed",
                    ),
                }
            }
            XWaylandEvent::Error => {
                tracing::warn!("XWayland reported an error");
            }
        })
        .map_err(|e| format!("insert XWayland source: {}", e))?;

    Ok(Some(display))
}

// Dialogs, transients, popups and fixed-size windows float.
fn floats(window: &X11Surface) -> bool {
    let fixed = matches!((window.min_size(), window.max_size()), (Some(min), Some(max)) if min == max && min.w > 0);
    fixed
        || window.is_transient_for().is_some()
        || window.is_popup()
        || matches!(
            window.window_type(),
            Some(
                WmWindowType::Dialog
                    | WmWindowType::Utility
                    | WmWindowType::Toolbar
                    | WmWindowType::Splash
                    | WmWindowType::Menu
                    | WmWindowType::DropdownMenu
                    | WmWindowType::PopupMenu
                    | WmWindowType::Tooltip
                    | WmWindowType::Notification
            )
        )
}

impl Wm {
    // The clipboard's DisplayHandle, from the running compositor.
    fn display(&self) -> Option<DisplayHandle> {
        self.runtime.as_ref().map(|rt| rt.display.clone())
    }

    // Whether the window at `i` keeps its own geometry: Floating (on its
    // own or by its tag's layout), and neither fullscreen nor zoomed.
    fn places_itself(&self, i: usize) -> bool {
        let c = &self.clients[i];
        (c.floating || self.layouts[c.tag] == Layout::Floating) && !c.fullscreen && c.saved_zoom.is_none()
    }

    // An X11 window placed: Mapped and managed, one keeping its own
    // geometry where cherrypie put it or else centered on its monitor.
    pub(crate) fn admit_x11(&mut self, id: u64, window: X11Surface, fullscreen: bool, placement: Option<&Placement>) {
        let _ = window.set_mapped(true);
        self.manage(id, Toplevel::X11(window.clone()), floats(&window), window.geometry(), placement);
        let placed = placement.is_some_and(|p| p.geometry.is_some());
        if let Some(i) = self.clients.iter().position(|c| c.id == id) {
            if self.places_itself(i) && !placed {
                let mon = self.client_monitor(i);
                let (w, h) = (self.clients[i].w.max(1), self.clients[i].h.max(1));
                let x = mon.x + (mon.width as i32 - w as i32).max(0) / 2;
                let y = mon.y + (mon.height as i32 - h as i32).max(0) / 2;
                self.set_geometry(i, x, y, w, h);
            }
        }
        if fullscreen {
            self.set_fullscreen(id, true);
        }
    }
}

impl XwmHandler for Wm {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        // Single XWayland instance, so the id is informational. The
        // Option must be Some -- X11 events arrive only after
        // XWaylandEvent::Ready has populated self.xwm.
        self.xwm.as_mut().expect("xwm queried before Ready")
    }

    fn new_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.x11_surfaces.push(window);
    }

    // A title changed: The bar shows the focused window's.
    fn property_notify(&mut self, _xwm: XwmId, _window: X11Surface, property: WmWindowProperty) {
        if property == WmWindowProperty::Title {
            self.queue_redraw();
        }
    }

    fn new_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.x11_surfaces.push(window);
    }

    // Mapped once placed (policy.rs): cherrypie is asked where it goes.
    fn map_window_request(&mut self, _xwm: XwmId, window: X11Surface) {
        // A window can ask for fullscreen before it maps, as a game does;
        // read before focusing it makes smithay write the property over.
        let fullscreen =
            window.is_fullscreen() || self.x11_reader.as_ref().is_some_and(|r| r.asks_fullscreen(window.window_id()));
        let id = self.unplace(Toplevel::X11(window), fullscreen);
        self.ask_policy(id);
    }

    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, _window: X11Surface) {
        self.queue_redraw();
    }

    fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
        let toplevel = Toplevel::X11(window.clone());
        if !self.forget_unplaced(&toplevel) {
            self.unmanage(&toplevel);
        }
        if !window.is_override_redirect() {
            let _ = window.set_mapped(false);
        }
        self.queue_redraw();
    }

    fn destroyed_window(&mut self, _xwm: XwmId, window: X11Surface) {
        let toplevel = Toplevel::X11(window.clone());
        if !self.forget_unplaced(&toplevel) {
            self.unmanage(&toplevel);
        }
        self.x11_surfaces.retain(|w| w != &window);
        self.queue_redraw();
    }

    // A window that keeps its own geometry gets what it asks for; a tiled,
    // fullscreen or zoomed one is told again where it is, so it cannot
    // move itself out of its place.
    #[allow(clippy::too_many_arguments)]
    fn configure_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        x: Option<i32>,
        y: Option<i32>,
        w: Option<u32>,
        h: Option<u32>,
        _reorder: Option<Reorder>,
    ) {
        let geo = window.geometry();
        let asked = Rectangle::<i32, Logical>::new(
            (x.unwrap_or(geo.loc.x), y.unwrap_or(geo.loc.y)).into(),
            (w.map_or(geo.size.w, |v| v as i32), h.map_or(geo.size.h, |v| v as i32)).into(),
        );
        match self.clients.iter().position(|c| c.toplevel.x11() == Some(&window)) {
            Some(i) if self.places_itself(i) => {
                self.set_geometry(i, asked.loc.x, asked.loc.y, asked.size.w.max(1) as u32, asked.size.h.max(1) as u32);
            }
            Some(i) => {
                let c = &self.clients[i];
                let (cx, cy, cw, ch) = (c.x, c.y, c.w, c.h);
                self.set_geometry(i, cx, cy, cw, ch);
            }
            None => {
                let _ = window.configure(asked);
            }
        }
        self.queue_redraw();
    }

    fn configure_notify(&mut self, _xwm: XwmId, _window: X11Surface, _geometry: Rectangle<i32, Logical>, _above: Option<X11Window>) {}

    // One made before the window is placed is kept until it is.
    fn fullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        let toplevel = Toplevel::X11(window);
        if self.unplaced_fullscreen(&toplevel, true) {
            return;
        }
        if let Some(id) = self.client_of(&toplevel) {
            self.set_fullscreen(id, true);
        }
    }

    fn unfullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        let toplevel = Toplevel::X11(window);
        if self.unplaced_fullscreen(&toplevel, false) {
            return;
        }
        if let Some(id) = self.client_of(&toplevel) {
            self.set_fullscreen(id, false);
        }
    }

    // Maximize is zoom, as for an xdg toplevel.
    fn maximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(id) = self.zoom_target(&Toplevel::X11(window), false) {
            self.zoom_in(id);
        }
    }

    fn unmaximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(id) = self.zoom_target(&Toplevel::X11(window), true) {
            self.zoom_out(id);
        }
    }

    // _NET_WM_MOVERESIZE: An X11 window's own title bar or edge, as a press
    // on a frame starts a drag on the X11 stack.
    fn resize_request(&mut self, _xwm: XwmId, window: X11Surface, _button: u32, resize_edge: ResizeEdge) {
        let (left, top, right, bottom) = match resize_edge {
            ResizeEdge::Top => (false, true, false, false),
            ResizeEdge::Bottom => (false, false, false, true),
            ResizeEdge::Left => (true, false, false, false),
            ResizeEdge::TopLeft => (true, true, false, false),
            ResizeEdge::BottomLeft => (true, false, false, true),
            ResizeEdge::Right => (false, false, true, false),
            ResizeEdge::TopRight => (false, true, true, false),
            ResizeEdge::BottomRight => (false, false, true, true),
        };
        let edges = demiurge_core::drag::Edges { left, top, right, bottom };
        self.client_drag(&Toplevel::X11(window), crate::drag::DragKind::Resize(edges));
    }

    fn move_request(&mut self, _xwm: XwmId, window: X11Surface, _button: u32) {
        self.client_drag(&Toplevel::X11(window), crate::drag::DragKind::Move);
    }

    fn minimize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(id) = self.client_of(&Toplevel::X11(window)) {
            self.minimize(id);
        }
    }

    fn unminimize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(id) = self.client_of(&Toplevel::X11(window)) {
            self.restore(id);
        }
    }

    fn allow_selection_access(&mut self, xwm: XwmId, _selection: SelectionTarget) -> bool {
        matches!(
            self.seat.get_keyboard().and_then(|keyboard| keyboard.current_focus()),
            Some(Focus::X11(window)) if window.xwm_id() == Some(xwm)
        )
    }

    // An X11 client pastes what a Wayland client copied.
    fn send_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_type: String, fd: OwnedFd) {
        if selection == SelectionTarget::Clipboard {
            if let Err(e) = request_data_device_client_selection(&self.seat, mime_type, fd) {
                tracing::warn!("clipboard to XWayland: {}", e);
            }
        }
    }

    // An X11 client copied: Wayland clients are offered it.
    fn new_selection(&mut self, xwm: XwmId, selection: SelectionTarget, mime_types: Vec<String>) {
        if let (SelectionTarget::Clipboard, Some(display)) = (selection, self.display()) {
            set_data_device_selection(&display, &self.seat, mime_types, xwm);
        }
    }

    fn cleared_selection(&mut self, xwm: XwmId, selection: SelectionTarget) {
        if selection != SelectionTarget::Clipboard {
            return;
        }
        let owned = current_data_device_selection_userdata(&self.seat).as_deref() == Some(&xwm);
        if let (true, Some(display)) = (owned, self.display()) {
            clear_data_device_selection(&display, &self.seat);
        }
    }
}
