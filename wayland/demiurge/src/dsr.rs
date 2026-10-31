// Dynamic Super Resolution for the Wayland implementation.
//
// Mirrors what the X11 implementation gets from RandR's CRTC scaler,
// but at the compositor layer where we own the entire pipeline. When
// the multiplier is above 1.0, each frame:
//
//   1. The windows are drawn at multiplier times their size into an
//      offscreen GLES texture of native size * multiplier, reallocated
//      when the output's size changes.
//   2. That texture becomes one element drawn at the output's native
//      size; GL_LINEAR sampling gives the bilinear downscale the X11
//      path defaults to.
//
// The bar is not part of the pass; it stays crisp at native size.
//
// At a multiplier of 1.0 none of this runs.
//
// Future quality upgrade path: replace the bilinear sampling with a
// Lanczos-2 compute shader (the one in DSR-PARALLAX.md). The swap is
// contained to this module; the render path doesn't change.

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::texture::{TextureBuffer, TextureRenderElement};
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::utils::draw_render_elements;
use smithay::backend::renderer::{Bind, Color32F, Frame, Offscreen, Renderer};
use smithay::utils::{Buffer, Physical, Point, Rectangle, Size, Transform};

pub struct Dsr {
    /// Per-output multiplier (1.0 = off, 2.0 = render at 2x and
    /// downscale, etc.). Read from `[[display]]` at startup.
    pub multiplier: f64,
    /// Offscreen render target, allocated on the first frame that
    /// needs it and reallocated when the output's size changes.
    offscreen: Option<GlesTexture>,
    /// The (high-res) size the offscreen was allocated at.
    last_render_size: Option<(i32, i32)>,
}

impl Dsr {
    pub fn new(multiplier: f64) -> Self {
        Self {
            multiplier,
            offscreen: None,
            last_render_size: None,
        }
    }

    /// True iff the DSR pass is active (multiplier > 1.0).
    pub fn active(&self) -> bool {
        self.multiplier > 1.0
    }

    /// Compute the high-resolution render size from the native
    /// scanout size. Round-half-to-even for stable resize behavior.
    pub fn render_size(&self, native_w: i32, native_h: i32) -> (i32, i32) {
        let w = (native_w as f64 * self.multiplier).round() as i32;
        let h = (native_h as f64 * self.multiplier).round() as i32;
        (w.max(1), h.max(1))
    }

    // Draw `scene`, built at the multiplier's scale, into the offscreen
    // target and return it as one element at the output's native size.
    pub fn downscale(
        &mut self,
        renderer: &mut GlesRenderer,
        native: Size<i32, Physical>,
        scene: &[crate::render::DemiurgeElement],
    ) -> Result<TextureRenderElement<GlesTexture>, String> {
        let (rw, rh) = self.render_size(native.w, native.h);
        let size = Size::<i32, Physical>::from((rw, rh));
        let damage = [Rectangle::from_size(size)];
        let mut target = self.ensure_offscreen(renderer, rw, rh)?.clone();
        {
            let mut framebuffer = renderer.bind(&mut target).map_err(|e| format!("bind offscreen: {}", e))?;
            let mut frame = renderer
                .render(&mut framebuffer, size, Transform::Normal)
                .map_err(|e| format!("render offscreen: {}", e))?;
            frame
                .clear(Color32F::new(0.07, 0.07, 0.07, 1.0), &damage)
                .map_err(|e| format!("clear offscreen: {}", e))?;
            draw_render_elements(&mut frame, self.multiplier, scene, &damage)
                .map_err(|e| format!("draw offscreen: {}", e))?;
            let _ = frame.finish().map_err(|e| format!("finish offscreen: {}", e))?;
        }
        let buffer = TextureBuffer::from_texture(renderer, target, 1, Transform::Normal, None);
        Ok(TextureRenderElement::from_texture_buffer(
            Point::from((0.0, 0.0)),
            &buffer,
            None,
            None,
            Some(Size::from((native.w, native.h))),
            Kind::Unspecified,
        ))
    }

    fn ensure_offscreen(&mut self, renderer: &mut GlesRenderer, rw: i32, rh: i32) -> Result<&GlesTexture, String> {
        if self.last_render_size != Some((rw, rh)) || self.offscreen.is_none() {
            // Argb8888 matches the SHM format clients use most often;
            // the GLES format the renderer maps it to is RGBA8.
            let tex = renderer
                .create_buffer(Fourcc::Argb8888, Size::<i32, Buffer>::from((rw, rh)))
                .map_err(|e| format!("create_buffer({}x{}): {}", rw, rh, e))?;
            self.offscreen = Some(tex);
            self.last_render_size = Some((rw, rh));
            tracing::info!("[dsr] allocated offscreen {}x{} (multiplier {})", rw, rh, self.multiplier);
        }
        Ok(self.offscreen.as_ref().expect("just ensured"))
    }
}
