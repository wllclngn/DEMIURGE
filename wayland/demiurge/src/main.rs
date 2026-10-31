// demiurge-wl in the test window. demiurge-wl-tty is the same
// compositor on the real screen.

use std::process::ExitCode;

use demiurge_wl::backend::BackendKind;

fn main() -> ExitCode {
    demiurge_wl::run(BackendKind::Window)
}
