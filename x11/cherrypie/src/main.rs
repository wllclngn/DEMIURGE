// cherrypie for the X11 stack: The daemon core/cherrypie holds, serving the
// API on $DISPLAY's socket and watching the X server directly when no window
// manager speaks it.

mod x11;

fn main() {
    cherrypie::cli::main("DISPLAY", |signal_fd| {
        x11::X11Backend::init(signal_fd).map(|b| Some(Box::new(b) as Box<dyn cherrypie::backend::Backend>))
    })
}
