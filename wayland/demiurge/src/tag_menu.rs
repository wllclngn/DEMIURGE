// Minimize and the tag menu under demiurge-wl, as on X11, with each
// window's icon. A minimized
// window keeps its tag but leaves the tiles, the screen, focus and Alt+Tab
// until the tag menu or its own activation brings it back. The tag menu is
// core's (demiurge_core::tag_menu), drawn above everything on its monitor:
// Clicking the tag in view on the bar, or pressing its key again, opens it.
// While it is open it holds the keyboard and the pointer, as the X11
// stack's grabs hold them: Up and Down move, Return or a click picks,
// Escape or a click anywhere else closes, and the pointer over an entry
// selects it.

use demiurge_core::tag_menu::{Entry, Icon, Menu};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::wayland::compositor::with_states;
use smithay::wayland::shm::with_buffer_contents;
use smithay::wayland::xdg_toplevel_icon::{ToplevelIconCachedState, XdgToplevelIconHandler};

use crate::drag::BTN_LEFT;
use crate::wm::{Toplevel, Wm};

pub struct TagMenu {
    pub menu: Menu,
    // Changed with every change to what the menu shows, for the texture the
    // bar keeps of it.
    pub version: u64,
}

// xdg-toplevel-icon-v1, behind quark-wayland's WM_SETICON: The icon a
// window sets is kept in its surface's state, read when the menu opens.
impl XdgToplevelIconHandler for Wm {}
smithay::delegate_xdg_toplevel_icon!(Wm);

// An icon from a shm buffer, as Cairo takes it; an opaque format made
// opaque.
fn shm_icon(buffer: &WlBuffer) -> Option<Icon> {
    with_buffer_contents(buffer, |ptr, len, data| {
        let opaque = match data.format {
            wl_shm::Format::Argb8888 => false,
            wl_shm::Format::Xrgb8888 => true,
            _ => return None,
        };
        let (w, h, stride, offset) = (data.width, data.height, data.stride, data.offset);
        if w <= 0 || h <= 0 || offset < 0 || stride < w * 4 || (offset + stride * h) as usize > len {
            return None;
        }
        let src = unsafe { std::slice::from_raw_parts(ptr.add(offset as usize), (stride * h) as usize) };
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        for row in 0..h as usize {
            pixels.extend_from_slice(&src[row * stride as usize..row * stride as usize + w as usize * 4]);
        }
        if opaque {
            pixels.chunks_exact_mut(4).for_each(|p| p[3] = 0xff);
        }
        Some(Icon { width: w, height: h, data: pixels })
    })
    .ok()
    .flatten()
}

impl Wm {
    // A window's icon: An xdg toplevel's biggest from xdg-toplevel-icon, an
    // X11 window's _NET_WM_ICON.
    fn icon_of(&self, toplevel: &Toplevel) -> Option<Icon> {
        match toplevel {
            Toplevel::Xdg(xdg) => {
                let buffers: Vec<WlBuffer> = with_states(xdg.wl_surface(), |states| {
                    states.cached_state.get::<ToplevelIconCachedState>().current().buffers().iter().map(|(b, _)| b.clone()).collect()
                });
                buffers.iter().filter_map(shm_icon).max_by_key(|icon| icon.width)
            }
            Toplevel::X11(window) => self.x11_reader.as_ref()?.icon(window.window_id()),
        }
    }

    /// Hide window `id` but keep it on its tag.
    pub fn minimize(&mut self, id: u64) {
        let Some(c) = self.clients.iter_mut().find(|c| c.id == id && !c.minimized) else {
            return;
        };
        c.minimized = true;
        if self.focused == Some(id) {
            self.refocus_active_tag();
        }
        self.arrange();
        self.queue_redraw();
    }

    /// Bring a minimized window `id` back to its tag.
    pub fn restore(&mut self, id: u64) {
        let Some(c) = self.clients.iter_mut().find(|c| c.id == id && c.minimized) else {
            return;
        };
        c.minimized = false;
        self.arrange();
        self.queue_redraw();
    }

    /// Open the tag menu under the tag `monitor` shows: From its key with the
    /// first entry selected, from a click with none. Nothing for an empty
    /// tag.
    pub fn open_tag_menu(&mut self, monitor: usize, from_key: bool) {
        if self.tag_menu.is_some() || self.locked() {
            return;
        }
        let (Some(style), Some(mon)) = (self.menu_style.as_ref(), self.monitors.get(monitor)) else {
            return;
        };
        let tag = self.active_tags[monitor];
        let entries: Vec<Entry> = self
            .clients
            .iter()
            .filter(|c| c.tag == tag)
            .map(|c| Entry { id: c.id, title: c.toplevel.title(), minimized: c.minimized, icon: self.icon_of(&c.toplevel) })
            .collect();
        if entries.is_empty() {
            return;
        }
        let tag_x = self.runtime.as_ref().and_then(|rt| rt.bar.tag_extents().get(tag)).map_or(0, |e| e.0);
        let at = (mon.x + tag_x, mon.y + self.bar_height as i32);
        match Menu::new(style, at, (mon.x, mon.width), entries, from_key.then_some(0)) {
            Ok(menu) => {
                self.menu_version += 1;
                self.tag_menu = Some(TagMenu { menu, version: self.menu_version });
                self.queue_redraw();
            }
            Err(e) => tracing::warn!("tag menu: {}", e),
        }
    }

    pub fn close_tag_menu(&mut self) {
        if self.tag_menu.take().is_some() {
            self.queue_redraw();
        }
    }

    // Close the menu and bring its entry `index` forward: Restored if
    // minimized, and focused.
    fn pick_tag_menu(&mut self, index: usize) {
        let Some(id) = self.tag_menu.as_ref().and_then(|m| m.menu.entries.get(index)).map(|e| e.id) else {
            return;
        };
        self.close_tag_menu();
        self.restore(id);
        self.focus_client(id);
        self.queue_redraw();
    }

    // The menu changed what it shows.
    fn tag_menu_changed(&mut self) {
        self.menu_version += 1;
        if let Some(open) = self.tag_menu.as_mut() {
            open.version = self.menu_version;
        }
        self.queue_redraw();
    }

    /// A key while the menu is open.
    pub(crate) fn tag_menu_key(&mut self, keysym: u32) {
        let Some(open) = self.tag_menu.as_mut() else {
            return;
        };
        match keysym {
            // Up, Down
            0xff52 => open.menu.step(false),
            0xff54 => open.menu.step(true),
            // Return, KP_Enter
            0xff0d | 0xff8d => {
                if let Some(i) = open.menu.selected {
                    self.pick_tag_menu(i);
                }
                return;
            }
            // Escape
            0xff1b => {
                self.close_tag_menu();
                return;
            }
            _ => return,
        }
        self.tag_menu_changed();
    }

    /// The pointer moved while the menu is open: The entry under it is
    /// selected.
    pub(crate) fn tag_menu_hover(&mut self) {
        let (x, y) = (self.pointer_location.x.floor() as i32, self.pointer_location.y.floor() as i32);
        let Some(open) = self.tag_menu.as_mut() else {
            return;
        };
        let hovered = open.menu.entry_at(x, y);
        if hovered.is_some() && hovered != open.menu.selected {
            open.menu.selected = hovered;
            self.tag_menu_changed();
        }
    }

    /// A press while the menu is open: On an entry it picks it, anywhere
    /// else it closes the menu.
    pub(crate) fn tag_menu_press(&mut self) {
        let (x, y) = (self.pointer_location.x.floor() as i32, self.pointer_location.y.floor() as i32);
        match self.tag_menu.as_ref().and_then(|m| m.menu.entry_at(x, y)) {
            Some(i) => self.pick_tag_menu(i),
            None => self.close_tag_menu(),
        }
    }

    /// A left press on a monitor's bar: The monitor becomes the focused one,
    /// and a tag clicked comes into view, or opens its menu if it is in view
    /// already. True when the press was the bar's.
    pub(crate) fn bar_press(&mut self, button: u32) -> bool {
        let (x, y) = (self.pointer_location.x.floor() as i32, self.pointer_location.y.floor() as i32);
        let Some(monitor) = self
            .monitors
            .iter()
            .position(|m| x >= m.x && x < m.x + m.width as i32 && y >= m.y && y < m.y + self.bar_height as i32)
        else {
            return false;
        };
        if self.fullscreen_shown(monitor) {
            return false;
        }
        if button != BTN_LEFT {
            return true;
        }
        let x = x - self.monitors[monitor].x;
        let Some(tag) = self.runtime.as_ref().and_then(|rt| rt.bar.tag_extents().iter().position(|&(s, e)| x >= s && x < e))
        else {
            return true;
        };
        self.focused_monitor = monitor;
        if self.active_tags[monitor] == tag {
            self.open_tag_menu(monitor, false);
        } else {
            self.view_tag(tag);
        }
        true
    }
}
