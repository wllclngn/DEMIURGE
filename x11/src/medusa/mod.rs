// MEDUSA -- screenshot capture + annotation utility for DEMIURGE.
//
// Named for the Gorgon whose gaze petrifies: a screenshot freezes
// what's on screen into a static image, the way her stare turned the
// living to stone. (Also a nod to the user's senior-project
// breadboard computer of the same name.)
//
// UX modeled on flameshot (https://github.com/flameshot-org/flameshot,
// GPLv3) -- the freeze-screen / drag-region / annotate / export flow,
// the toolbar, and the default keybinds (A arrow, R rectangle, C
// circle, M marker, P pencil, B blur, T text, S selection). This is a
// fresh Rust implementation against DEMIURGE's own Cairo + x11rb
// rendering stack, not a port of flameshot's Qt code. Credit to the
// flameshot developers for the interaction design.
//
// Phase 1 (this slice): freeze the screen, drag a region, save it to
// disk and copy it to the clipboard. No annotation tools yet -- those
// land in Phase 2 (two-point shapes), Phase 3 (path / text / blur).
//
// Module layout:
//   overlay -- the fullscreen capture surface: grab input, render the
//              frozen-and-dimmed backdrop with a bright selection
//              rectangle, run the selection state machine, export.
//
// The capture primitives (capture_root_to_surface,
// capture_region_to_surface, surface_to_png) live in
// crate::torrentius alongside the existing screenshot code.

pub mod clipboard;
pub mod overlay;
pub mod tools;
