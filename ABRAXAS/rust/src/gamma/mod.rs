//! Gamma control with automatic backend selection.
//!
//! Detection order:
//!   1. Wayland (wlr-gamma-control) - Sway, Hyprland, river, etc.
//!   2. GNOME (Mutter DBus) - GNOME Wayland sessions
//!   3. DRM (kernel ioctl) - always available
//!   4. X11 (RandR) - NVIDIA fallback, Xorg sessions

pub mod colorramp;
pub mod drm;

#[cfg(feature = "wayland")]
pub mod wayland;

#[cfg(feature = "x11")]
pub mod x11;

#[cfg(feature = "gnome")]
pub mod gnome;

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
    #[cfg(feature = "wayland")]
    WaylandConnect,
    #[cfg(feature = "wayland")]
    WaylandProtocol,
    #[cfg(feature = "gnome")]
    GnomeDbus,
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
            #[cfg(feature = "wayland")]
            Error::WaylandConnect => write!(f, "Failed to connect to Wayland display"),
            #[cfg(feature = "wayland")]
            Error::WaylandProtocol => write!(f, "Wayland compositor lacks gamma control protocol"),
            #[cfg(feature = "gnome")]
            Error::GnomeDbus => write!(f, "Failed to communicate with Mutter via DBus"),
        }
    }
}

impl std::error::Error for Error {}

/// Backend type
enum Backend {
    Drm(drm::DrmState),
    #[cfg(feature = "wayland")]
    Wayland(wayland::WaylandState),
    #[cfg(feature = "x11")]
    X11(x11::X11State),
    #[cfg(feature = "gnome")]
    Gnome(gnome::GnomeState),
}

/// Unified gamma state
pub struct GammaState {
    backend: Backend,
}

impl GammaState {
    pub fn backend_name(&self) -> &str {
        match &self.backend {
            Backend::Drm(_) => "drm",
            #[cfg(feature = "wayland")]
            Backend::Wayland(_) => "wayland",
            #[cfg(feature = "x11")]
            Backend::X11(_) => "x11",
            #[cfg(feature = "gnome")]
            Backend::Gnome(_) => "gnome",
        }
    }

    pub fn set_temperature(&mut self, temp: i32, brightness: f32) -> Result<(), Error> {
        match &mut self.backend {
            Backend::Drm(state) => state.set_temperature(temp, brightness),
            #[cfg(feature = "wayland")]
            Backend::Wayland(state) => state.set_temperature(temp, brightness),
            #[cfg(feature = "x11")]
            Backend::X11(state) => state.set_temperature(temp, brightness),
            #[cfg(feature = "gnome")]
            Backend::Gnome(state) => state.set_temperature(temp, brightness),
        }
    }

    pub fn restore(&mut self) -> Result<(), Error> {
        match &mut self.backend {
            Backend::Drm(state) => state.restore(),
            #[cfg(feature = "wayland")]
            Backend::Wayland(state) => state.restore(),
            #[cfg(feature = "x11")]
            Backend::X11(state) => state.restore(),
            #[cfg(feature = "gnome")]
            Backend::Gnome(state) => state.restore(),
        }
    }
}

/// Initialize gamma control with automatic backend selection.
/// Tries DRM first (card0).
pub fn init() -> Result<GammaState, Error> {
    init_card(0)
}

/// Initialize gamma control for a specific graphics card.
///
/// Detection order: Wayland > GNOME > DRM > X11
pub fn init_card(card_num: i32) -> Result<GammaState, Error> {
    // 1. Try Wayland (wlr-gamma-control) -- only if WAYLAND_DISPLAY is set
    #[cfg(feature = "wayland")]
    {
        if std::env::var("WAYLAND_DISPLAY").map(|v| !v.is_empty()).unwrap_or(false) {
            match wayland::WaylandState::init() {
                Ok(state) => {
                    let usable = (0..state.crtc_count())
                        .filter(|&i| state.gamma_size(i) > 0)
                        .count();
                    if usable > 0 {
                        return Ok(GammaState {
                            backend: Backend::Wayland(state),
                        });
                    }
                    eprintln!("[gamma] wayland: connected but 0 usable CRTCs");
                }
                Err(e) => eprintln!("[gamma] wayland: {}", e),
            }
        } else {
            eprintln!("[gamma] wayland: skipped (WAYLAND_DISPLAY not set)");
        }
    }

    // 2. Try GNOME (Mutter DBus)
    #[cfg(feature = "gnome")]
    {
        match gnome::GnomeState::init() {
            Ok(state) => {
                if state.crtc_count() > 0 {
                    return Ok(GammaState {
                        backend: Backend::Gnome(state),
                    });
                }
                eprintln!("[gamma] gnome: connected but 0 CRTCs");
            }
            Err(e) => eprintln!("[gamma] gnome: {}", e),
        }
    }

    // 3. Try DRM (kernel ioctl)
    match drm::DrmState::init(card_num) {
        Ok(state) => {
            let usable = (0..state.crtc_count())
                .filter(|&i| state.gamma_size(i) > 1)
                .count();
            if usable > 0 {
                return Ok(GammaState {
                    backend: Backend::Drm(state),
                });
            }
            eprintln!("[gamma] drm: opened card{} but 0 usable CRTCs (compositor owns gamma?)", card_num);
        }
        Err(e) => eprintln!("[gamma] drm: {}", e),
    }

    // 4. Try X11 (RandR)
    #[cfg(feature = "x11")]
    {
        match x11::X11State::init() {
            Ok(state) => {
                let usable = (0..state.crtc_count())
                    .filter(|&i| state.gamma_size(i) > 0)
                    .count();
                if usable > 0 {
                    return Ok(GammaState {
                        backend: Backend::X11(state),
                    });
                }
                eprintln!("[gamma] x11: connected but 0 usable CRTCs");
            }
            Err(e) => eprintln!("[gamma] x11: {}", e),
        }
    }

    Err(Error::NoCrtc)
}
