//! Wayland gamma control via wlr-gamma-control-unstable-v1.
//!
//! Covers compositors implementing the wlr protocol:
//!   Sway, Hyprland, river, labwc, wayfire, niri
//!
//! Uses memfd for gamma ramp transfer (no tmpfile needed).
//! Protocol auto-restores gamma when controls are destroyed.

use super::{colorramp, Error};
use std::os::fd::AsFd;
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};

use wayland_client::protocol::{wl_output::WlOutput, wl_registry};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols_wlr::gamma_control::v1::client::{
    zwlr_gamma_control_manager_v1::ZwlrGammaControlManagerV1,
    zwlr_gamma_control_v1::{self, ZwlrGammaControlV1},
};

/// Per-output state
struct OutputState {
    output: WlOutput,
    gamma_control: Option<ZwlrGammaControlV1>,
    gamma_size: u32,
    failed: bool,
}

/// Internal state used during Wayland dispatch
struct WaylandInner {
    gamma_manager: Option<ZwlrGammaControlManagerV1>,
    outputs: Vec<OutputState>,
}

// Registry listener: discover globals
impl Dispatch<wl_registry::WlRegistry, ()> for WaylandInner {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version: _,
        } = event
        {
            if interface == "zwlr_gamma_control_manager_v1" {
                state.gamma_manager =
                    Some(registry.bind::<ZwlrGammaControlManagerV1, _, _>(name, 1, qh, ()));
            } else if interface == "wl_output" {
                let output = registry.bind::<WlOutput, _, _>(name, 1, qh, ());
                state.outputs.push(OutputState {
                    output,
                    gamma_control: None,
                    gamma_size: 0,
                    failed: false,
                });
            }
        }
    }
}

// Gamma control listener: receive gamma_size / failed events
// The usize user data is the output index
impl Dispatch<ZwlrGammaControlV1, usize> for WaylandInner {
    fn event(
        state: &mut Self,
        _proxy: &ZwlrGammaControlV1,
        event: zwlr_gamma_control_v1::Event,
        idx: &usize,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let Some(out) = state.outputs.get_mut(*idx) {
            match event {
                zwlr_gamma_control_v1::Event::GammaSize { size } => {
                    out.gamma_size = size;
                }
                zwlr_gamma_control_v1::Event::Failed => {
                    out.failed = true;
                    if let Some(ctrl) = out.gamma_control.take() {
                        ctrl.destroy();
                    }
                }
                _ => {}
            }
        }
    }
}

// No-op dispatchers for types we don't handle events on
delegate_noop!(WaylandInner: ignore WlOutput);
delegate_noop!(WaylandInner: ignore ZwlrGammaControlManagerV1);

/// Public Wayland gamma state
pub struct WaylandState {
    conn: Connection,
    queue: EventQueue<WaylandInner>,
    inner: WaylandInner,
}

impl WaylandState {
    pub fn init() -> Result<Self, Error> {
        let conn = Connection::connect_to_env().map_err(|_| Error::WaylandConnect)?;
        let display = conn.display();

        let mut inner = WaylandInner {
            gamma_manager: None,
            outputs: Vec::new(),
        };

        let mut queue = conn.new_event_queue();
        let qh = queue.handle();

        // Get registry and discover globals
        let _registry = display.get_registry(&qh, ());
        queue
            .roundtrip(&mut inner)
            .map_err(|_| Error::WaylandConnect)?;

        // Check gamma manager was found
        let manager = match inner.gamma_manager {
            Some(ref m) => m.clone(),
            None => return Err(Error::WaylandProtocol),
        };

        if inner.outputs.is_empty() {
            return Err(Error::NoCrtc);
        }

        // Acquire gamma control for each output
        for i in 0..inner.outputs.len() {
            let ctrl =
                manager.get_gamma_control(&inner.outputs[i].output, &qh, i);
            inner.outputs[i].gamma_control = Some(ctrl);
        }

        // Second roundtrip: receive gamma_size events
        queue
            .roundtrip(&mut inner)
            .map_err(|_| Error::WaylandConnect)?;

        // Check at least one output has usable gamma
        let usable = inner
            .outputs
            .iter()
            .filter(|o| !o.failed && o.gamma_size > 0)
            .count();
        if usable == 0 {
            return Err(Error::NoCrtc);
        }

        Ok(WaylandState { conn, queue, inner })
    }

    pub fn crtc_count(&self) -> usize {
        self.inner.outputs.len()
    }

    pub fn gamma_size(&self, crtc_idx: usize) -> usize {
        self.inner
            .outputs
            .get(crtc_idx)
            .filter(|o| !o.failed)
            .map(|o| o.gamma_size as usize)
            .unwrap_or(0)
    }

    pub fn set_temperature_crtc(
        &mut self,
        crtc_idx: usize,
        temp: i32,
        brightness: f32,
    ) -> Result<(), Error> {
        let out = self.inner.outputs.get(crtc_idx).ok_or(Error::Crtc)?;
        if out.failed || out.gamma_control.is_none() || out.gamma_size == 0 {
            return Err(Error::WaylandProtocol);
        }

        let gs = out.gamma_size as usize;
        let ramp_bytes = gs * std::mem::size_of::<u16>();
        let total = ramp_bytes * 3; // R + G + B contiguous

        // Create memfd for gamma ramp transfer
        let fd: OwnedFd = create_memfd(total)?;
        let raw_fd = fd.as_raw_fd();

        // mmap, fill, munmap
        let map = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                total,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                raw_fd,
                0,
            )
        };
        if map == libc::MAP_FAILED {
            return Err(Error::Resources);
        }

        let r_ptr = map as *mut u16;
        let g_ptr = unsafe { r_ptr.add(gs) };
        let b_ptr = unsafe { g_ptr.add(gs) };

        let r_slice = unsafe { std::slice::from_raw_parts_mut(r_ptr, gs) };
        let g_slice = unsafe { std::slice::from_raw_parts_mut(g_ptr, gs) };
        let b_slice = unsafe { std::slice::from_raw_parts_mut(b_ptr, gs) };

        let fill_result = colorramp::fill_gamma_ramps(temp, gs, r_slice, g_slice, b_slice, brightness);

        unsafe { libc::munmap(map, total) };

        fill_result?;

        // Seal the fd as required by the protocol
        unsafe {
            libc::fcntl(
                raw_fd,
                libc::F_ADD_SEALS,
                libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE,
            );
        }

        // Send gamma ramp to compositor
        let ctrl = out.gamma_control.as_ref().unwrap();
        ctrl.set_gamma(fd.as_fd());

        // Flush to compositor
        let _ = self.conn.flush();

        Ok(())
    }

    pub fn set_temperature(&mut self, temp: i32, brightness: f32) -> Result<(), Error> {
        let mut last_err = None;
        let mut success_count = 0;

        for i in 0..self.inner.outputs.len() {
            let out = &self.inner.outputs[i];
            if !out.failed && out.gamma_size > 0 {
                match self.set_temperature_crtc(i, temp, brightness) {
                    Ok(()) => success_count += 1,
                    Err(e) => last_err = Some(e),
                }
            }
        }

        if success_count > 0 {
            Ok(())
        } else {
            Err(last_err.unwrap_or(Error::NoCrtc))
        }
    }

    pub fn restore(&mut self) -> Result<(), Error> {
        // wlr-gamma-control restores original gamma when the control object
        // is destroyed. Destroy existing controls and re-acquire fresh ones.
        let qh = self.queue.handle();

        for out in &mut self.inner.outputs {
            if let Some(ctrl) = out.gamma_control.take() {
                ctrl.destroy();
            }
            out.failed = false;
            out.gamma_size = 0;
        }

        let _ = self.conn.flush();

        // Re-acquire gamma controls
        if let Some(ref manager) = self.inner.gamma_manager {
            for i in 0..self.inner.outputs.len() {
                let ctrl =
                    manager.get_gamma_control(&self.inner.outputs[i].output, &qh, i);
                self.inner.outputs[i].gamma_control = Some(ctrl);
            }
        }

        self.queue
            .roundtrip(&mut self.inner)
            .map_err(|_| Error::WaylandConnect)?;

        Ok(())
    }
}

impl Drop for WaylandState {
    fn drop(&mut self) {
        // Destroying gamma controls auto-restores original gamma
        for out in &mut self.inner.outputs {
            if let Some(ctrl) = out.gamma_control.take() {
                ctrl.destroy();
            }
        }
        let _ = self.conn.flush();
    }
}

/// Create a sealed memfd of the given size
fn create_memfd(size: usize) -> Result<OwnedFd, Error> {
    let name = c"meridian-gamma";
    let fd = unsafe {
        libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING)
    };
    if fd < 0 {
        return Err(Error::Resources);
    }

    let owned = unsafe { OwnedFd::from_raw_fd(fd) };

    if unsafe { libc::ftruncate(owned.as_raw_fd(), size as libc::off_t) } < 0 {
        return Err(Error::Resources);
    }

    Ok(owned)
}
