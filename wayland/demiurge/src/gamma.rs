// wlr-gamma-control-v1: A client's gamma ramps on a monitor's CRTC, as
// ABRAXAS sets its color temperature. One control holds an output at a
// time; a second control for it, an output with no gamma LUT (the test
// window's always) or an output that goes is answered with failed. The
// ramps go to the screen whole, red, green then blue, and the CRTC gets
// back the LUT it was found with when the control goes, its client with
// it.

use std::os::fd::{AsRawFd, OwnedFd};

use smithay::output::Output;
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource};
use wayland_protocols_wlr::gamma_control::v1::server::zwlr_gamma_control_manager_v1::{self, ZwlrGammaControlManagerV1};
use wayland_protocols_wlr::gamma_control::v1::server::zwlr_gamma_control_v1::{self, ZwlrGammaControlV1};

use crate::wm::Wm;

pub const VERSION: u32 = 1;

impl Wm {
    fn gamma_size(&self, output: &Output) -> Option<u32> {
        self.runtime.as_ref()?.backend.gamma_size(output)
    }

    fn set_gamma(&mut self, output: &Output, ramps: Option<Vec<u16>>) -> Result<(), String> {
        match self.runtime.as_mut() {
            Some(rt) => rt.backend.set_gamma(output, ramps),
            None => Err("no screen".to_string()),
        }
    }

    /// An output gone: The control holding it fails.
    pub fn gamma_output_gone(&mut self, output: &Output) {
        self.gamma_controls.retain(|(held, control)| {
            if held == output {
                control.failed();
            }
            held != output
        });
    }

    // A control done with, failed or destroyed: The CRTC gets back the LUT
    // it was found with.
    fn release_gamma(&mut self, control: &ZwlrGammaControlV1) {
        let Some(pos) = self.gamma_controls.iter().position(|(_, c)| c == control) else {
            return;
        };
        let (output, _) = self.gamma_controls.remove(pos);
        if let Err(e) = self.set_gamma(&output, None) {
            tracing::warn!(output = %output.name(), "gamma not restored: {}", e);
        }
    }
}

// The ramps in `fd`: 3 x size 16-bit entries from its start. None when it
// holds fewer.
fn read_ramps(fd: &OwnedFd, size: u32) -> Option<Vec<u16>> {
    let mut bytes = vec![0u8; 3 * size as usize * 2];
    let mut done = 0;
    while done < bytes.len() {
        let rest = &mut bytes[done..];
        let n = unsafe { libc::pread(fd.as_raw_fd(), rest.as_mut_ptr().cast(), rest.len(), done as libc::off_t) };
        match n {
            n if n > 0 => done += n as usize,
            n if n < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted => {}
            _ => return None,
        }
    }
    Some(bytes.chunks_exact(2).map(|b| u16::from_ne_bytes([b[0], b[1]])).collect())
}

impl GlobalDispatch<ZwlrGammaControlManagerV1, ()> for Wm {
    fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, resource: New<ZwlrGammaControlManagerV1>, _: &(), data_init: &mut DataInit<'_, Self>) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwlrGammaControlManagerV1, ()> for Wm {
    fn request(
        wm: &mut Self,
        _: &Client,
        _: &ZwlrGammaControlManagerV1,
        request: zwlr_gamma_control_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let zwlr_gamma_control_manager_v1::Request::GetGammaControl { id, output } = request {
            let output = Output::from_resource(&output);
            let control = data_init.init(id, output.clone());
            let free = output.filter(|o| !wm.gamma_controls.iter().any(|(held, _)| held == o));
            match free.and_then(|o| wm.gamma_size(&o).map(|size| (o, size))) {
                Some((output, size)) => {
                    control.gamma_size(size);
                    wm.gamma_controls.push((output, control));
                }
                None => control.failed(),
            }
        }
    }
}

impl Dispatch<ZwlrGammaControlV1, Option<Output>> for Wm {
    fn request(
        wm: &mut Self,
        _: &Client,
        control: &ZwlrGammaControlV1,
        request: zwlr_gamma_control_v1::Request,
        _: &Option<Output>,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        // A control that failed holds nothing, and its ramps go nowhere.
        let zwlr_gamma_control_v1::Request::SetGamma { fd } = request else {
            return;
        };
        let Some(output) = wm.gamma_controls.iter().find(|(_, c)| c == control).map(|(o, _)| o.clone()) else {
            return;
        };
        let Some(size) = wm.gamma_size(&output) else {
            control.failed();
            wm.release_gamma(control);
            return;
        };
        let Some(ramps) = read_ramps(&fd, size) else {
            control.post_error(zwlr_gamma_control_v1::Error::InvalidGamma, "the ramps are not 3 x gamma_size 16-bit entries");
            return;
        };
        if let Err(e) = wm.set_gamma(&output, Some(ramps)) {
            tracing::warn!(output = %output.name(), "gamma refused: {}", e);
            control.failed();
            wm.release_gamma(control);
        }
    }

    fn destroyed(wm: &mut Self, _: ClientId, control: &ZwlrGammaControlV1, _: &Option<Output>) {
        wm.release_gamma(control);
    }
}
