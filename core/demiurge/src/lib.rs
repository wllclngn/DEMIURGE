// demiurge-core: server-agnostic primitives shared by the X11 and
// Wayland implementations.
//
// The split is "what would survive a protocol swap unchanged":
//   - Config schema + load/validate (TOML; server-agnostic)
//   - Action enum (the verbs the user binds to keys), and keybind
//     actions and key names read from the config
//   - Layout enum (Floating / Tile / Monocle as types) and the tiled
//     layout's geometry; placing the windows lives in each implementation
//   - Monitor + Monitors (the data type + non-empty wrapper; the
//     per-protocol query lives in each implementation crate)
//   - MruRing<W> (cyclical most-recently-used ring, generic over the
//     window-id type; per-protocol grab/focus/cycle wiring lives in
//     each implementation)
//   - Torrentius rendering primitives (Cairo + Pango; both backends
//     blit Cairo surfaces, just into different buffers)
//   - The title bar (traffic lights and title), drawn the same by the X11
//     frames, GNOSIS and the Wayland decorations
//   - The bar's painter: Its regions, what each shows and their drawing,
//     each stack putting the regions drawn on its own screen
//   - The graphical session in the user's systemd manager, which both
//     stacks begin with their displays
//   - Local time in a strftime format, and the wait to the next second a
//     seconds clock ticks on
//   - Session logging: stderr moved off a VT onto journald
//   - Starting the session's programs, tracking and reaping them
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
pub mod bar;
pub mod clock;
pub mod config;
pub mod drag;
pub mod journal;
pub mod keys;
pub mod layout;
pub mod monitor;
pub mod mru;
pub mod notify;
pub mod prompt;
pub mod session;
pub mod spawn;
pub mod tag_menu;
pub mod titlebar;
pub mod torrentius;
pub mod xsettings;

pub use action::Action;
pub use layout::{Layout, Zoom};
pub use monitor::{Monitor, Monitors};
pub use mru::{CycleScope, CycleState, MruRing};
