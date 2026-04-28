// GORDIAN KNOT -- VT + X11 screen locker for DEMIURGE.
//
// Named for the Phrygian knot that only the destined could untie. The lock
// binds the machine until a valid credential unbinds it.
//
// Module layout:
//   sysinfo  -- hostname, kernel, uptime, time (for the SYSTEM panel)
//   ipc      -- message protocol for the privileged/unprivileged fork pair
//   pam      -- inline FFI to libpam + conversation callback
//   privsep  -- fork the privileged (VT ioctl) parent from the unprivileged
//               (PAM + UI) child, wire the IPC socketpair
//   sandbox  -- seccomp-bpf syscall allowlist + landlock fs sandbox, applied
//               in the unprivileged child after PAM init
//   panel    -- framed-panel renderer (SYSTEM, USER) atop torrentius
//   vt       -- VT-based lock UI (fallback path for TTY sessions)
//   x11_lock -- X11 in-session lock UI (primary path for graphical sessions)
//   daemon   -- --daemon mode: XScreenSaver idle polling + inhibit watcher
//   inhibit  -- logind D-Bus inhibit query via zbus

pub mod daemon;
pub mod inhibit;
pub mod landlock;
pub mod pam;
pub mod privsep;
pub mod seccomp;
pub mod sysinfo;
pub mod vt;
pub mod x11_lock;
