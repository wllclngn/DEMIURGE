// demiurge-wl on the real screen: The session's compositor, run from a
// VT. demiurge-wl is the same compositor in the test window.

use std::process::ExitCode;

use demiurge_wl::backend::BackendKind;

fn main() -> ExitCode {
    demiurge_wl::run(BackendKind::Screen)
}
