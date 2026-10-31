//! Wayland gamma control via wlr-gamma-control-unstable-v1.
//!
//! Covers compositors implementing the wlr protocol:
//!   Sway, Hyprland, river, labwc, wayfire, niri
//!
//! Uses memfd for gamma ramp transfer (no tmpfile needed).
//! Protocol auto-restores gamma when controls are destroyed.
//!
//! Monitors come and go while the daemon runs: Each wl_output the compositor
//! announces gets a control, and one it removes drops its control.

use abraxas::gamma::{Error, Gamma, colorramp};
use std::os::fd::AsFd;
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};

use wayland_client::backend::WaylandError;
use wayland_client::protocol::{wl_output::WlOutput, wl_registry};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols_wlr::gamma_control::v1::client::{
    zwlr_gamma_control_manager_v1::ZwlrGammaControlManagerV1,
    zwlr_gamma_control_v1::{self, ZwlrGammaControlV1},
};

/// Per-output state
struct OutputState {
    name: u32,
    output: WlOutput,
    gamma_control: Option<ZwlrGammaControlV1>,
    gamma_size: u32,
    failed: bool,
}

/// Internal state used during Wayland dispatch
struct WaylandInner {
    gamma_manager: Option<ZwlrGammaControlManagerV1>,
    outputs: Vec<OutputState>,
    // A control took its gamma size since the daemon last asked
    fresh: bool,
}

impl WaylandInner {
    fn acquire(&mut self, qh: &QueueHandle<Self>) {
        let Some(manager) = &self.gamma_manager else {
            return;
        };
        for out in self.outputs.iter_mut().filter(|o| o.gamma_control.is_none() && !o.failed) {
            out.gamma_control = Some(manager.get_gamma_control(&out.output, qh, out.name));
        }
    }
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
        match event {
            wl_registry::Event::Global { name, interface, version: _ } => {
                if interface == "zwlr_gamma_control_manager_v1" {
                    state.gamma_manager =
                        Some(registry.bind::<ZwlrGammaControlManagerV1, _, _>(name, 1, qh, ()));
                } else if interface == "wl_output" {
                    let output = registry.bind::<WlOutput, _, _>(name, 1, qh, ());
                    state.outputs.push(OutputState {
                        name,
                        output,
                        gamma_control: None,
                        gamma_size: 0,
                        failed: false,
                    });
                }
                state.acquire(qh);
            }
            wl_registry::Event::GlobalRemove { name } => {
                if let Some(pos) = state.outputs.iter().position(|o| o.name == name) {
                    if let Some(ctrl) = state.outputs.remove(pos).gamma_control {
                        ctrl.destroy();
                    }
                }
            }
            _ => {}
        }
    }
}

// Gamma control listener: receive gamma_size / failed events
// The u32 user data is the output's global name
impl Dispatch<ZwlrGammaControlV1, u32> for WaylandInner {
    fn event(
        state: &mut Self,
        _proxy: &ZwlrGammaControlV1,
        event: zwlr_gamma_control_v1::Event,
        name: &u32,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let Some(out) = state.outputs.iter_mut().find(|o| o.name == *name) {
            match event {
                zwlr_gamma_control_v1::Event::GammaSize { size } => {
                    out.gamma_size = size;
                    state.fresh = true;
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
        let conn = Connection::connect_to_env().map_err(|_| Error::Connect)?;
        let display = conn.display();

        let mut inner = WaylandInner {
            gamma_manager: None,
            outputs: Vec::new(),
            fresh: false,
        };

        let mut queue = conn.new_event_queue();
        let qh = queue.handle();

        // Get registry and discover globals; each output gets its gamma
        // control as it is announced
        let _registry = display.get_registry(&qh, ());
        queue
            .roundtrip(&mut inner)
            .map_err(|_| Error::Connect)?;

        if inner.gamma_manager.is_none() {
            return Err(Error::Protocol);
        }

        // Second roundtrip: receive gamma_size events
        queue
            .roundtrip(&mut inner)
            .map_err(|_| Error::Connect)?;
        inner.fresh = false;

        Ok(WaylandState { conn, queue, inner })
    }

    /// The connection's fd, readable when the compositor has events
    pub fn fd(&self) -> RawFd {
        self.conn.backend().poll_fd().as_raw_fd()
    }

    /// Read and handle the compositor's events without blocking. True when
    /// an output took a gamma control since the last call, so the current
    /// temperature should go to it.
    pub fn dispatch(&mut self) -> Result<bool, Error> {
        // A read that brings in nothing ends it: The pure-Rust backend says
        // so with WouldBlock, libwayland's with Ok(0).
        loop {
            self.queue
                .dispatch_pending(&mut self.inner)
                .map_err(|_| Error::Connect)?;
            let Some(guard) = self.queue.prepare_read() else {
                continue;
            };
            match guard.read() {
                Ok(0) => break,
                Ok(_) => {}
                Err(WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => return Err(Error::Connect),
            }
        }
        // Controls requested for new outputs
        self.conn.flush().map_err(|_| Error::Connect)?;
        Ok(std::mem::take(&mut self.inner.fresh))
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
            return Err(Error::Protocol);
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
        self.inner.acquire(&qh);

        self.queue
            .roundtrip(&mut self.inner)
            .map_err(|_| Error::Connect)?;

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

impl Gamma for WaylandState {
    fn name(&self) -> &str {
        "wayland"
    }

    fn set_temperature(&mut self, temp: i32, brightness: f32) -> Result<(), Error> {
        WaylandState::set_temperature(self, temp, brightness)
    }

    fn restore(&mut self) -> Result<(), Error> {
        WaylandState::restore(self)
    }

    fn fd(&self) -> Option<i32> {
        Some(WaylandState::fd(self))
    }

    fn dispatch(&mut self) -> Result<bool, Error> {
        WaylandState::dispatch(self)
    }
}

/// The compositor's gamma through wlr-gamma-control. A compositor with no
/// monitor lit keeps the backend: Monitors take controls as they come.
pub fn init() -> Result<Box<dyn Gamma>, Error> {
    if std::env::var("WAYLAND_DISPLAY").map_or(true, |v| v.is_empty()) {
        eprintln!("[gamma] wayland: WAYLAND_DISPLAY not set");
        return Err(Error::Connect);
    }
    let state = WaylandState::init().inspect_err(|e| eprintln!("[gamma] wayland: {}", e))?;
    if (0..state.crtc_count()).all(|i| state.gamma_size(i) == 0) {
        eprintln!("[gamma] wayland: connected, waiting for a monitor");
    }
    Ok(Box::new(state))
}
