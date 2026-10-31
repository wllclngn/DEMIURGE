// The two ways demiurge-wl reaches a screen, and what the compositor
// holds while it runs. Every rule lives on Wm; a backend only says where
// frames go and where input comes from.

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::Format;
use smithay::backend::allocator::format::FormatSet;
use smithay::backend::drm::{DrmDeviceFd, DrmNode};
use smithay::reexports::rustix::fs::{Mode, OFlags};
use smithay::utils::DeviceFd;
use smithay::wayland::drm_syncobj::DrmSyncobjState;
use smithay::backend::egl::EGLDevice;
use smithay::backend::renderer::ImportDma;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::output::Output;
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::wayland_protocols::wp::linux_dmabuf::zv1::server::zwp_linux_dmabuf_feedback_v1::TrancheFlags;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::utils::{Clock, Monotonic};
use smithay::wayland::cursor_shape::CursorShapeManagerState;
use smithay::wayland::dmabuf::{DmabufFeedback, DmabufFeedbackBuilder, DmabufGlobal};
use smithay::wayland::pointer_constraints::PointerConstraintsState;
use smithay::wayland::presentation::PresentationState;
use smithay::wayland::relative_pointer::RelativePointerManagerState;
use smithay::wayland::viewporter::ViewporterState;
use smithay::wayland::xdg_activation::XdgActivationState;
use smithay::wayland::commit_timing::CommitTimingManagerState;
use smithay::wayland::fifo::FifoManagerState;

use demiurge_wl_screen::color::{DisplayColor, HdrMetadata};
use demiurge_wl_screen::{self as screen, Screen, ScreenHandler};
use demiurge_wl_window::{self as window, Window, WindowHandler};

use crate::bar::Bar;
use crate::cursor::Cursor;
use crate::dsr::Dsr;
use crate::render::OutputFrame;
use crate::wm::Wm;

#[derive(Clone, Copy)]
pub enum BackendKind {
    // The test window, inside whatever session runs it.
    Window,
    // The machine's own monitors, from a VT.
    Screen,
}

pub enum Backend {
    Window(Window<Wm>),
    Screen(Screen<Wm>),
}

impl Backend {
    pub fn outputs(&self) -> Vec<Output> {
        match self {
            Backend::Window(window) => vec![window.output().clone()],
            Backend::Screen(screen) => screen.outputs(),
        }
    }

    // The real screen's monitors off until a frame turns them on; the test
    // window has none to turn off.
    pub fn power_off(&mut self) {
        if let Backend::Screen(screen) = self {
            screen.power_off();
        }
    }

    pub fn change_vt(&mut self, vt: i32) {
        if let Backend::Screen(screen) = self {
            screen.change_vt(vt);
        }
    }

    // The renderer client buffers are imported into: The window's, or
    // that of the GPU the screen drives.
    pub fn renderer(&mut self) -> Option<&mut GlesRenderer> {
        match self {
            Backend::Window(window) => Some(window.renderer()),
            Backend::Screen(screen) => screen.gpu_renderer(),
        }
    }

    // That renderer's GPU, by render node. The screen knows the device it
    // opened; the window asks EGL, which a software renderer cannot answer.
    pub fn render_node(&mut self) -> Result<DrmNode, String> {
        match self {
            Backend::Window(window) => EGLDevice::device_for_display(window.renderer().egl_context().display())
                .map_err(|e| format!("EGL device: {}", e))?
                .try_get_render_node()
                .map_err(|e| format!("render node: {}", e))?
                .ok_or_else(|| "EGL names no render node".to_string()),
            Backend::Screen(screen) => screen.render_node().ok_or_else(|| "the GPU has no render node".to_string()),
        }
    }

    // The device explicit sync imports clients' timelines on: The screen's
    // GPU, or the test window's render node.
    pub fn sync_device(&mut self) -> Result<DrmDeviceFd, String> {
        if let Backend::Screen(screen) = self {
            return screen.device_fd().ok_or_else(|| "no GPU driven".to_string());
        }
        let path = self.render_node()?.dev_path().ok_or("the render node has no path")?;
        let fd = smithay::reexports::rustix::fs::open(&path, OFlags::RDWR | OFlags::CLOEXEC, Mode::empty())
            .map_err(|e| format!("{}: {}", path.display(), e))?;
        Ok(DrmDeviceFd::new(DeviceFd::from(fd)))
    }

    // What `output`'s monitor says about its color. The test window knows
    // nothing of the monitor under it.
    pub fn display_color(&self, output: &Output) -> Option<DisplayColor> {
        match self {
            Backend::Window(_) => None,
            Backend::Screen(screen) => screen.display_color(output),
        }
    }

    // `output` in HDR10 with `metadata`, or SDR. The test window has no
    // monitor of its own to put in HDR10.
    pub fn set_hdr(&mut self, output: &Output, metadata: Option<HdrMetadata>) {
        if let Backend::Screen(screen) = self {
            screen.set_hdr(output, metadata);
        }
    }

    // The entries in each of `output`'s gamma ramps, where its CRTC has a
    // gamma LUT. The test window has no CRTC of its own.
    pub fn gamma_size(&self, output: &Output) -> Option<u32> {
        match self {
            Backend::Window(_) => None,
            Backend::Screen(screen) => screen.gamma_size(output),
        }
    }

    // `ramps` (red, green then blue) on `output`'s CRTC, or with None the
    // LUT it was found with.
    pub fn set_gamma(&mut self, output: &Output, ramps: Option<Vec<u16>>) -> Result<(), String> {
        match self {
            Backend::Window(_) => Err("the test window has no gamma".to_string()),
            Backend::Screen(screen) => screen.set_gamma(output, ramps),
        }
    }

    // Take a client's buffer if the renderer imports it; the screen also
    // readies it for its monitors' planes.
    pub fn import_dmabuf(&mut self, dmabuf: &Dmabuf) -> bool {
        match self {
            Backend::Window(window) => window.renderer().import_dmabuf(dmabuf, None).is_ok(),
            Backend::Screen(screen) => screen.import_dmabuf(dmabuf).is_ok(),
        }
    }
}

// linux-dmabuf: Its global and the feedback every client gets, naming the
// renderer's GPU and the formats it imports.
pub struct LinuxDmabuf {
    pub global: DmabufGlobal,
    pub feedback: DmabufFeedback,
    pub node: DrmNode,
    pub formats: FormatSet,
}

impl LinuxDmabuf {
    // What a fullscreen window on `output` is told: The default feedback
    // led by a scanout tranche, the formats the monitor's primary plane
    // takes on its own device, so the window's buffers can go to the
    // screen as they are. None in the test window, which has no planes.
    pub fn scanout(&self, backend: &Backend, output: &Output) -> Option<DmabufFeedback> {
        let Backend::Screen(screen) = backend else {
            return None;
        };
        let (device, planes) = screen.scanout(output)?;
        let formats: Vec<Format> = planes.intersection(&self.formats).copied().collect();
        if formats.is_empty() {
            return None;
        }
        DmabufFeedbackBuilder::new(self.node.dev_id(), self.formats.iter().copied())
            .add_preference_tranche(device.dev_id(), Some(TrancheFlags::Scanout), formats)
            .build()
            .ok()
    }
}

// Everything a running compositor holds beyond the window manager's own
// state. Absent when Wm runs headless under test.
pub struct Runtime {
    pub backend: Backend,
    pub bar: Bar,
    pub dsr: Dsr,
    pub cursor: Cursor,
    pub cursor_shape: CursorShapeManagerState,
    // Mouse-look: Relative motion for the clients that ask for it, and a
    // pointer locked in place or confined to a surface's region.
    pub relative_pointer: RelativePointerManagerState,
    pub pointer_constraints: PointerConstraintsState,
    // A surface shown at a size of its own, whatever its buffer's: How
    // XWayland shows a game's emulated mode change while the output keeps
    // its mode.
    pub viewporter: ViewporterState,
    // A window focused by the token of the client holding the keyboard.
    pub activation: XdgActivationState,
    // The icons windows set, for the tag menu.
    pub toplevel_icons: smithay::wayland::xdg_toplevel_icon::XdgToplevelIconManager,
    // Windows holding off the idle lock.
    pub idle_inhibit: smithay::wayland::idle_inhibit::IdleInhibitManagerState,
    // Text typed through an input method into the focused window.
    pub text_input: smithay::wayland::text_input::TextInputManagerState,
    pub input_method: smithay::wayland::input_method::InputMethodManagerState,
    // Server-side title bars for the toplevels that ask.
    pub decorations: smithay::wayland::shell::xdg::decoration::XdgDecorationState,
    // Frame pacing: A content update held until the one before it has been
    // on screen (fifo), or until the refresh its timestamp names (commit
    // timing).
    pub fifo: FifoManagerState,
    pub commit_timing: CommitTimingManagerState,
    // Surfaces on a monitor's layers rather than windows (wlr-layer-shell).
    pub layer_shell: smithay::wayland::shell::wlr_layer::WlrLayerShellState,
    // The monitors' frames copied by clients (ext-image-copy-capture).
    pub output_capture_source: smithay::wayland::image_capture_source::OutputCaptureSourceState,
    pub image_copy_capture: smithay::wayland::image_copy_capture::ImageCopyCaptureState,
    // The clipboard for clients with no surface focused.
    pub data_control: smithay::wayland::selection::ext_data_control::DataControlState,
    // Explicit sync, where the GPU's device can wait on a timeline point
    // through an eventfd; None elsewhere.
    pub syncobj: Option<DrmSyncobjState>,
    pub handle: LoopHandle<'static, Wm>,
    pub display: DisplayHandle,
    pub frames: Vec<OutputFrame>,
    pub clock: Clock<Monotonic>,
    pub presentation: PresentationState,
    // None when the renderer names no GPU (a software renderer).
    pub dmabuf: Option<LinuxDmabuf>,
    // What each child is started with on top of the compositor's own
    // environment: WAYLAND_DISPLAY, DISPLAY once XWayland is up, and the
    // cursor theme.
    pub env: Vec<(&'static str, String)>,
    // A bind's notification, sent back from the thread that ran its
    // commands.
    pub notices: smithay::reexports::calloop::channel::Sender<String>,
}

impl WindowHandler for Wm {
    fn window(&mut self) -> &mut Window<Wm> {
        match self.runtime.as_mut().map(|rt| &mut rt.backend) {
            Some(Backend::Window(window)) => window,
            _ => unreachable!("window events come only from the window backend"),
        }
    }

    fn window_event(&mut self, event: window::Event) {
        match event {
            window::Event::Resized => {
                let output = self.output.clone();
                self.set_monitors(&[output]);
                self.queue_redraw();
            }
            window::Event::Input(event) => self.process_input(event),
            window::Event::Presented { time, seq } => {
                let output = self.output.clone();
                self.frame_done(&output, Some((time, seq)));
            }
            window::Event::Closed => self.running = false,
        }
    }
}

impl ScreenHandler for Wm {
    fn screen(&mut self) -> &mut Screen<Wm> {
        match self.runtime.as_mut().map(|rt| &mut rt.backend) {
            Some(Backend::Screen(screen)) => screen,
            _ => unreachable!("screen events come only from the screen backend"),
        }
    }

    fn screen_event(&mut self, event: screen::Event) {
        match event {
            screen::Event::Input(event) => self.process_input(event),
            screen::Event::Presented { output, time, seq } => self.frame_done(&output, Some((time, seq))),
            screen::Event::Paused => {}
            screen::Event::Resumed => self.queue_redraw(),
            screen::Event::OutputAdded(output) => {
                if let Some(rt) = self.runtime.as_mut() {
                    let global = output.create_global::<Wm>(&rt.display);
                    tracing::info!(output = %output.name(), "monitor added");
                    let mut frame = OutputFrame::new(output.clone(), global);
                    frame.scanout = rt.dmabuf.as_ref().and_then(|d| d.scanout(&rt.backend, &output));
                    rt.frames.push(frame);
                }
                self.monitors_changed();
            }
            screen::Event::OutputRemoved(output) => {
                if let Some(rt) = self.runtime.as_mut() {
                    if let Some(i) = rt.frames.iter().position(|f| f.output == output) {
                        let frame = rt.frames.remove(i);
                        rt.display.remove_global::<Wm>(frame.global);
                        tracing::info!(output = %output.name(), "monitor removed");
                    }
                }
                self.gamma_output_gone(&output);
                self.layers_output_gone(&output);
                self.monitors_changed();
            }
        }
    }
}

impl Wm {
    // The monitors laid out again from the outputs lit, the first of them
    // the primary one, whose image description every surface prefers.
    fn monitors_changed(&mut self) {
        let outputs: Vec<Output> =
            self.runtime.as_ref().map(|rt| rt.frames.iter().map(|f| f.output.clone()).collect()).unwrap_or_default();
        let Some(primary) = outputs.first().cloned() else {
            return;
        };
        self.set_monitors(&outputs);
        self.layers_rearranged();
        self.capture_monitors_changed();
        if primary != self.output {
            self.output = primary;
            crate::color::preferred_changed(self);
        }
        self.queue_redraw();
    }
}
