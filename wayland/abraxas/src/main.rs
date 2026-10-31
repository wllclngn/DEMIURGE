// ABRAXAS for the Wayland stack: The daemon and CLI core/abraxas holds,
// setting gamma through the compositor's wlr-gamma-control, with nothing of
// X11 in it.

mod wayland;

fn main() {
    abraxas::cli::main(wayland::init)
}
