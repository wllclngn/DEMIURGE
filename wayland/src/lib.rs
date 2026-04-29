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

pub mod client;
pub mod dsr;
pub mod wm;
