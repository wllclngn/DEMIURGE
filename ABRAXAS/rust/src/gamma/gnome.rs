//! GNOME/Mutter gamma control via raw sd-bus FFI.
//!
//! Uses org.gnome.Mutter.DisplayConfig.SetCrtcGamma to set gamma
//! ramps on GNOME Wayland sessions (Mutter compositor).
//!
//! Links directly against libsystemd -- same approach as the C23
//! implementation. No async runtime, no zbus.
//!
//! Covers: GNOME on Debian, Ubuntu, Fedora, RHEL, etc.

use super::{colorramp, Error};
use std::ffi::{c_char, c_int, c_void};
use std::ptr;

const GNOME_GAMMA_SIZE: usize = 256;

// Null-terminated C strings for DBus
const DBUS_NAME: &[u8] = b"org.gnome.Mutter.DisplayConfig\0";
const DBUS_PATH: &[u8] = b"/org/gnome/Mutter/DisplayConfig\0";
const DBUS_IFACE: &[u8] = b"org.gnome.Mutter.DisplayConfig\0";

// --- sd-bus FFI declarations ---

#[repr(C)]
struct SdBus {
    _opaque: [u8; 0],
}

#[repr(C)]
struct SdBusMessage {
    _opaque: [u8; 0],
}

#[repr(C)]
struct SdBusError {
    name: *const c_char,
    message: *const c_char,
    _need_free: c_int,
}

impl SdBusError {
    fn null() -> Self {
        SdBusError {
            name: ptr::null(),
            message: ptr::null(),
            _need_free: 0,
        }
    }
}

#[link(name = "systemd")]
extern "C" {
    fn sd_bus_open_user(bus: *mut *mut SdBus) -> c_int;
    fn sd_bus_unref(bus: *mut SdBus) -> *mut SdBus;

    fn sd_bus_call_method(
        bus: *mut SdBus,
        destination: *const c_char,
        path: *const c_char,
        interface: *const c_char,
        member: *const c_char,
        error: *mut SdBusError,
        reply: *mut *mut SdBusMessage,
        types: *const c_char,
        ...
    ) -> c_int;

    fn sd_bus_message_read(
        msg: *mut SdBusMessage,
        types: *const c_char,
        ...
    ) -> c_int;

    fn sd_bus_message_enter_container(
        msg: *mut SdBusMessage,
        type_: c_char,
        contents: *const c_char,
    ) -> c_int;

    fn sd_bus_message_exit_container(msg: *mut SdBusMessage) -> c_int;

    fn sd_bus_message_skip(
        msg: *mut SdBusMessage,
        types: *const c_char,
    ) -> c_int;

    fn sd_bus_message_new_method_call(
        bus: *mut SdBus,
        msg: *mut *mut SdBusMessage,
        destination: *const c_char,
        path: *const c_char,
        interface: *const c_char,
        member: *const c_char,
    ) -> c_int;

    fn sd_bus_message_append(
        msg: *mut SdBusMessage,
        types: *const c_char,
        ...
    ) -> c_int;

    fn sd_bus_message_append_array(
        msg: *mut SdBusMessage,
        type_: c_char,
        ptr: *const c_void,
        size: usize,
    ) -> c_int;

    fn sd_bus_call(
        bus: *mut SdBus,
        msg: *mut SdBusMessage,
        usec: u64,
        error: *mut SdBusError,
        reply: *mut *mut SdBusMessage,
    ) -> c_int;

    fn sd_bus_message_unref(msg: *mut SdBusMessage) -> *mut SdBusMessage;
    fn sd_bus_error_free(error: *mut SdBusError);
}

// --- GNOME state ---

struct GnomeCrtc {
    crtc_id: u32,
}

pub struct GnomeState {
    bus: *mut SdBus,
    serial: u32,
    crtcs: Vec<GnomeCrtc>,
    // Pre-allocated ramp buffers (always GNOME_GAMMA_SIZE = 256)
    work_r: Vec<u16>,
    work_g: Vec<u16>,
    work_b: Vec<u16>,
}

// sd_bus is single-threaded; daemon uses one thread
unsafe impl Send for GnomeState {}

impl GnomeState {
    pub fn init() -> Result<Self, Error> {
        let mut bus: *mut SdBus = ptr::null_mut();
        let r = unsafe { sd_bus_open_user(&mut bus) };
        if r < 0 {
            return Err(Error::GnomeDbus);
        }

        let mut state = GnomeState {
            bus,
            serial: 0,
            crtcs: Vec::new(),
            work_r: vec![0u16; GNOME_GAMMA_SIZE],
            work_g: vec![0u16; GNOME_GAMMA_SIZE],
            work_b: vec![0u16; GNOME_GAMMA_SIZE],
        };

        state.get_resources()?;

        if state.crtcs.is_empty() {
            return Err(Error::NoCrtc);
        }

        Ok(state)
    }

    /// Call GetResources to discover CRTC IDs and serial number.
    ///
    /// GetResources returns: (ua(uxiiiiiuaua{sv})a(uxiausauau)a(uxuudu)ii)
    /// We only need: serial (first u) and crtc_id (first u in each CRTC struct).
    fn get_resources(&mut self) -> Result<(), Error> {
        let mut error = SdBusError::null();
        let mut reply: *mut SdBusMessage = ptr::null_mut();

        let r = unsafe {
            sd_bus_call_method(
                self.bus,
                DBUS_NAME.as_ptr() as *const c_char,
                DBUS_PATH.as_ptr() as *const c_char,
                DBUS_IFACE.as_ptr() as *const c_char,
                b"GetResources\0".as_ptr() as *const c_char,
                &mut error,
                &mut reply,
                b"\0".as_ptr() as *const c_char,
            )
        };
        if r < 0 {
            unsafe { sd_bus_error_free(&mut error) };
            return Err(Error::GnomeDbus);
        }

        // Read serial
        let mut serial: u32 = 0;
        let r = unsafe {
            sd_bus_message_read(
                reply,
                b"u\0".as_ptr() as *const c_char,
                &mut serial as *mut u32,
            )
        };
        if r < 0 {
            unsafe {
                sd_bus_message_unref(reply);
                sd_bus_error_free(&mut error);
            }
            return Err(Error::GnomeDbus);
        }
        self.serial = serial;

        // Enter CRTC array: a(uxiiiiiuaua{sv})
        let r = unsafe {
            sd_bus_message_enter_container(
                reply,
                b'a' as c_char,
                b"(uxiiiiiuaua{sv})\0".as_ptr() as *const c_char,
            )
        };
        if r < 0 {
            unsafe {
                sd_bus_message_unref(reply);
                sd_bus_error_free(&mut error);
            }
            return Err(Error::GnomeDbus);
        }

        self.crtcs.clear();

        loop {
            let r = unsafe {
                sd_bus_message_enter_container(
                    reply,
                    b'r' as c_char,
                    b"uxiiiiiuaua{sv}\0".as_ptr() as *const c_char,
                )
            };
            if r <= 0 {
                break;
            }

            let mut crtc_id: u32 = 0;
            let r = unsafe {
                sd_bus_message_read(
                    reply,
                    b"u\0".as_ptr() as *const c_char,
                    &mut crtc_id as *mut u32,
                )
            };
            if r < 0 {
                break;
            }

            // Skip remaining fields in this CRTC struct
            let r = unsafe {
                sd_bus_message_skip(
                    reply,
                    b"xiiiiiuaua{sv}\0".as_ptr() as *const c_char,
                )
            };
            if r < 0 {
                break;
            }

            unsafe { sd_bus_message_exit_container(reply) };

            self.crtcs.push(GnomeCrtc { crtc_id });
        }

        unsafe {
            sd_bus_message_exit_container(reply);
            sd_bus_message_unref(reply);
            sd_bus_error_free(&mut error);
        }

        if self.crtcs.is_empty() {
            Err(Error::NoCrtc)
        } else {
            Ok(())
        }
    }

    pub fn crtc_count(&self) -> usize {
        self.crtcs.len()
    }

    /// Set gamma ramp on a specific CRTC via SetCrtcGamma DBus call.
    /// Signature: SetCrtcGamma(uu aq aq aq) = (serial, crtc_id, red[], green[], blue[])
    fn set_gamma_crtc_raw(
        bus: *mut SdBus,
        serial: u32,
        crtc_id: u32,
        r: &[u16],
        g: &[u16],
        b: &[u16],
    ) -> Result<(), Error> {
        let mut msg: *mut SdBusMessage = ptr::null_mut();
        let mut error = SdBusError::null();

        let ret = unsafe {
            sd_bus_message_new_method_call(
                bus,
                &mut msg,
                DBUS_NAME.as_ptr() as *const c_char,
                DBUS_PATH.as_ptr() as *const c_char,
                DBUS_IFACE.as_ptr() as *const c_char,
                b"SetCrtcGamma\0".as_ptr() as *const c_char,
            )
        };
        if ret < 0 {
            return Err(Error::GnomeDbus);
        }

        // Append serial and CRTC ID
        let ret = unsafe {
            sd_bus_message_append(
                msg,
                b"uu\0".as_ptr() as *const c_char,
                serial,
                crtc_id,
            )
        };
        if ret < 0 {
            unsafe { sd_bus_message_unref(msg) };
            return Err(Error::GnomeDbus);
        }

        // Append three gamma ramp arrays (aq = array of uint16)
        let ramp_bytes = GNOME_GAMMA_SIZE * std::mem::size_of::<u16>();

        for arr in [r, g, b] {
            let ret = unsafe {
                sd_bus_message_append_array(
                    msg,
                    b'q' as c_char,
                    arr.as_ptr() as *const c_void,
                    ramp_bytes,
                )
            };
            if ret < 0 {
                unsafe { sd_bus_message_unref(msg) };
                return Err(Error::GnomeDbus);
            }
        }

        let ret = unsafe {
            sd_bus_call(bus, msg, 0, &mut error, ptr::null_mut())
        };

        unsafe {
            sd_bus_message_unref(msg);
            sd_bus_error_free(&mut error);
        }

        if ret < 0 {
            Err(Error::GnomeDbus)
        } else {
            Ok(())
        }
    }

    pub fn set_temperature_crtc(
        &mut self,
        crtc_idx: usize,
        temp: i32,
        brightness: f32,
    ) -> Result<(), Error> {
        let crtc_id = match self.crtcs.get(crtc_idx) {
            Some(c) => c.crtc_id,
            None => return Err(Error::GnomeDbus),
        };

        // Reuse pre-allocated working buffers
        colorramp::fill_gamma_ramps(temp, GNOME_GAMMA_SIZE, &mut self.work_r, &mut self.work_g, &mut self.work_b, brightness)?;

        Self::set_gamma_crtc_raw(self.bus, self.serial, crtc_id, &self.work_r, &self.work_g, &self.work_b)
    }

    pub fn set_temperature(&mut self, temp: i32, brightness: f32) -> Result<(), Error> {
        let mut last_err = None;
        let mut success_count = 0;

        for i in 0..self.crtcs.len() {
            match self.set_temperature_crtc(i, temp, brightness) {
                Ok(()) => success_count += 1,
                Err(e) => last_err = Some(e),
            }
        }

        if success_count > 0 {
            Ok(())
        } else {
            Err(last_err.unwrap_or(Error::NoCrtc))
        }
    }

    pub fn restore(&mut self) -> Result<(), Error> {
        // Fill work buffers with linear identity ramp
        for i in 0..GNOME_GAMMA_SIZE {
            let val = (i as f32 / (GNOME_GAMMA_SIZE - 1) as f32 * u16::MAX as f32) as u16;
            self.work_r[i] = val;
            self.work_g[i] = val;
            self.work_b[i] = val;
        }

        let mut last_err = None;
        for crtc in &self.crtcs {
            if let Err(e) = Self::set_gamma_crtc_raw(self.bus, self.serial, crtc.crtc_id, &self.work_r, &self.work_g, &self.work_b) {
                last_err = Some(e);
            }
        }

        match last_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

impl Drop for GnomeState {
    fn drop(&mut self) {
        let _ = self.restore();
        if !self.bus.is_null() {
            unsafe { sd_bus_unref(self.bus) };
        }
    }
}
