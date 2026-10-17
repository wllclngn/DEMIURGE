// demiurge-wl-tty: real-session entry point for DEMIURGE's Wayland
// compositor. Runs as the session's TTY-side process (no nesting),
// driven by libseat / udev / libinput / drm.
//
// The actual backend wiring lives in demiurge_wl::tty -- this binary
// is a thin entrypoint that calls into it. Same shape as the X11
// path's gordian_knot binary calling into the gordian_knot module.

use std::process::ExitCode;

fn main() -> ExitCode {
    demiurge_wl::tty::run()
}
