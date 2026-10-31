//! Gamma control: What each stack's binary implements, the X11 binary
//! through RandR and the Wayland binary through wlr-gamma-control.

pub mod colorramp;

use std::fmt;

/// Error type for gamma operations
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Error {
    InvalidTemp,
    Open,
    Resources,
    Crtc,
    Gamma,
    NoCrtc,
    Permission,
    Connect,
    Protocol,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidTemp => write!(f, "Invalid temperature"),
            Error::Open => write!(f, "Failed to open display device"),
            Error::Resources => write!(f, "Failed to get display resources"),
            Error::Crtc => write!(f, "Failed to get CRTC info"),
            Error::Gamma => write!(f, "Failed to set gamma ramp"),
            Error::NoCrtc => write!(f, "No usable CRTC found"),
            Error::Permission => write!(f, "Permission denied (need video group?)"),
            Error::Connect => write!(f, "Failed to connect to the display server"),
            Error::Protocol => write!(f, "The compositor lacks the gamma control protocol"),
        }
    }
}

impl std::error::Error for Error {}

/// A display server's gamma, set a temperature at a time.
pub trait Gamma {
    fn name(&self) -> &str;
    fn set_temperature(&mut self, temp: i32, brightness: f32) -> Result<(), Error>;
    fn restore(&mut self) -> Result<(), Error>;
    /// The fd to poll for the backend's events, when it has any.
    fn fd(&self) -> Option<i32> {
        None
    }
    /// Handle the backend's pending events. True when a monitor took gamma
    /// control and should get the current temperature.
    fn dispatch(&mut self) -> Result<bool, Error> {
        Ok(false)
    }
}

/// How a binary opens its display server's gamma.
pub type Init = fn() -> Result<Box<dyn Gamma>, Error>;
