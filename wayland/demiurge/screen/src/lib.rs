// The real screen: demiurge-wl on the machine's own monitors. libseat
// holds the session, udev finds the GPU, a DrmCompositor drives each lit
// monitor and libinput reads the keyboard and mouse. It passes monitors,
// input and presentation times up and shows the frames it is handed. It
// holds no DEMIURGE behavior and cannot see the window manager.
//
// One GPU (the boot one) is driven, and every connected monitor on it,
// each lit to the right of those already lit; the device map is where
// more GPUs go. A monitor plugged, unplugged or changed is picked up from
// udev.
//
// HDR10 is asked of a monitor by the compositor and put on its connector
// between flips: HDR_OUTPUT_METADATA, the BT.2020 colorspace and, where
// the driver has it, 10 bits per channel, in a commit of their own. A
// gamma ramp goes on its CRTC the same way, as GAMMA_LUT. smithay commits
// only the properties it knows, so they stand until changed, and it
// restores the state it found when the session ends.

pub mod color;

use demiurge_core::monitor::{Timing, desktop_mode, sized_mode};
use std::collections::HashMap;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};

use smithay::backend::allocator::Fourcc;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::format::FormatSet;
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::drm::compositor::{DrmCompositor, FrameFlags, PrimaryPlaneElement};
use smithay::backend::drm::exporter::gbm::{GbmFramebufferExporter, NodeFilter};
use smithay::backend::drm::{DrmDevice, DrmDeviceFd, DrmEvent, DrmEventTime, DrmNode, NodeType};
use smithay::backend::egl::{EGLContext, EGLDisplay};
use smithay::backend::input::InputEvent;
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::renderer::element::{RenderElement, RenderElementStates};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::{Color32F, ImportDma};
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::session::{Event as SessionEvent, Session};
use smithay::backend::udev::{UdevBackend, UdevEvent, primary_gpu};
use smithay::output::{Mode, Output, OutputModeSource, PhysicalProperties, Scale};
use smithay::reexports::calloop::{LoopHandle, RegistrationToken};
use smithay::reexports::drm::control::atomic::AtomicModeReq;
use smithay::reexports::drm::control::property::{self, ValueType};
use smithay::reexports::drm::control::{self, AtomicCommitFlags, Device as _, ModeTypeFlags, connector, crtc};
use smithay::reexports::input::Libinput;
use smithay::reexports::rustix::fs::OFlags;
use smithay::utils::{Clock, DeviceFd, Monotonic, Time, Transform};

use crate::color::{DisplayColor, HdrMetadata};

type Compositor = DrmCompositor<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

// What the compositor implements to receive the screen's events.
pub trait ScreenHandler: Sized + 'static {
    fn screen(&mut self) -> &mut Screen<Self>;
    fn screen_event(&mut self, event: Event);
}

pub enum Event {
    OutputAdded(Output),
    OutputRemoved(Output),
    Input(InputEvent<LibinputInputBackend>),
    // The frame `render` last queued on `output` is on screen.
    Presented { output: Output, time: Time<Monotonic>, seq: u64 },
    // The session lost or regained the VT.
    Paused,
    Resumed,
}

pub enum Rendered {
    Queued(RenderElementStates),
    // Nothing changed since the last frame, so nothing was queued.
    Empty(RenderElementStates),
    // The session is paused; nothing can be shown until it resumes.
    Inactive,
}

pub struct Screen<D: 'static> {
    session: LibSeatSession,
    libinput: Libinput,
    handle: LoopHandle<'static, D>,
    primary: Option<PathBuf>,
    devices: HashMap<DrmNode, Device>,
    clock: Clock<Monotonic>,
    modes: Vec<ModeRequest>,
}

/// A mode asked of the monitors whose connector name holds `pattern`, case
/// aside: Its size, at the highest progressive rate the monitor offers it.
/// A monitor that offers no mode of that size is lit as it would be
/// unasked.
#[derive(Clone, Debug)]
pub struct ModeRequest {
    pub pattern: String,
    pub size: (u16, u16),
}

struct Device {
    drm: DrmDevice,
    gbm: GbmDevice<DrmDeviceFd>,
    renderer: GlesRenderer,
    surfaces: HashMap<crtc::Handle, Surface>,
    token: RegistrationToken,
}

struct Surface {
    output: Output,
    compositor: Compositor,
    connector: connector::Handle,
    mode: control::Mode,
    // What its monitor's EDID says about color, and the connector's HDR
    // properties where its driver has them.
    color: Option<DisplayColor>,
    hdr: Option<HdrProperties>,
    // HDR10 as the compositor wants it and as the connector has it, None
    // for SDR; the connector's is unknown until the first commit and after
    // the VT comes back. A flip in flight, which a connector commit waits
    // behind.
    hdr_wanted: Option<HdrMetadata>,
    hdr_shown: Option<Option<HdrMetadata>>,
    // The CRTC's gamma LUT where its driver has one; the ramps the
    // compositor wants on it, red, green then blue, None for the value it
    // was found at; and whether the CRTC has them, false until the first
    // commit and after the VT comes back.
    gamma: Option<GammaProperties>,
    gamma_wanted: Option<Vec<u16>>,
    gamma_shown: bool,
    flipping: bool,
}

struct GammaProperties {
    lut: property::Handle,
    size: u32,
    // The LUT the CRTC was found with, None for none: Its own blob may live
    // only as long as the CRTC holds it, so it is made again to restore.
    found: Option<Vec<u8>>,
}

struct HdrProperties {
    metadata: property::Handle,
    colorspace: property::Handle,
    bt2020: property::RawValue,
    default: property::RawValue,
    // "max bpc" and the value the connector was found at.
    max_bpc: Option<(property::Handle, property::RawValue)>,
}

impl<D: ScreenHandler> Screen<D> {
    pub fn new(handle: LoopHandle<'static, D>, modes: Vec<ModeRequest>) -> Result<Self, String> {
        let (session, notifier) = LibSeatSession::new().map_err(|e| format!("libseat: {}", e))?;
        let seat = session.seat();

        let mut libinput = Libinput::new_with_udev(LibinputSessionInterface::from(session.clone()));
        libinput
            .udev_assign_seat(&seat)
            .map_err(|()| format!("libinput: no seat {}", seat))?;
        handle
            .insert_source(LibinputInputBackend::new(libinput.clone()), |event, _, data: &mut D| {
                data.screen_event(Event::Input(event));
            })
            .map_err(|e| format!("insert libinput: {}", e))?;
        handle
            .insert_source(notifier, |event, _, data: &mut D| match event {
                SessionEvent::PauseSession => {
                    data.screen().pause();
                    data.screen_event(Event::Paused);
                }
                SessionEvent::ActivateSession => {
                    data.screen().resume();
                    // A monitor changed while the VT was away is caught up.
                    let devices: Vec<_> = data.screen().devices.keys().map(|node| node.dev_id()).collect();
                    for device_id in devices {
                        hotplug(data, device_id);
                    }
                    data.screen_event(Event::Resumed);
                }
            })
            .map_err(|e| format!("insert session: {}", e))?;

        let udev = UdevBackend::new(&seat).map_err(|e| format!("udev: {}", e))?;
        let mut screen = Self {
            session,
            libinput,
            handle: handle.clone(),
            primary: primary_gpu(&seat).ok().flatten(),
            devices: HashMap::new(),
            clock: Clock::new(),
            modes,
        };
        // udev reports only devices that change from here on; the ones
        // already present come from its list.
        let present: Vec<_> = udev.device_list().map(|(id, path)| (id, path.to_path_buf())).collect();
        for (id, path) in present {
            if let Err(e) = screen.add_device(id, &path) {
                tracing::warn!(path = %path.display(), "GPU not driven: {}", e);
            }
        }
        handle
            .insert_source(udev, |event, _, data: &mut D| match event {
                UdevEvent::Added { device_id, path } => match data.screen().add_device(device_id, &path) {
                    Ok(outputs) => {
                        for output in outputs {
                            data.screen_event(Event::OutputAdded(output));
                        }
                    }
                    Err(e) => tracing::warn!(path = %path.display(), "GPU not driven: {}", e),
                },
                // A connector's hotplug: A monitor plugged, unplugged, woken
                // from power-off or swapped for another.
                UdevEvent::Changed { device_id } => hotplug(data, device_id),
                UdevEvent::Removed { device_id } => {
                    for output in data.screen().remove_device(device_id) {
                        data.screen_event(Event::OutputRemoved(output));
                    }
                }
            })
            .map_err(|e| format!("insert udev: {}", e))?;
        Ok(screen)
    }

    /// Every monitor's CRTC off, DPMS off, until the next frame queued for
    /// it turns it on again.
    pub fn power_off(&mut self) {
        for device in self.devices.values_mut() {
            for surface in device.surfaces.values_mut() {
                if let Err(e) = surface.compositor.clear() {
                    tracing::warn!(output = %surface.output.name(), "power off: {}", e);
                }
            }
        }
    }

    pub fn outputs(&self) -> Vec<Output> {
        self.devices
            .values()
            .flat_map(|d| d.surfaces.values().map(|s| s.output.clone()))
            .collect()
    }

    // The render node of the GPU driven, which clients allocate their
    // buffers on.
    pub fn render_node(&self) -> Option<DrmNode> {
        self.devices.keys().next()?.node_with_type(NodeType::Render)?.ok()
    }

    // That GPU's renderer, which client buffers are imported into whether
    // a monitor is lit or not.
    pub fn gpu_renderer(&mut self) -> Option<&mut GlesRenderer> {
        self.devices.values_mut().next().map(|d| &mut d.renderer)
    }

    // The renderer of the GPU driving `output`; frames for it are built
    // with this one.
    pub fn renderer(&mut self, output: &Output) -> Option<&mut GlesRenderer> {
        self.devices
            .values_mut()
            .find(|d| d.surfaces.values().any(|s| &s.output == output))
            .map(|d| &mut d.renderer)
    }

    // Import a client's buffer into the GPU's renderer. One it takes is
    // marked as that GPU's, which lets a monitor's planes show it as it
    // is instead of compositing it.
    pub fn import_dmabuf(&mut self, dmabuf: &Dmabuf) -> Result<(), String> {
        let (&node, device) = self.devices.iter_mut().next().ok_or("no GPU driven")?;
        device
            .renderer
            .import_dmabuf(dmabuf, None)
            .map_err(|e| format!("import: {}", e))?;
        dmabuf.set_node(node);
        Ok(())
    }

    // The driven GPU's device, which explicit sync imports clients'
    // timelines on.
    pub fn device_fd(&self) -> Option<DrmDeviceFd> {
        self.devices.values().next().map(|d| d.drm.device_fd().clone())
    }

    // What a client's buffer needs for `output`'s primary plane to show it
    // as it is: The device the plane belongs to and the formats it scans
    // out.
    pub fn scanout(&self, output: &Output) -> Option<(DrmNode, FormatSet)> {
        self.devices.iter().find_map(|(&node, d)| {
            let surface = d.surfaces.values().find(|s| &s.output == output)?;
            Some((node, surface.compositor.surface().plane_info().formats.clone()))
        })
    }

    // What `output`'s monitor says about its color in its EDID.
    pub fn display_color(&self, output: &Output) -> Option<DisplayColor> {
        self.devices.values().flat_map(|d| d.surfaces.values()).find(|s| &s.output == output)?.color.clone()
    }

    // Put `output` in HDR10 with `metadata`, or back to SDR with None: At
    // once with no flip in flight, else as the one in flight lands. A
    // monitor that does not take HDR10, or a connector without the
    // properties for it, stays SDR, saying why.
    pub fn set_hdr(&mut self, output: &Output, metadata: Option<HdrMetadata>) {
        let Some(device) = self.devices.values_mut().find(|d| d.surfaces.values().any(|s| &s.output == output)) else {
            return;
        };
        let surface = device.surfaces.values_mut().find(|s| &s.output == output).expect("device found by this output");
        if surface.hdr_wanted == metadata {
            return;
        }
        if metadata.is_some() && surface.hdr_wanted.is_none() {
            let why = match (&surface.color, &surface.hdr) {
                (Some(c), Some(_)) if c.hdr10() => None,
                (None, _) => Some("its monitor has no EDID"),
                (Some(c), _) if !c.pq => Some("its monitor's EDID does not take PQ"),
                (Some(c), _) if !c.bt2020 => Some("its monitor's EDID does not take BT.2020"),
                _ => Some("its connector has no HDR_OUTPUT_METADATA or BT.2020 colorspace"),
            };
            if let Some(why) = why {
                tracing::info!(output = %output.name(), "HDR10 not shown: {}", why);
            }
        }
        surface.hdr_wanted = metadata;
        if device.drm.is_active() && !surface.flipping {
            show_hdr(&device.drm, surface);
        }
    }

    // The entries in each of `output`'s gamma ramps, where its CRTC has a
    // gamma LUT.
    pub fn gamma_size(&self, output: &Output) -> Option<u32> {
        self.devices.values().flat_map(|d| d.surfaces.values()).find(|s| &s.output == output)?.gamma.as_ref().map(|g| g.size)
    }

    // Put `ramps` (red, green then blue, gamma_size entries each) on
    // `output`'s CRTC, or with None the value it was found at: At once with
    // no flip in flight, else as the one in flight lands. An error where the
    // CRTC has no gamma LUT or the ramps are not its size.
    pub fn set_gamma(&mut self, output: &Output, ramps: Option<Vec<u16>>) -> Result<(), String> {
        let device = self
            .devices
            .values_mut()
            .find(|d| d.surfaces.values().any(|s| &s.output == output))
            .ok_or_else(|| format!("no surface for {}", output.name()))?;
        let (&crtc, surface) = device.surfaces.iter_mut().find(|(_, s)| &s.output == output).expect("device found by this output");
        let Some(gamma) = &surface.gamma else {
            return Err(format!("{}'s CRTC has no gamma LUT", output.name()));
        };
        if ramps.as_ref().is_some_and(|r| r.len() != 3 * gamma.size as usize) {
            return Err(format!("{}'s gamma ramps take {} entries each", output.name(), gamma.size));
        }
        surface.gamma_wanted = ramps;
        surface.gamma_shown = false;
        if device.drm.is_active() && !surface.flipping {
            show_gamma(&device.drm, crtc, surface);
        }
        Ok(())
    }

    // Draw `elements` (front to back) on `output` and queue them for its
    // next vblank, if anything changed.
    pub fn render<E: RenderElement<GlesRenderer>>(
        &mut self,
        output: &Output,
        elements: &[E],
        clear: Color32F,
    ) -> Result<Rendered, String> {
        let device = self
            .devices
            .values_mut()
            .find(|d| d.surfaces.values().any(|s| &s.output == output))
            .ok_or_else(|| format!("no surface for {}", output.name()))?;
        if !device.drm.is_active() {
            return Ok(Rendered::Inactive);
        }
        let surface = device
            .surfaces
            .values_mut()
            .find(|s| &s.output == output)
            .expect("device found by this output");
        // A client's buffer may take the primary plane in any format the
        // plane scans out, the atomic test deciding, not only in the
        // swapchain's: An opaque buffer's framebuffer is always XRGB, which
        // the ARGB swapchain never matches.
        let flags = FrameFlags::DEFAULT | FrameFlags::ALLOW_PRIMARY_PLANE_SCANOUT_ANY;
        let (empty, states) = {
            let result = surface
                .compositor
                .render_frame(&mut device.renderer, elements, clear, flags)
                .map_err(|e| format!("render_frame: {:?}", e))?;
            // Where the kernel cannot be handed the render's fence (no
            // IN_FENCE_FD, or a renderer with no native fence, like a
            // software one), the flip must wait here for the render to
            // finish, or a half-drawn buffer goes to the screen.
            if result.needs_sync() {
                if let PrimaryPlaneElement::Swapchain(element) = &result.primary_element {
                    let _ = element.sync.wait();
                }
            }
            (result.is_empty, result.states)
        };
        if empty {
            return Ok(Rendered::Empty(states));
        }
        surface
            .compositor
            .queue_frame(())
            .map_err(|e| format!("queue_frame: {:?}", e))?;
        surface.flipping = true;
        Ok(Rendered::Queued(states))
    }

    pub fn change_vt(&mut self, vt: i32) {
        if let Err(e) = self.session.change_vt(vt) {
            tracing::warn!("change to VT {}: {}", vt, e);
        }
    }

    fn add_device(&mut self, device_id: libc::dev_t, path: &Path) -> Result<Vec<Output>, String> {
        let wanted = match &self.primary {
            Some(primary) => primary == path,
            None => self.devices.is_empty(),
        };
        if !wanted {
            tracing::info!(path = %path.display(), "not the boot GPU; left alone");
            return Ok(Vec::new());
        }
        let node = DrmNode::from_dev_id(device_id).map_err(|e| format!("drm node: {}", e))?;
        let fd = self
            .session
            .open(path, OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK)
            .map_err(|e| format!("open: {}", e))?;
        let fd = DrmDeviceFd::new(DeviceFd::from(fd));
        let (drm, notifier) = DrmDevice::new(fd.clone(), true).map_err(|e| format!("drm: {}", e))?;
        let gbm = GbmDevice::new(fd).map_err(|e| format!("gbm: {}", e))?;
        let egl = unsafe { EGLDisplay::new(gbm.clone()) }.map_err(|e| format!("egl display: {}", e))?;
        let context = EGLContext::new(&egl).map_err(|e| format!("egl context: {}", e))?;
        let renderer = unsafe { GlesRenderer::new(context) }.map_err(|e| format!("gles: {}", e))?;

        let token = self
            .handle
            .insert_source(notifier, move |event, metadata, data: &mut D| match event {
                DrmEvent::VBlank(crtc) => {
                    let screen = data.screen();
                    let Some(output) = screen.vblank(node, crtc) else {
                        return;
                    };
                    let time = match metadata.as_ref().map(|m| m.time) {
                        Some(DrmEventTime::Monotonic(time)) => Time::from(time),
                        _ => screen.clock.now(),
                    };
                    let seq = metadata.as_ref().map_or(0, |m| m.sequence as u64);
                    data.screen_event(Event::Presented { output, time, seq });
                }
                DrmEvent::Error(e) => tracing::error!("drm: {}", e),
            })
            .map_err(|e| format!("insert drm notifier: {}", e))?;

        let mut device = Device { drm, gbm, renderer, surfaces: HashMap::new(), token };
        let lit = device.light_new(node, &self.modes);
        self.devices.insert(node, device);
        lit
    }

    // Recheck a driven GPU's monitors after a hotplug: One whose connector
    // is gone, or which now prefers another mode, is dropped, and every
    // connected one not lit is lit. Returns those dropped and those lit.
    fn rescan(&mut self, device_id: libc::dev_t) -> (Vec<Output>, Vec<Output>) {
        let Some((node, device)) = DrmNode::from_dev_id(device_id)
            .ok()
            .and_then(|node| Some((node, self.devices.get_mut(&node)?)))
        else {
            return (Vec::new(), Vec::new());
        };
        if !device.drm.is_active() {
            // Off our VT, the device cannot be probed or lit; resuming
            // restores the monitors as they were.
            return (Vec::new(), Vec::new());
        }
        let mut dropped = Vec::new();
        let Device { drm, surfaces, .. } = &mut *device;
        surfaces.retain(|_, surface| {
            // A full probe: A hotplug updates a connector's status, not
            // always its modes.
            let still = drm
                .get_connector(surface.connector, true)
                .is_ok_and(|info| info.state() == connector::State::Connected && lit_mode(&info, &self.modes) == Some(surface.mode));
            if !still {
                tracing::info!(output = %surface.output.name(), "monitor dropped");
                dropped.push(surface.output.clone());
            }
            still
        });
        let lit = device.light_new(node, &self.modes).unwrap_or_else(|e| {
            tracing::info!("no monitor lit: {}", e);
            Vec::new()
        });
        (dropped, lit)
    }

    fn remove_device(&mut self, device_id: libc::dev_t) -> Vec<Output> {
        let Some(device) = DrmNode::from_dev_id(device_id).ok().and_then(|node| self.devices.remove(&node)) else {
            return Vec::new();
        };
        self.handle.remove(device.token);
        device.surfaces.into_values().map(|s| s.output).collect()
    }

    // A page flip finished on `crtc`: Its buffer goes back to the swapchain,
    // and an HDR or gamma change waiting behind it goes to the connector or
    // the CRTC.
    fn vblank(&mut self, node: DrmNode, crtc: crtc::Handle) -> Option<Output> {
        let device = self.devices.get_mut(&node)?;
        let surface = device.surfaces.get_mut(&crtc)?;
        if let Err(e) = surface.compositor.frame_submitted() {
            tracing::warn!("frame_submitted: {:?}", e);
        }
        surface.flipping = false;
        show_hdr(&device.drm, surface);
        show_gamma(&device.drm, crtc, surface);
        Some(surface.output.clone())
    }

    fn pause(&mut self) {
        self.libinput.suspend();
        for device in self.devices.values_mut() {
            device.drm.pause();
        }
    }

    fn resume(&mut self) {
        if self.libinput.resume().is_err() {
            tracing::warn!("libinput did not resume");
        }
        for device in self.devices.values_mut() {
            if let Err(e) = device.drm.activate(false) {
                tracing::warn!("drm activate: {}", e);
            }
            for (&crtc, surface) in device.surfaces.iter_mut() {
                if let Err(e) = surface.compositor.reset_state() {
                    tracing::warn!("reset {}: {}", surface.output.name(), e);
                }
                // Whoever held the VT may have changed the connector and
                // the CRTC; they get what the compositor wants again.
                surface.flipping = false;
                surface.hdr_shown = None;
                show_hdr(&device.drm, surface);
                surface.gamma_shown = false;
                show_gamma(&device.drm, crtc, surface);
            }
        }
    }
}

impl Device {
    // Light every connected monitor not lit yet, each at its preferred
    // mode and to the right of those lit. The probe is a full one, since
    // after a hotplug a connector's modes may not have been read yet. One
    // that cannot be lit is passed over for the rest.
    fn light_new(&mut self, node: DrmNode, modes: &[ModeRequest]) -> Result<Vec<Output>, String> {
        let resources = self.drm.resource_handles().map_err(|e| format!("resources: {}", e))?;
        let mut lit = Vec::new();
        for &handle in resources.connectors() {
            if self.surfaces.values().any(|s| s.connector == handle) {
                continue;
            }
            let Ok(info) = self.drm.get_connector(handle, true) else {
                continue;
            };
            if info.state() != connector::State::Connected {
                continue;
            }
            let Some(mode) = lit_mode(&info, modes) else {
                continue;
            };
            let Some(crtc) = info
                .encoders()
                .iter()
                .filter_map(|&e| self.drm.get_encoder(e).ok())
                .flat_map(|e| resources.filter_crtcs(e.possible_crtcs()))
                .find(|c| !self.surfaces.contains_key(c))
            else {
                continue;
            };

            let output = Output::new(
                connector_name(&info),
                PhysicalProperties {
                    size: info.size().map_or((0, 0), |(w, h)| (w as i32, h as i32)).into(),
                    subpixel: info.subpixel().into(),
                    make: "Unknown".to_string(),
                    model: "Unknown".to_string(),
                    serial_number: "Unknown".to_string(),
                },
            );
            for m in info.modes() {
                output.add_mode(Mode::from(*m));
            }
            output.set_preferred(Mode::from(preferred(&info).unwrap_or(mode)));
            let x = self.surfaces.values().map(|s| s.output.current_location().x + s.mode.size().0 as i32).max().unwrap_or(0);
            output.change_current_state(
                Some(Mode::from(mode)),
                Some(Transform::Normal),
                Some(Scale::Integer(1)),
                Some((x, 0).into()),
            );

            let surface = match self.drm.create_surface(crtc, mode, &[handle]) {
                Ok(surface) => surface,
                Err(e) => {
                    tracing::warn!(output = %output.name(), "monitor not lit: surface: {}", e);
                    continue;
                }
            };
            let compositor = match DrmCompositor::new(
                OutputModeSource::Auto(output.downgrade()),
                surface,
                None,
                GbmAllocator::new(self.gbm.clone(), GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT),
                GbmFramebufferExporter::new(self.gbm.clone(), NodeFilter::Node(node)),
                [Fourcc::Argb8888, Fourcc::Xrgb8888],
                self.renderer.egl_context().dmabuf_render_formats().iter().copied(),
                self.drm.cursor_size(),
                Some(self.gbm.clone()),
            ) {
                Ok(compositor) => compositor,
                Err(e) => {
                    tracing::warn!(output = %output.name(), "monitor not lit: compositor: {:?}", e);
                    continue;
                }
            };
            let color = self.edid(handle).and_then(|edid| color::parse(&edid));
            let hdr = self.hdr_properties(handle);
            let gamma = self.gamma_properties(crtc);
            let frame = mode.hsync().2 as f64 * mode.vsync().2 as f64;
            let hz = if frame > 0.0 { mode.clock() as f64 * 1000.0 / frame } else { 0.0 };
            tracing::info!(
                output = %output.name(),
                size = ?mode.size(),
                hz = %format_args!("{hz:.2}"),
                x,
                hdr10 = color.as_ref().is_some_and(DisplayColor::hdr10) && hdr.is_some(),
                gamma = gamma.as_ref().map_or(0, |g| g.size),
                "monitor lit"
            );
            self.surfaces.insert(
                crtc,
                Surface {
                    output: output.clone(),
                    compositor,
                    connector: handle,
                    mode,
                    color,
                    hdr,
                    hdr_wanted: None,
                    hdr_shown: None,
                    gamma,
                    gamma_wanted: None,
                    gamma_shown: true,
                    flipping: false,
                },
            );
            lit.push(output);
        }
        if self.surfaces.is_empty() {
            return Err("no connected monitor".to_string());
        }
        Ok(lit)
    }

    fn edid(&self, connector: connector::Handle) -> Option<Vec<u8>> {
        let (_, value) = self.property(connector, "EDID")?;
        self.drm.get_property_blob(value).ok().filter(|b| !b.is_empty())
    }

    // The connector's HDR_OUTPUT_METADATA and Colorspace with its BT.2020
    // RGB and Default values, and "max bpc" where the driver has it.
    fn hdr_properties(&self, connector: connector::Handle) -> Option<HdrProperties> {
        let (metadata, _) = self.property(connector, "HDR_OUTPUT_METADATA")?;
        let (colorspace, _) = self.property(connector, "Colorspace")?;
        let ValueType::Enum(values) = self.drm.get_property(colorspace).ok()?.value_type() else {
            return None;
        };
        let named = |name: &str| values.values().1.iter().find(|v| v.name().to_bytes() == name.as_bytes()).map(|v| v.value());
        Some(HdrProperties {
            metadata,
            colorspace,
            bt2020: named("BT2020_RGB")?,
            default: named("Default")?,
            max_bpc: self.property(connector, "max bpc"),
        })
    }

    // The CRTC's GAMMA_LUT, with the LUT it was found with, and the size
    // GAMMA_LUT_SIZE gives it.
    fn gamma_properties(&self, crtc: crtc::Handle) -> Option<GammaProperties> {
        let (lut, found) = self.property(crtc, "GAMMA_LUT")?;
        let (_, size) = self.property(crtc, "GAMMA_LUT_SIZE")?;
        let found = (found != 0).then(|| self.drm.get_property_blob(found).ok()).flatten().filter(|b| !b.is_empty());
        (size > 0).then_some(GammaProperties { lut, size: size as u32, found })
    }

    fn property<H: control::ResourceHandle>(&self, handle: H, name: &str) -> Option<(property::Handle, property::RawValue)> {
        let set = self.drm.get_properties(handle).ok()?;
        set.iter()
            .find(|(handle, _)| self.drm.get_property(**handle).is_ok_and(|p| p.name().to_bytes() == name.as_bytes()))
            .map(|(&handle, &value)| (handle, value))
    }
}

// Put the HDR10 the compositor wants for `surface` on its connector, in a
// commit of its own, if the connector does not have it already: The
// metadata, BT.2020 and 10 bits per channel for HDR10; no metadata, the
// default colorspace and the bpc it was found at for SDR. A commit the
// driver refuses, other than for a flip in flight, leaves the monitor SDR
// from then on.
fn show_hdr(drm: &DrmDevice, surface: &mut Surface) {
    if surface.hdr_shown == Some(surface.hdr_wanted) {
        return;
    }
    let Some(props) = &surface.hdr else {
        surface.hdr_shown = Some(surface.hdr_wanted);
        return;
    };
    if !surface.color.as_ref().is_some_and(DisplayColor::hdr10) && surface.hdr_wanted.is_some() {
        surface.hdr_shown = Some(surface.hdr_wanted);
        return;
    }
    let blob = match &surface.hdr_wanted {
        Some(metadata) => match drm.create_property_blob(&metadata.blob()) {
            Ok(property::Value::Blob(id)) => Some(id),
            Ok(_) => None,
            Err(e) => {
                tracing::warn!(output = %surface.output.name(), "HDR10 metadata blob: {}", e);
                surface.hdr = None;
                return;
            }
        },
        None => None,
    };
    let mut req = AtomicModeReq::new();
    let connector = surface.connector.into();
    req.add_raw_property(connector, props.metadata, blob.unwrap_or(0));
    let hdr = surface.hdr_wanted.is_some();
    req.add_raw_property(connector, props.colorspace, if hdr { props.bt2020 } else { props.default });
    if let Some((max_bpc, found)) = props.max_bpc {
        req.add_raw_property(connector, max_bpc, if hdr { found.max(10) } else { found });
    }
    let result = drm.atomic_commit(AtomicCommitFlags::ALLOW_MODESET, req);
    if let Some(id) = blob {
        // The connector holds its own reference to the blob.
        let _ = drm.destroy_property_blob(id);
    }
    match result {
        Ok(()) => {
            match (&surface.hdr_wanted, surface.hdr_shown) {
                (Some(m), _) => tracing::info!(output = %surface.output.name(), metadata = ?m, "HDR10 on"),
                (None, Some(Some(_))) => tracing::info!(output = %surface.output.name(), "HDR10 off"),
                (None, _) => {}
            }
            surface.hdr_shown = Some(surface.hdr_wanted);
        }
        // A flip in flight; the next vblank tries again.
        Err(e) if e.raw_os_error() == Some(libc::EBUSY) => {}
        Err(e) => {
            tracing::warn!(output = %surface.output.name(), "the driver refused the connector's HDR properties, so the monitor stays SDR: {}", e);
            surface.hdr = None;
            surface.hdr_shown = Some(surface.hdr_wanted);
        }
    }
}

// Put the gamma ramps the compositor wants for `surface` on its CRTC, in
// a commit of its own, if the CRTC does not have them already: A blob of
// drm_color_lut entries, 16-bit red, green and blue and a reserved word
// each, or the LUT the CRTC was found with. A commit the driver refuses,
// other than for a flip in flight, leaves the CRTC without gamma from then
// on.
fn show_gamma(drm: &DrmDevice, crtc: crtc::Handle, surface: &mut Surface) {
    if surface.gamma_shown {
        return;
    }
    let Some(props) = &surface.gamma else {
        surface.gamma_shown = true;
        return;
    };
    let lut = match &surface.gamma_wanted {
        Some(ramps) => {
            let n = props.size as usize;
            let mut lut = Vec::with_capacity(n * 8);
            for i in 0..n {
                for channel in [ramps[i], ramps[n + i], ramps[2 * n + i], 0] {
                    lut.extend_from_slice(&channel.to_ne_bytes());
                }
            }
            Some(lut)
        }
        None => props.found.clone(),
    };
    let blob = match lut {
        Some(mut lut) => match drm_ffi::mode::create_property_blob(drm.as_fd(), &mut lut) {
            Ok(blob) => Some(blob.blob_id as u64),
            Err(e) => {
                tracing::warn!(output = %surface.output.name(), "gamma LUT blob: {}", e);
                surface.gamma = None;
                return;
            }
        },
        None => None,
    };
    let mut req = AtomicModeReq::new();
    req.add_raw_property(crtc.into(), props.lut, blob.unwrap_or(0));
    let result = drm.atomic_commit(AtomicCommitFlags::empty(), req);
    if let Some(id) = blob {
        // The CRTC holds its own reference to the blob.
        let _ = drm.destroy_property_blob(id);
    }
    match result {
        Ok(()) => surface.gamma_shown = true,
        // A flip in flight; the next vblank tries again.
        Err(e) if e.raw_os_error() == Some(libc::EBUSY) => {}
        Err(e) => {
            tracing::warn!(output = %surface.output.name(), "the driver refused the CRTC's gamma LUT, so the monitor keeps its gamma: {}", e);
            surface.gamma = None;
            surface.gamma_shown = true;
        }
    }
}

// Pass a GPU's monitors dropped and lit by a rescan up to the compositor.
fn hotplug<D: ScreenHandler>(data: &mut D, device_id: libc::dev_t) {
    let (dropped, lit) = data.screen().rescan(device_id);
    for output in dropped {
        data.screen_event(Event::OutputRemoved(output));
    }
    for output in lit {
        data.screen_event(Event::OutputAdded(output));
    }
}

// The mode a monitor's EDID prefers, else its first.
fn preferred(info: &connector::Info) -> Option<control::Mode> {
    info.modes()
        .iter()
        .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
        .or_else(|| info.modes().first())
        .copied()
}

// A connector's name as the outputs carry it: "DP-3", "HDMI-A-1".
fn connector_name(info: &connector::Info) -> String {
    format!("{}-{}", info.interface().as_str(), info.interface_id())
}

// The mode a monitor is lit at: The size a request names for its connector
// where the monitor offers it, else its desktop mode, else its first mode
// where it prefers none.
fn lit_mode(info: &connector::Info, requests: &[ModeRequest]) -> Option<control::Mode> {
    let timings: Vec<Timing> = info
        .modes()
        .iter()
        .map(|m| Timing {
            size: m.size(),
            clock: m.clock(),
            htotal: m.hsync().2,
            vtotal: m.vsync().2,
            progressive: !m.flags().intersects(control::ModeFlags::INTERLACE | control::ModeFlags::DBLSCAN),
            preferred: m.mode_type().contains(ModeTypeFlags::PREFERRED),
        })
        .collect();
    let name = connector_name(info).to_lowercase();
    let asked = requests
        .iter()
        .find(|r| name.contains(&r.pattern.to_lowercase()))
        .and_then(|r| sized_mode(&timings, r.size));
    asked
        .or_else(|| desktop_mode(&timings))
        .or((!timings.is_empty()).then_some(0))
        .map(|i| info.modes()[i])
}
