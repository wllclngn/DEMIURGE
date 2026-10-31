// ext-image-copy-capture over ext-image-capture-source's output sources: A
// client copies a monitor's frame, as the screen shows it, into a
// shared-memory buffer of its own. MEDUSA's freeze is these frames. While
// the session is locked no frame is given.

use std::time::Duration;

use smithay::output::{Output, WeakOutput};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::utils::Transform;
use smithay::wayland::image_capture_source::{
    ImageCaptureSource, ImageCaptureSourceHandler, OutputCaptureSourceHandler, OutputCaptureSourceState,
};
use smithay::wayland::image_copy_capture::{
    BufferConstraints, CaptureFailureReason, Frame, ImageCopyCaptureHandler, ImageCopyCaptureState, Session, SessionRef,
};
use smithay::wayland::shm::with_buffer_contents_mut;

use crate::wm::Wm;

// The monitor a source shows, while it is still lit.
fn source_output(source: &ImageCaptureSource) -> Option<Output> {
    source.user_data().get::<WeakOutput>()?.upgrade()
}

// A frame is the monitor's mode in size, in either 32-bit RGB layout.
fn constraints(output: &Output) -> Option<BufferConstraints> {
    let size = output.current_mode()?.size;
    Some(BufferConstraints {
        size: (size.w, size.h).into(),
        shm: vec![wl_shm::Format::Xrgb8888, wl_shm::Format::Argb8888],
        dma: None,
    })
}

impl ImageCaptureSourceHandler for Wm {}

impl OutputCaptureSourceHandler for Wm {
    fn output_capture_source_state(&mut self) -> &mut OutputCaptureSourceState {
        &mut self.runtime.as_mut().expect("screen capture is served only with a screen").output_capture_source
    }

    fn output_source_created(&mut self, source: ImageCaptureSource, output: &Output) {
        source.user_data().insert_if_missing(|| output.downgrade());
    }
}

impl ImageCopyCaptureHandler for Wm {
    fn image_copy_capture_state(&mut self) -> &mut ImageCopyCaptureState {
        &mut self.runtime.as_mut().expect("screen capture is served only with a screen").image_copy_capture
    }

    fn capture_constraints(&mut self, source: &ImageCaptureSource) -> Option<BufferConstraints> {
        constraints(&source_output(source)?)
    }

    fn new_session(&mut self, session: Session) {
        self.capture_sessions.push(session);
    }

    fn session_destroyed(&mut self, session: SessionRef) {
        self.capture_sessions.retain(|s| *s != session);
    }

    fn frame(&mut self, session: &SessionRef, frame: Frame) {
        let shot = match source_output(&session.source()) {
            Some(_) if self.locked() => Err("the session is locked".to_string()),
            Some(output) => self.capture_output(&output, session.draw_cursor()),
            None => Err("its monitor is gone".to_string()),
        };
        match shot.and_then(|shot| copy_into(&frame.buffer(), shot)) {
            Ok(()) => {
                let now = self.runtime.as_ref().map(|rt| Duration::from(rt.clock.now())).unwrap_or_default();
                frame.success(Transform::Normal, None, now);
            }
            Err(e) => {
                tracing::debug!("capture frame refused: {}", e);
                frame.fail(CaptureFailureReason::Unknown);
            }
        }
    }
}

impl Wm {
    /// The monitors changed: Each session learns its monitor's new size, and
    /// one whose monitor went is stopped.
    pub(crate) fn capture_monitors_changed(&mut self) {
        self.capture_sessions.retain(|session| {
            match source_output(&session.source()).and_then(|output| constraints(&output)) {
                Some(constraints) => {
                    if session.current_constraints().is_none_or(|current| current.size != constraints.size) {
                        session.update_constraints(constraints);
                    }
                    true
                }
                None => false,
            }
        });
    }
}

// The frame's pixels into the client's buffer, row by row: Cairo's RGB24 and
// wl_shm's XRGB8888 are the same bytes on a little-endian machine.
fn copy_into(buffer: &WlBuffer, mut shot: cairo::ImageSurface) -> Result<(), String> {
    let (w, h, stride) = (shot.width() as usize, shot.height() as usize, shot.stride() as usize);
    shot.flush();
    let pixels = shot.data().map_err(|e| e.to_string())?;
    with_buffer_contents_mut(buffer, |ptr, len, data| {
        if (data.width as usize, data.height as usize) != (w, h) {
            return Err(format!("a {}x{} buffer for a {}x{} frame", data.width, data.height, w, h));
        }
        let row = w * 4;
        for y in 0..h {
            let at = data.offset as usize + y * data.stride as usize;
            if at + row > len {
                return Err("the buffer runs past its pool".to_string());
            }
            // SAFETY: `at + row` is within the pool's `len` bytes, checked
            // above, and `pixels` holds `row` bytes from each row's start.
            unsafe { std::ptr::copy_nonoverlapping(pixels[y * stride..].as_ptr(), ptr.add(at), row) };
        }
        Ok(())
    })
    .map_err(|e| format!("buffer: {:?}", e))?
}
