// The Linux framebuffer the VT lock draws its panels into: Its geometry from
// the fbdev ioctls, accepted only as 32-bit XRGB, whose bytes in memory are
// Cairo's RGB24, so a panel copies in row by row. Anything else and the VT
// lock keeps to its text prompt.

use std::os::fd::RawFd;

const FBIOGET_VSCREENINFO: libc::c_ulong = 0x4600;
const FBIOGET_FSCREENINFO: libc::c_ulong = 0x4602;

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Bitfield {
    offset: u32,
    length: u32,
    msb_right: u32,
}

#[repr(C)]
#[derive(Default)]
struct VarScreenInfo {
    xres: u32,
    yres: u32,
    xres_virtual: u32,
    yres_virtual: u32,
    xoffset: u32,
    yoffset: u32,
    bits_per_pixel: u32,
    grayscale: u32,
    red: Bitfield,
    green: Bitfield,
    blue: Bitfield,
    transp: Bitfield,
    rest: [u32; 20],
}

#[repr(C)]
struct FixScreenInfo {
    id: [u8; 16],
    smem_start: libc::c_ulong,
    smem_len: u32,
    kind: u32,
    type_aux: u32,
    visual: u32,
    xpanstep: u16,
    ypanstep: u16,
    ywrapstep: u16,
    line_length: u32,
    mmio_start: libc::c_ulong,
    mmio_len: u32,
    accel: u32,
    capabilities: u16,
    reserved: [u16; 2],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Info {
    pub width: i32,
    pub height: i32,
    // Bytes per row, and where the visible screen starts in the mapping.
    pub stride: usize,
    pub offset: usize,
    // The whole mapping.
    pub len: usize,
}

// The framebuffer behind `fd`, if it is 32-bit XRGB.
pub fn info(fd: RawFd) -> Option<Info> {
    let mut var = VarScreenInfo::default();
    let mut fix: FixScreenInfo = unsafe { std::mem::zeroed() };
    unsafe {
        if libc::ioctl(fd, FBIOGET_VSCREENINFO, &mut var) != 0 || libc::ioctl(fd, FBIOGET_FSCREENINFO, &mut fix) != 0 {
            return None;
        }
    }
    let xrgb = var.bits_per_pixel == 32
        && (var.red.offset, var.green.offset, var.blue.offset) == (16, 8, 0)
        && (var.red.length, var.green.length, var.blue.length) == (8, 8, 8);
    let stride = fix.line_length as usize;
    let offset = var.yoffset as usize * stride + var.xoffset as usize * 4;
    let len = fix.smem_len as usize;
    let fits = offset + var.yres as usize * stride <= len && var.xres as usize * 4 <= stride;
    (xrgb && fits && var.xres > 0 && var.yres > 0).then_some(Info {
        width: var.xres as i32,
        height: var.yres as i32,
        stride,
        offset,
        len,
    })
}
