//! X11 RandR gamma control fallback.
//!
//! Used when DRM gamma fails (NVIDIA proprietary, etc.)
//! Uses x11rb crate -- no libX11/libXrandr link dependency.

use super::{colorramp, Error};
use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as RandrExt;
use x11rb::rust_connection::RustConnection;

/// Saved per-CRTC gamma state
struct CrtcState {
    crtc: u32,
    gamma_size: u16,
    saved_r: Vec<u16>,
    saved_g: Vec<u16>,
    saved_b: Vec<u16>,
    // Pre-allocated working buffers
    work_r: Vec<u16>,
    work_g: Vec<u16>,
    work_b: Vec<u16>,
}

/// X11 RandR gamma state
pub struct X11State {
    conn: RustConnection,
    crtcs: Vec<CrtcState>,
}

impl X11State {
    pub fn init() -> Result<Self, Error> {
        let (conn, screen_num) =
            RustConnection::connect(None).map_err(|_| Error::Open)?;

        let screen = &conn.setup().roots[screen_num];
        let root = screen.root;

        // Get screen resources
        let resources = conn
            .randr_get_screen_resources_current(root)
            .map_err(|_| Error::Resources)?
            .reply()
            .map_err(|_| Error::Resources)?;

        if resources.crtcs.is_empty() {
            return Err(Error::NoCrtc);
        }

        let mut crtcs = Vec::with_capacity(resources.crtcs.len());

        for &crtc_id in &resources.crtcs {
            let gamma_size = conn
                .randr_get_crtc_gamma_size(crtc_id)
                .map_err(|_| Error::Crtc)?
                .reply()
                .map_err(|_| Error::Crtc)?
                .size;

            if gamma_size == 0 {
                crtcs.push(CrtcState {
                    crtc: crtc_id,
                    gamma_size: 0,
                    saved_r: Vec::new(),
                    saved_g: Vec::new(),
                    saved_b: Vec::new(),
                    work_r: Vec::new(),
                    work_g: Vec::new(),
                    work_b: Vec::new(),
                });
                continue;
            }

            // Save original gamma
            let gamma = conn
                .randr_get_crtc_gamma(crtc_id)
                .map_err(|_| Error::Gamma)?
                .reply()
                .map_err(|_| Error::Gamma)?;

            crtcs.push(CrtcState {
                crtc: crtc_id,
                gamma_size,
                saved_r: gamma.red,
                saved_g: gamma.green,
                saved_b: gamma.blue,
                work_r: vec![0u16; gamma_size as usize],
                work_g: vec![0u16; gamma_size as usize],
                work_b: vec![0u16; gamma_size as usize],
            });
        }

        Ok(X11State { conn, crtcs })
    }

    pub fn crtc_count(&self) -> usize {
        self.crtcs.len()
    }

    pub fn gamma_size(&self, crtc_idx: usize) -> usize {
        self.crtcs
            .get(crtc_idx)
            .map(|c| c.gamma_size as usize)
            .unwrap_or(0)
    }

    pub fn set_temperature_crtc(
        &mut self,
        crtc_idx: usize,
        temp: i32,
        brightness: f32,
    ) -> Result<(), Error> {
        let crtc = self.crtcs.get_mut(crtc_idx).ok_or(Error::Crtc)?;
        if crtc.gamma_size == 0 {
            return Err(Error::Crtc);
        }

        let size = crtc.gamma_size as usize;

        // Reuse pre-allocated working buffers
        colorramp::fill_gamma_ramps(temp, size, &mut crtc.work_r, &mut crtc.work_g, &mut crtc.work_b, brightness)?;

        let crtc_id = crtc.crtc;
        self.conn
            .randr_set_crtc_gamma(crtc_id, &crtc.work_r, &crtc.work_g, &crtc.work_b)
            .map_err(|_| Error::Gamma)?;

        self.conn.flush().map_err(|_| Error::Gamma)?;

        Ok(())
    }

    pub fn set_temperature(&mut self, temp: i32, brightness: f32) -> Result<(), Error> {
        let mut last_err = None;
        let mut success_count = 0;

        for i in 0..self.crtcs.len() {
            if self.crtcs[i].gamma_size > 0 {
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
        for crtc in &self.crtcs {
            if crtc.gamma_size > 0 && !crtc.saved_r.is_empty() {
                let _ = self.conn.randr_set_crtc_gamma(
                    crtc.crtc,
                    &crtc.saved_r,
                    &crtc.saved_g,
                    &crtc.saved_b,
                );
            }
        }
        let _ = self.conn.flush();
        Ok(())
    }
}

impl Drop for X11State {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}
