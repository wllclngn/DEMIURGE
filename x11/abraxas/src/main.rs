// ABRAXAS for the X11 stack: The daemon and CLI core/abraxas holds, setting
// gamma through RandR.

mod x11;

fn main() {
    abraxas::cli::main(x11::init)
}
