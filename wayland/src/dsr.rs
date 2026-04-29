// Dynamic Super Resolution for the Wayland implementation.
//
// Mirrors what the X11 implementation gets from RandR's CRTC scaler,
// but at the compositor layer where we own the entire pipeline. The
// pass:
//
//   1. Once per session (and on resize): allocate an offscreen GLES
//      texture sized at native_size * multiplier. This is the high-
//      resolution framebuffer.
//
//   2. Per frame, when multiplier > 1.0:
//      a. Bind the offscreen texture as the render target.
//      b. Render every client surface tree at the multiplier scale.
//         Smithay's render_elements_from_surface_tree uses the output
//         scale to upscale buffer coords; passing `multiplier` as
//         that scale makes a 800x600 surface render at 1600x1200 in
//         the offscreen target (for 2x DSR).
//      c. Bind the winit framebuffer as the render target.
//      d. Wrap the offscreen texture in a TextureBuffer and render
//         it at the native size. Smithay's GLES sampler uses
//         GL_LINEAR by default, giving bilinear downscale -- the
//         same filter the X11 path defaults to.
//      e. Submit.
//
// When multiplier == 1.0, the offscreen pass is skipped entirely
// (zero overhead vs the pre-DSR render path).
//
// Future quality upgrade path: replace step 2.d's bilinear sampling
// with a Lanczos-2 compute shader (the one in DSR-PARALLAX.md). The
// architecture is designed so that swap-in is a contained change to
// this module; the per-frame caller doesn't change.

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::Offscreen;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::utils::{Buffer, Size};

pub struct Dsr {
    /// Per-output multiplier (1.0 = off, 2.0 = render at 2x and
    /// downscale, etc.). Read from `[[display]]` at startup; a
    /// future hot-reload path can update this and force-recreate
    /// the offscreen target.
    pub multiplier: f64,
    /// Offscreen render target. Lazily allocated on first frame
    /// where multiplier > 1.0; reallocated on winit resize.
    offscreen: Option<GlesTexture>,
    /// The (high-res) size the offscreen was allocated at. Compared
    /// against `native * multiplier` each frame to detect resize.
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

    /// True iff the DSR pass is active (multiplier > 1.0). Callers
    /// branch on this to decide whether to allocate an offscreen
    /// target and run the two-pass path or render directly to the
    /// scanout target.
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

    /// Ensure the offscreen target exists at the right size for the
    /// current native scanout. Allocates on first call; reallocates
    /// when the native size changes (winit window resize). Returns
    /// the texture by &mut for the caller to bind against.
    pub fn ensure_offscreen<'a>(
        &'a mut self,
        renderer: &mut GlesRenderer,
        native_w: i32,
        native_h: i32,
    ) -> Result<&'a mut GlesTexture, String> {
        let (rw, rh) = self.render_size(native_w, native_h);
        let need_new = self.last_render_size != Some((rw, rh)) || self.offscreen.is_none();
        if need_new {
            // Argb8888 matches the SHM format clients use most often;
            // the GLES format the renderer maps it to is RGBA8.
            let tex = renderer
                .create_buffer(Fourcc::Argb8888, Size::<i32, Buffer>::from((rw, rh)))
                .map_err(|e| format!("create_buffer({}x{}): {}", rw, rh, e))?;
            self.offscreen = Some(tex);
            self.last_render_size = Some((rw, rh));
            tracing::info!(
                "[dsr] allocated offscreen {}x{} (native {}x{}, multiplier {})",
                rw, rh, native_w, native_h, self.multiplier,
            );
        }
        Ok(self.offscreen.as_mut().expect("just ensured"))
    }

    /// Take ownership of the offscreen texture if any. Used during
    /// the downscale pass when we need to wrap it in a TextureBuffer
    /// and pass it back to render. The Dsr keeps a clone via
    /// GlesTexture's reference counting (textures are Rc-backed in
    /// smithay), so this isn't a destructive take.
    pub fn offscreen_clone(&self) -> Option<GlesTexture> {
        self.offscreen.clone()
    }
}
