
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use crate::atoms::Atoms;
use crate::config::Config;
use crate::monitor::Monitor;
use demiurge_core::bar::{Painter, Panel, Rect, View};
use demiurge_core::prompt::{self, Prompt};

// One monitor's bar: Its dock window, and the pixels core's painter draws
// for it.
struct BarPanel {
    window: Window,
    gc: Gcontext,
    panel: Panel,
}

pub struct Bar {
    panels: Vec<BarPanel>,
    depth: u8,
    bg_pixel: u32,
    painter: Painter,
    pub path_executables: Vec<String>,
    pub prompt: Option<Prompt>,
    // An Expose asked for the panels' pixels again; the next commit puts
    // each one whole.
    exposed: bool,
}

// The _NET_WM_STRUT_PARTIAL reserving a `height` bar atop `mon`.
fn strut(mon: &Monitor, height: u32) -> [u32; 12] {
    [0, 0, height, 0, 0, 0, 0, 0, mon.x as u32, mon.x as u32 + mon.width - 1, 0, 0]
}

// One dock window per monitor, override-redirect and kept above, with the
// painter's panel for it. Used by both Bar::create (initial setup) and
// Bar::rebuild_panels (post-RandR).
fn build_panels(
    conn: &RustConnection,
    root: Window,
    atoms: &Atoms,
    depth: u8,
    bg_pixel: u32,
    painter: &Painter,
    monitors: &[Monitor],
) -> Result<Vec<BarPanel>, String> {
    let height = painter.height;
    let mut panels = Vec::with_capacity(monitors.len());

    for (i, mon) in monitors.iter().enumerate() {
        let window = conn.generate_id().map_err(|e| format!("bar window id: {}", e))?;
        let gc = conn.generate_id().map_err(|e| format!("bar gc id: {}", e))?;

        conn.create_window(
            depth,
            window,
            root,
            mon.x as i16,
            mon.y as i16,
            mon.width as u16,
            height as u16,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new()
                .override_redirect(1)
                .background_pixel(bg_pixel)
                .event_mask(EventMask::EXPOSURE | EventMask::BUTTON_PRESS),
        )
        .map_err(|e| format!("create bar window: {}", e))?;

        let _ = conn.change_property32(
            PropMode::REPLACE,
            window,
            atoms._NET_WM_WINDOW_TYPE,
            AtomEnum::ATOM,
            &[atoms._NET_WM_WINDOW_TYPE_DOCK],
        );
        let _ = conn.change_property32(
            PropMode::REPLACE,
            window,
            atoms._NET_WM_STRUT_PARTIAL,
            AtomEnum::CARDINAL,
            &strut(mon, height),
        );

        conn.create_gc(gc, window, &CreateGCAux::new())
            .map_err(|e| format!("create bar gc: {}", e))?;

        let _ = conn.map_window(window);
        let _ = conn.configure_window(
            window,
            &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
        );

        panels.push(BarPanel {
            window,
            gc,
            panel: painter.panel(mon.width)?,
        });

        eprintln!(
            "[demiurge] bar {}: {}x{} on '{}'",
            i, mon.width, height, mon.name,
        );
    }

    Ok(panels)
}

impl Bar {
    pub fn create(
        conn: &RustConnection,
        root: Window,
        atoms: &Atoms,
        config: &Config,
        monitors: &[Monitor],
    ) -> Result<Self, String> {
        let depth = conn.setup().roots[0].root_depth;
        let bg_pixel = crate::torrentius::hex_to_pixel(&config.bar.bg);
        let painter = Painter::new(config)?;
        let panels = build_panels(conn, root, atoms, depth, bg_pixel, &painter, monitors)?;
        let _ = conn.flush();

        let path_executables = prompt::scan_path();
        eprintln!(
            "[demiurge] bar: {} PATH executables",
            path_executables.len(),
        );

        Ok(Self {
            panels,
            depth,
            bg_pixel,
            painter,
            path_executables,
            prompt: None,
            exposed: false,
        })
    }

    pub fn contains_window(&self, window: Window) -> bool {
        self.panels.iter().any(|p| p.window == window)
    }

    // Index of the panel whose X window matches. Used by the bar
    // tag-click router so the click resolves to the monitor that owns
    // the clicked panel (and therefore the monitor whose active tag
    // should be changed). Panels are stored parallel to monitors --
    // panels[i] corresponds to monitors[i].
    pub fn panel_index(&self, window: Window) -> Option<usize> {
        self.panels.iter().position(|p| p.window == window)
    }

    // Each tag's span on the bar, left and right edge, for clicks and for
    // where the tag menu drops.
    pub fn tag_extents(&self) -> &[(i32, i32)] {
        self.painter.tag_extents()
    }

    pub fn raise_all(&self, conn: &RustConnection) {
        for panel in &self.panels {
            let _ = conn.configure_window(
                panel.window,
                &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
            );
        }
    }

    pub fn destroy_all(&self, conn: &RustConnection) {
        for panel in &self.panels {
            let _ = conn.destroy_window(panel.window);
        }
    }

    // Tear down every existing panel window and rebuild against a new
    // monitor list. Called by Wm::refresh_monitors after RandR reports
    // a layout change; handles count change (hot-plug) as well as size
    // change (xrandr -s). The new panels are drawn whole at the next commit.
    pub fn rebuild_panels(
        &mut self,
        conn: &RustConnection,
        root: Window,
        atoms: &Atoms,
        monitors: &[Monitor],
    ) -> Result<(), String> {
        for panel in &self.panels {
            let _ = conn.destroy_window(panel.window);
        }
        self.panels.clear();
        self.panels = build_panels(conn, root, atoms, self.depth, self.bg_pixel, &self.painter, monitors)?;
        let _ = conn.flush();
        Ok(())
    }

    // The clock read again on its 1 Hz tick; a tick inside the same
    // second changes nothing, so the next commit puts nothing for it.
    pub fn tick_clock(&mut self) {
        self.painter.tick_clock();
    }

    // The bar windows were exposed: The next commit puts every panel whole.
    pub fn expose(&mut self) {
        self.exposed = true;
    }

    // Called once per event-loop iteration. The painter draws each panel's
    // regions whose view changed since it last drew them, and those rects
    // alone are put; with nothing changed, nothing is drawn or sent.
    //
    // active_tags: per-panel active tag (parallel to self.panels), so each
    // monitor's bar highlights its own tag. The prompt and the title or
    // notification show on the primary panel only.
    pub fn commit(
        &mut self,
        conn: &RustConnection,
        active_tags: &[usize],
        occupied: &[bool],
        urgent: &[bool],
        center_text: &str,
        is_notification: bool,
    ) {
        let exposed = std::mem::take(&mut self.exposed);
        let mut put = false;
        for (pi, bp) in self.panels.iter_mut().enumerate() {
            // A RandR refresh racing this commit can leave active_tags
            // shorter than the panels; such a panel shows panel 0's tag.
            let active_tag = active_tags.get(pi).or(active_tags.first()).copied().unwrap_or(0);
            let primary = pi == 0;
            let view = View {
                active_tag,
                occupied,
                urgent,
                prompt: self.prompt.as_ref().filter(|_| primary),
                center: (primary && !center_text.is_empty()).then_some((center_text, is_notification)),
            };
            let painted = self.painter.paint(&mut bp.panel, &view);
            if exposed {
                let whole = Rect { x: 0, y: 0, w: bp.panel.width() as i32, h: self.painter.height as i32 };
                put |= put_region(bp, conn, whole, self.depth);
            } else {
                for rect in painted.rects(&bp.panel) {
                    put |= put_region(bp, conn, rect, self.depth);
                }
            }
        }
        if put {
            let _ = conn.flush();
        }
    }

    pub fn start_prompt(&mut self) {
        self.prompt = Some(Prompt::default());
    }

    // A reloaded config's colors, font, clock format and height: The
    // painter is made again from it, and every panel is drawn whole at the
    // next commit.
    pub fn update_appearance(
        &mut self,
        config: &Config,
        conn: &RustConnection,
        atoms: &Atoms,
        monitors: &[Monitor],
    ) -> Result<(), String> {
        let painter = Painter::new(config)?;
        if painter.height != self.painter.height {
            for (panel, mon) in self.panels.iter().zip(monitors.iter()) {
                let _ = conn.configure_window(
                    panel.window,
                    &ConfigureWindowAux::new().height(painter.height),
                );
                let _ = conn.change_property32(
                    PropMode::REPLACE,
                    panel.window,
                    atoms._NET_WM_STRUT_PARTIAL,
                    AtomEnum::CARDINAL,
                    &strut(mon, painter.height),
                );
            }
            let _ = conn.flush();
        }
        for bp in self.panels.iter_mut() {
            bp.panel = painter.panel(bp.panel.width())?;
        }
        self.bg_pixel = crate::torrentius::hex_to_pixel(&config.bar.bg);
        self.painter = painter;
        Ok(())
    }

    pub fn handle_prompt_key(&mut self, keysym: u32) -> Option<String> {
        let prompt = self.prompt.as_mut()?;
        match prompt.key(keysym, &self.path_executables) {
            prompt::Key::Run(cmd) => {
                self.prompt = None;
                Some(cmd)
            }
            prompt::Key::Closed => {
                self.prompt = None;
                None
            }
            prompt::Key::Open => None,
        }
    }
}

// Copy `rect` out of the panel's pixels, row by row out of their stride,
// and put it to the panel's window at (rect.x, rect.y). True when sent.
fn put_region(bp: &BarPanel, conn: &RustConnection, rect: Rect, depth: u8) -> bool {
    if rect.w <= 0 || rect.h <= 0 {
        return false;
    }
    let row_bytes = rect.w as usize * 4;
    bp.panel
        .with_pixels(|full, stride| {
            let mut buf: Vec<u8> = Vec::with_capacity(row_bytes * rect.h as usize);
            for row in rect.y as usize..(rect.y + rect.h) as usize {
                let start = row * stride + rect.x as usize * 4;
                buf.extend_from_slice(&full[start..start + row_bytes]);
            }
            let _ = conn.put_image(
                ImageFormat::Z_PIXMAP,
                bp.window,
                bp.gc,
                rect.w as u16,
                rect.h as u16,
                rect.x as i16,
                rect.y as i16,
                0,
                depth,
                &buf,
            );
        })
        .is_ok()
}

// Kept public because tests/bar.rs references bar::parse_hex directly.
// Delegates to torrentius where the actual implementation lives.
#[allow(dead_code)]
pub fn parse_hex(color: &str) -> (f64, f64, f64) {
    crate::torrentius::parse_hex(color)
}
