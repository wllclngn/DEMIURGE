// Wm: the central state object. Mirrors x11/src/wm.rs's Wm in role --
// it owns the protocol surface, holds the client list, drives focus
// and visibility, and carries the per-monitor tiling model that the
// X11 implementation already runs against. The contents differ
// because the protocols differ: where the X11 Wm holds RustConnection
// + Atoms + Bar, the Wayland Wm holds CompositorState + XdgShellState
// + ShmState + SeatState + Seat. Tiling/tags/MRU shapes are imported
// from demiurge-core so both implementations agree on the model.

use std::os::unix::io::OwnedFd;
use std::time::{Duration, Instant};

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::desktop::utils::bbox_from_surface_tree;
use smithay::desktop::{
    PopupKeyboardGrab, PopupKind, PopupManager, PopupPointerGrab, PopupUngrabStrategy, find_popup_root_surface,
    get_popup_toplevel_coords,
};
use smithay::input::pointer::{CursorIcon, CursorImageStatus, Focus as GrabFocus};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{Interest, RegistrationToken};
use smithay::output::Output;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::{Client, Resource};
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_seat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{SERIAL_COUNTER, Serial};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    BufferAssignment, CompositorClientState, CompositorHandler, CompositorState, SurfaceAttributes,
    add_blocker, add_pre_commit_hook, with_states,
};
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier, get_dmabuf};
use smithay::wayland::output::{OutputHandler, OutputManagerState};
use smithay::wayland::seat::WaylandFocus;
use smithay::input::dnd::{DnDGrab, DndGrabHandler, GrabType, Source};
use smithay::wayland::selection::data_device::{
    DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler, set_data_device_focus,
};
use smithay::wayland::selection::{SelectionHandler, SelectionSource, SelectionTarget};
use smithay::wayland::session_lock::{
    LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker,
};
use smithay::reexports::wayland_protocols::ext::session_lock::v1::server::ext_session_lock_v1::ExtSessionLockV1;
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, SurfaceCachedState, ToplevelSurface, XdgShellHandler, XdgShellState,
    XdgToplevelSurfaceData,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::wayland::tablet_manager::TabletSeatHandler;
use smithay::wayland::xwayland_shell::{XWaylandShellHandler, XWaylandShellState};
use smithay::xwayland::xwm::XwmId;
use smithay::xwayland::{X11Surface, X11Wm, XWaylandClientData};
use smithay::utils::{Logical, Point, Rectangle};
use smithay::input::pointer::PointerHandle;
use smithay::wayland::drm_syncobj::{DrmSyncobjCachedState, DrmSyncobjHandler, DrmSyncobjState};
use smithay::wayland::pointer_constraints::{PointerConstraintsHandler, with_pointer_constraint};
use smithay::{
    delegate_commit_timing, delegate_compositor, delegate_cursor_shape, delegate_data_device, delegate_dmabuf,
    delegate_drm_syncobj, delegate_fifo, delegate_output,
    delegate_pointer_constraints, delegate_presentation, delegate_relative_pointer, delegate_seat,
    delegate_session_lock, delegate_shm, delegate_viewporter, delegate_xdg_shell, delegate_xwayland_shell,
    delegate_layer_shell, delegate_image_capture_source, delegate_output_capture_source, delegate_image_copy_capture,
    delegate_ext_data_control,
};
use smithay::wayland::selection::ext_data_control::{DataControlHandler, DataControlState};

use demiurge_core::{Action, Layout, Monitor, Monitors, MruRing, Zoom};
use demiurge_core::notify;
use demiurge_core::prompt::{self, Prompt};
use wayland_protocols::wp::color_management::v1::server::wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1;
use wayland_protocols_wlr::gamma_control::v1::server::zwlr_gamma_control_v1::ZwlrGammaControlV1;

use crate::backend::Runtime;
use crate::client::ClientState;
use crate::focus::Focus;
use crate::keys::Binding;

// The locker the lock bind runs, and the one relaunched if it dies locked.
const LOCKER: &str = "gordian_knot";

// A managed window: An xdg toplevel, or an X11 window XWayland maps for
// an X client. Both tile, stack, focus and go fullscreen the same way;
// only the way they hear about it differs.
#[derive(Debug, Clone, PartialEq)]
pub enum Toplevel {
    Xdg(ToplevelSurface),
    X11(X11Surface),
}

impl Toplevel {
    // An X11 window has no surface until XWayland pairs one with it.
    pub fn wl_surface(&self) -> Option<WlSurface> {
        match self {
            Toplevel::Xdg(toplevel) => Some(toplevel.wl_surface().clone()),
            Toplevel::X11(window) => window.wl_surface(),
        }
    }

    pub fn xdg(&self) -> Option<&ToplevelSurface> {
        match self {
            Toplevel::Xdg(toplevel) => Some(toplevel),
            Toplevel::X11(_) => None,
        }
    }

    pub fn x11(&self) -> Option<&X11Surface> {
        match self {
            Toplevel::X11(window) => Some(window),
            Toplevel::Xdg(_) => None,
        }
    }

    pub fn focus(&self) -> Focus {
        match self {
            Toplevel::Xdg(toplevel) => Focus::Surface(toplevel.wl_surface().clone()),
            Toplevel::X11(window) => Focus::X11(window.clone()),
        }
    }

    /// Its class: An xdg toplevel's app_id, an X11 window's WM_CLASS.
    pub fn class(&self) -> String {
        match self {
            Toplevel::Xdg(toplevel) => with_states(toplevel.wl_surface(), |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .and_then(|data| data.lock().unwrap().app_id.clone())
            })
            .unwrap_or_default(),
            Toplevel::X11(window) => window.class(),
        }
    }

    pub fn title(&self) -> String {
        match self {
            Toplevel::Xdg(toplevel) => with_states(toplevel.wl_surface(), |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .and_then(|data| data.lock().unwrap().title.clone())
            })
            .unwrap_or_default(),
            Toplevel::X11(window) => window.title(),
        }
    }

    pub fn close(&self) {
        match self {
            Toplevel::Xdg(toplevel) => toplevel.send_close(),
            Toplevel::X11(window) => {
                let _ = window.close();
            }
        }
    }
}

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
    pub toplevel: Toplevel,
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
    // Set while zoomed to the work area; what Super+- returns to.
    pub saved_zoom: Option<Zoom>,
    // Hidden on its tag until the tag menu or its activation brings it back.
    pub minimized: bool,
    // The title bar's height it shows, inside its x, y, w and h: 0 without.
    pub decor: u32,
}

impl ManagedClient {
    // Where the surface goes so its window geometry (the window as the
    // client means it, without its shadow) sits at (x, y). An X11
    // window's surface is the window.
    pub fn surface_origin(&self) -> Point<i32, Logical> {
        let geometry = self.toplevel.xdg().and_then(|toplevel| {
            with_states(toplevel.wl_surface(), |states| {
                states.cached_state.get::<SurfaceCachedState>().current().geometry
            })
        });
        Point::from((self.x, self.y + self.decor as i32)) - geometry.map(|g| g.loc).unwrap_or_default()
    }

    // The client's popups with their surfaces' origins, front to back: A
    // child popup before its parent. X11 menus are override-redirect
    // windows of their own, not popups.
    pub fn popups(&self) -> Vec<(WlSurface, Point<i32, Logical>)> {
        let Some(toplevel) = self.toplevel.xdg() else {
            return Vec::new();
        };
        let mut popups: Vec<_> = PopupManager::popups_for_surface(toplevel.wl_surface())
            .map(|(popup, offset)| {
                let origin = Point::from((self.x, self.y)) + offset - popup.geometry().loc;
                (popup.wl_surface().clone(), origin)
            })
            .collect();
        popups.reverse();
        popups
    }
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
    // linux-dmabuf. Its global goes up once a backend's renderer can say
    // which GPU it is and what it imports.
    pub dmabuf_state: DmabufState,

    // XWayland integration. xwayland_shell_state advertises the
    // XWaylandShellV1 global. xwm holds the X11 window manager once
    // XWayland is up and ready (None before, set when XWayland reports
    // Ready), x11_reader the compositor's own connection beside it.
    // x11_surfaces tracks every X11 window; a mapped one that is not
    // override-redirect is also a ManagedClient.
    pub xwayland_shell_state: XWaylandShellState,
    pub xwm: Option<X11Wm>,
    pub x11_reader: Option<crate::xwm::X11Reader>,
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
    // The tags by name, as cherrypie may name one.
    pub tag_names: Vec<String>,

    // cherrypie's window-policy API (policy.rs): Where each new window
    // goes. A window waits in `unplaced` for its initial commit and then
    // for the answer; policy_watch is the socket's source in the event
    // loop while the link is up.
    pub policy: cherrypie::client::Policy,
    pub policy_watch: Option<RegistrationToken>,
    pub unplaced: Vec<crate::policy::Unplaced>,

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

    // The run prompt while it is open, and PATH's executables as they were
    // when it opened, for its completion.
    pub prompt: Option<Prompt>,
    executables: Vec<String>,

    // A window being moved or resized with the pointer.
    pub(crate) drag: Option<crate::drag::Drag>,

    // DEMIURGE's settings served to XWayland's X11 apps.
    pub(crate) xsettings: Option<crate::xsettings::XSettings>,

    // [general] title_bars.
    pub(crate) title_bars: bool,

    // An Alt+Tab cycle while its modifier is held, and whether its overlay
    // is drawn ([general] alt_tab_preview).
    pub cycle: Option<demiurge_core::mru::CycleState<u64>>,
    pub(crate) alt_tab_preview: bool,

    // An input method's candidate popups.
    pub ime_popups: Vec<smithay::wayland::input_method::PopupSurface>,

    // The tag menu while it is open, how it is drawn, and a count bumped
    // whenever what it shows changes.
    pub tag_menu: Option<crate::tag_menu::TagMenu>,
    pub menu_style: Option<demiurge_core::tag_menu::Style>,
    pub(crate) menu_version: u64,

    // Tagged [[startup.spawn]] entries not yet matched: A class, and the tag
    // the first window of that class goes to.
    pub pending_spawns: Vec<(String, usize)>,

    // A bar notification from a volume, brightness or media bind, and when
    // it gives the bar back to the focused window's title.
    pub notification: Option<(String, Instant)>,

    // Where the pointer is, and what it shows there: A client's surface,
    // a named cursor the compositor draws, or nothing.
    pub pointer_location: Point<f64, Logical>,
    pub cursor_status: CursorImageStatus,
    // [cursor] auto_hide: How long the pointer stays still before the
    // cursor hides (None: never), whether it is hidden, when the pointer
    // last moved, and whether a timer is waiting to hide it.
    cursor_auto_hide: Option<Duration>,
    pub cursor_hidden: bool,
    last_motion: Instant,
    hide_pending: bool,
    // [input.idle] lock_seconds: How long without input before the session
    // locks (None: never), when input last came, whether a timer is
    // waiting, and the surfaces whose idle inhibitors hold the lock off.
    pub(crate) idle_lock_after: Option<Duration>,
    pub(crate) last_input: Instant,
    pub(crate) idle_pending: bool,
    pub(crate) idle_inhibitors: Vec<WlSurface>,
    // Blanking after `screensaver_seconds` and power-off after the first
    // DPMS timeout, and whether this idle spell has locked, blanked or
    // powered off yet.
    pub(crate) idle_blank_after: Option<Duration>,
    pub(crate) idle_off_after: Option<Duration>,
    pub(crate) idle_locked: bool,
    pub blanked: bool,
    pub outputs_off: bool,

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
    // The lock the locker holds, kept to see it die with its client; when
    // the locker was last relaunched; and whether a timer is watching.
    locker: Option<ExtSessionLockV1>,
    relaunched: Option<Instant>,
    watching_locker: bool,
    // The layer surface holding the keyboard exclusively, while one does:
    // No window takes the keyboard from it.
    pub exclusive_layer: Option<WlSurface>,
    // Every monitor lit, in the order the screen lit them, whose layer maps
    // hold the layer surfaces.
    pub outputs: Vec<Output>,
    // Clients copying the monitors' frames, each session stopped as it is
    // dropped.
    pub capture_sessions: Vec<smithay::wayland::image_copy_capture::Session>,

    // Every client's popups, each tree rooted at its toplevel.
    pub popups: PopupManager,

    // Every color-management feedback object, told when the preferred
    // description changes with the primary monitor.
    pub color_feedbacks: Vec<WpColorManagementSurfaceFeedbackV1>,

    // The gamma control holding each output that has one (gamma.rs).
    pub gamma_controls: Vec<(Output, ZwlrGammaControlV1)>,

    // The backend, the bar and the per-output frame state of a running
    // compositor. None when the Wm runs headless under test.
    pub runtime: Option<Runtime>,
    /// The helper that holds XWayland's input focus for a Steam game's
    /// Wayland window, so Steam Input gives the game its controller.
    pub steam_focus: crate::steam::SteamFocus,
}

impl Wm {
    /// Build a Wm with tiling state initialized from config. Monitors
    /// starts with a single placeholder entry; run() sizes it from the
    /// first output before any client connects.
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

        // Placeholder monitor; run() overwrites it with the first
        // output's size before any toplevel can map.
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
            dmabuf_state: DmabufState::new(),
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
            tag_names: cfg.general.tags.clone(),
            policy: cherrypie::client::Policy::new(""),
            policy_watch: None,
            unplaced: Vec::new(),
            bindings: crate::keys::compile(&cfg.keybinds),
            focused: None,
            running: true,
            prompt: None,
            executables: Vec::new(),
            drag: None,
            xsettings: None,
            title_bars: cfg.general.title_bars,
            cycle: None,
            alt_tab_preview: cfg.general.alt_tab_preview,
            ime_popups: Vec::new(),
            tag_menu: None,
            menu_style: demiurge_core::tag_menu::Style::from_config(cfg).map_err(|e| tracing::warn!("tag menu: {}", e)).ok(),
            menu_version: 0,
            pending_spawns: Vec::new(),
            notification: None,
            pointer_location: Point::from((0.0, 0.0)),
            cursor_status: CursorImageStatus::Named(CursorIcon::Default),
            // At least X11's 100 ms poll, so a timeout of 0 hides the cursor
            // as soon as the pointer stops, not between two motions.
            cursor_auto_hide: cfg
                .cursor
                .auto_hide
                .then(|| Duration::from_secs(cfg.cursor.auto_hide_seconds as u64).max(Duration::from_millis(100))),
            cursor_hidden: false,
            last_motion: Instant::now(),
            hide_pending: false,
            idle_lock_after: (cfg.input.idle.lock_seconds > 0).then(|| Duration::from_secs(cfg.input.idle.lock_seconds as u64)),
            last_input: Instant::now(),
            idle_pending: false,
            idle_inhibitors: Vec::new(),
            idle_blank_after: (cfg.input.idle.screensaver_seconds > 0)
                .then(|| Duration::from_secs(cfg.input.idle.screensaver_seconds as u64)),
            idle_off_after: [cfg.input.idle.dpms_standby_seconds, cfg.input.idle.dpms_suspend_seconds, cfg.input.idle.dpms_off_seconds]
                .into_iter()
                .filter(|&s| s > 0)
                .min()
                .map(|s| Duration::from_secs(s as u64)),
            idle_locked: false,
            blanked: false,
            outputs_off: false,
            pending_lock: None,
            lock_active: false,
            lock_surfaces: Vec::new(),
            locker: None,
            relaunched: None,
            watching_locker: false,
            exclusive_layer: None,
            outputs: Vec::new(),
            capture_sessions: Vec::new(),
            popups: PopupManager::default(),
            color_feedbacks: Vec::new(),
            gamma_controls: Vec::new(),
            xwayland_shell_state,
            xwm: None,
            x11_reader: None,
            x11_surfaces: Vec::new(),
            steam_focus: crate::steam::SteamFocus::default(),
            runtime: None,
        }
    }

    pub(crate) fn mint_id(&mut self) -> u64 {
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

    /// The monitors, from the outputs lit, in their order: At startup, as
    /// the test window resizes, and as monitors are plugged, unplugged or
    /// lit at another mode. Each visible tag stays on its monitor; a
    /// monitor added shows a tag no other monitor does, as on X11, and one
    /// removed takes its view with it. cherrypie hears the new list.
    pub fn set_monitors(&mut self, outputs: &[Output]) {
        self.outputs = outputs.to_vec();
        let monitors: Vec<Monitor> = outputs
            .iter()
            .filter_map(|o| {
                let mode = o.current_mode()?;
                let at = o.current_location();
                Some(Monitor { name: o.name(), x: at.x, y: at.y, width: mode.size.w as u32, height: mode.size.h as u32 })
            })
            .collect();
        let Some(monitors) = Monitors::try_new(monitors) else {
            return;
        };
        self.monitors = monitors;
        let count = self.monitors.len();
        self.active_tags.truncate(count);
        while self.active_tags.len() < count {
            let pick = (0..self.num_tags)
                .find(|t| !self.active_tags.contains(t))
                .unwrap_or_else(|| self.num_tags.saturating_sub(1));
            self.active_tags.push(pick);
        }
        self.focused_monitor = self.focused_monitor.min(count - 1);
        // A zoomed client follows its monitor's new work area, a
        // fullscreen one the monitor itself.
        for i in 0..self.clients.len() {
            if self.clients[i].fullscreen {
                let mon = self.client_monitor(i);
                let (x, y, w, h) = (mon.x, mon.y, mon.width, mon.height);
                self.set_geometry(i, x, y, w, h);
            } else if self.clients[i].saved_zoom.is_some() {
                let (x, y, w, h) = self.client_work_area(i);
                self.set_geometry_state(i, x, y, w, h, xdg_toplevel::State::Maximized, true);
            }
        }
        self.arrange();
        self.send_monitors();
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
                .filter(|(_, c)| c.tag == tag && !c.fullscreen && !c.floating && !c.minimized)
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

                    let tiles = demiurge_core::layout::tile((work_x, work_y, work_w, work_h), indices.len(), self.master_ratio);
                    for (&i, (x, y, w, h)) in indices.iter().zip(tiles) {
                        self.set_geometry(i, x, y, w, h);
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

    // Update Client geometry and tell the client: An xdg configure with
    // the chosen size, the client committing a buffer of that size next;
    // an X11 window is moved and resized outright. The render loop reads
    // (x, y) at composite time.
    pub(crate) fn set_geometry(&mut self, i: usize, x: i32, y: i32, w: u32, h: u32) {
        self.place(i, x, y, w, h, None);
    }

    // Same as set_geometry but also sets or clears a toplevel state
    // (Fullscreen, Maximized) so the client drops or restores its
    // decorations. Used by toggle_fullscreen and zoom; arrange() uses
    // set_geometry.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn set_geometry_state(
        &mut self,
        i: usize,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
        flag: xdg_toplevel::State,
        on: bool,
    ) {
        self.place(i, x, y, w, h, Some((flag, on)));
    }

    fn place(&mut self, i: usize, x: i32, y: i32, w: u32, h: u32, state: Option<(xdg_toplevel::State, bool)>) {
        let decor = if self.titled(i) { demiurge_core::titlebar::HEIGHT } else { 0 };
        let c = &mut self.clients[i];
        c.x = x;
        c.y = y;
        c.w = w;
        c.h = h;
        c.decor = decor;
        match &c.toplevel {
            Toplevel::Xdg(toplevel) => {
                // The client's room is below its title bar.
                toplevel.with_pending_state(|pending| {
                    pending.size = Some((w as i32, h.saturating_sub(decor) as i32).into());
                    match state {
                        Some((flag, true)) => pending.states.set(flag),
                        Some((flag, false)) => pending.states.unset(flag),
                        None => false,
                    };
                });
                toplevel.send_configure();
            }
            Toplevel::X11(window) => {
                let _ = window.configure(Rectangle::new((x, y).into(), (w as i32, h as i32).into()));
                let _ = match state {
                    Some((xdg_toplevel::State::Fullscreen, on)) => window.set_fullscreen(on),
                    Some((xdg_toplevel::State::Maximized, on)) => window.set_maximized(on),
                    _ => Ok(()),
                };
            }
        }
    }

    /// True if the client's tag is currently visible on some monitor.
    /// The render paths use this to skip clients on hidden tags --
    /// without it, switching tags wouldn't hide anything (every client
    /// stays in the element list regardless of tag).
    pub fn is_visible(&self, c: &ManagedClient) -> bool {
        !c.minimized && self.active_tags.iter().any(|&t| t == c.tag)
    }

    /// True if the monitor's active tag holds a fullscreen client, or an
    /// X11 override-redirect window covers the monitor, as a
    /// game's own fullscreen window does (at an emulated mode, through the
    /// viewport XWayland gives its surface). The bar is suppressed while
    /// this holds so a fullscreen window isn't clipped by the panel.
    pub fn fullscreen_shown(&self, monitor: usize) -> bool {
        let tag = self.active_tags[monitor];
        let mon = &self.monitors[monitor];
        let monitor = Rectangle::new((mon.x, mon.y).into(), (mon.width as i32, mon.height as i32).into());
        self.clients.iter().any(|c| c.tag == tag && c.fullscreen)
            || self.x11_surfaces.iter().filter(|x| x.is_mapped() && x.is_override_redirect()).any(|x| {
                x.wl_surface().is_some_and(|s| bbox_from_surface_tree(&s, x.geometry().loc).contains_rect(monitor))
            })
    }

    /// True from a lock's request until its unlock: Everything but the
    /// lock is hidden and gets no input.
    pub fn locked(&self) -> bool {
        self.lock_active || self.pending_lock.is_some()
    }

    // Set keyboard focus to a surface (or clear it). Threads self
    // through the keyboard handle so smithay drives focus_changed,
    // which updates self.focused + the MRU ring. While locked the
    // keyboard stays on the lock, and while a layer surface holds it on that
    // surface; no window mapping or closing takes it.
    fn set_keyboard_focus(&mut self, focus: Option<Focus>) {
        if !self.locked() && self.exclusive_layer.is_none() {
            self.keyboard_to(focus);
        }
    }

    pub(crate) fn keyboard_to(&mut self, focus: Option<Focus>) {
        let cleared = focus.is_none();
        if let Some(kbd) = self.seat.get_keyboard() {
            let serial = SERIAL_COUNTER.next_serial();
            kbd.set_focus(self, focus, serial);
        }
        // smithay calls focus_changed only for a new focus, so a keyboard
        // left with none deactivates the windows here.
        if cleared {
            self.activate(None);
            self.clipboard_to(None);
        }
    }

    // The client holding the keyboard is the one offered the clipboard;
    // smithay offers it to none until told which.
    fn clipboard_to(&self, focused: Option<&Focus>) {
        let Some(dh) = self.runtime.as_ref().map(|rt| &rt.display) else {
            return;
        };
        let client = focused.and_then(|f| f.wl_surface()).and_then(|s| dh.get_client(s.id()).ok());
        set_data_device_focus(dh, &self.seat, client);
    }

    // The window holding the keyboard is told it is the active one and
    // every other that it is not: An xdg toplevel through its Activated
    // state, an X11 window through its own.
    fn activate(&self, id: Option<u64>) {
        for c in &self.clients {
            let on = Some(c.id) == id;
            match &c.toplevel {
                Toplevel::Xdg(toplevel) => {
                    toplevel.with_pending_state(|pending| {
                        if on {
                            pending.states.set(xdg_toplevel::State::Activated)
                        } else {
                            pending.states.unset(xdg_toplevel::State::Activated)
                        }
                    });
                    toplevel.send_pending_configure();
                }
                Toplevel::X11(window) => {
                    let _ = window.set_activated(on);
                }
            }
        }
    }

    /// Give keyboard focus to the client with the given id.
    pub fn focus_client(&mut self, id: u64) {
        if let Some(focus) = self.clients.iter().find(|c| c.id == id).map(|c| c.toplevel.focus()) {
            self.set_keyboard_focus(Some(focus));
        }
    }

    // Focus the most-recently-used client on the focused monitor's
    // active tag; clear focus if that tag is empty. Called after any
    // operation that changes which clients are visible (view_tag,
    // move_to_tag, destroy).
    pub(crate) fn refocus_active_tag(&mut self) {
        let tag = self.active_tags[self.focused_monitor];
        let target = self
            .mru
            .as_slice()
            .iter()
            .copied()
            .find(|id| self.clients.iter().any(|c| c.id == *id && c.tag == tag && !c.minimized));
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
        if !demiurge_core::monitor::view(&mut self.active_tags, self.focused_monitor, tag) {
            return;
        }
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

    /// Toggle fullscreen on the focused client.
    pub fn toggle_fullscreen(&mut self) {
        let Some(id) = self.focused else {
            return;
        };
        let on = self.clients.iter().find(|c| c.id == id).is_some_and(|c| !c.fullscreen);
        self.set_fullscreen(id, on);
    }

    /// Fullscreen fills the whole monitor showing the client's tag; leaving
    /// it restores the prior geometry and re-runs arrange(). The keybind
    /// and the client's own request both come here.
    pub fn set_fullscreen(&mut self, id: u64, on: bool) {
        let Some(pos) = self.clients.iter().position(|c| c.id == id) else {
            return;
        };
        if self.clients[pos].fullscreen == on {
            // An xdg request is answered with a configure even when
            // nothing changes.
            if let Some(toplevel) = self.clients[pos].toplevel.xdg() {
                toplevel.send_configure();
            }
            return;
        }
        self.clients[pos].fullscreen = on;
        if on {
            let c = &self.clients[pos];
            self.clients[pos].saved_fullscreen = Some((c.x, c.y, c.w, c.h));
            let mon = self.client_monitor(pos);
            let (mx, my, mw, mh) = (mon.x, mon.y, mon.width, mon.height);
            self.set_geometry_state(pos, mx, my, mw, mh, xdg_toplevel::State::Fullscreen, true);
        } else {
            let c = &self.clients[pos];
            let (x, y, w, h) = c.saved_fullscreen.unwrap_or((c.x, c.y, c.w, c.h));
            self.clients[pos].saved_fullscreen = None;
            self.set_geometry_state(pos, x, y, w, h, xdg_toplevel::State::Fullscreen, false);
            self.arrange();
        }
        self.queue_redraw();
    }

    // The monitor `output` is, by name; the first if it is none.
    pub fn monitor_of(&self, output: &Output) -> usize {
        self.monitors.iter().position(|m| m.name == output.name()).unwrap_or(0)
    }

    // The monitor showing the client's tag, else the first.
    pub(crate) fn client_monitor(&self, i: usize) -> &Monitor {
        let tag = self.clients[i].tag;
        self.active_tags
            .iter()
            .position(|&t| t == tag)
            .and_then(|m| self.monitors.get(m))
            .unwrap_or_else(|| self.monitors.first())
    }

    /// Fill the work area below the bar with the client (not fullscreen);
    /// a tiled client floats out for it. The client is told it is
    /// maximized, which is what xdg-shell calls it.
    pub fn zoom_in(&mut self, id: u64) {
        let Some(i) = self.clients.iter().position(|c| c.id == id) else {
            return;
        };
        let c = &self.clients[i];
        if c.fullscreen || c.saved_zoom.is_some() {
            return;
        }
        let zoom = Zoom::new((c.x, c.y, c.w, c.h), c.floating, self.layouts[c.tag]);
        self.clients[i].saved_zoom = Some(zoom);
        if zoom.floated {
            self.clients[i].floating = true;
            self.arrange();
        }
        let (x, y, w, h) = self.client_work_area(i);
        self.set_geometry_state(i, x, y, w, h, xdg_toplevel::State::Maximized, true);
    }

    /// Back to where the client was before it zoomed: Its own geometry,
    /// or its tile.
    pub fn zoom_out(&mut self, id: u64) {
        let Some(i) = self.clients.iter().position(|c| c.id == id) else {
            return;
        };
        if self.clients[i].fullscreen {
            return;
        }
        let Some(zoom) = self.clients[i].saved_zoom.take() else {
            return;
        };
        if zoom.floated {
            self.clients[i].floating = false;
        }
        let (x, y, w, h) = zoom.geometry;
        self.set_geometry_state(i, x, y, w, h, xdg_toplevel::State::Maximized, false);
        self.arrange();
    }

    // The client behind `surface`, if a zoom request can change it: Not
    // fullscreen, and zoomed or not as `zoomed` says.
    pub(crate) fn zoom_target(&self, toplevel: &Toplevel, zoomed: bool) -> Option<u64> {
        self.clients
            .iter()
            .find(|c| &c.toplevel == toplevel)
            .filter(|c| !c.fullscreen && c.saved_zoom.is_some() == zoomed)
            .map(|c| c.id)
    }

    // The work area of the monitor showing the client's tag.
    fn client_work_area(&self, i: usize) -> (i32, i32, u32, u32) {
        self.client_monitor(i).work_area(self.bar_height)
    }

    /// Central keybind dispatch. Mirrors x11/src/wm.rs::handle_action.
    /// The WM-state verbs operate on the shared tiling model; the CLI
    /// shellouts (volume / brightness / media / lock) fire the same
    /// tools the X11 path uses. The X11-only bar notification + run
    /// prompt + native screenshot are not yet ported to the Wayland
    /// bar -- those arms log and return.
    pub fn handle_action(&mut self, action: &Action) {
        match action {
            Action::Spawn(cmd) => self.spawn(cmd),
            Action::CloseWindow => {
                if let Some(id) = self.focused {
                    if let Some(c) = self.clients.iter().find(|c| c.id == id) {
                        c.toplevel.close();
                    }
                }
            }
            Action::Quit => self.running = false,
            // The tag already in view: Its key again opens the tag menu.
            Action::ViewTag(t) if self.active_tags[self.focused_monitor] == *t => {
                self.open_tag_menu(self.focused_monitor, true)
            }
            Action::ViewTag(t) => self.view_tag(*t),
            Action::ViewPrevTag => self.view_prev_tag(),
            Action::ViewNextTag => self.view_next_tag(),
            Action::MoveToTag(t) => self.move_to_tag(*t),
            Action::ToggleAbove => self.toggle_above(),
            Action::ToggleFullscreen => self.toggle_fullscreen(),
            Action::ZoomIn => {
                if let Some(id) = self.focused {
                    self.zoom_in(id);
                }
            }
            Action::ZoomOut => {
                if let Some(id) = self.focused {
                    self.zoom_out(id);
                }
            }
            Action::ToggleLayout => {
                let tag = self.active_tags[self.focused_monitor];
                self.layouts[tag] = self.layouts[tag].next();
                self.arrange();
            }
            Action::MruNext => self.mru_cycle(true, false),
            Action::MruPrev => self.mru_cycle(false, false),
            Action::MruNextGlobal => self.mru_cycle(true, true),
            Action::MruPrevGlobal => self.mru_cycle(false, true),
            Action::Lock => self.spawn_locker(),
            // The X11 stack's commands and notifications, brightnessctl in
            // place of X11's xbacklight.
            Action::VolumeUp => self.notify_after(|| notify::volume_delta("5%+")),
            Action::VolumeDown => self.notify_after(|| notify::volume_delta("5%-")),
            Action::VolumeMute => self.notify_after(|| notify::toggle_mute(false)),
            Action::VolumeMicMute => self.notify_after(|| notify::toggle_mute(true)),
            Action::BrightnessUp => self.notify_after(|| notify::brightness_delta("5%+")),
            Action::BrightnessDown => self.notify_after(|| notify::brightness_delta("5%-")),
            Action::MediaPlayPause => self.notify_after(|| notify::media_command("play-pause")),
            Action::MediaNext => self.notify_after(|| notify::media_command("next")),
            Action::MediaPrev => self.notify_after(|| notify::media_command("previous")),
            Action::RunPrompt => {
                if self.prompt.is_none() {
                    self.executables = prompt::scan_path();
                    self.prompt = Some(Prompt::default());
                }
            }
            Action::Minimize => {
                if let Some(id) = self.focused {
                    self.minimize(id);
                }
            }
            Action::Screenshot => self.screenshot(),
        }
        self.queue_redraw();
    }

    // A child of the compositor, started with its WAYLAND_DISPLAY,
    // DISPLAY and cursor theme.
    fn spawn(&self, cmd: &str) {
        crate::keys::spawn(cmd, self.runtime.as_ref().map_or(&[], |rt| rt.env.as_slice()));
    }

    pub(crate) fn spawn_locker(&self) {
        self.spawn(LOCKER);
    }

    // While locked, a locker that died with its client is relaunched, as
    // the lock bind runs it, and the dead one's surfaces are let go; a
    // relaunched locker that never locks gets another try five seconds on.
    fn watch_locker(&mut self) {
        let Some(rt) = self.runtime.as_ref() else {
            return;
        };
        if self.watching_locker {
            return;
        }
        self.watching_locker = true;
        let _ = rt.handle.insert_source(Timer::from_duration(Duration::from_secs(1)), |_, _, wm| {
            if !wm.locked() {
                wm.watching_locker = false;
                return TimeoutAction::Drop;
            }
            let alive = wm.locker.as_ref().is_some_and(|lock| lock.is_alive());
            let waited = wm.relaunched.is_none_or(|at| at.elapsed() >= Duration::from_secs(5));
            if !alive && waited {
                tracing::warn!("the locker died with the session locked; relaunching it");
                wm.lock_surfaces.retain(|(surface, _)| surface.wl_surface().is_alive());
                wm.locker = None;
                wm.relaunched = Some(Instant::now());
                wm.spawn(LOCKER);
            }
            TimeoutAction::ToDuration(Duration::from_secs(1))
        });
    }

    /// The pointer moved, or the session began: The cursor shows, and with
    /// [cursor] auto_hide it hides once the pointer has been still that
    /// long, as on X11. One timer at a time, timed from the last motion
    /// rather than X11's 100 ms poll; a drag keeps the cursor shown.
    pub(crate) fn pointer_stirred(&mut self) {
        self.last_motion = Instant::now();
        if self.cursor_hidden {
            self.cursor_hidden = false;
            self.queue_redraw();
        }
        let Some(after) = self.cursor_auto_hide else {
            return;
        };
        let Some(rt) = self.runtime.as_ref() else {
            return;
        };
        if self.hide_pending {
            return;
        }
        self.hide_pending = true;
        let _ = rt.handle.insert_source(Timer::from_duration(after), move |_, _, wm| {
            let left = after.saturating_sub(wm.last_motion.elapsed());
            if !left.is_zero() {
                return TimeoutAction::ToDuration(left);
            }
            if wm.drag.is_some() {
                return TimeoutAction::ToDuration(after);
            }
            wm.hide_pending = false;
            wm.cursor_hidden = true;
            wm.queue_redraw();
            TimeoutAction::Drop
        });
    }

    // A bind's commands, run off the compositor's loop since they wait on
    // their children; the notification they return comes back to it.
    fn notify_after(&self, work: impl FnOnce() -> String + Send + 'static) {
        let notices = self.runtime.as_ref().map(|rt| rt.notices.clone());
        std::thread::spawn(move || {
            let text = work();
            if let Some(notices) = notices {
                let _ = notices.send(text);
            }
        });
    }

    /// Show `text` in the bar in place of the focused window's title for
    /// NOTIFICATION_TTL_MS.
    pub fn notify(&mut self, text: String) {
        let ttl = Duration::from_millis(notify::NOTIFICATION_TTL_MS);
        self.notification = Some((text, Instant::now() + ttl));
        if let Some(rt) = self.runtime.as_ref() {
            let _ = rt.handle.insert_source(Timer::from_duration(ttl), |_, _, wm| {
                if wm.notification.as_ref().is_some_and(|(_, until)| Instant::now() >= *until) {
                    wm.notification = None;
                    wm.queue_redraw();
                }
                TimeoutAction::Drop
            });
        }
        self.queue_redraw();
    }

    /// The bar's center: A notification, else the focused window's title;
    /// true for a notification.
    pub fn bar_center(&self) -> Option<(String, bool)> {
        if let Some((text, _)) = &self.notification {
            return Some((text.clone(), true));
        }
        let title = self.clients.iter().find(|c| Some(c.id) == self.focused)?.toplevel.title();
        (!title.is_empty()).then_some((title, false))
    }

    // A key while the run prompt is open: Return runs what was typed and
    // closes it, Escape closes it, anything else edits it.
    pub(crate) fn prompt_key(&mut self, keysym: u32) {
        let Some(open) = self.prompt.as_mut() else {
            return;
        };
        match open.key(keysym, &self.executables) {
            prompt::Key::Run(cmd) => {
                self.prompt = None;
                self.spawn(&cmd);
            }
            prompt::Key::Closed => self.prompt = None,
            prompt::Key::Open => {}
        }
        self.queue_redraw();
    }
}

impl CompositorHandler for Wm {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    // XWayland's own client carries smithay's data, every other one ours.
    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        if let Some(xwayland) = client.get_data::<XWaylandClientData>() {
            return &xwayland.compositor_state;
        }
        &client
            .get_data::<ClientState>()
            .expect("ClientState attached at insert_client")
            .compositor_state
    }

    // Implicit sync: A commit of a dmabuf takes effect once the GPU has
    // finished writing it, which its fd reports by turning readable.
    fn new_surface(&mut self, surface: &WlSurface) {
        add_pre_commit_hook::<Self, _>(surface, |wm, _, surface| {
            // Every commit gets a frame as it arrives: One held behind a
            // commit timer is let through only at a frame's end, and the
            // commit handler, which would ask for one, runs only once the
            // update is applied.
            wm.queue_redraw();
            let Some(rt) = wm.runtime.as_ref() else {
                return;
            };
            let (dmabuf, acquire) = with_states(surface, |states| {
                let dmabuf = match states.cached_state.get::<SurfaceAttributes>().pending().buffer.as_ref() {
                    Some(BufferAssignment::NewBuffer(buffer)) => get_dmabuf(buffer).cloned().ok(),
                    _ => None,
                };
                (dmabuf, states.cached_state.get::<DrmSyncobjCachedState>().pending().acquire_point.clone())
            });
            let (Some(dmabuf), Some(client)) = (dmabuf, surface.client()) else {
                return;
            };
            // Explicit sync: The buffer is ready once the client's acquire
            // point is signalled. Else the buffer's own implicit fence.
            match acquire {
                Some(point) => {
                    if let Ok((blocker, source)) = point.generate_blocker() {
                        if rt.handle.insert_source(source, unblock(client)).is_ok() {
                            add_blocker(surface, blocker);
                        }
                    }
                }
                None => {
                    if let Ok((blocker, source)) = dmabuf.generate_blocker(Interest::READ) {
                        if rt.handle.insert_source(source, unblock(client)).is_ok() {
                            add_blocker(surface, blocker);
                        }
                    }
                }
            }
        });
    }

    fn commit(&mut self, surface: &WlSurface) {
        // Tells smithay to advance the buffer state machine on the
        // committed surface tree (acks any pending wl_buffer attaches,
        // etc.). Without this, the buffer associated with a commit is
        // never made available to the renderer.
        smithay::backend::renderer::utils::on_commit_buffer_handler::<Self>(surface);
        crate::color::commit(surface);
        // A popup's first commit is answered with its first configure.
        self.popups.commit(surface);
        if let Some(PopupKind::Xdg(popup)) = self.popups.find_popup(surface) {
            if !popup.is_initial_configure_sent() {
                let _ = popup.send_configure();
            }
        }
        self.popups.cleanup();
        // A toplevel's first commit asks where it goes.
        self.initial_commit(surface);
        self.layer_commit(surface);
        self.follow_size(surface);
        self.queue_redraw();
    }
}

impl Wm {
    // A floating window sizes itself: Its committed window geometry becomes
    // its size, and its first one centers it in its monitor's work area, as
    // the X11 stack centers a window that keeps its own geometry.
    fn follow_size(&mut self, surface: &WlSurface) {
        let Some(i) = self.clients.iter().position(|c| c.toplevel.xdg().is_some_and(|t| t.wl_surface() == surface)) else {
            return;
        };
        let c = &self.clients[i];
        if c.fullscreen || c.saved_zoom.is_some() || !(c.floating || self.layouts[c.tag] == Layout::Floating) {
            return;
        }
        let size = with_states(surface, |states| {
            states.cached_state.get::<SurfaceCachedState>().current().geometry.map(|g| g.size)
        })
        .or_else(|| Some(bbox_from_surface_tree(surface, (0, 0)).size).filter(|s| s.w > 0 && s.h > 0));
        let Some(size) = size else {
            return;
        };
        // The window's size is the client's and its title bar's.
        let decor = if self.titled(i) { demiurge_core::titlebar::HEIGHT } else { 0 };
        let (w, h) = (size.w.max(1) as u32, size.h.max(1) as u32 + decor);
        if (c.w, c.h, c.decor) == (w, h, decor) {
            return;
        }
        let first = c.w == 0 && c.h == 0;
        let (wx, wy, ww, wh) = self.client_work_area(i);
        let c = &mut self.clients[i];
        c.w = w;
        c.h = h;
        c.decor = decor;
        if first {
            c.x = wx + (ww as i32 - w as i32) / 2;
            c.y = (wy + (wh as i32 - h as i32) / 2).max(wy);
        }
    }
}

impl XdgShellHandler for Wm {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn title_changed(&mut self, _surface: ToplevelSurface) {
        self.queue_redraw();
    }

    fn minimize_request(&mut self, surface: ToplevelSurface) {
        if let Some(id) = self.clients.iter().find(|c| c.toplevel == Toplevel::Xdg(surface.clone())).map(|c| c.id) {
            self.minimize(id);
        }
    }

    // A client's own title bar or edge moving or resizing its window while
    // a button is held, as a press on an X11 frame does.
    fn move_request(&mut self, surface: ToplevelSurface, _seat: wl_seat::WlSeat, _serial: Serial) {
        self.client_drag(&Toplevel::Xdg(surface), crate::drag::DragKind::Move);
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        _seat: wl_seat::WlSeat,
        _serial: Serial,
        edges: xdg_toplevel::ResizeEdge,
    ) {
        let bits = edges as u32;
        let edges = demiurge_core::drag::Edges { left: bits & 4 != 0, top: bits & 1 != 0, right: bits & 8 != 0, bottom: bits & 2 != 0 };
        self.client_drag(&Toplevel::Xdg(surface), crate::drag::DragKind::Resize(edges));
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        // Managed once placed (policy.rs): Its initial commit, with the
        // app_id and title it set before it, asks cherrypie where it goes,
        // and the first configure answers that commit once it is placed
        // (xdg-shell mandates that configure precede any buffer attach).
        // For Tile/Monocle the arrange() pass sends a tile-fit size; for
        // Floating with no geometry of cherrypie's, size=None lets the
        // client pick its own preferred size (matches the X11
        // default-window path).
        self.unplace(Toplevel::Xdg(surface), false);
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let toplevel = Toplevel::Xdg(surface);
        if !self.forget_unplaced(&toplevel) {
            self.unmanage(&toplevel);
        }
    }

    // Maximize is zoom: The client's own maximize button fills the work
    // area the way Super+= does. One not yet placed is answered by the
    // placement's first configure.
    fn maximize_request(&mut self, surface: ToplevelSurface) {
        if self.is_unplaced(&Toplevel::Xdg(surface.clone())) {
            return;
        }
        match self.zoom_target(&Toplevel::Xdg(surface.clone()), false) {
            Some(id) => self.zoom_in(id),
            // xdg-shell wants a configure in answer either way.
            None => {
                surface.send_configure();
            }
        }
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        if self.is_unplaced(&Toplevel::Xdg(surface.clone())) {
            return;
        }
        match self.zoom_target(&Toplevel::Xdg(surface.clone()), true) {
            Some(id) => self.zoom_out(id),
            None => {
                surface.send_configure();
            }
        }
    }

    // A client's own fullscreen request, as a game makes on start, is the
    // fullscreen the keybind toggles; one made before the window is placed
    // is kept until it is.
    fn fullscreen_request(&mut self, surface: ToplevelSurface, _output: Option<WlOutput>) {
        if self.unplaced_fullscreen(&Toplevel::Xdg(surface.clone()), true) {
            return;
        }
        match self.client_of(&Toplevel::Xdg(surface.clone())) {
            Some(id) => self.set_fullscreen(id, true),
            None => {
                surface.send_configure();
            }
        }
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        if self.unplaced_fullscreen(&Toplevel::Xdg(surface.clone()), false) {
            return;
        }
        match self.client_of(&Toplevel::Xdg(surface.clone())) {
            Some(id) => self.set_fullscreen(id, false),
            None => {
                surface.send_configure();
            }
        }
    }

    // Menus, dropdowns and tooltips: Placed by their positioner, kept on
    // their monitor, configured once their first commit arrives.
    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        self.unconstrain_popup(&surface);
        if let Err(e) = self.popups.track_popup(PopupKind::Xdg(surface)) {
            tracing::warn!("popup: {:?}", e);
        }
    }

    fn reposition_request(&mut self, surface: PopupSurface, positioner: PositionerState, token: u32) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    // A menu that takes the keyboard and pointer until it is dismissed: A
    // press outside its client closes it.
    fn grab(&mut self, surface: PopupSurface, _seat: wl_seat::WlSeat, serial: Serial) {
        let kind = PopupKind::Xdg(surface);
        let Ok(root) = find_popup_root_surface(&kind) else {
            return;
        };
        let seat = self.seat.clone();
        let Ok(mut grab) = self.popups.grab_popup(Focus::Surface(root), kind, &seat, serial) else {
            return;
        };
        if let Some(keyboard) = seat.get_keyboard() {
            if keyboard.is_grabbed()
                && !(keyboard.has_grab(serial) || keyboard.has_grab(grab.previous_serial().unwrap_or(serial)))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            keyboard.set_focus(self, grab.current_grab(), serial);
            keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
        }
        if let Some(pointer) = seat.get_pointer() {
            if pointer.is_grabbed()
                && !(pointer.has_grab(serial) || pointer.has_grab(grab.previous_serial().unwrap_or_else(|| grab.serial())))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, GrabFocus::Keep);
        }
    }
}

impl Wm {
    pub(crate) fn client_of(&self, toplevel: &Toplevel) -> Option<u64> {
        self.clients.iter().find(|c| &c.toplevel == toplevel).map(|c| c.id)
    }

    // Take a new window under management, on the tag cherrypie's placement
    // names or the focused monitor's: Front of the MRU, laid out, focused
    // so the user can type into it without moving the pointer, unless its
    // tag is out of view or cherrypie said not to. A floating window, or
    // one cherrypie puts above, goes where cherrypie put it, below the bar
    // as on X11; a tiled one keeps that geometry for when it floats.
    pub(crate) fn manage(
        &mut self,
        id: u64,
        toplevel: Toplevel,
        floating: bool,
        geometry: Rectangle<i32, Logical>,
        placement: Option<&cherrypie::api::Placement>,
    ) {
        // A tagged spawn's first window is used up either way, as on X11;
        // cherrypie's tag comes first.
        let class = toplevel.class();
        let spawned = self.pending_spawns.iter().position(|(c, _)| *c == class).map(|i| self.pending_spawns.remove(i).1);
        let tag = self.placed_tag(placement).or(spawned).unwrap_or(self.active_tags[self.focused_monitor]);
        let floating = floating || placement.and_then(|p| p.above) == Some(true);
        let asked = placement.and_then(|p| p.geometry).map(|r| (r.x, r.y.max(self.bar_height as i32), r.w, r.h));
        let (x, y, w, h) = match asked {
            Some(g) if floating => g,
            _ => (geometry.loc.x, geometry.loc.y, geometry.size.w.max(0) as u32, geometry.size.h.max(0) as u32),
        };
        self.clients.push(ManagedClient {
            id,
            toplevel,
            tag,
            x,
            y,
            w,
            h,
            fullscreen: false,
            floating,
            saved_floating: if floating { None } else { asked },
            saved_fullscreen: None,
            saved_zoom: None,
            minimized: false,
            decor: 0,
        });
        self.mru.on_focus(id);
        tracing::info!(client_id = id, tag, floating, "toplevel mapped");
        self.arrange();
        if floating && asked.is_some() {
            self.set_geometry(self.clients.len() - 1, x, y, w, h);
        }
        if self.active_tags.contains(&tag) && placement.and_then(|p| p.focus) != Some(false) {
            self.focus_client(id);
        }
    }

    // A window gone (destroyed, or unmapped on X11): The survivors on its
    // tag rebalance, and the keyboard moves to the next window there so it
    // is not left on a dead surface.
    pub(crate) fn unmanage(&mut self, toplevel: &Toplevel) {
        let Some(pos) = self.clients.iter().position(|c| &c.toplevel == toplevel) else {
            return;
        };
        let id = self.clients.remove(pos).id;
        self.mru.on_unmanage(id);
        if self.focused == Some(id) {
            self.focused = None;
        }
        self.policy.send("DELETE", &format!("/windows/{}", id as u32), ());
        tracing::info!(client_id = id, "toplevel gone");
        self.arrange();
        self.refocus_active_tag();
        self.queue_redraw();
    }

    // Keep a popup on its window's monitor: The positioner's own rules
    // (slide, flip, resize) applied against the monitor, given in the
    // coordinates of the popup's parent.
    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let kind = PopupKind::Xdg(popup.clone());
        let Ok(root) = find_popup_root_surface(&kind) else {
            return;
        };
        let Some(i) = self.clients.iter().position(|c| c.toplevel.wl_surface().as_ref() == Some(&root)) else {
            return;
        };
        let mon = self.client_monitor(i);
        let mut target = Rectangle::<i32, Logical>::new((mon.x, mon.y).into(), (mon.width as i32, mon.height as i32).into());
        target.loc -= Point::from((self.clients[i].x, self.clients[i].y));
        target.loc -= get_popup_toplevel_coords(&kind);
        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }
}

// The clipboard's Wayland side of the bridge to XWayland; xwm.rs holds the
// other side.
impl SelectionHandler for Wm {
    // Set when an X11 client owns the clipboard: Which XWayland.
    type SelectionUserData = XwmId;

    // A Wayland client set the clipboard: XWayland's clients see it too.
    fn new_selection(&mut self, target: SelectionTarget, source: Option<SelectionSource>, _seat: Seat<Self>) {
        if let Some(xwm) = self.xwm.as_mut() {
            if let Err(e) = xwm.new_selection(target, source.map(|s| s.mime_types())) {
                tracing::warn!("clipboard to XWayland: {}", e);
            }
        }
    }

    // A Wayland client pastes what an X11 client copied.
    fn send_selection(&mut self, target: SelectionTarget, mime_type: String, fd: OwnedFd, _seat: Seat<Self>, _xwm: &XwmId) {
        if let Some(xwm) = self.xwm.as_mut() {
            if let Err(e) = xwm.send_selection(target, mime_type, fd) {
                tracing::warn!("clipboard from XWayland: {}", e);
            }
        }
    }
}

impl DataDeviceHandler for Wm {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }
}

// ext-data-control: A client sets or reads the clipboard with no surface
// focused, as MEDUSA's clipboard holder does.
impl DataControlHandler for Wm {
    fn data_control_state(&mut self) -> &mut DataControlState {
        &mut self.runtime.as_mut().expect("data control is served only with a screen").data_control
    }
}

// A client's drag, started from the pointer press it was asked on: smithay's
// drag grab carries it over Wayland and X11 windows alike. Touch has no
// drag here, and the drag's icon is not drawn.
impl WaylandDndGrabHandler for Wm {
    fn dnd_requested<S: Source>(
        &mut self,
        source: S,
        _icon: Option<WlSurface>,
        seat: Seat<Self>,
        serial: Serial,
        type_: GrabType,
    ) {
        let dh = self.runtime.as_ref().map(|rt| rt.display.clone());
        let pointer = seat.get_pointer();
        let start = pointer.as_ref().and_then(|p| p.grab_start_data());
        let (Some(dh), GrabType::Pointer, Some(pointer), Some(start)) = (dh, type_, pointer, start) else {
            source.cancel();
            return;
        };
        pointer.set_grab(self, DnDGrab::new_pointer(&dh, start, source, seat), serial, GrabFocus::Keep);
    }
}

impl DndGrabHandler for Wm {}

impl ShmHandler for Wm {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl SeatHandler for Wm {
    type KeyboardFocus = Focus;
    type PointerFocus = Focus;
    type TouchFocus = Focus;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, _seat: &Seat<Self>, focused: Option<&Focus>) {
        // Track the focused client id and promote it in the MRU ring,
        // mirroring x11/src/wm.rs::focus_window. self.focused is the
        // target for focus-relative actions (close / fullscreen /
        // move_to_tag); the MRU front drives Alt+Tab cycling start. A
        // popup's grab focuses the popup; its window stays the target.
        let id = focused.and_then(|focus| match focus {
            Focus::X11(window) => self.client_of(&Toplevel::X11(window.clone())),
            Focus::Surface(surface) => {
                let root = self
                    .popups
                    .find_popup(surface)
                    .and_then(|popup| find_popup_root_surface(&popup).ok())
                    .unwrap_or_else(|| surface.clone());
                self.clients.iter().find(|c| c.toplevel.wl_surface().as_ref() == Some(&root)).map(|c| c.id)
            }
        });
        self.focused = id;
        // A cycle leaves the ring as it was until its modifier is let go.
        if let Some(id) = id.filter(|_| self.cycle.is_none()) {
            self.mru.on_focus(id);
        }
        // The window learns it is the active one, its client is offered the
        // clipboard, and an X11 one focused goes to the top of X's own stack.
        self.activate(id);
        self.clipboard_to(focused);
        if let (Some(Focus::X11(window)), Some(xwm)) = (focused, self.xwm.as_mut()) {
            let _ = xwm.raise_window(window);
        }
        let pid = match focused {
            Some(Focus::Surface(surface)) => surface.client().and_then(|c| c.get_data::<ClientState>().and_then(|s| s.pid)),
            _ => None,
        };
        self.steam_focus.focused(pid, self.runtime.as_ref().map_or(&[], |rt| rt.env.as_slice()));
        // The bar's title follows the keyboard.
        self.queue_redraw();
    }

    // A client under the pointer set its cursor: Its own surface, a name
    // (cursor-shape) or none.
    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.cursor_status = image;
        self.queue_redraw();
    }
}

// cursor-shape asks it of the seat; no tablet is handled beyond that.
impl TabletSeatHandler for Wm {}

impl BufferHandler for Wm {
    fn buffer_destroyed(
        &mut self,
        _buffer: &smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer,
    ) {
    }
}

impl OutputHandler for Wm {}

impl DmabufHandler for Wm {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    // A client's buffer is accepted only if the renderer can import it.
    fn dmabuf_imported(&mut self, _global: &DmabufGlobal, dmabuf: Dmabuf, notifier: ImportNotifier) {
        let imported = self.runtime.as_mut().is_some_and(|rt| rt.backend.import_dmabuf(&dmabuf));
        if imported {
            let _ = notifier.successful::<Wm>();
        } else {
            notifier.failed();
        }
    }
}

impl SessionLockHandler for Wm {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.session_lock_state
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        // Stash the SessionLocker. The render path calls
        // SessionLocker::lock() once a frame showing nothing but the
        // lock is on screen -- the compositor must guarantee no client
        // content is visible before acknowledging the lock.
        tracing::info!("session lock requested");
        self.close_tag_menu();
        self.locker = Some(confirmation.ext_session_lock().clone());
        self.relaunched = None;
        self.pending_lock = Some(confirmation);
        self.watch_locker();
        // The keyboard leaves the windows now, before any lock surface
        // exists to take it.
        self.keyboard_to(None);
        self.focused = None;
        self.queue_redraw();
    }

    fn unlock(&mut self) {
        // The locker called unlock_and_destroy. Drop the lock state;
        // the next render frame will composite normal clients again,
        // and the keyboard goes back to the layer surface holding it, else
        // to the window last used.
        tracing::info!("session unlocked");
        self.lock_active = false;
        self.lock_surfaces.clear();
        self.locker = None;
        match self.exclusive_layer.clone() {
            Some(surface) => self.keyboard_to(Some(Focus::Surface(surface))),
            None => self.refocus_active_tag(),
        }
        self.queue_redraw();
    }

    fn new_surface(&mut self, surface: LockSurface, output: WlOutput) {
        // Lock surfaces are sized to their full output. Set the size on
        // pending state, then send the configure -- the locker
        // attaches a buffer at this size on its first commit.
        let (w, h) = Output::from_resource(&output)
            .and_then(|o| o.current_mode())
            .map(|m| (m.size.w as u32, m.size.h as u32))
            .unwrap_or((self.monitors.first().width, self.monitors.first().height));
        surface.with_pending_state(|state| {
            state.size = Some((w, h).into());
        });
        surface.send_configure();
        // The keyboard goes to the lock, so a password typed reaches the
        // locker and nothing else.
        let first = self.lock_surfaces.is_empty();
        self.lock_surfaces.push((surface.clone(), output));
        if first {
            self.keyboard_to(Some(Focus::Surface(surface.wl_surface().clone())));
        }
        self.queue_redraw();
    }
}

impl XWaylandShellHandler for Wm {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland_shell_state
    }
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
delegate_presentation!(Wm);
delegate_dmabuf!(Wm);
delegate_cursor_shape!(Wm);
delegate_relative_pointer!(Wm);
delegate_pointer_constraints!(Wm);
delegate_viewporter!(Wm);
delegate_fifo!(Wm);
delegate_drm_syncobj!(Wm);

// What a buffer's fence or timeline point clearing does: The client's
// commits held behind it are applied.
fn unblock<M>(client: Client) -> impl FnMut((), &mut M, &mut Wm) -> std::io::Result<()> {
    move |_, _, wm| {
        if let Some(display) = wm.runtime.as_ref().map(|rt| rt.display.clone()) {
            wm.client_compositor_state(&client).blocker_cleared(wm, &display);
        }
        Ok(())
    }
}

impl DrmSyncobjHandler for Wm {
    fn drm_syncobj_state(&mut self) -> Option<&mut DrmSyncobjState> {
        self.runtime.as_mut()?.syncobj.as_mut()
    }
}
delegate_commit_timing!(Wm);
delegate_layer_shell!(Wm);
delegate_image_capture_source!(Wm);
delegate_output_capture_source!(Wm);
delegate_image_copy_capture!(Wm);
delegate_ext_data_control!(Wm);

impl PointerConstraintsHandler for Wm {
    // A constraint takes hold at once if its surface has the pointer, else
    // as the pointer enters it.
    fn new_constraint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>) {
        self.activate_constraint(surface, pointer);
    }

    // Where the client says its locked pointer is, relative to its surface,
    // is where the pointer is once the lock lets go.
    fn cursor_position_hint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>, location: Point<f64, Logical>) {
        if !with_pointer_constraint(surface, pointer, |c| c.is_some_and(|c| c.is_active())) {
            return;
        }
        if let Some(origin) = self.focused_origin(surface) {
            self.pointer_location = origin + location;
            pointer.set_location(self.pointer_location);
        }
    }
}
