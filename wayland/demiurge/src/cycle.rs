// Alt+Tab under demiurge-wl, as on X11: The cycle walks the windows most
// recently used first, focusing each as it goes while the ring stays as it
// was, and the window chosen goes to the ring's front once Alt, or Super for
// the cycle across tags, is let go. With `[general] alt_tab_preview` on, an
// overlay on the focused monitor shows the candidates while the cycle lasts:
// Each window's surfaces rendered small into a texture of its own, as the
// window's own elements would collide with it in the scene, its title under
// it, the chosen one framed.

use demiurge_core::mru::CycleState;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree};
use smithay::backend::renderer::element::texture::{TextureBuffer, TextureRenderElement};
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::utils::draw_render_elements;
use smithay::backend::renderer::{Bind, Color32F, Frame, Offscreen, Renderer};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Size, Transform};

use crate::bar::Bar;
use crate::render::DemiurgeElement;
use crate::wm::Wm;

// A tile's room for a thumbnail, the title under it and the space around.
pub const TILE_W: i32 = 240;
pub const TILE_H: i32 = 150;
pub const LABEL_H: i32 = 26;
pub const PAD: i32 = 16;

impl Wm {
    /// A cycle bind pressed: The first press starts the cycle at the window
    /// after the focused one (or the last, backward), each later one steps;
    /// the target is focused, its tag shown if no monitor shows it.
    pub fn mru_cycle(&mut self, forward: bool, global: bool) {
        let (candidates, index) = match self.cycle.take() {
            Some(mut cycle) => {
                if cycle.candidates.len() < 2 {
                    self.cycle = Some(cycle);
                    return;
                }
                cycle.advance(forward);
                (cycle.candidates, cycle.index)
            }
            None => {
                let tag = self.active_tags[self.focused_monitor];
                // Minimized windows sit out the cycle; the tag menu is their
                // way back.
                let candidates: Vec<u64> = self
                    .mru
                    .as_slice()
                    .iter()
                    .copied()
                    .filter(|&id| self.clients.iter().any(|c| c.id == id && !c.minimized && (global || c.tag == tag)))
                    .collect();
                if candidates.len() < 2 {
                    return;
                }
                let index = if forward { 1 } else { candidates.len() - 1 };
                (candidates, index)
            }
        };
        let target = candidates[index];
        self.cycle = Some(CycleState { candidates, index });
        if let Some(tag) = self.clients.iter().find(|c| c.id == target).map(|c| c.tag) {
            if !self.active_tags.contains(&tag) {
                self.view_tag(tag);
            }
        }
        self.focus_client(target);
        self.queue_redraw();
    }

    /// The cycle's modifier let go: The window chosen goes to the ring's
    /// front.
    pub(crate) fn finish_cycle(&mut self) {
        if let Some(cycle) = self.cycle.take() {
            if let Some(target) = cycle.current() {
                self.mru.on_focus(target);
            }
            self.queue_redraw();
        }
    }
}

/// The overlay for `cycle` on the monitor at `origin` of `monitor`'s size:
/// The thumbnails, then the panel behind them, front to back.
pub(crate) fn overlay(
    renderer: &mut GlesRenderer,
    wm: &Wm,
    bar: &mut Bar,
    cycle: &CycleState<u64>,
    monitor: (i32, i32, u32, u32),
    origin: Point<i32, Logical>,
) -> Result<Vec<DemiurgeElement>, String> {
    let (mx, my, mw, mh) = monitor;
    let n = cycle.candidates.len() as i32;
    let cols = n.min(((mw as i32 - PAD) / (TILE_W + PAD)).max(1));
    let rows = (n + cols - 1) / cols;
    let (pw, ph) = (cols * (TILE_W + PAD) + PAD, rows * (TILE_H + LABEL_H + PAD) + PAD);
    let (px, py) = (mx + (mw as i32 - pw) / 2, my + (mh as i32 - ph) / 2);
    let tile = |i: i32| (px + PAD + (i % cols) * (TILE_W + PAD), py + PAD + (i / cols) * (TILE_H + LABEL_H + PAD));

    let mut out = Vec::new();
    let mut titles = Vec::new();
    for (i, &id) in cycle.candidates.iter().enumerate() {
        let Some(c) = wm.clients.iter().find(|c| c.id == id) else {
            titles.push(String::new());
            continue;
        };
        titles.push(c.toplevel.title());
        let Some(surface) = c.toplevel.wl_surface() else {
            continue;
        };
        let (cw, ch) = (c.w.max(1) as f64, c.h.saturating_sub(c.decor).max(1) as f64);
        let scale = (TILE_W as f64 / cw).min(TILE_H as f64 / ch).min(1.0);
        let size = ((cw * scale).round().max(1.0) as i32, (ch * scale).round().max(1.0) as i32);
        let (tx, ty) = tile(i as i32);
        // The surface's own origin, past its window geometry's offset.
        let inset = c.surface_origin() - Point::from((c.x, c.y + c.decor as i32));
        let at = (tx + (TILE_W - size.0) / 2 - origin.x, ty + (TILE_H - size.1) / 2 - origin.y);
        out.push(DemiurgeElement::Texture(thumbnail(renderer, &surface, inset, scale, size, at)?));
    }
    let panel = bar.cycle_element(renderer, (px - origin.x, py - origin.y), (pw, ph), cols, &titles, cycle.index)?;
    out.push(DemiurgeElement::Texture(panel));
    Ok(out)
}

// `surface` and its subsurfaces at `scale`, `inset` from its window's corner,
// rendered into a texture `size` big, shown at `at`.
fn thumbnail(
    renderer: &mut GlesRenderer,
    surface: &WlSurface,
    inset: Point<i32, Logical>,
    scale: f64,
    size: (i32, i32),
    at: (i32, i32),
) -> Result<TextureRenderElement<GlesTexture>, String> {
    let from = Point::<i32, Physical>::from(((inset.x as f64 * scale).round() as i32, (inset.y as f64 * scale).round() as i32));
    let elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
        render_elements_from_surface_tree(renderer, surface, from, scale, 1.0, Kind::Unspecified);
    let mut texture: GlesTexture = renderer
        .create_buffer(Fourcc::Abgr8888, Size::<i32, Buffer>::from(size))
        .map_err(|e| format!("thumbnail buffer: {}", e))?;
    {
        let physical = Size::<i32, Physical>::from(size);
        let damage = [Rectangle::from_size(physical)];
        let mut framebuffer = renderer.bind(&mut texture).map_err(|e| format!("bind thumbnail: {}", e))?;
        let mut frame = renderer
            .render(&mut framebuffer, physical, Transform::Normal)
            .map_err(|e| format!("render thumbnail: {}", e))?;
        frame.clear(Color32F::new(0.0, 0.0, 0.0, 1.0), &damage).map_err(|e| format!("clear thumbnail: {}", e))?;
        draw_render_elements(&mut frame, 1.0, &elements, &damage).map_err(|e| format!("draw thumbnail: {}", e))?;
        let _ = frame.finish().map_err(|e| format!("finish thumbnail: {}", e))?;
    }
    let buffer = TextureBuffer::from_texture(renderer, texture, 1, Transform::Normal, None);
    Ok(TextureRenderElement::from_texture_buffer(
        Point::from((at.0 as f64, at.1 as f64)),
        &buffer,
        None,
        None,
        None,
        Kind::Unspecified,
    ))
}
