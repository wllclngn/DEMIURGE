// demiurge-wl: Wayland implementation. This crate mirrors the
// X11 implementation's file layout 1:1 wherever the patterns
// transfer cleanly:
//
//   x11/src/wm.rs       <-> wayland/src/wm.rs       (central state object)
//   x11/src/event.rs    <-> wayland/src/event.rs    (event dispatch)  -- TBD
//   x11/src/layout.rs   <-> wayland/src/layout.rs   (per-monitor tile)-- TBD
//   x11/src/monitor.rs  <-> wayland/src/monitor.rs  (output query)    -- TBD
//   x11/src/keys.rs     <-> wayland/src/keys.rs     (xkb, bindings)   -- TBD
//   x11/src/mru.rs      <-> wayland/src/mru.rs      (cycle wiring)    -- TBD
//   x11/src/bar.rs      <-> wayland/src/bar.rs      (status bar)      -- TBD
//
// Server-agnostic logic (Action enum, Layout enum, Config schema,
// MruRing<W>, torrentius rendering primitives) lives in the
// `demiurge-core` workspace crate and is re-used here.
//
// What's in this v0: the central Wm state object, a Wayland socket
// + display, xdg-shell so clients can connect and create toplevels,
// shm so clients can render via wl_shm, a seat with keyboard, and
// the winit backend (a nested-X-window dev mode for iteration before
// taking on DRM/udev/libinput).
//
// What's NOT in this v0: tiling, tags, MRU, bar, multi-output,
// xwayland, layer-shell, session-lock, hardware backends. Each is
// its own follow-on slice.

pub mod bar;
pub mod client;
pub mod dsr;
pub mod keys;
pub mod render;
pub mod tty;
pub mod wm;
pub mod xwm;

use std::ffi::OsStr;

use demiurge_core::config::Config;

/// Fork each command in `cfg.startup.commands` via `sh -c`, with
/// `$WAYLAND_DISPLAY` set on the child so the spawned client knows
/// which compositor socket to connect to. Mirrors the X11 path's
/// `spawn::spawn(cmd)` loop in x11/src/main.rs.
///
/// Designed to be called by both the winit dev backend (main.rs) and
/// the TTY backend (tty.rs) right after the wayland socket is bound
/// + the WAYLAND_DISPLAY env is set on the parent process. Children
/// inherit the parent env by default, so the explicit env() call here
/// is belt-and-suspenders.
///
/// Failures to spawn are logged but don't abort the compositor: a
/// missing terminal binary in startup.commands shouldn't black-hole
/// the session.
pub fn spawn_startup_commands(cfg: &Config, wayland_display: &OsStr) {
    for cmd in &cfg.startup.commands {
        match std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .env("WAYLAND_DISPLAY", wayland_display)
            .spawn()
        {
            Ok(child) => {
                tracing::info!(pid = child.id(), %cmd, "startup command spawned");
            }
            Err(e) => {
                tracing::warn!(%cmd, error = %e, "startup command failed");
            }
        }
    }
    // startup.spawn (tag-targeted) needs WM_CLASS / app_id matching
    // wired through cherrypie or the Wm directly. Not yet ported to
    // Wayland; the entries are silently skipped here.
    for sp in &cfg.startup.spawn {
        tracing::debug!(cmd = %sp.cmd, tag = %sp.tag, "tagged startup.spawn skipped (Wayland app_id matching not wired)");
    }
}
