// demiurge-core: server-agnostic primitives shared by the X11 and
// Wayland implementations.
//
// The split is "what would survive a protocol swap unchanged":
//   - Config schema + load/validate (TOML; server-agnostic)
//   - Action enum (the verbs the user binds to keys)
//   - Layout enum (Floating / Tile / Monocle as types; the per-protocol
//     arrange function lives in each implementation crate)
//   - Monitor + Monitors (the data type + non-empty wrapper; the
//     per-protocol query lives in each implementation crate)
//   - MruRing<W> (cyclical most-recently-used ring, generic over the
//     window-id type; per-protocol grab/focus/cycle wiring lives in
//     each implementation)
//   - Torrentius rendering primitives (Cairo + Pango; both backends
//     blit Cairo surfaces, just into different buffers)
//
// What stays per-implementation:
//   - The window manager state itself (Wm), since its connection /
//     event handling / EWMH-or-equivalent property setters are tied
//     to the protocol.
//   - Tile/Monocle dispatch (configure_window calls vs Wayland surface
//     positioning).
//   - Anything that touches the protocol surface directly: keybind
//     compilation (keycodes), atoms, mouse grabs, screen-locker glue.

pub mod action;
pub mod config;
pub mod layout;
pub mod monitor;
pub mod mru;
pub mod torrentius;

pub use action::Action;
pub use layout::Layout;
pub use monitor::{Monitor, Monitors};
pub use mru::{CycleScope, CycleState, MruRing};
