// Layout enum lives in demiurge-core (server-agnostic). The arrange
// function below is X11-specific (issues ConfigureWindow on each
// client) so it stays here.

use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;

use crate::wm::Wm;
pub use demiurge_core::Layout;

impl Wm {
    // Cycle the layout for the focused monitor's active tag. Layouts
    // remain keyed on tag (not monitor-tag) -- a tag has at most one
    // monitor showing it at a time, so per-tag is sufficient and
    // matches the user's mental model ("this tag is tiled / floating").
    pub fn toggle_layout(&mut self) {
        let tag = self.active_tags[self.focused_monitor];
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

    // Per-monitor tiling. Iterates self.monitors, and for each monitor
    // tiles its active_tags entry's clients within that monitor's work
    // area. Pre-v0.6.0 this picked one monitor (heuristically the one
    // hosting the first client on a single global active_tag) and tiled
    // there only -- multi-monitor tiling was the README's most visible
    // limitation. Now each monitor lays out its own visible tag's
    // clients independently.
    //
    // Bar reservation: every monitor pays bar_height off the top
    // because every monitor has its own bar panel (see Bar::create).
    //
    // Floating-layout monitors only get the saved-geometry restore
    // pass, no tile/monocle math. Fullscreen and floating clients are
    // skipped by the indices filter below; they keep their own
    // explicit geometry.
    pub fn arrange(&mut self) {
        let bar_h = self.bar_height;
        let focused_win = self.focus;

        // Snapshot what each monitor is showing -- (work area, layout,
        // tag) triples -- so we can drop the borrow on self.monitors
        // before iterating clients (which borrow self mutably).
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
            // Indices of tilable clients on this monitor's tag.
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
                        continue;
                    }
                    // Save floating geometry before tiling.
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
                        continue;
                    }

                    let master_w = (work_w as f64 * self.master_ratio) as u32;
                    let stack_w = work_w - master_w;
                    let stack_count = indices.len() - 1;
                    let slice_h = work_h / stack_count as u32;

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
                    // Raise the focused window if it's on this monitor's
                    // monocle stack so it sits above its peers.
                    if let Some(win) = focused_win {
                        if indices
                            .iter()
                            .any(|&i| self.clients[i].window == win)
                        {
                            let _ = self.conn.configure_window(
                                win,
                                &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
                            );
                        }
                    }
                }
            }
        }

        let _ = self.conn.flush();
    }
}
