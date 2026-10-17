// Wm: the central state object. Mirrors x11/src/wm.rs's Wm in role --
// it owns the protocol surface, holds the client list, drives focus
// and visibility, and carries the per-monitor tiling model that the
// X11 implementation already runs against. The contents differ
// because the protocols differ: where the X11 Wm holds RustConnection
// + Atoms + Bar, the Wayland Wm holds CompositorState + XdgShellState
// + ShmState + SeatState + Seat. Tiling/tags/MRU shapes are imported
// from demiurge-core so both implementations agree on the model.

use std::os::unix::io::OwnedFd;

use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::output::Output;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::Client;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_seat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{SERIAL_COUNTER, Serial};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    CompositorClientState, CompositorHandler, CompositorState,
};
use smithay::wayland::output::{OutputHandler, OutputManagerState};
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::selection::data_device::{
    ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
};
use smithay::wayland::session_lock::{
    LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker,
};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::wayland::xwayland_shell::{XWaylandShellHandler, XWaylandShellState};
use smithay::xwayland::{X11Surface, X11Wm, XwmHandler};
use smithay::xwayland::xwm::{Reorder, ResizeEdge, XwmId};
use smithay::utils::{Logical, Rectangle};
use smithay::{
    delegate_compositor, delegate_data_device, delegate_output, delegate_seat,
    delegate_session_lock, delegate_shm, delegate_xdg_shell, delegate_xwayland_shell,
};

use demiurge_core::{Action, Layout, Monitor, Monitors, MruRing};

use crate::client::ClientState;
use crate::keys::Binding;

// Per-toplevel managed state. Mirrors x11/src/wm.rs's Client; the
// fields are protocol-agnostic (position/size/tag/flags) so the tile
// arithmetic in arrange() reads the same as on X11.
//
// `id` is a Wm-minted u64. We use it instead of the wl_surface's
// protocol_id because protocol IDs collide across clients (each client
// has its own object-ID space). A monotonic Wm-side counter gives us
// a globally-unique stable key for the MruRing.
pub struct ManagedClient {
    pub id: u64,
    pub toplevel: ToplevelSurface,
    pub tag: usize,
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub fullscreen: bool,
    pub floating: bool,
    pub saved_floating: Option<(i32, i32, u32, u32)>,
    // Geometry stashed when entering fullscreen, restored on exit.
    // Separate from saved_floating: a client can be tiled (no
    // saved_floating) and still need its tile geometry remembered
    // across a fullscreen round-trip.
    pub saved_fullscreen: Option<(i32, i32, u32, u32)>,
}

pub struct Wm {
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    pub seat_state: SeatState<Self>,
    pub data_device_state: DataDeviceState,
    pub seat: Seat<Self>,
    pub output_manager_state: OutputManagerState,
    pub session_lock_state: SessionLockManagerState,
    pub output: Output,

    // XWayland integration. xwayland_shell_state advertises the
    // XWaylandShellV1 global. xwm holds the X11 window manager once
    // XWayland is up and ready (None before, set in main.rs's
    // XWaylandEvent::Ready handler). x11_surfaces tracks X11 windows
    // for the render path -- once a surface is associated via
    // xwayland_shell, its WlSurface is rendered alongside the
    // ManagedClients.
    //
    // X11 surfaces are not tile-managed yet; they render at their
    // self-chosen X11 geometry. Bringing them under arrange() is a
    // follow-on slice.
    pub xwayland_shell_state: XWaylandShellState,
    pub xwm: Option<X11Wm>,
    pub x11_surfaces: Vec<X11Surface>,

    // Tiling state ported from x11/src/wm.rs. monitors holds the
    // current output geometry (single-monitor while the dev backend
    // is winit; multi-output lands with the DRM backend slice).
    // active_tags[i] is the visible tag on monitors[i]; layouts[t]
    // is the layout for tag t. focused_monitor indexes into monitors
    // and active_tags in lockstep with the X11 Wm's invariant.
    pub clients: Vec<ManagedClient>,
    pub monitors: Monitors,
    pub active_tags: Vec<usize>,
    pub focused_monitor: usize,
    pub num_tags: usize,
    pub layouts: Vec<Layout>,
    pub master_ratio: f64,
    pub bar_height: u32,
    pub mru: MruRing<u64>,
    next_client_id: u64,

    // Keybind dispatch. bindings is compiled from cfg.keybinds at
    // construction (keysym + modifier predicate -> Action); the input
    // filter in each backend matches incoming keys against it and calls
    // handle_action on a hit. focused is the id of the client holding
    // keyboard focus (tracked through focus_changed) so actions that
    // operate on "the focused window" (close, fullscreen, move_to_tag)
    // have a target. running drives the backend's main-loop exit: the
    // Quit action clears it and the loop breaks.
    pub bindings: Vec<Binding>,
    pub focused: Option<u64>,
    pub running: bool,

    // Session-lock state. session-lock-v1 is a guarantee:
    // while locked, the compositor MUST hide the desktop and only
    // show ext_session_lock_surface_v1 surfaces. The render path
    // checks `lock_active` and switches to the locked element list
    // when set.
    //
    // pending_lock holds the SessionLocker handle from the moment
    // the client requested a lock until the next "cleared frame"
    // is drawn. At that point we call SessionLocker::lock to send
    // the `locked` event, transition to `lock_active = true`, and
    // drop the handle. If the client disconnects before that hop,
    // dropping pending_lock sends `finished` (via SessionLocker's
    // Drop), letting the client know lock acquisition failed.
    pub pending_lock: Option<SessionLocker>,
    pub lock_active: bool,
    pub lock_surfaces: Vec<(LockSurface, WlOutput)>,
}

impl Wm {
    /// Build a Wm with tiling state initialized from config. Monitors
    /// starts with a single placeholder entry; main.rs replaces it
    /// with the real winit window size on the first resize event,
    /// then arrange() picks up.
    pub fn new(
        compositor_state: CompositorState,
        xdg_shell_state: XdgShellState,
        shm_state: ShmState,
        seat_state: SeatState<Self>,
        data_device_state: DataDeviceState,
        seat: Seat<Self>,
        output_manager_state: OutputManagerState,
        session_lock_state: SessionLockManagerState,
        output: Output,
        xwayland_shell_state: XWaylandShellState,
        cfg: &demiurge_core::config::Config,
    ) -> Self {
        let num_tags = cfg.general.tags.len().max(1);
        let default_layout = Layout::from_str(&cfg.general.default_layout);
        let layouts = vec![default_layout; num_tags];

        // Placeholder monitor; main.rs's first WinitEvent::Resized will
        // overwrite it with the real window dimensions before any
        // toplevel can map (clients connect after winit is up).
        let monitors = Monitors::single(Monitor {
            name: "winit".to_string(),
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        });

        Self {
            compositor_state,
            xdg_shell_state,
            shm_state,
            seat_state,
            data_device_state,
            seat,
            output_manager_state,
            session_lock_state,
            output,
            clients: Vec::new(),
            monitors,
            active_tags: vec![0],
            focused_monitor: 0,
            num_tags,
            layouts,
            master_ratio: cfg.general.master_ratio,
            bar_height: cfg.bar.height,
            mru: MruRing::new(),
            next_client_id: 1,
            bindings: crate::keys::compile(&cfg.keybinds),
            focused: None,
            running: true,
            pending_lock: None,
            lock_active: false,
            lock_surfaces: Vec::new(),
            xwayland_shell_state,
            xwm: None,
            x11_surfaces: Vec::new(),
        }
    }

    fn mint_id(&mut self) -> u64 {
        let id = self.next_client_id;
        self.next_client_id = self.next_client_id.saturating_add(1);
        id
    }

    /// Per-tag occupancy bitmap. Index t is true if any client lives
    /// on tag t. Bar uses this to color empty vs occupied non-active
    /// tag indicators.
    pub fn occupied_tags(&self) -> Vec<bool> {
        let mut occ = vec![false; self.num_tags];
        for c in &self.clients {
            if let Some(slot) = occ.get_mut(c.tag) {
                *slot = true;
            }
        }
        occ
    }

    /// Replace monitor 0's geometry. Called from main.rs when winit
    /// reports a resize. Multi-monitor uses index-aware updates; the
    /// single-monitor winit dev backend always touches index 0.
    pub fn resize_primary_monitor(&mut self, width: u32, height: u32) {
        let mut new_monitor = self.monitors.first().clone();
        new_monitor.width = width;
        new_monitor.height = height;
        self.monitors = Monitors::single(new_monitor);
        self.arrange();
    }

    // Per-monitor tile/monocle/floating arrange. Ported from
    // x11/src/layout.rs; reads identically because Layout, master_ratio
    // and the work-area math are server-agnostic. The protocol-side
    // dispatch differs:
    //   - X11 issues ConfigureWindow on each client window
    //   - Wayland sets ToplevelState.size and sends an xdg_surface
    //     configure; the client commits a buffer at that size, and the
    //     compositor's render pass draws it at (client.x, client.y).
    pub fn arrange(&mut self) {
        let bar_h = self.bar_height;

        // Snapshot per-monitor (work-area, layout) plans before
        // borrowing self.clients mutably. Same shape as x11's
        // Vec<(tag, x, y, w, h, layout)> snapshot.
        let plans: Vec<(usize, i32, i32, u32, u32, Layout)> = self
            .monitors
            .iter()
            .enumerate()
            .map(|(mi, mon)| {
                let tag = self.active_tags[mi];
                (
                    tag,
                    mon.x,
                    mon.y + bar_h as i32,
                    mon.width,
                    mon.height.saturating_sub(bar_h),
                    self.layouts[tag],
                )
            })
            .collect();

        for (tag, work_x, work_y, work_w, work_h, layout) in plans {
            let indices: Vec<usize> = self
                .clients
                .iter()
                .enumerate()
                .filter(|(_, c)| c.tag == tag && !c.fullscreen && !c.floating)
                .map(|(i, _)| i)
                .collect();

            match layout {
                Layout::Floating => {
                    for &i in &indices {
                        if let Some((x, y, w, h)) = self.clients[i].saved_floating.take() {
                            self.set_geometry(i, x, y, w, h);
                        }
                    }
                }
                Layout::Tile => {
                    if indices.is_empty() {
                        continue;
                    }
                    for &i in &indices {
                        if self.clients[i].saved_floating.is_none() {
                            let c = &self.clients[i];
                            self.clients[i].saved_floating = Some((c.x, c.y, c.w, c.h));
                        }
                    }

                    if indices.len() == 1 {
                        self.set_geometry(indices[0], work_x, work_y, work_w, work_h);
                        continue;
                    }

                    let master_w = (work_w as f64 * self.master_ratio) as u32;
                    let stack_w = work_w - master_w;
                    let stack_count = indices.len() - 1;
                    let slice_h = work_h / stack_count as u32;

                    self.set_geometry(indices[0], work_x, work_y, master_w, work_h);

                    for (j, &i) in indices[1..].iter().enumerate() {
                        let sx = work_x + master_w as i32;
                        let sy = work_y + (j as u32 * slice_h) as i32;
                        let sh = if j == stack_count - 1 {
                            work_h - j as u32 * slice_h
                        } else {
                            slice_h
                        };
                        self.set_geometry(i, sx, sy, stack_w, sh);
                    }
                }
                Layout::Monocle => {
                    for &i in &indices {
                        if self.clients[i].saved_floating.is_none() {
                            let c = &self.clients[i];
                            self.clients[i].saved_floating = Some((c.x, c.y, c.w, c.h));
                        }
                    }
                    for &i in &indices {
                        self.set_geometry(i, work_x, work_y, work_w, work_h);
                    }
                }
            }
        }
    }

    // Update Client geometry + send the xdg_surface configure with the
    // chosen size. The client commits a buffer of that size on the next
    // commit; the render loop reads (x, y) at composite time.
    fn set_geometry(&mut self, i: usize, x: i32, y: i32, w: u32, h: u32) {
        let c = &mut self.clients[i];
        c.x = x;
        c.y = y;
        c.w = w;
        c.h = h;
        let toplevel = c.toplevel.clone();
        toplevel.with_pending_state(|state| {
            state.size = Some((w as i32, h as i32).into());
            state.states.set(xdg_toplevel::State::Activated);
        });
        toplevel.send_configure();
    }

    // Same as set_geometry but also sets/clears the Fullscreen toplevel
    // state so the client drops/restores its decorations. Used only by
    // toggle_fullscreen; arrange() uses set_geometry (never fullscreen).
    fn set_geometry_fullscreen(&mut self, i: usize, x: i32, y: i32, w: u32, h: u32, fs: bool) {
        let c = &mut self.clients[i];
        c.x = x;
        c.y = y;
        c.w = w;
        c.h = h;
        let toplevel = c.toplevel.clone();
        toplevel.with_pending_state(|state| {
            state.size = Some((w as i32, h as i32).into());
            state.states.set(xdg_toplevel::State::Activated);
            if fs {
                state.states.set(xdg_toplevel::State::Fullscreen);
            } else {
                state.states.unset(xdg_toplevel::State::Fullscreen);
            }
        });
        toplevel.send_configure();
    }

    /// True if the client's tag is currently visible on some monitor.
    /// The render paths use this to skip clients on hidden tags --
    /// without it, switching tags wouldn't hide anything (every client
    /// stays in the element list regardless of tag).
    pub fn is_visible(&self, c: &ManagedClient) -> bool {
        self.active_tags.iter().any(|&t| t == c.tag)
    }

    /// True if the focused monitor's active tag holds a fullscreen
    /// client. The bar is suppressed while this holds so a fullscreen
    /// window isn't clipped by the panel.
    pub fn active_tag_fullscreen(&self) -> bool {
        let tag = self.active_tags[self.focused_monitor];
        self.clients.iter().any(|c| c.tag == tag && c.fullscreen)
    }

    // Set keyboard focus to a surface (or clear it). Threads self
    // through the keyboard handle so smithay drives focus_changed,
    // which updates self.focused + the MRU ring.
    fn set_keyboard_focus(&mut self, surface: Option<WlSurface>) {
        if let Some(kbd) = self.seat.get_keyboard() {
            let serial = SERIAL_COUNTER.next_serial();
            kbd.set_focus(self, surface, serial);
        }
    }

    /// Give keyboard focus to the client with the given id.
    pub fn focus_client(&mut self, id: u64) {
        if let Some(surf) = self
            .clients
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.toplevel.wl_surface().clone())
        {
            self.set_keyboard_focus(Some(surf));
        }
    }

    // Focus the most-recently-used client on the focused monitor's
    // active tag; clear focus if that tag is empty. Called after any
    // operation that changes which clients are visible (view_tag,
    // move_to_tag, destroy).
    fn refocus_active_tag(&mut self) {
        let tag = self.active_tags[self.focused_monitor];
        let target = self
            .mru
            .as_slice()
            .iter()
            .copied()
            .find(|id| self.clients.iter().any(|c| c.id == *id && c.tag == tag));
        match target {
            Some(id) => self.focus_client(id),
            None => {
                // smithay's set_focus(None) sends `leave` but does NOT
                // invoke focus_changed (that fires only for Some), so
                // clear the tracked focus id explicitly here.
                self.set_keyboard_focus(None);
                self.focused = None;
            }
        }
    }

    /// Switch the focused monitor to `tag`. If another monitor already
    /// shows that tag, the two views swap (XMonad-style) -- a tag is
    /// visible on at most one monitor. Single-monitor (winit) reduces
    /// to a plain set.
    pub fn view_tag(&mut self, tag: usize) {
        if tag >= self.num_tags {
            return;
        }
        let m = self.focused_monitor;
        if self.active_tags[m] == tag {
            return;
        }
        if let Some(other) = self.active_tags.iter().position(|&t| t == tag) {
            self.active_tags[other] = self.active_tags[m];
        }
        self.active_tags[m] = tag;
        self.arrange();
        self.refocus_active_tag();
    }

    pub fn view_prev_tag(&mut self) {
        let cur = self.active_tags[self.focused_monitor];
        self.view_tag((cur + self.num_tags - 1) % self.num_tags);
    }

    pub fn view_next_tag(&mut self) {
        let cur = self.active_tags[self.focused_monitor];
        self.view_tag((cur + 1) % self.num_tags);
    }

    /// Move the focused client to `tag`. The window becomes hidden if
    /// that tag isn't currently visible (visibility follows the tag).
    pub fn move_to_tag(&mut self, tag: usize) {
        if tag >= self.num_tags {
            return;
        }
        let id = match self.focused {
            Some(i) => i,
            None => return,
        };
        match self.clients.iter_mut().find(|c| c.id == id) {
            Some(c) if c.tag != tag => {
                c.tag = tag;
                // Reset per-tag tile bookkeeping + fullscreen on the
                // destination so arrange() re-lays it out fresh.
                c.saved_floating = None;
                c.saved_fullscreen = None;
                c.fullscreen = false;
            }
            _ => return,
        }
        self.arrange();
        self.refocus_active_tag();
    }

    /// Toggle the focused client between tiled and floating. The
    /// Wayland analog of the X11 _NET_WM_STATE_ABOVE toggle: a floating
    /// client is excluded from the tile grid and keeps its own
    /// geometry, sitting above the tiled stack.
    pub fn toggle_above(&mut self) {
        let id = match self.focused {
            Some(i) => i,
            None => return,
        };
        if let Some(c) = self.clients.iter_mut().find(|c| c.id == id) {
            c.floating = !c.floating;
            if !c.floating {
                c.saved_floating = None;
            }
        }
        self.arrange();
    }

    /// Toggle fullscreen on the focused client. Fills the whole monitor
    /// while fullscreen; restores the prior geometry (and re-runs
    /// arrange()) on exit.
    pub fn toggle_fullscreen(&mut self) {
        let id = match self.focused {
            Some(i) => i,
            None => return,
        };
        let pos = match self.clients.iter().position(|c| c.id == id) {
            Some(p) => p,
            None => return,
        };
        let new_fs = !self.clients[pos].fullscreen;
        self.clients[pos].fullscreen = new_fs;
        if new_fs {
            let c = &self.clients[pos];
            self.clients[pos].saved_fullscreen = Some((c.x, c.y, c.w, c.h));
            let mon = self.monitors.first();
            let (mx, my, mw, mh) = (mon.x, mon.y, mon.width, mon.height);
            self.set_geometry_fullscreen(pos, mx, my, mw, mh, true);
        } else {
            if let Some((x, y, w, h)) = self.clients[pos].saved_fullscreen.take() {
                self.set_geometry_fullscreen(pos, x, y, w, h, false);
            }
            self.arrange();
        }
    }

    /// Cycle keyboard focus through candidate clients. Scope::Tag (the
    /// non-global actions) restricts candidates to the active tag;
    /// global cycling spans all tags and switches the view to the
    /// target's tag. Candidates are taken in client-creation order (a
    /// stable ring) starting from the current focus, so repeated
    /// presses round-robin predictably -- the press-hold-release
    /// freeze + thumbnail overlay the X11 path has is a later slice.
    pub fn mru_cycle(&mut self, forward: bool, global: bool) {
        let tag = self.active_tags[self.focused_monitor];
        let cands: Vec<u64> = self
            .clients
            .iter()
            .filter(|c| global || c.tag == tag)
            .map(|c| c.id)
            .collect();
        if cands.len() < 2 {
            if let Some(&only) = cands.first() {
                self.focus_client(only);
            }
            return;
        }
        let next = match self.focused.and_then(|f| cands.iter().position(|&id| id == f)) {
            Some(p) => {
                if forward {
                    (p + 1) % cands.len()
                } else {
                    (p + cands.len() - 1) % cands.len()
                }
            }
            None => 0,
        };
        let target = cands[next];
        if global {
            if let Some(t) = self.clients.iter().find(|c| c.id == target).map(|c| c.tag) {
                if self.active_tags[self.focused_monitor] != t {
                    self.view_tag(t);
                }
            }
        }
        self.focus_client(target);
    }

    /// Central keybind dispatch. Mirrors x11/src/wm.rs::handle_action.
    /// The WM-state verbs operate on the shared tiling model; the CLI
    /// shellouts (volume / brightness / media / lock) fire the same
    /// tools the X11 path uses. The X11-only bar notification + run
    /// prompt + native screenshot are not yet ported to the Wayland
    /// bar -- those arms log and return.
    pub fn handle_action(&mut self, action: &Action) {
        match action {
            Action::Spawn(cmd) => crate::keys::spawn(cmd),
            Action::CloseWindow => {
                if let Some(id) = self.focused {
                    if let Some(c) = self.clients.iter().find(|c| c.id == id) {
                        c.toplevel.send_close();
                    }
                }
            }
            Action::Quit => self.running = false,
            Action::ViewTag(t) => self.view_tag(*t),
            Action::ViewPrevTag => self.view_prev_tag(),
            Action::ViewNextTag => self.view_next_tag(),
            Action::MoveToTag(t) => self.move_to_tag(*t),
            Action::ToggleAbove => self.toggle_above(),
            Action::ToggleFullscreen => self.toggle_fullscreen(),
            Action::ToggleLayout => {
                let tag = self.active_tags[self.focused_monitor];
                self.layouts[tag] = self.layouts[tag].next();
                self.arrange();
            }
            Action::MruNext => self.mru_cycle(true, false),
            Action::MruPrev => self.mru_cycle(false, false),
            Action::MruNextGlobal => self.mru_cycle(true, true),
            Action::MruPrevGlobal => self.mru_cycle(false, true),
            Action::Lock => crate::keys::spawn("gordian_knot"),
            // Shellouts: same CLIs as the X11 path. wpctl / playerctl
            // are server-agnostic; brightnessctl replaces xbacklight
            // (which is X11-only) on the Wayland side. No bar
            // notification yet -- the Wayland bar lacks the transient
            // NOTIFICATION region.
            Action::VolumeUp => crate::keys::spawn("wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%+"),
            Action::VolumeDown => crate::keys::spawn("wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%-"),
            Action::VolumeMute => crate::keys::spawn("wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle"),
            Action::VolumeMicMute => {
                crate::keys::spawn("wpctl set-mute @DEFAULT_AUDIO_SOURCE@ toggle")
            }
            Action::BrightnessUp => crate::keys::spawn("brightnessctl set 5%+"),
            Action::BrightnessDown => crate::keys::spawn("brightnessctl set 5%-"),
            Action::MediaPlayPause => crate::keys::spawn("playerctl play-pause"),
            Action::MediaNext => crate::keys::spawn("playerctl next"),
            Action::MediaPrev => crate::keys::spawn("playerctl previous"),
            Action::RunPrompt => {
                tracing::info!("run_prompt: not yet implemented on Wayland");
            }
            Action::Screenshot => {
                tracing::info!("screenshot: not yet wired on Wayland (grim integration pending)");
            }
        }
    }
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
        // Mint a Wm-side id, append a Client on the focused monitor's
        // active tag, push to MRU front, send an initial Activated
        // configure (xdg-shell mandates the first configure precede
        // any buffer attach), then arrange(). For Tile/Monocle the
        // arrange() pass overwrites the initial configure with a
        // tile-fit size; for Floating with no saved geometry the
        // initial configure with size=None lets the client pick its
        // own preferred size (matches the X11 default-window path).
        let id = self.mint_id();
        let tag = self.active_tags[self.focused_monitor];
        surface.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Activated);
        });
        surface.send_configure();
        self.clients.push(ManagedClient {
            id,
            toplevel: surface,
            tag,
            x: 0,
            y: 0,
            w: 0,
            h: 0,
            fullscreen: false,
            floating: false,
            saved_floating: None,
            saved_fullscreen: None,
        });
        self.mru.on_focus(id);
        tracing::info!(client_id = id, tag, "toplevel mapped");
        self.arrange();
        // Keyboard-driven WM: a freshly mapped window takes focus so the
        // user can type into it immediately without moving the pointer.
        self.focus_client(id);
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        // Remove the client + drop it from MRU. arrange() rebalances
        // the surviving clients in the same tag.
        if let Some(pos) = self
            .clients
            .iter()
            .position(|c| c.toplevel.wl_surface() == surface.wl_surface())
        {
            let id = self.clients[pos].id;
            self.clients.remove(pos);
            self.mru.on_unmanage(id);
            if self.focused == Some(id) {
                self.focused = None;
            }
            tracing::info!(client_id = id, "toplevel destroyed");
            self.arrange();
            // Hand focus to the next MRU client on the active tag (or
            // clear it if the tag is now empty) so the keyboard isn't
            // left pointing at a destroyed surface.
            self.refocus_active_tag();
        }
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

    fn focus_changed(&mut self, _seat: &Seat<Self>, focused: Option<&WlSurface>) {
        // Track the focused client id and promote it in the MRU ring,
        // mirroring x11/src/wm.rs::focus_window. self.focused is the
        // target for focus-relative actions (close / fullscreen /
        // move_to_tag); the MRU front drives Alt+Tab cycling start.
        let id = focused.and_then(|surf| {
            self.clients
                .iter()
                .find(|c| c.toplevel.wl_surface() == surf)
                .map(|c| c.id)
        });
        self.focused = id;
        if let Some(id) = id {
            self.mru.on_focus(id);
        }
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

impl OutputHandler for Wm {}

impl SessionLockHandler for Wm {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.session_lock_state
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        // Stash the SessionLocker. The render loop calls
        // SessionLocker::lock() once it has rendered a fully cleared
        // frame -- the compositor must guarantee no client content is
        // visible before acknowledging the lock.
        tracing::info!("session lock requested");
        self.pending_lock = Some(confirmation);
    }

    fn unlock(&mut self) {
        // The locker called unlock_and_destroy. Drop the lock state;
        // the next render frame will composite normal clients again.
        tracing::info!("session unlocked");
        self.lock_active = false;
        self.lock_surfaces.clear();
    }

    fn new_surface(&mut self, surface: LockSurface, output: WlOutput) {
        // Lock surfaces are sized to the full output. Set the size on
        // pending state, then send the configure -- the locker
        // attaches a buffer at this size on its first commit.
        let (w, h) = self
            .output
            .current_mode()
            .map(|m| (m.size.w as u32, m.size.h as u32))
            .unwrap_or((self.monitors.first().width, self.monitors.first().height));
        surface.with_pending_state(|state| {
            state.size = Some((w, h).into());
        });
        surface.send_configure();
        self.lock_surfaces.push((surface, output));
    }
}

impl XWaylandShellHandler for Wm {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland_shell_state
    }
}

impl XwmHandler for Wm {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        // Single XWayland instance, so the id is informational. The
        // Option must be Some -- main.rs only dispatches X11 events
        // after XWaylandEvent::Ready has populated self.xwm.
        self.xwm.as_mut().expect("xwm queried before Ready")
    }

    fn new_window(&mut self, _xwm: XwmId, window: X11Surface) {
        // X11 client created a window. Map it on demand; track the
        // surface so the render loop sees it once Xwayland associates
        // a wl_surface to it.
        tracing::info!(?window, "X11 new_window");
        self.x11_surfaces.push(window);
    }

    fn new_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        // Override-redirect windows (menus, tooltips) skip the WM.
        // We still track them so they render.
        self.x11_surfaces.push(window);
    }

    fn map_window_request(&mut self, _xwm: XwmId, window: X11Surface) {
        // Grant the map. v0: accept the X11-chosen geometry; tile
        // integration lands when arrange() learns to dispatch to
        // X11Surface.configure alongside ToplevelSurface configure.
        let _ = window.set_mapped(true);
    }

    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
        // Keep tracking the surface; X11 lifecycle separates "unmap"
        // from "destroy" and the window may be remapped.
        let _ = window;
    }

    fn destroyed_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.x11_surfaces.retain(|w| w != &window);
    }

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
        // Honor the client's requested geometry as-is. A future
        // tile-manager pass will override with the arrange()
        // computed cell.
        let geo = window.geometry();
        let new_x = x.unwrap_or(geo.loc.x);
        let new_y = y.unwrap_or(geo.loc.y);
        let new_w = w.map(|v| v as i32).unwrap_or(geo.size.w);
        let new_h = h.map(|v| v as i32).unwrap_or(geo.size.h);
        let _ = window.configure(Rectangle::<_, Logical>::new(
            (new_x, new_y).into(),
            (new_w, new_h).into(),
        ));
    }

    fn configure_notify(
        &mut self,
        _xwm: XwmId,
        _window: X11Surface,
        _geometry: Rectangle<i32, Logical>,
        _above: Option<smithay::xwayland::xwm::X11Window>,
    ) {
    }

    fn resize_request(
        &mut self,
        _xwm: XwmId,
        _window: X11Surface,
        _button: u32,
        _resize_edge: ResizeEdge,
    ) {
    }

    fn move_request(&mut self, _xwm: XwmId, _window: X11Surface, _button: u32) {}
}

// Macros that wire smithay's auto-generated dispatch for each protocol
// to the trait impls above. One per protocol we serve.
delegate_compositor!(Wm);
delegate_xdg_shell!(Wm);
delegate_shm!(Wm);
delegate_seat!(Wm);
delegate_data_device!(Wm);
delegate_output!(Wm);
delegate_session_lock!(Wm);
delegate_xwayland_shell!(Wm);
