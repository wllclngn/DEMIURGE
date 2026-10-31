// Layout enum lives in demiurge-core (server-agnostic). The arrange
// function below is X11-specific (issues ConfigureWindow on each
// client) so it stays here.

use x11rb::connection::Connection;

use crate::wm::Wm;
pub use demiurge_core::Layout;

// Default placement for a floating window that asked for no position of
// its own: Its size clamped to the work area, centered in it.
pub fn center_in_work_area(
    work_x: i32,
    work_y: i32,
    work_w: u32,
    work_h: u32,
    w: u32,
    h: u32,
) -> (i32, i32, u32, u32) {
    let w = w.clamp(1, work_w.max(1));
    let h = h.clamp(1, work_h.max(1));
    (
        work_x + (work_w.saturating_sub(w) / 2) as i32,
        work_y + (work_h.saturating_sub(h) / 2) as i32,
        w,
        h,
    )
}

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
        self.sync_frames();
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
                    // Save floating geometry before tiling.
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
                    // Raise the focused window if it's on this monitor's
                    // monocle stack so it sits above its peers.
                    if let Some(win) = focused_win
                        && let Some(&i) = indices.iter().find(|&&i| self.clients[i].window == win)
                    {
                        self.raise(i);
                    }
                }
            }
        }

        let _ = self.conn.flush();
    }
}
