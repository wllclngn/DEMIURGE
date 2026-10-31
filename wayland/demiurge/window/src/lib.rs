// The test window: demiurge-wl drawn into a window on whatever session
// it runs under, the window standing in for one monitor. It passes the
// window's size and input up and shows the frames it is handed. It
// holds no DEMIURGE behavior and cannot see the window manager.
//
// A window has no vblank to report, so after each frame a timer at the
// window's refresh stands in for one.

use std::time::Duration;

use smithay::backend::input::InputEvent;
use smithay::backend::renderer::Color32F;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::{RenderElement, RenderElementStates};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::winit::{self, WinitEvent, WinitGraphicsBackend, WinitInput};
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::winit::dpi::LogicalSize;
use smithay::reexports::winit::window::Window as WinitWindow;
use smithay::utils::{Clock, Monotonic, Physical, Size, Time, Transform};

const REFRESH_MHZ: i32 = 60_000;

// What the compositor implements to receive the window's events.
pub trait WindowHandler: Sized + 'static {
    fn window(&mut self) -> &mut Window<Self>;
    fn window_event(&mut self, event: Event);
}

pub enum Event {
    // The window changed size; its output's mode already says so.
    Resized,
    Input(InputEvent<WinitInput>),
    // The frame `render` last submitted is on screen.
    Presented { time: Time<Monotonic>, seq: u64 },
    Closed,
}

pub enum Rendered {
    Submitted(RenderElementStates),
    // Nothing changed since the last frame, so nothing was shown.
    Empty(RenderElementStates),
}

pub struct Window<D: 'static> {
    backend: WinitGraphicsBackend<GlesRenderer>,
    damage: OutputDamageTracker,
    output: Output,
    handle: LoopHandle<'static, D>,
    clock: Clock<Monotonic>,
    seq: u64,
}

impl<D: WindowHandler> Window<D> {
    pub fn new(handle: LoopHandle<'static, D>) -> Result<Self, String> {
        let attributes = WinitWindow::default_attributes()
            .with_inner_size(LogicalSize::new(1280.0, 800.0))
            .with_title("demiurge-wl")
            .with_visible(true);
        let (mut backend, events) = winit::init_from_attributes::<GlesRenderer>(attributes)
            .map_err(|e| format!("winit: {}", e))?;
        swap_without_waiting(&mut backend)?;
        // The compositor draws its own cursor inside the window.
        backend.window().set_cursor_visible(false);
        let output = Output::new(
            "winit-0".to_string(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "DEMIURGE".to_string(),
                model: "window".to_string(),
                serial_number: String::new(),
            },
        );
        let size = backend.window_size();
        let mode = Mode { size, refresh: REFRESH_MHZ };
        output.set_preferred(mode);
        output.change_current_state(Some(mode), Some(Transform::Normal), Some(Scale::Integer(1)), Some((0, 0).into()));
        handle
            .insert_source(events, |event, _, data: &mut D| match event {
                WinitEvent::Resized { size, .. } => {
                    data.window().resize(size);
                    data.window_event(Event::Resized);
                }
                WinitEvent::Input(event) => data.window_event(Event::Input(event)),
                WinitEvent::CloseRequested => data.window_event(Event::Closed),
                WinitEvent::Focus(_) | WinitEvent::Redraw => {}
            })
            .map_err(|e| format!("insert winit source: {}", e))?;
        Ok(Self {
            backend,
            damage: tracker(size),
            output,
            handle,
            clock: Clock::new(),
            seq: 0,
        })
    }

    pub fn output(&self) -> &Output {
        &self.output
    }

    pub fn renderer(&mut self) -> &mut GlesRenderer {
        self.backend.renderer()
    }

    // Draw `elements` (front to back) and show them, if anything changed.
    pub fn render<E: RenderElement<GlesRenderer>>(&mut self, elements: &[E], clear: Color32F) -> Result<Rendered, String> {
        // Every frame is drawn in full (buffer age 0). EGL reads a
        // surface's buffer age only while the surface is current, and
        // smithay's winit backend gives no way to keep it current across
        // the query; a test window has no need to save the redraw.
        let (renderer, mut framebuffer) = self.backend.bind().map_err(|e| format!("bind: {}", e))?;
        let result = self
            .damage
            .render_output(renderer, &mut framebuffer, 0, elements, clear)
            .map_err(|e| format!("render: {:?}", e))?;
        let damage = result.damage.cloned();
        let states = result.states;
        drop(framebuffer);
        let Some(damage) = damage else {
            return Ok(Rendered::Empty(states));
        };
        self.backend.submit(Some(&damage)).map_err(|e| format!("submit: {}", e))?;
        self.seq += 1;
        let seq = self.seq;
        let refresh = Duration::from_nanos(1_000_000_000_000 / REFRESH_MHZ as u64);
        self.handle
            .insert_source(Timer::from_duration(refresh), move |_, _, data: &mut D| {
                let time = data.window().clock.now();
                data.window_event(Event::Presented { time, seq });
                TimeoutAction::Drop
            })
            .map_err(|e| format!("insert frame timer: {}", e))?;
        Ok(Rendered::Submitted(states))
    }

    fn resize(&mut self, size: Size<i32, Physical>) {
        let mode = Mode { size, refresh: REFRESH_MHZ };
        self.output.change_current_state(Some(mode), None, None, None);
        self.output.set_preferred(mode);
        self.damage = tracker(size);
    }
}

// The window's EGL surface is drawn bottom-up, so frames are flipped
// here instead of advertising a flipped output to clients.
fn tracker(size: Size<i32, Physical>) -> OutputDamageTracker {
    OutputDamageTracker::new(size, 1.0, Transform::Flipped180)
}

// Swap interval 0 on the window's surface, made current for the call: A
// swap returns at once, the window presenting on its own refresh timer, so
// how the host paces the window, a frame callback a second while it does
// not show it, never holds the compositor inside. smithay's winit backend
// picks a config for no vsync but leaves the surface at EGL's default of 1,
// and its renderer's bind leaves no surface current for the call.
fn swap_without_waiting(backend: &mut WinitGraphicsBackend<GlesRenderer>) -> Result<(), String> {
    use smithay::backend::egl::ffi::egl;
    let surface = backend.egl_surface().get_surface_handle();
    let context = backend.renderer().egl_context();
    let (display, raw) = (context.display().get_display_handle(), context.get_context_handle());
    let set = unsafe { egl::MakeCurrent(display.handle, surface, surface, raw) != 0 && egl::SwapInterval(display.handle, 0) != 0 };
    if !set {
        return Err("eglSwapInterval(0) failed on the window's surface".to_string());
    }
    Ok(())
}
