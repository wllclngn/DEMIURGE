// GNOSIS -- file manager for DEMIURGE. Replaces pcmanfm.
//
// Named for gnostic knowledge -- the desktop's organ of seeing-into:
// it lets you *know* your filesystem. Pairs with demiurge (the
// craftsman) and abraxas (the gnostic deity) so the suite's naming
// coheres.
//
// What both stacks' binaries share: The browser (the listing, the filter,
// the picker, the sidebar, what each key does and the drawing), the file
// model, thumbnails, drives and the command line. The X11 binary
// (x11/gnosis) shows it in an X window, the Wayland binary (wayland/gnosis)
// in an xdg toplevel, both drawn with Cairo + Pango, the same torrentius
// vocabulary as the bar / lock / MEDUSA; no GTK, no Qt. Keyboard-first (the user is often
// mouseless): arrows / hjkl to move, Enter to descend, Backspace up,
// xdg-open to launch. Mouse works when present but is never assumed.
//
// Near-zero compute: redraw only on a state change (navigation,
// selection move, filter input, resize). Idle = zero work.
//
// Module layout:
//   browser -- the window's contents and behavior, either stack's
//   cli     -- the command line
//   fs      -- directory reading + the Entry model + sort order
//   thumbs  -- freedesktop image thumbnails, made on a worker thread
//   drives  -- udisks2 drives for the sidebar, mounted off the UI thread

pub mod browser;
pub mod cli;
pub mod drives;
pub mod fs;
pub mod thumbs;
