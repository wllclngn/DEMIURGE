// A Cairo frame drawn straight into a wl_shm buffer, with no copy between
// them: Cairo's RGB24 and ARGB32 are wl_shm's XRGB8888 and ARGB8888, the
// same bytes on a little-endian machine.

use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use wayland_client::protocol::wl_shm;
use wayland_client::protocol::wl_surface::WlSurface;

/// Draw a `w` by `h` frame with `draw` into a fresh buffer from `pool`,
/// opaque or with `alpha`, attach it to `surface`, damage all of it and
/// commit. The buffer comes back to be kept until the next frame replaces
/// it.
pub fn present(
    pool: &mut SlotPool,
    surface: &WlSurface,
    w: i32,
    h: i32,
    alpha: bool,
    draw: impl FnOnce(&cairo::Context) -> Result<(), String>,
) -> Result<Buffer, String> {
    let stride = w * 4;
    let (format, cairo_format) = if alpha {
        (wl_shm::Format::Argb8888, cairo::Format::ARgb32)
    } else {
        (wl_shm::Format::Xrgb8888, cairo::Format::Rgb24)
    };
    let (buffer, canvas) = pool.create_buffer(w, h, stride, format).map_err(|e| format!("shm buffer: {}", e))?;
    // A reused slot still holds an old frame.
    if alpha {
        canvas.fill(0);
    }
    {
        // SAFETY: The canvas is `h` rows of `stride` bytes and outlives the
        // image, which is finished before the buffer goes to the compositor.
        let image = unsafe { cairo::ImageSurface::create_for_data_unsafe(canvas.as_mut_ptr(), cairo_format, w, h, stride) }
            .map_err(|e| format!("cairo surface {}x{}: {}", w, h, e))?;
        let cr = cairo::Context::new(&image).map_err(|e| format!("cairo context: {}", e))?;
        draw(&cr)?;
        drop(cr);
        image.finish();
    }
    buffer.attach_to(surface).map_err(|e| format!("attach: {:?}", e))?;
    surface.damage_buffer(0, 0, w, h);
    surface.commit();
    Ok(buffer)
}
