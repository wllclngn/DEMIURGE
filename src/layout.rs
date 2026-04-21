use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;

use crate::wm::Wm;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Layout {
    Floating,
    Tile,
    Monocle,
}

impl Layout {
    pub fn next(self) -> Layout {
        match self {
            Layout::Floating => Layout::Tile,
            Layout::Tile => Layout::Monocle,
            Layout::Monocle => Layout::Floating,
        }
    }

    pub fn from_str(s: &str) -> Layout {
        match s {
            "tile" => Layout::Tile,
            "monocle" => Layout::Monocle,
            _ => Layout::Floating,
        }
    }
}

impl Wm {
    pub fn toggle_layout(&mut self) {
        let tag = self.active_tag;
        self.layouts[tag] = self.layouts[tag].next();
        eprintln!(
            "[{}] [INFO]   layout {:?} on tag {}",
            crate::wm::local_time(),
            self.layouts[tag],
            tag,
        );
        self.arrange();
        // Layout name isn't displayed in the bar today; nothing to mark.
    }

    pub fn arrange(&mut self) {
        let layout = self.layouts[self.active_tag];
        // Pick the monitor that owns the first visible client on the active
        // tag, falling back to monitors[0]. Proper per-monitor tiling (where
        // each monitor lays out only its own clients) is a future redesign.
        let mon = self
            .clients
            .iter()
            .find(|c| c.tag == self.active_tag && !c.fullscreen)
            .map(|c| self.client_monitor(c))
            .unwrap_or(&self.monitors[0]);
        let work_x = mon.x;
        let work_y = mon.y + self.bar_height as i32;
        let work_w = mon.width;
        let work_h = mon.height - self.bar_height;

        // Collect indices of visible, non-fullscreen clients on active tag
        let indices: Vec<usize> = self
            .clients
            .iter()
            .enumerate()
            .filter(|(_, c)| c.tag == self.active_tag && !c.fullscreen && !c.floating)
            .map(|(i, _)| i)
            .collect();

        match layout {
            Layout::Floating => {
                // Restore saved floating geometry
                for &i in &indices {
                    if let Some((x, y, w, h)) = self.clients[i].saved_floating.take() {
                        self.clients[i].x = x;
                        self.clients[i].y = y;
                        self.clients[i].w = w;
                        self.clients[i].h = h;
                        let _ = self.conn.configure_window(
                            self.clients[i].window,
                            &ConfigureWindowAux::new()
                                .x(x)
                                .y(y)
                                .width(w)
                                .height(h),
                        );
                    }
                }
            }
            Layout::Tile => {
                if indices.is_empty() {
                    return;
                }
                // Save floating geometry before tiling
                for &i in &indices {
                    if self.clients[i].saved_floating.is_none() {
                        let c = &self.clients[i];
                        self.clients[i].saved_floating = Some((c.x, c.y, c.w, c.h));
                    }
                }

                if indices.len() == 1 {
                    let i = indices[0];
                    self.clients[i].x = work_x;
                    self.clients[i].y = work_y;
                    self.clients[i].w = work_w;
                    self.clients[i].h = work_h;
                    let _ = self.conn.configure_window(
                        self.clients[i].window,
                        &ConfigureWindowAux::new()
                            .x(work_x)
                            .y(work_y)
                            .width(work_w)
                            .height(work_h),
                    );
                    return;
                }

                let master_w = (work_w as f64 * self.master_ratio) as u32;
                let stack_w = work_w - master_w;
                let stack_count = indices.len() - 1;
                let slice_h = work_h / stack_count as u32;

                // Master
                let mi = indices[0];
                self.clients[mi].x = work_x;
                self.clients[mi].y = work_y;
                self.clients[mi].w = master_w;
                self.clients[mi].h = work_h;
                let _ = self.conn.configure_window(
                    self.clients[mi].window,
                    &ConfigureWindowAux::new()
                        .x(work_x)
                        .y(work_y)
                        .width(master_w)
                        .height(work_h),
                );

                // Stack
                for (j, &i) in indices[1..].iter().enumerate() {
                    let sx = work_x + master_w as i32;
                    let sy = work_y + (j as u32 * slice_h) as i32;
                    let sh = if j == stack_count - 1 {
                        work_h - j as u32 * slice_h
                    } else {
                        slice_h
                    };
                    self.clients[i].x = sx;
                    self.clients[i].y = sy;
                    self.clients[i].w = stack_w;
                    self.clients[i].h = sh;
                    let _ = self.conn.configure_window(
                        self.clients[i].window,
                        &ConfigureWindowAux::new()
                            .x(sx)
                            .y(sy)
                            .width(stack_w)
                            .height(sh),
                    );
                }
            }
            Layout::Monocle => {
                // Save floating geometry before monocle
                for &i in &indices {
                    if self.clients[i].saved_floating.is_none() {
                        let c = &self.clients[i];
                        self.clients[i].saved_floating = Some((c.x, c.y, c.w, c.h));
                    }
                }

                for &i in &indices {
                    self.clients[i].x = work_x;
                    self.clients[i].y = work_y;
                    self.clients[i].w = work_w;
                    self.clients[i].h = work_h;
                    let _ = self.conn.configure_window(
                        self.clients[i].window,
                        &ConfigureWindowAux::new()
                            .x(work_x)
                            .y(work_y)
                            .width(work_w)
                            .height(work_h),
                    );
                }
                // Raise focused
                if let Some(win) = self.focus {
                    let _ = self.conn.configure_window(
                        win,
                        &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
                    );
                }
            }
        }

        let _ = self.conn.flush();
    }
}
