#[cfg(feature = "x11")]
pub mod x11;

use crate::rules::CompiledRule;

#[cfg(feature = "x11")]
use self::x11::X11Backend;

enum Backend {
    #[cfg(feature = "x11")]
    X11(X11Backend),
}

pub struct WindowManager {
    backend: Backend,
}

impl WindowManager {
    pub fn init(signal_fd: i32) -> Result<Self, String> {
        // X11
        #[cfg(feature = "x11")]
        {
            match X11Backend::init(signal_fd) {
                Ok(b) => {
                    return Ok(Self {
                        backend: Backend::X11(b),
                    });
                }
                Err(e) => eprintln!("[backend] x11: {}", e),
            }
        }

        Err("no usable backend found".into())
    }

    pub fn backend_name(&self) -> &str {
        match &self.backend {
            #[cfg(feature = "x11")]
            Backend::X11(_) => "x11",
        }
    }

    pub fn connection_fd(&self) -> i32 {
        match &self.backend {
            #[cfg(feature = "x11")]
            Backend::X11(b) => b.connection_fd(),
        }
    }

    pub fn process_events(&self, rules: &[CompiledRule], dry_run: bool) {
        match &self.backend {
            #[cfg(feature = "x11")]
            Backend::X11(b) => b.process_events(rules, dry_run),
        }
    }
}
