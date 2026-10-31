// GORDIAN KNOT -- the screen locker and greeter for DEMIURGE: What both
// stacks' binaries share, the greeter inside cage among it. The X11 binary
// (x11/gordian_knot) adds the in-X lock and the X idle daemon, the Wayland
// binary (wayland/gordian_knot) the ext-session-lock lock.
//
// Named for the Phrygian knot that only the destined could untie. The lock
// binds the machine until a valid credential unbinds it.
//
// Module layout:
//   privsep   -- who runs as what: The root holder, the user-level UI
//                child, daemon and greeter; process hardening
//   secret    -- the mlocked, undumpable, explicitly wiped password buffer
//   keyinput  -- what a decoded key does to the password field
//   pam       -- inline FFI to libpam; each attempt in a forked child
//   ui        -- renderer child: Panels via torrentius into its output, the
//                VT lock's framebuffer here or the X11 lock window, then
//                sandboxed
//   landlock  -- the UI child's filesystem sandbox
//   seccomp   -- the UI child's syscall filter (logs and refuses, no kill)
//   vt        -- VT lock (root holder on a fresh console, the panels in the
//                framebuffer or a text prompt) + --login text UI
//   fb        -- the framebuffer the VT lock draws into
//   warden    -- the VT lock's recovery path should its holder die
//   greetd    -- greetd JSON-IPC client; login -- the greeter state machine
//   greeter   -- the greeter inside cage, either stack's
//   screen    -- the panels the greeter and the Wayland lock fill a window with
//   sysinfo   -- hostname, kernel, uptime, time for the SYSTEM panel
//   cli       -- what both binaries' entry points share

pub mod cli;
pub mod fb;
pub mod greetd;
pub mod greeter;
pub mod keyinput;
pub mod landlock;
pub mod login;
pub mod pam;
pub mod privsep;
pub mod screen;
pub mod seccomp;
pub mod secret;
pub mod sysinfo;
pub mod ui;
pub mod vt;
pub mod warden;

// How a lock ended without an unlock.
pub enum LockError {
    // Nothing was locked; a fallback is safe.
    Unavailable(String),
    // The lock was up and failed.
    Failed(String),
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::Unavailable(s) => write!(f, "lock unavailable: {}", s),
            LockError::Failed(s) => write!(f, "lock failed: {}", s),
        }
    }
}
