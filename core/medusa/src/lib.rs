// MEDUSA -- screenshot capture + annotation utility for DEMIURGE.
//
// Named for the Gorgon whose gaze petrifies: a screenshot freezes
// what's on screen into a static image, the way her stare turned the
// living to stone. (Also a nod to the user's senior-project
// breadboard computer of the same name.)
//
// What both stacks' binaries share: The overlay (the frozen desktop, the
// selection, what each key does) and the annotation tools, plain Cairo. The
// X11 binary (x11/medusa) freezes the X screen and shows the overlay as an X
// window; the Wayland binary (wayland/medusa) freezes the compositor's
// frames and shows it as a layer surface on each monitor.
//
// UX modeled on flameshot (https://github.com/flameshot-org/flameshot,
// GPLv3) -- the freeze-screen / drag-region / annotate / export flow,
// the toolbar, and the default keybinds (A arrow, R rectangle, C
// circle, M marker, P pencil, B blur, T text, S selection). This is a
// fresh Rust implementation against DEMIURGE's own Cairo rendering on
// both stacks, not a port of flameshot's Qt code. Credit to the
// flameshot developers for the interaction design.
//
pub mod cli;
pub mod overlay;
pub mod tools;
