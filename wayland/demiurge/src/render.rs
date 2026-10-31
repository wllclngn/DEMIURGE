// One render path for both backends.
//
// Each output keeps a small redraw state: A redraw request renders on the
// loop's next idle turn. A frame that changed something waits for the
// backend to say it is on screen; one that changed nothing waits a
// refresh interval instead. Either way clients asking for frame
// callbacks are paced at the output's refresh, and a request made while
// a frame is on its way renders once that frame lands.
//
// Elements go to the backends front to back:
//   1. the cursor, which the real screen can lift onto the cursor plane
//   2. the bar, unless a fullscreen client owns the tag
//   3. X11 override-redirect windows (menus, tooltips)
//   4. the windows, xdg and X11 alike: fullscreen, then floating, then
//      tiled, each group most recently focused first, each window's
//      popups in front of it
// While locked, only the cursor and the lock surfaces are drawn.

use std::time::Duration;

use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::{
    Id, Kind, RenderElementPresentationState, RenderElementStates, default_primary_scanout_output_compare,
};
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::{Bind, Color32F, ExportMem, Offscreen, TextureMapping};
use smithay::desktop::utils::{
    OutputPresentationFeedback, send_dmabuf_feedback_surface_tree, send_frames_surface_tree,
    surface_presentation_feedback_flags_from_states, surface_primary_scanout_output,
    take_presentation_feedback_surface_tree, update_surface_primary_scanout_output, with_surfaces_surface_tree,
};
use smithay::input::pointer::{CursorImageStatus, CursorImageSurfaceData};
use smithay::output::Output;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback;
use smithay::reexports::wayland_server::backend::GlobalId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Buffer, Logical, Monotonic, Point, Rectangle, Size, Time, Transform};
use smithay::reexports::wayland_server::{Client, Resource};
use smithay::wayland::commit_timing::CommitTimerBarrierStateUserData;
use smithay::wayland::compositor::{CompositorHandler, with_states};
use smithay::wayland::fifo::FifoBarrierCachedState;
use smithay::wayland::dmabuf::DmabufFeedback;
use smithay::wayland::presentation::Refresh;
use smithay::wayland::shell::wlr_layer::Layer;

use demiurge_core::Layout;
use demiurge_wl_screen::color::HdrMetadata;

use crate::backend::{Backend, Runtime};
use crate::bar::Bar;
use crate::cursor::Cursor;
use crate::dsr::Dsr;
use crate::wm::{ManagedClient, Wm};

smithay::backend::renderer::element::render_elements! {
    pub DemiurgeElement<=GlesRenderer>;
    Surface=WaylandSurfaceRenderElement<GlesRenderer>,
    Texture=TextureRenderElement<GlesTexture>,
    Memory=MemoryRenderBufferRenderElement<GlesRenderer>,
}

const DESKTOP: Color32F = Color32F::new(0.07, 0.07, 0.07, 1.0);
const LOCKED: Color32F = Color32F::new(0.0, 0.0, 0.0, 1.0);
// A client off screen still gets a frame callback this often, so one
// blocked on it does not stall forever.
const HIDDEN_FRAME_INTERVAL: Duration = Duration::from_secs(1);

pub struct OutputFrame {
    pub output: Output,
    pub global: GlobalId,
    // The linux-dmabuf feedback a fullscreen window here gets, where the
    // monitor has a plane to give it.
    pub scanout: Option<DmabufFeedback>,
    // What this output shows of the lock: The frame on its way shows only
    // it, and a frame on screen already does.
    lock_shown: bool,
    redraw: Redraw,
    // Feedback for the frame on its way to the screen.
    feedback: Option<OutputPresentationFeedback>,
    // The frame on its way shows the lock and nothing else.
    locked: bool,
}

enum Redraw {
    Idle,
    Queued,
    // A frame is on its way; `again` asks for another once it lands.
    Pending { again: bool },
}

impl OutputFrame {
    pub fn new(output: Output, global: GlobalId) -> Self {
        Self { output, global, scanout: None, redraw: Redraw::Idle, feedback: None, locked: false, lock_shown: false }
    }
}

enum Shown {
    Queued(RenderElementStates),
    Empty(RenderElementStates),
    Inactive,
}

impl Wm {
    pub fn queue_redraw(&mut self) {
        let Some(rt) = self.runtime.as_ref() else {
            return;
        };
        let outputs: Vec<Output> = rt.frames.iter().map(|f| f.output.clone()).collect();
        for output in outputs {
            self.queue_redraw_output(&output);
        }
    }

    fn queue_redraw_output(&mut self, output: &Output) {
        let Some(rt) = self.runtime.as_mut() else {
            return;
        };
        let Some(frame) = rt.frames.iter_mut().find(|f| &f.output == output) else {
            return;
        };
        match &mut frame.redraw {
            Redraw::Idle => {
                frame.redraw = Redraw::Queued;
                let output = output.clone();
                rt.handle.insert_idle(move |wm| wm.render_output(&output));
            }
            Redraw::Queued => {}
            Redraw::Pending { again } => *again = true,
        }
    }

    fn render_output(&mut self, output: &Output) {
        let Some(mut rt) = self.runtime.take() else {
            return;
        };
        let locked = self.lock_active || self.pending_lock.is_some();
        let shown = draw(&mut rt, self, output, locked);
        let time = rt.clock.now();
        let roots = self.surface_roots();
        let Some(frame) = rt.frames.iter_mut().find(|f| &f.output == output) else {
            self.runtime = Some(rt);
            return;
        };
        let mut confirm = false;
        let latched = matches!(shown, Ok(Shown::Queued(_) | Shown::Empty(_)));
        let changed = matches!(shown, Ok(Shown::Queued(_)));
        match shown {
            Ok(Shown::Queued(states)) => {
                rt.backend.set_hdr(output, self.hdr_wanted(output, &states));
                self.send_frames(output, &states, time, &roots);
                if let (Some(scanout), Some(dmabuf)) = (&frame.scanout, &rt.dmabuf) {
                    self.send_dmabuf_feedback(output, scanout, &dmabuf.feedback);
                }
                frame.feedback = Some(self.take_feedback(output, &states, &roots));
                frame.locked = locked;
                frame.lock_shown &= locked;
                frame.redraw = Redraw::Pending { again: false };
            }
            Ok(Shown::Empty(states)) => {
                // Nothing changed, so what is on screen already shows
                // the lock if the lock is wanted.
                frame.lock_shown = locked;
                confirm = locked;
                self.send_frames(output, &states, time, &roots);
                if let (Some(scanout), Some(dmabuf)) = (&frame.scanout, &rt.dmabuf) {
                    self.send_dmabuf_feedback(output, scanout, &dmabuf.feedback);
                }
                frame.feedback = Some(self.take_feedback(output, &states, &roots));
                frame.locked = false;
                frame.redraw = Redraw::Pending { again: false };
                let interval = refresh_interval(output);
                let output = output.clone();
                let timer = rt.handle.insert_source(Timer::from_duration(interval), move |_, _, wm| {
                    wm.frame_done(&output, None);
                    TimeoutAction::Drop
                });
                if let Err(e) = timer {
                    tracing::warn!("frame timer: {}", e);
                    frame.redraw = Redraw::Idle;
                }
            }
            Ok(Shown::Inactive) => frame.redraw = Redraw::Idle,
            Err(e) => {
                tracing::warn!(output = %output.name(), "render: {}", e);
                frame.redraw = Redraw::Idle;
            }
        }
        // `locked` goes out once every output shows the lock.
        confirm &= rt.frames.iter().all(|f| f.lock_shown);
        self.runtime = Some(rt);
        if latched {
            self.release_pacing(&roots, true, None);
        }
        if changed {
            self.refresh_pointer_focus(time.as_millis());
        }
        if confirm {
            self.confirm_lock();
        }
    }

    // The frame last shown on `output` reached the screen: At the given
    // vblank, or, with none, after the interval that stands in for one.
    pub fn frame_done(&mut self, output: &Output, presented: Option<(Time<Monotonic>, u64)>) {
        let roots = self.surface_roots();
        let Some(rt) = self.runtime.as_mut() else {
            return;
        };
        let hardware = matches!(rt.backend, Backend::Screen(_));
        let now = rt.clock.now();
        let Some(frame) = rt.frames.iter_mut().find(|f| &f.output == output) else {
            return;
        };
        if let Some(mut feedback) = frame.feedback.take() {
            let refresh = Refresh::Fixed(refresh_interval(output));
            match presented {
                Some((time, seq)) if hardware => feedback.presented(
                    time,
                    refresh,
                    seq,
                    wp_presentation_feedback::Kind::Vsync
                        | wp_presentation_feedback::Kind::HwClock
                        | wp_presentation_feedback::Kind::HwCompletion,
                ),
                Some((time, seq)) => feedback.presented(time, refresh, seq, wp_presentation_feedback::Kind::empty()),
                None => feedback.presented(now, refresh, 0, wp_presentation_feedback::Kind::empty()),
            }
        }
        if std::mem::take(&mut frame.locked) && presented.is_some() {
            frame.lock_shown = true;
        }
        let again = matches!(frame.redraw, Redraw::Pending { again: true });
        frame.redraw = Redraw::Idle;
        let confirm = rt.frames.iter().all(|f| f.lock_shown);
        let next = presented.map_or(now, |(time, _)| time) + refresh_interval(output);
        self.release_pacing(&roots, false, Some(next));
        if confirm {
            self.confirm_lock();
        }
        // An update held for a later refresh keeps the refresh cycle going
        // until its own comes, whether or not anything else draws.
        if again || self.timed_updates_held(&roots) {
            self.queue_redraw_output(output);
        }
    }

    // Whether any surface holds a content update behind a commit timer.
    fn timed_updates_held(&self, roots: &[WlSurface]) -> bool {
        let mut held = false;
        for root in roots {
            with_surfaces_surface_tree(root, |_, states| {
                held |= states
                    .data_map
                    .get::<CommitTimerBarrierStateUserData>()
                    .is_some_and(|timers| timers.lock().unwrap().next_deadline().is_some());
            });
        }
        held
    }

    // Frame pacing. As a frame is drawn, it latches every surface's current
    // content update: Each FIFO barrier one set is signalled, so the update
    // waiting on it goes to the next frame. As a frame reaches the screen,
    // each commit timer due by `next`, the next refresh, lets its update
    // through. Their clients then apply what was held.
    fn release_pacing(&mut self, roots: &[WlSurface], latched: bool, next: Option<Time<Monotonic>>) {
        let mut released: Vec<Client> = Vec::new();
        for root in roots {
            with_surfaces_surface_tree(root, |surface, states| {
                let barrier = latched.then(|| states.cached_state.get::<FifoBarrierCachedState>().current().barrier.take()).flatten();
                if let Some(barrier) = &barrier {
                    barrier.signal();
                }
                let timer = next.is_some_and(|next| {
                    states
                        .data_map
                        .get::<CommitTimerBarrierStateUserData>()
                        .is_some_and(|timers| timers.lock().unwrap().signal_until(next))
                });
                if barrier.is_some() || timer {
                    if let Some(client) = surface.client().filter(|c| !released.contains(c)) {
                        released.push(client);
                    }
                }
            });
        }
        let Some(dh) = self.runtime.as_ref().map(|rt| rt.display.clone()) else {
            return;
        };
        for client in released {
            self.client_compositor_state(&client).blocker_cleared(self, &dh);
        }
    }

    // ext-session-lock: `locked` goes out only once a frame showing
    // nothing but the lock is on screen.
    fn confirm_lock(&mut self) {
        if let Some(pending) = self.pending_lock.take() {
            self.lock_active = true;
            pending.lock();
            tracing::info!("session lock confirmed");
        }
    }

    // Every surface tree a frame could show, visible or not.
    fn surface_roots(&self) -> Vec<WlSurface> {
        let mut roots: Vec<WlSurface> = self.clients.iter().filter_map(|c| c.toplevel.wl_surface()).collect();
        roots.extend(self.clients.iter().flat_map(|c| c.popups()).map(|(popup, _)| popup));
        roots.extend(self.x11_surfaces.iter().filter(|x| x.is_override_redirect()).filter_map(|x| x.wl_surface()));
        roots.extend(self.lock_surfaces.iter().map(|(s, _)| s.wl_surface().clone()));
        roots.extend(self.ime_popups.iter().filter(|p| p.alive()).map(|p| p.wl_surface().clone()));
        roots.extend(self.layer_roots());
        if let CursorImageStatus::Surface(surface) = &self.cursor_status {
            roots.push(surface.clone());
        }
        roots
    }

    fn send_frames(&self, output: &Output, states: &RenderElementStates, time: Time<Monotonic>, roots: &[WlSurface]) {
        for root in roots {
            with_surfaces_surface_tree(root, |surface, data| {
                update_surface_primary_scanout_output(surface, output, data, None, states, default_primary_scanout_output_compare);
            });
            send_frames_surface_tree(root, output, time, Some(HIDDEN_FRAME_INTERVAL), surface_primary_scanout_output);
        }
    }

    // linux-dmabuf per surface, sent only when it changes: A fullscreen
    // window on `output` is told the formats the monitor's primary plane
    // scans out, every other surface the default.
    fn send_dmabuf_feedback<'a>(&self, output: &Output, scanout: &'a DmabufFeedback, default: &'a DmabufFeedback) {
        for c in &self.clients {
            let Some(root) = c.toplevel.wl_surface() else {
                continue;
            };
            let fullscreen = c.fullscreen && self.is_visible(c);
            send_dmabuf_feedback_surface_tree(&root, output, surface_primary_scanout_output, |surface, _| {
                if fullscreen && *surface == root { scanout } else { default }
            });
        }
    }

    fn take_feedback(&self, output: &Output, states: &RenderElementStates, roots: &[WlSurface]) -> OutputPresentationFeedback {
        let mut feedback = OutputPresentationFeedback::new(output);
        for root in roots {
            take_presentation_feedback_surface_tree(root, &mut feedback, surface_primary_scanout_output, |surface, _| {
                surface_presentation_feedback_flags_from_states(surface, None, states)
            });
        }
        feedback
    }

    // HDR10 for the frame just queued on `output`: Its front window fullscreen, its
    // buffer on the primary plane as it is and its description PQ over
    // BT.2020. A composited frame is SDR, as the compositor composites.
    fn hdr_wanted(&self, output: &Output, states: &RenderElementStates) -> Option<HdrMetadata> {
        let tag = self.active_tags[self.monitor_of(output)];
        let front = self.stacking().into_iter().find(|c| c.tag == tag).filter(|c| c.fullscreen)?;
        let surface = front.toplevel.wl_surface()?;
        let state = states.element_render_state(Id::from_wayland_resource(&surface))?;
        if state.presentation_state != RenderElementPresentationState::ZeroCopy {
            return None;
        }
        crate::color::hdr10(&surface)
    }

    // The windows on visible tags, front to back.
    pub(crate) fn stacking(&self) -> Vec<&ManagedClient> {
        let rank = |c: &ManagedClient| {
            if c.fullscreen {
                0
            } else if c.floating || self.layouts[c.tag] == Layout::Floating {
                1
            } else {
                2
            }
        };
        let recency = |c: &ManagedClient| self.mru.as_slice().iter().position(|&id| id == c.id).unwrap_or(usize::MAX);
        let mut visible: Vec<&ManagedClient> = self.clients.iter().filter(|c| self.is_visible(c)).collect();
        visible.sort_by_key(|c| (rank(c), recency(c)));
        visible
    }
}

impl Wm {
    /// The Screenshot action: Every monitor as the screen shows it, placed
    /// as they sit, into ~/Pictures/screenshot-<time>.png, as the X11 stack
    /// writes its root.
    pub fn screenshot(&mut self) {
        let Some(mut rt) = self.runtime.take() else {
            return;
        };
        let outputs: Vec<Output> = rt.frames.iter().map(|f| f.output.clone()).collect();
        let shot = compose(&mut rt, self, &outputs);
        self.runtime = Some(rt);
        let path = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
            .join("Pictures")
            .join(format!("screenshot-{}.png", demiurge_core::clock::now("%Y%m%d-%H%M%S")));
        let written = shot.and_then(|surface| {
            let mut file = std::fs::File::create(&path).map_err(|e| format!("{}: {}", path.display(), e))?;
            surface.write_to_png(&mut file).map_err(|e| format!("png: {}", e))
        });
        match written {
            Ok(()) => tracing::info!(path = %path.display(), "screenshot"),
            Err(e) => tracing::warn!("screenshot failed: {}", e),
        }
    }
}

// The outputs' frames on one surface spanning them, each at its place.
fn compose(rt: &mut Runtime, wm: &Wm, outputs: &[Output]) -> Result<cairo::ImageSurface, String> {
    let places: Vec<(i32, i32)> = outputs.iter().map(|o| (o.current_location().x, o.current_location().y)).collect();
    let mut frames = Vec::new();
    for output in outputs {
        frames.push(capture(rt, wm, output, true)?);
    }
    let left = places.iter().map(|p| p.0).min().unwrap_or(0);
    let top = places.iter().map(|p| p.1).min().unwrap_or(0);
    let right = places.iter().zip(&frames).map(|(p, f)| p.0 + f.width()).max().unwrap_or(0);
    let bottom = places.iter().zip(&frames).map(|(p, f)| p.1 + f.height()).max().unwrap_or(0);
    let canvas = cairo::ImageSurface::create(cairo::Format::Rgb24, right - left, bottom - top)
        .map_err(|e| format!("screenshot surface: {}", e))?;
    let cr = cairo::Context::new(&canvas).map_err(|e| format!("screenshot context: {}", e))?;
    for (place, frame) in places.iter().zip(&frames) {
        cr.set_source_surface(frame, (place.0 - left) as f64, (place.1 - top) as f64).map_err(|e| e.to_string())?;
        cr.paint().map_err(|e| e.to_string())?;
    }
    drop(cr);
    Ok(canvas)
}

impl Wm {
    /// `output`'s frame as the screen shows it, the cursor drawn or not.
    pub(crate) fn capture_output(&mut self, output: &Output, with_cursor: bool) -> Result<cairo::ImageSurface, String> {
        let mut rt = self.runtime.take().ok_or("no screen to capture")?;
        let shot = capture(&mut rt, self, output, with_cursor);
        self.runtime = Some(rt);
        shot
    }
}

// One output's frame as the screen shows it: Its elements rendered into a
// texture of their own and read back from the GPU.
fn capture(rt: &mut Runtime, wm: &Wm, output: &Output, with_cursor: bool) -> Result<cairo::ImageSurface, String> {
    let size = output.current_mode().map(|m| m.size).ok_or("output has no mode")?;
    let locked = wm.locked();
    let clear = if locked { LOCKED } else { DESKTOP };
    let renderer = match &mut rt.backend {
        Backend::Window(window) => window.renderer(),
        Backend::Screen(screen) => screen.renderer(output).ok_or("no GPU drives this output")?,
    };
    let elements = elements(renderer, wm, &mut rt.bar, &mut rt.dsr, &mut rt.cursor, output, locked, with_cursor)?;
    let buffer_size = Size::<i32, Buffer>::from((size.w, size.h));
    let mut texture: GlesTexture = renderer.create_buffer(Fourcc::Abgr8888, buffer_size).map_err(|e| format!("capture buffer: {}", e))?;
    let mut target = renderer.bind(&mut texture).map_err(|e| format!("capture bind: {}", e))?;
    let mut tracker = OutputDamageTracker::new(size, 1.0, Transform::Normal);
    tracker.render_output(renderer, &mut target, 0, &elements, clear).map_err(|e| format!("capture render: {:?}", e))?;
    let mapping = renderer
        .copy_framebuffer(&target, Rectangle::from_size(buffer_size), Fourcc::Abgr8888)
        .map_err(|e| format!("capture read back: {}", e))?;
    let flipped = mapping.flipped();
    let rgba = renderer.map_texture(&mapping).map_err(|e| format!("capture map: {}", e))?;

    // R, G, B, A in memory to Cairo's B, G, R, A, top row first. An
    // unflipped mapping counts its rows from the lower left, as GL does.
    let (w, h) = (size.w as usize, size.h as usize);
    let mut surface = cairo::ImageSurface::create(cairo::Format::Rgb24, size.w, size.h).map_err(|e| e.to_string())?;
    let stride = surface.stride() as usize;
    {
        let mut data = surface.data().map_err(|e| e.to_string())?;
        for y in 0..h {
            let from = if flipped { y } else { h - 1 - y };
            for x in 0..w {
                let p = &rgba[(from * w + x) * 4..(from * w + x) * 4 + 4];
                data[y * stride + x * 4..y * stride + x * 4 + 4].copy_from_slice(&[p[2], p[1], p[0], 0xff]);
            }
        }
    }
    Ok(surface)
}

fn draw(rt: &mut Runtime, wm: &Wm, output: &Output, locked: bool) -> Result<Shown, String> {
    // Powered off for idleness: Nothing is drawn until input turns it on.
    if wm.outputs_off {
        return Ok(Shown::Inactive);
    }
    let clear = if locked || wm.blanked { LOCKED } else { DESKTOP };
    match &mut rt.backend {
        Backend::Window(window) => {
            let elements = elements(window.renderer(), wm, &mut rt.bar, &mut rt.dsr, &mut rt.cursor, output, locked, true)?;
            Ok(match window.render(&elements, clear)? {
                demiurge_wl_window::Rendered::Submitted(states) => Shown::Queued(states),
                demiurge_wl_window::Rendered::Empty(states) => Shown::Empty(states),
            })
        }
        Backend::Screen(screen) => {
            let renderer = screen.renderer(output).ok_or("no GPU drives this output")?;
            let elements = elements(renderer, wm, &mut rt.bar, &mut rt.dsr, &mut rt.cursor, output, locked, true)?;
            Ok(match screen.render(output, &elements, clear)? {
                demiurge_wl_screen::Rendered::Queued(states) => Shown::Queued(states),
                demiurge_wl_screen::Rendered::Empty(states) => Shown::Empty(states),
                demiurge_wl_screen::Rendered::Inactive => Shown::Inactive,
            })
        }
    }
}

// Everything `output` shows this frame, front to back.
fn elements(
    renderer: &mut GlesRenderer,
    wm: &Wm,
    bar: &mut Bar,
    dsr: &mut Dsr,
    cursor: &mut Cursor,
    output: &Output,
    locked: bool,
    with_cursor: bool,
) -> Result<Vec<DemiurgeElement>, String> {
    let mut out = Vec::new();
    // Blanked for idleness: Black, with nothing on it.
    if wm.blanked {
        return Ok(out);
    }
    // Everything is placed in the output's own coordinates, from its
    // origin in the layout.
    let origin = output.current_location();
    let monitor = wm.monitor_of(output);
    let at = (wm.pointer_location - origin.to_f64()).to_physical(1.0);
    match &wm.cursor_status {
        _ if wm.cursor_hidden || !with_cursor => {}
        CursorImageStatus::Hidden => {}
        CursorImageStatus::Named(icon) => out.extend(cursor.element(renderer, *icon, at).map(DemiurgeElement::Memory)),
        CursorImageStatus::Surface(surface) => {
            let hotspot = with_states(surface, |states| {
                states
                    .data_map
                    .get::<CursorImageSurfaceData>()
                    .map(|attributes| attributes.lock().unwrap().hotspot)
            })
            .unwrap_or_default();
            let origin: Point<i32, _> = (at - hotspot.to_f64().to_physical(1.0)).to_i32_round();
            out.extend(
                render_elements_from_surface_tree(renderer, surface, origin, 1.0, 1.0, Kind::Cursor)
                    .into_iter()
                    .map(DemiurgeElement::Surface),
            );
        }
    }
    // The overlay layer, in front of all but the cursor; none while locked.
    if !locked {
        out.extend(crate::layer::elements(renderer, output, &[Layer::Overlay]));
    }
    // The Alt+Tab overlay on the focused monitor while a cycle lasts.
    if let (Some(cycle), true, false) = (&wm.cycle, wm.alt_tab_preview, locked) {
        if monitor == wm.focused_monitor {
            let mon = &wm.monitors[monitor];
            out.extend(crate::cycle::overlay(renderer, wm, bar, cycle, (mon.x, mon.y, mon.width, mon.height), origin)?);
        }
    }
    // The tag menu, above everything but the cursor.
    if let (Some(open), Some(style)) = (&wm.tag_menu, &wm.menu_style) {
        let at = (open.menu.x - origin.x, open.menu.y - origin.y);
        out.push(DemiurgeElement::Texture(bar.menu_element(renderer, at, open, style)?));
    }
    if locked {
        out.extend(
            wm.lock_surfaces
                .iter()
                .filter(|(_, o)| output.owns(o))
                .flat_map(|(s, _)| {
                    render_elements_from_surface_tree(renderer, s.wl_surface(), (0, 0), 1.0, 1.0, Kind::Unspecified)
                })
                .map(DemiurgeElement::Surface),
        );
        return Ok(out);
    }
    if bar.height() > 0 && !wm.fullscreen_shown(monitor) {
        let mon = &wm.monitors[monitor];
        let center = if monitor == 0 { wm.bar_center() } else { None };
        let occupied = wm.occupied_tags();
        let view = demiurge_core::bar::View {
            active_tag: wm.active_tags[monitor],
            occupied: &occupied,
            urgent: &[],
            prompt: wm.prompt.as_ref().filter(|_| monitor == 0),
            center: center.as_ref().map(|(text, notification)| (text.as_str(), *notification)),
        };
        let element = bar.element(renderer, monitor, (0, 0), mon.width, &view).map_err(|e| format!("bar: {}", e))?;
        out.push(DemiurgeElement::Texture(element));
    }
    // An input method's popups, above the windows.
    for popup in wm.ime_popups.iter().filter(|p| p.alive()) {
        if let Some(at) = wm.ime_popup_place(popup) {
            let at = (at - origin).to_physical(1);
            out.extend(
                render_elements_from_surface_tree(renderer, popup.wl_surface(), at, 1.0, 1.0, Kind::Unspecified)
                    .into_iter()
                    .map(DemiurgeElement::Surface),
            );
        }
    }
    out.extend(crate::layer::elements(renderer, output, wm.top_layers(output)));
    if dsr.active() {
        let size = output.current_mode().map(|m| m.size).ok_or("output has no mode")?;
        let scene = scene(renderer, wm, bar, dsr.multiplier, origin)?;
        out.push(DemiurgeElement::Texture(dsr.downscale(renderer, size, &scene)?));
    } else {
        out.extend(scene(renderer, wm, bar, 1.0, origin)?);
    }
    out.extend(crate::layer::elements(renderer, output, &[Layer::Bottom, Layer::Background]));
    Ok(out)
}

// The windows and their title bars, front to back, at `scale` times their
// size, placed from the output's origin.
fn scene(
    renderer: &mut GlesRenderer,
    wm: &Wm,
    bar: &mut Bar,
    scale: f64,
    origin: Point<i32, Logical>,
) -> Result<Vec<DemiurgeElement>, String> {
    let at = |x: i32, y: i32| (((x - origin.x) as f64 * scale).round() as i32, ((y - origin.y) as f64 * scale).round() as i32);
    let surface = |elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>>| elements.into_iter().map(DemiurgeElement::Surface);
    let mut out = Vec::new();
    for window in wm.x11_surfaces.iter().filter(|x| x.is_mapped() && x.is_override_redirect()) {
        if let Some(wl) = window.wl_surface() {
            let loc = window.geometry().loc;
            out.extend(surface(render_elements_from_surface_tree(renderer, &wl, at(loc.x, loc.y), scale, 1.0, Kind::Unspecified)));
        }
    }
    for c in wm.stacking() {
        for (popup, origin) in c.popups() {
            out.extend(surface(render_elements_from_surface_tree(renderer, &popup, at(origin.x, origin.y), scale, 1.0, Kind::Unspecified)));
        }
        // An X11 window shows once XWayland has paired a surface with it.
        if let Some(wl) = c.toplevel.wl_surface() {
            let origin = c.surface_origin();
            out.extend(surface(render_elements_from_surface_tree(renderer, &wl, at(origin.x, origin.y), scale, 1.0, Kind::Unspecified)));
        }
        if c.decor > 0 {
            let active = wm.focused == Some(c.id);
            let title = c.toplevel.title();
            out.push(DemiurgeElement::Texture(bar.title_element(renderer, c.id, at(c.x, c.y), c.w, &title, active, scale)?));
        }
    }
    bar.keep_titles(|id| wm.clients.iter().any(|c| c.id == id && c.decor > 0));
    Ok(out)
}

fn refresh_interval(output: &Output) -> Duration {
    let mhz = output.current_mode().map_or(60_000, |m| m.refresh).max(1);
    Duration::from_nanos(1_000_000_000_000 / mhz as u64)
}
