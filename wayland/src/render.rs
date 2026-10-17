// Shared render-element construction. Both the winit dev backend
// (main.rs) and the TTY backend (tty.rs) build the same elements
// list each frame; this module is the single source of truth so a
// fix or feature in one path lands in the other automatically.
//
// Three categories of element:
//   1. Wayland surface trees -- xdg toplevels (per ManagedClient.x/y),
//      X11 surfaces (per X11Surface.geometry), or lock surfaces (full
//      screen) when the session is locked.
//   2. The bar -- a TextureRenderElement wrapping the GlesTexture the
//      Bar module uploads each frame.
//
// The render_elements! macro generates a wrapping enum so both can
// live in the same Vec when handed to DrmCompositor::render_frame
// (which takes a single elements list of one type). main.rs's
// existing two-branch render path uses the surface elements directly
// since it draws bar + clients in separate draw_render_elements
// calls; the TTY path uses the wrapping enum.

use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::element::texture::{TextureBuffer, TextureRenderElement};
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::utils::{Point, Transform};

use crate::bar::Bar;
use crate::wm::Wm;

// Wrapping enum for "anything we composite into one frame." Generated
// by smithay's render_elements! macro -- it produces the enum + the
// Element / RenderElement trait impls so DrmCompositor can iterate
// over a single Vec<DemiurgeElement>.
smithay::backend::renderer::element::render_elements! {
    pub DemiurgeElement<=GlesRenderer>;
    Surface=WaylandSurfaceRenderElement<GlesRenderer>,
    Texture=TextureRenderElement<GlesTexture>,
}

/// Build the surface-tree elements for the current Wm state.
///
/// `lock_mode` selects between the lock-surfaces-only path (sent
/// when the compositor is locked or in the pending-lock transition)
/// and the normal clients + X11-surfaces path. `scale` is the render
/// scale; the winit dev path passes `1.0` (or the DSR multiplier in
/// the high-res offscreen pass), the TTY path passes `1.0`.
pub fn build_surface_elements(
    renderer: &mut GlesRenderer,
    wm: &Wm,
    scale: f32,
    lock_mode: bool,
) -> Vec<WaylandSurfaceRenderElement<GlesRenderer>> {
    if lock_mode {
        return wm
            .lock_surfaces
            .iter()
            .flat_map(|(s, _)| {
                render_elements_from_surface_tree(
                    renderer,
                    s.wl_surface(),
                    (0, 0),
                    1.0,
                    scale,
                    Kind::Unspecified,
                )
            })
            .collect();
    }

    let mut elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = wm
        .clients
        .iter()
        // Only clients on a currently-visible tag reach the GPU; this
        // is what makes tag switching actually hide windows.
        .filter(|c| wm.is_visible(c))
        .flat_map(|c| {
            render_elements_from_surface_tree(
                renderer,
                c.toplevel.wl_surface(),
                (c.x, c.y),
                1.0,
                scale,
                Kind::Unspecified,
            )
        })
        .collect();

    // X11 surfaces (XWayland clients) join the same elements list.
    // Pre-association (no wl_surface yet) and unmapped surfaces are
    // skipped -- nothing to draw.
    for x11 in &wm.x11_surfaces {
        if let Some(wl_surface) = x11.wl_surface() {
            if !x11.is_mapped() {
                continue;
            }
            let geo = x11.geometry();
            elements.extend(render_elements_from_surface_tree(
                renderer,
                &wl_surface,
                (geo.loc.x, geo.loc.y),
                1.0,
                scale,
                Kind::Unspecified,
            ));
        }
    }
    elements
}

/// Refresh the bar texture and wrap it as a TextureRenderElement
/// positioned at the primary monitor's origin. Returns None when the
/// bar is disabled (height == 0) or the session is locked (the lock
/// path hides the desktop, bar included).
///
/// The Bar caches the texture across frames; this just triggers
/// re-render when content changes (clock tick, active tag, etc.) and
/// returns the element to composite.
pub fn build_bar_element(
    renderer: &mut GlesRenderer,
    bar: &mut Bar,
    wm: &Wm,
    lock_mode: bool,
) -> Result<Option<TextureRenderElement<GlesTexture>>, String> {
    // Suppressed while locked, when disabled (height 0), or when a
    // fullscreen client owns the active tag (so it isn't clipped).
    if lock_mode || bar.height == 0 || wm.active_tag_fullscreen() {
        return Ok(None);
    }
    let mon = wm.monitors.first();
    let active_tag = wm.active_tags[wm.focused_monitor];
    let occupied = wm.occupied_tags();
    let texture = bar
        .texture(renderer, mon.width, active_tag, &occupied)
        .map_err(|e| format!("bar texture: {}", e))?
        .clone();
    let buf = TextureBuffer::from_texture(renderer, texture, 1, Transform::Normal, None);
    Ok(Some(TextureRenderElement::from_texture_buffer(
        Point::from((mon.x as f64, mon.y as f64)),
        &buf,
        None,
        None,
        None,
        Kind::Unspecified,
    )))
}

/// Convenience for the TTY backend: build clients + bar in one
/// DemiurgeElement vec ready for DrmCompositor::render_frame.
pub fn build_tty_elements(
    renderer: &mut GlesRenderer,
    bar: &mut Bar,
    wm: &Wm,
    lock_mode: bool,
) -> Result<Vec<DemiurgeElement>, String> {
    let surface_elems = build_surface_elements(renderer, wm, 1.0, lock_mode);
    let bar_elem = build_bar_element(renderer, bar, wm, lock_mode)?;
    let mut out: Vec<DemiurgeElement> =
        surface_elems.into_iter().map(DemiurgeElement::Surface).collect();
    if let Some(b) = bar_elem {
        out.push(DemiurgeElement::Texture(b));
    }
    Ok(out)
}
