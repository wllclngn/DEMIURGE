// cherrypie for the Wayland stack: The daemon core/cherrypie holds, serving
// the API on $WAYLAND_DISPLAY's socket with nothing of X11 in it. The
// compositor asks it where every window goes, its X11 windows included.

fn main() {
    cherrypie::cli::main("WAYLAND_DISPLAY", |_| Ok(None))
}
