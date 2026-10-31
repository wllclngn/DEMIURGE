// DEMIURGE's half of cherrypie's window-policy API on Wayland
// (cherrypie::client), as the X11 stack speaks it: A new window is asked
// about before it is managed, and managed with the answer, or with
// DEMIURGE's own default when cherrypie is not running or does not answer
// within the client's deadline. An xdg toplevel is asked about at its
// initial commit, with the app_id and title it set before it, and its first
// configure answers that commit once it is placed; an X11 window is asked
// about at its map request and mapped once placed. Closed windows and the
// monitors go to cherrypie as they change.
//
// The socket is keyed by the compositor's own WAYLAND_DISPLAY, which the
// Wayland cherrypie serves on with no X connection: X11 windows are asked
// about over the API like Wayland ones, so placement needs no XWayland.

use std::os::fd::{FromRawFd, OwnedFd};

use cherrypie::api::{self, NewWindow, Placement, Rect, Tag};
use cherrypie::client::{Link, Policy};
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{Interest, Mode, PostAction};
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::Rectangle;
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::{SurfaceCachedState, XdgToplevelSurfaceData};
use smithay::xwayland::xwm::WmWindowType;

use crate::wm::{Toplevel, Wm};

/// A window known but not yet managed: An xdg toplevel before its initial
/// commit, or a window waiting on cherrypie's answer. A fullscreen it asks
/// for meanwhile is kept for when it is managed.
pub struct Unplaced {
    pub id: u64,
    pub toplevel: Toplevel,
    pub asked: bool,
    pub fullscreen: bool,
}

// _NET_WM_WINDOW_TYPE in cherrypie's spelling.
fn x11_type_name(kind: Option<WmWindowType>) -> &'static str {
    match kind {
        None | Some(WmWindowType::Normal) => "normal",
        Some(WmWindowType::Dialog) => "dialog",
        Some(WmWindowType::Toolbar) => "toolbar",
        Some(WmWindowType::Menu) => "menu",
        Some(WmWindowType::Utility) => "utility",
        Some(WmWindowType::Splash) => "splash",
        Some(_) => "unknown",
    }
}

impl Wm {
    /// cherrypie's API for the session, on the compositor's socket name.
    /// Its deadline timer goes into the event loop here, its socket as the
    /// link comes up.
    pub fn attach_policy(&mut self, display: &str) {
        self.policy = Policy::new(display);
        let Some(rt) = self.runtime.as_ref() else {
            return;
        };
        let fd = unsafe { libc::dup(self.policy.timer_fd()) };
        if fd < 0 {
            return;
        }
        let timer = unsafe { OwnedFd::from_raw_fd(fd) };
        let source = Generic::new(timer, Interest::READ, Mode::Level);
        if let Err(e) = rt.handle.insert_source(source, |_, _, wm| {
            wm.policy_overdue();
            Ok(PostAction::Continue)
        }) {
            tracing::warn!(error = %e, "policy: no deadline timer; windows place themselves");
        }
    }

    // The socket in the event loop, replacing a lost link's; the source goes
    // as the link does.
    fn watch_policy(&mut self) {
        let Some(rt) = self.runtime.as_ref() else {
            return;
        };
        if let Some(token) = self.policy_watch.take() {
            rt.handle.remove(token);
        }
        let fd = unsafe { libc::dup(self.policy.fd()) };
        if fd < 0 {
            return;
        }
        let stream = unsafe { OwnedFd::from_raw_fd(fd) };
        self.policy_watch = rt
            .handle
            .insert_source(Generic::new(stream, Interest::READ, Mode::Level), |_, _, wm| {
                wm.policy_answers();
                if wm.policy.is_up() {
                    Ok(PostAction::Continue)
                } else {
                    wm.policy_watch = None;
                    Ok(PostAction::Remove)
                }
            })
            .ok();
    }

    /// Answers off the socket: Each window answered is managed with its
    /// placement; on a lost link every waiting window with the default.
    pub fn policy_answers(&mut self) {
        for (window, placement) in self.policy.read() {
            self.admit(window as u64, placement);
        }
    }

    /// The windows whose answers are overdue, managed with the default.
    pub fn policy_overdue(&mut self) {
        for window in self.policy.expired() {
            self.admit(window as u64, None);
        }
    }

    /// A window to place before it is managed, its id minted now.
    pub(crate) fn unplace(&mut self, toplevel: Toplevel, fullscreen: bool) -> u64 {
        let id = self.mint_id();
        self.unplaced.push(Unplaced { id, toplevel, asked: false, fullscreen });
        id
    }

    /// An unplaced xdg toplevel's first commit: Ask about it.
    pub(crate) fn initial_commit(&mut self, surface: &WlSurface) {
        let id = self
            .unplaced
            .iter()
            .find(|u| !u.asked && u.toplevel.xdg().is_some_and(|t| t.wl_surface() == surface))
            .map(|u| u.id);
        if let Some(id) = id {
            self.ask_policy(id);
        }
    }

    /// Ask cherrypie where the unplaced window `id` goes; with no one to
    /// ask, it is managed at once with the default.
    pub(crate) fn ask_policy(&mut self, id: u64) {
        let asked = match self.policy.link() {
            Link::Down => false,
            link => {
                if matches!(link, Link::Fresh) {
                    self.watch_policy();
                    self.send_monitors();
                }
                let Some(info) = self.unplaced.iter().find(|u| u.id == id).map(|u| self.describe(u)) else {
                    return;
                };
                self.policy.ask(&info)
            }
        };
        if asked {
            if let Some(u) = self.unplaced.iter_mut().find(|u| u.id == id) {
                u.asked = true;
            }
        } else {
            self.admit(id, None);
        }
    }

    /// Whether `toplevel` waits to be placed.
    pub(crate) fn is_unplaced(&self, toplevel: &Toplevel) -> bool {
        self.unplaced.iter().any(|u| &u.toplevel == toplevel)
    }

    /// A fullscreen an unplaced window asks for, kept for when it is
    /// managed. False when the window is not unplaced.
    pub(crate) fn unplaced_fullscreen(&mut self, toplevel: &Toplevel, on: bool) -> bool {
        match self.unplaced.iter_mut().find(|u| &u.toplevel == toplevel) {
            Some(u) => {
                u.fullscreen = on;
                true
            }
            None => false,
        }
    }

    /// A window gone before it was placed: Its question withdrawn. False
    /// when the window was not unplaced.
    pub(crate) fn forget_unplaced(&mut self, toplevel: &Toplevel) -> bool {
        let Some(pos) = self.unplaced.iter().position(|u| &u.toplevel == toplevel) else {
            return false;
        };
        let u = self.unplaced.remove(pos);
        if u.asked {
            self.policy.forget(u.id as u32);
        }
        true
    }

    // Manage the unplaced window `id` with cherrypie's placement (None:
    // DEMIURGE's own default).
    fn admit(&mut self, id: u64, placement: Option<Placement>) {
        let Some(pos) = self.unplaced.iter().position(|u| u.id == id) else {
            return;
        };
        let u = self.unplaced.remove(pos);
        let fullscreen = u.fullscreen || placement.as_ref().and_then(|p| p.fullscreen) == Some(true);
        match u.toplevel {
            Toplevel::Xdg(toplevel) => {
                self.manage(id, Toplevel::Xdg(toplevel.clone()), false, Rectangle::default(), placement.as_ref());
                if fullscreen {
                    self.set_fullscreen(id, true);
                }
                // The initial commit is answered with a configure, which a
                // window placed out of view or floating may not have had.
                if !toplevel.is_initial_configure_sent() {
                    toplevel.send_configure();
                }
            }
            Toplevel::X11(window) => self.admit_x11(id, window, fullscreen, placement.as_ref()),
        }
        self.queue_redraw();
    }

    /// The tag cherrypie's placement names, by index or by name, if it is
    /// one of the tags.
    pub(crate) fn placed_tag(&self, placement: Option<&Placement>) -> Option<usize> {
        match placement?.tag.as_ref()? {
            Tag::Index(i) => Some(*i as usize),
            Tag::Name(name) => self.tag_names.iter().position(|t| t == name),
        }
        .filter(|&t| t < self.num_tags)
    }

    /// The monitors and their work areas, below the bar, for cherrypie.
    pub(crate) fn send_monitors(&mut self) {
        let monitors: Vec<api::Monitor> = self
            .monitors
            .iter()
            .map(|m| {
                let (x, y, w, h) = m.work_area(self.bar_height);
                api::Monitor {
                    name: m.name.clone(),
                    rect: Rect { x: m.x, y: m.y, w: m.width, h: m.height },
                    work_area: Rect { x, y, w, h },
                }
            })
            .collect();
        self.policy.send("PUT", "/monitors", monitors);
    }

    // The window as cherrypie needs to see it, before it is managed. An xdg
    // toplevel asks for no size before its first configure; the minimum it
    // set is the size it means. A toplevel with a parent is a dialog.
    fn describe(&self, u: &Unplaced) -> NewWindow {
        let (class, title, transient, window_type, requested, pid) = match &u.toplevel {
            Toplevel::Xdg(toplevel) => {
                let surface = toplevel.wl_surface();
                let (class, title, parent) = with_states(surface, |states| {
                    states.data_map.get::<XdgToplevelSurfaceData>().and_then(|d| d.lock().ok()).map_or(
                        (String::new(), String::new(), false),
                        |d| (d.app_id.clone().unwrap_or_default(), d.title.clone().unwrap_or_default(), d.parent.is_some()),
                    )
                });
                let min = with_states(surface, |states| states.cached_state.get::<SurfaceCachedState>().current().min_size);
                let pid = surface
                    .client()
                    .zip(self.runtime.as_ref())
                    .and_then(|(client, rt)| client.get_credentials(&rt.display).ok())
                    .map(|c| c.pid as u32);
                let kind = if parent { "dialog" } else { "normal" };
                (class, title, parent, kind, Rect { x: 0, y: 0, w: min.w.max(0) as u32, h: min.h.max(0) as u32 }, pid)
            }
            Toplevel::X11(window) => {
                let g = window.geometry();
                let requested = Rect { x: g.loc.x, y: g.loc.y, w: g.size.w.max(0) as u32, h: g.size.h.max(0) as u32 };
                let transient = window.is_transient_for().is_some();
                (window.class(), window.title(), transient, x11_type_name(window.window_type()), requested, window.pid())
            }
        };
        let process = pid
            .and_then(|p| std::fs::read_to_string(format!("/proc/{p}/comm")).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let monitor = self.monitors.get(self.focused_monitor).unwrap_or_else(|| self.monitors.first()).name.clone();
        NewWindow {
            window: u.id as u32,
            class,
            title,
            role: String::new(),
            process,
            window_type: window_type.into(),
            transient,
            requested,
            fullscreen: u.fullscreen,
            monitor,
        }
    }
}
