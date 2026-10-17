// GORDIAN KNOT -- X11 / VT / Wayland screen locker for DEMIURGE.
//
// Named for the Phrygian knot that only the destined could untie. The lock
// binds the machine until a valid credential unbinds it.
//
// Module layout:
//   privsep   -- who runs as what: The root holder, the user-level UI
//                child, daemon and greeter; process hardening
//   secret    -- the mlocked, undumpable, explicitly wiped password buffer
//   keyinput  -- X11 key decoding through xkbcommon with the server layout
//   pam       -- inline FFI to libpam; each attempt in a forked child
//   x11_lock  -- X11 holder: Window, grab, keys, PAM; never draws
//   ui        -- X11 renderer child: Panels via torrentius, then sandboxed
//   landlock  -- the UI child's filesystem sandbox
//   seccomp   -- the UI child's syscall filter (logs and refuses, no kill)
//   vt        -- VT lock (root holder on a fresh console) + --login text UI
//   wl_lock   -- Wayland ext-session-lock locker (test environment)
//   daemon    -- idle + lock-before-suspend watcher
//   inhibit   -- logind idle-inhibitor query via zbus
//   greetd    -- greetd JSON-IPC client; login -- the greeter state machine
//   sysinfo   -- hostname, kernel, uptime, time for the SYSTEM panel

pub mod daemon;
pub mod greetd;
pub mod inhibit;
pub mod keyinput;
pub mod landlock;
pub mod login;
pub mod pam;
pub mod privsep;
pub mod seccomp;
pub mod secret;
pub mod sysinfo;
pub mod ui;
pub mod vt;
pub mod wl_lock;
pub mod x11_lock;
