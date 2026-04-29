//! Direct DRM/KMS gamma control via raw kernel ioctl.
//!
//! Pure kernel interface -- no libdrm dependency.
//! Opens /dev/dri/card* directly.

use super::{colorramp, Error};
use std::fs::OpenOptions;
use std::os::unix::io::{AsRawFd, RawFd};

// DRM ioctl command numbers
const DRM_IOCTL_BASE: u8 = b'd';
const DRM_IOCTL_MODE_GETRESOURCES: u8 = 0xA0;
const DRM_IOCTL_MODE_GETCRTC: u8 = 0xA1;
const DRM_IOCTL_MODE_GETGAMMA: u8 = 0xA4;
const DRM_IOCTL_MODE_SETGAMMA: u8 = 0xA5;

/// drm_mode_card_res
#[repr(C)]
#[derive(Default)]
struct DrmModeCardRes {
    fb_id_ptr: u64,
    crtc_id_ptr: u64,
    connector_id_ptr: u64,
    encoder_id_ptr: u64,
    count_fbs: u32,
    count_crtcs: u32,
    count_connectors: u32,
    count_encoders: u32,
    min_width: u32,
    max_width: u32,
    min_height: u32,
    max_height: u32,
}

/// drm_mode_crtc
#[repr(C)]
struct DrmModeCrtc {
    set_connectors_ptr: u64,
    count_connectors: u32,
    crtc_id: u32,
    fb_id: u32,
    x: u32,
    y: u32,
    gamma_size: u32,
    mode_valid: u32,
    mode: [u8; 68],
}

impl Default for DrmModeCrtc {
    fn default() -> Self {
        Self {
            set_connectors_ptr: 0,
            count_connectors: 0,
            crtc_id: 0,
            fb_id: 0,
            x: 0,
            y: 0,
            gamma_size: 0,
            mode_valid: 0,
            mode: [0u8; 68],
        }
    }
}

/// drm_mode_crtc_lut
#[repr(C)]
#[derive(Default)]
struct DrmModeCrtcLut {
    crtc_id: u32,
    gamma_size: u32,
    red: u64,
    green: u64,
    blue: u64,
}

// ioctl helpers
fn ioctl_rw<T>(fd: RawFd, nr: u8, data: &mut T) -> Result<(), Error> {
    let size = std::mem::size_of::<T>();
    // _IOWR = direction: read|write (3), size, type, nr
    let request: libc::c_ulong =
        (3 << 30) | ((size as libc::c_ulong & 0x3FFF) << 16) | ((DRM_IOCTL_BASE as libc::c_ulong) << 8) | nr as libc::c_ulong;

    let ret = unsafe { libc::ioctl(fd, request as libc::Ioctl, data as *mut T) };
    if ret < 0 {
        Err(Error::Resources)
    } else {
        Ok(())
    }
}

/// Per-CRTC saved state
struct CrtcState {
    crtc_id: u32,
    gamma_size: u32,
    saved_r: Vec<u16>,
    saved_g: Vec<u16>,
    saved_b: Vec<u16>,
    // Pre-allocated working buffers (reused across set_temperature calls)
    work_r: Vec<u16>,
    work_g: Vec<u16>,
    work_b: Vec<u16>,
}

/// DRM gamma state
pub struct DrmState {
    fd: RawFd,
    _file: std::fs::File, // owns the fd
    crtcs: Vec<CrtcState>,
}

impl DrmState {
    pub fn init(card_num: i32) -> Result<Self, Error> {
        let path = format!("/dev/dri/card{}", card_num);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::PermissionDenied {
                    Error::Permission
                } else {
                    Error::Open
                }
            })?;

        let fd = file.as_raw_fd();

        // First call: get count of CRTCs
        let mut res = DrmModeCardRes::default();
        ioctl_rw(fd, DRM_IOCTL_MODE_GETRESOURCES, &mut res)?;

        if res.count_crtcs == 0 {
            return Err(Error::NoCrtc);
        }

        // Allocate array for CRTC IDs
        let mut crtc_ids = vec![0u32; res.count_crtcs as usize];
        res.crtc_id_ptr = crtc_ids.as_mut_ptr() as u64;

        // Second call: get CRTC IDs
        ioctl_rw(fd, DRM_IOCTL_MODE_GETRESOURCES, &mut res)?;

        // Initialize each CRTC and save original gamma
        let mut crtcs = Vec::with_capacity(res.count_crtcs as usize);

        for &crtc_id in &crtc_ids[..res.count_crtcs as usize] {
            let mut crtc_info = DrmModeCrtc::default();
            crtc_info.crtc_id = crtc_id;

            if ioctl_rw(fd, DRM_IOCTL_MODE_GETCRTC, &mut crtc_info).is_err() {
                crtcs.push(CrtcState {
                    crtc_id,
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

            let gamma_size = crtc_info.gamma_size;
            if gamma_size <= 1 {
                crtcs.push(CrtcState {
                    crtc_id,
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

            // Save original gamma ramps
            let mut saved_r = vec![0u16; gamma_size as usize];
            let mut saved_g = vec![0u16; gamma_size as usize];
            let mut saved_b = vec![0u16; gamma_size as usize];

            let mut lut = DrmModeCrtcLut {
                crtc_id,
                gamma_size,
                red: saved_r.as_mut_ptr() as u64,
                green: saved_g.as_mut_ptr() as u64,
                blue: saved_b.as_mut_ptr() as u64,
            };

            if ioctl_rw(fd, DRM_IOCTL_MODE_GETGAMMA, &mut lut).is_err() {
                crtcs.push(CrtcState {
                    crtc_id,
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

            crtcs.push(CrtcState {
                crtc_id,
                gamma_size,
                saved_r,
                saved_g,
                saved_b,
                work_r: vec![0u16; gamma_size as usize],
                work_g: vec![0u16; gamma_size as usize],
                work_b: vec![0u16; gamma_size as usize],
            });
        }

        Ok(Self {
            fd,
            _file: file,
            crtcs,
        })
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
        if crtc.gamma_size <= 1 {
            return Err(Error::Crtc);
        }

        let size = crtc.gamma_size as usize;

        // Reuse pre-allocated working buffers
        colorramp::fill_gamma_ramps(temp, size, &mut crtc.work_r, &mut crtc.work_g, &mut crtc.work_b, brightness)?;

        let mut lut = DrmModeCrtcLut {
            crtc_id: crtc.crtc_id,
            gamma_size: crtc.gamma_size,
            red: crtc.work_r.as_mut_ptr() as u64,
            green: crtc.work_g.as_mut_ptr() as u64,
            blue: crtc.work_b.as_mut_ptr() as u64,
        };

        ioctl_rw(self.fd, DRM_IOCTL_MODE_SETGAMMA, &mut lut)
            .map_err(|_| Error::Gamma)
    }

    pub fn set_temperature(&mut self, temp: i32, brightness: f32) -> Result<(), Error> {
        let mut last_err = None;
        let mut success_count = 0;

        for i in 0..self.crtcs.len() {
            if self.crtcs[i].gamma_size > 1 {
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
        for crtc in &mut self.crtcs {
            if crtc.gamma_size > 1 && !crtc.saved_r.is_empty() {
                let mut lut = DrmModeCrtcLut {
                    crtc_id: crtc.crtc_id,
                    gamma_size: crtc.gamma_size,
                    red: crtc.saved_r.as_mut_ptr() as u64,
                    green: crtc.saved_g.as_mut_ptr() as u64,
                    blue: crtc.saved_b.as_mut_ptr() as u64,
                };
                let _ = ioctl_rw(self.fd, DRM_IOCTL_MODE_SETGAMMA, &mut lut);
            }
        }
        Ok(())
    }
}

impl Drop for DrmState {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}
