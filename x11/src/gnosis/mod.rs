// GNOSIS -- file manager for DEMIURGE. Replaces pcmanfm.
//
// Named for gnostic knowledge -- the desktop's organ of seeing-into:
// it lets you *know* your filesystem. Pairs with demiurge (the
// craftsman) and abraxas (the gnostic deity) so the suite's naming
// coheres.
//
// No GTK, no Qt. Cairo + Pango + x11rb, the same torrentius vocabulary
// as the bar / lock / MEDUSA. Keyboard-first (the user is often
// mouseless): arrows / hjkl to move, Enter to descend, Backspace up,
// xdg-open to launch. Mouse works when present but is never assumed.
//
// Near-zero compute: redraw only on a state change (navigation,
// selection move, filter input, resize). Idle = zero work.
//
// Module layout:
//   fs    -- directory reading + the Entry model + sort order
//   app   -- the App state object, event loop, render, keyboard nav

pub mod app;
pub mod fs;
