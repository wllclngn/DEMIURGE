// The monitors' frames as the compositor shows them, through
// ext-image-copy-capture on ext-image-capture-source's output sources:
// MEDUSA's freeze, and `medusa full`.

use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{delegate_output, delegate_registry, delegate_shm, registry_handlers};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_shm;
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::ext::image_capture_source::v1::client::ext_image_capture_source_v1::ExtImageCaptureSourceV1;
use wayland_protocols::ext::image_capture_source::v1::client::ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1;
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1};
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_manager_v1::{
    ExtImageCopyCaptureManagerV1, Options,
};
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_session_v1::{
    self, ExtImageCopyCaptureSessionV1,
};

/// Every monitor's frame, without the cursor, on one surface spanning them
/// at their places, and that surface's top-left in the compositor's space.
pub fn desktop(conn: &Connection) -> Result<(cairo::ImageSurface, (i32, i32)), String> {
    let (globals, mut queue) = registry_queue_init::<Capture>(conn).map_err(|e| format!("registry: {}", e))?;
    let qh = queue.handle();
    let sources: ExtOutputImageCaptureSourceManagerV1 =
        globals.bind(&qh, 1..=1, ()).map_err(|_| "the compositor offers no ext-image-capture-source")?;
    let copier: ExtImageCopyCaptureManagerV1 =
        globals.bind(&qh, 1..=1, ()).map_err(|_| "the compositor offers no ext-image-copy-capture")?;
    let shm = Shm::bind(&globals, &qh).map_err(|e| format!("wl_shm: {}", e))?;
    let mut capture = Capture {
        registry: RegistryState::new(&globals),
        outputs: OutputState::new(&globals, &qh),
        shm,
        monitors: Vec::new(),
    };
    // Two round trips: The outputs, then each one's place and size.
    for _ in 0..2 {
        queue.roundtrip(&mut capture).map_err(|e| format!("roundtrip: {}", e))?;
    }

    let outputs: Vec<(WlOutput, (i32, i32))> = capture
        .outputs
        .outputs()
        .filter_map(|output| {
            let info = capture.outputs.info(&output)?;
            Some((output, info.logical_position.unwrap_or(info.location)))
        })
        .collect();
    if outputs.is_empty() {
        return Err("no monitor to capture".into());
    }
    for (i, (output, place)) in outputs.iter().enumerate() {
        let source = sources.create_source(output, &qh, ());
        let session = copier.create_session(&source, Options::empty(), &qh, i);
        capture.monitors.push(Monitor { place: *place, source, session, size: None, xrgb: false, done: false, frame: None, buffer: None, result: None });
    }
    // Each session's constraints, then each frame copied into its buffer.
    while capture.monitors.iter().any(|m| !m.done && m.result.is_none()) {
        queue.blocking_dispatch(&mut capture).map_err(|e| format!("dispatch: {}", e))?;
    }
    let mut pool = SlotPool::new(4096, &capture.shm).map_err(|e| format!("shm pool: {}", e))?;
    for (i, m) in capture.monitors.iter_mut().enumerate() {
        if m.result.is_some() {
            continue;
        }
        let Some((w, h)) = m.size else {
            m.result = Some(Err("no size was given".into()));
            continue;
        };
        let format = if m.xrgb { wl_shm::Format::Xrgb8888 } else { wl_shm::Format::Argb8888 };
        let (buffer, _) =
            pool.create_buffer(w as i32, h as i32, w as i32 * 4, format).map_err(|e| format!("shm buffer: {}", e))?;
        let frame = m.session.create_frame(&qh, i);
        frame.attach_buffer(buffer.wl_buffer());
        frame.damage_buffer(0, 0, w as i32, h as i32);
        frame.capture();
        m.frame = Some(frame);
        m.buffer = Some(buffer);
    }
    while capture.monitors.iter().any(|m| m.result.is_none()) {
        queue.blocking_dispatch(&mut capture).map_err(|e| format!("dispatch: {}", e))?;
    }

    let mut frames = Vec::new();
    for m in &capture.monitors {
        m.result.clone().expect("every frame ended")?;
        let (w, h) = m.size.expect("a frame copied has a size");
        let buffer = m.buffer.as_ref().expect("a frame copied has a buffer");
        let pixels = buffer.canvas(&mut pool).ok_or("the frame's buffer is still the compositor's")?.to_vec();
        let frame = cairo::ImageSurface::create_for_data(pixels, cairo::Format::Rgb24, w as i32, h as i32, w as i32 * 4)
            .map_err(|e| format!("frame surface: {}", e))?;
        frames.push((m.place, frame));
    }
    for m in &capture.monitors {
        if let Some(frame) = &m.frame {
            frame.destroy();
        }
        m.session.destroy();
        m.source.destroy();
    }
    let _ = queue.roundtrip(&mut capture);

    let left = frames.iter().map(|(p, _)| p.0).min().unwrap_or(0);
    let top = frames.iter().map(|(p, _)| p.1).min().unwrap_or(0);
    let right = frames.iter().map(|(p, f)| p.0 + f.width()).max().unwrap_or(0);
    let bottom = frames.iter().map(|(p, f)| p.1 + f.height()).max().unwrap_or(0);
    let desktop = cairo::ImageSurface::create(cairo::Format::Rgb24, right - left, bottom - top)
        .map_err(|e| format!("desktop surface: {}", e))?;
    let cr = cairo::Context::new(&desktop).map_err(|e| format!("desktop context: {}", e))?;
    for (place, frame) in &frames {
        cr.set_source_surface(frame, (place.0 - left) as f64, (place.1 - top) as f64).map_err(|e| e.to_string())?;
        cr.paint().map_err(|e| e.to_string())?;
    }
    drop(cr);
    Ok((desktop, (left, top)))
}

struct Capture {
    registry: RegistryState,
    outputs: OutputState,
    shm: Shm,
    monitors: Vec<Monitor>,
}

// One monitor's capture: Its session's constraints as they come, then its
// frame and the buffer it is copied into, until it is ready or refused.
struct Monitor {
    place: (i32, i32),
    source: ExtImageCaptureSourceV1,
    session: ExtImageCopyCaptureSessionV1,
    size: Option<(u32, u32)>,
    xrgb: bool,
    done: bool,
    frame: Option<ExtImageCopyCaptureFrameV1>,
    buffer: Option<Buffer>,
    result: Option<Result<(), String>>,
}

impl Dispatch<ExtImageCopyCaptureSessionV1, usize> for Capture {
    fn event(
        capture: &mut Self,
        _: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        i: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let m = &mut capture.monitors[*i];
        match event {
            ext_image_copy_capture_session_v1::Event::BufferSize { width, height } => m.size = Some((width, height)),
            ext_image_copy_capture_session_v1::Event::ShmFormat { format } => {
                m.xrgb |= format == WEnum::Value(wl_shm::Format::Xrgb8888);
            }
            ext_image_copy_capture_session_v1::Event::Done => m.done = true,
            ext_image_copy_capture_session_v1::Event::Stopped => {
                m.result.get_or_insert(Err("the compositor stopped the capture".into()));
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, usize> for Capture {
    fn event(
        capture: &mut Self,
        _: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        i: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let m = &mut capture.monitors[*i];
        match event {
            ext_image_copy_capture_frame_v1::Event::Ready => m.result = Some(Ok(())),
            ext_image_copy_capture_frame_v1::Event::Failed { reason } => {
                m.result = Some(Err(format!("the compositor refused the frame ({:?})", reason)));
            }
            _ => {}
        }
    }
}

delegate_noop!(Capture: ignore ExtOutputImageCaptureSourceManagerV1);
delegate_noop!(Capture: ignore ExtImageCaptureSourceV1);
delegate_noop!(Capture: ignore ExtImageCopyCaptureManagerV1);

impl OutputHandler for Capture {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
}

impl ShmHandler for Capture {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Capture {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState];
}

delegate_output!(Capture);
delegate_shm!(Capture);
delegate_registry!(Capture);
