// x11_probe: The Wayland suite's X11 client, for XWayland windows
// (tests/qemu/). Built as an example so it is never installed with the
// product.
//
//   x11-probe color RRGGBB         map a 400x300 window of the color
//   x11-probe fullscreen RRGGBB    the same, asking for fullscreen
//                                  (_NET_WM_STATE_FULLSCREEN) on map
//   x11-probe mode RRGGBB          as a game changing resolution: RandR
//                                  sets 800x600 on the first output ("mode
//                                  800x600 set"), then an override-redirect
//                                  window of that size maps at its origin
//
// Prints what the window gets, one per line: "configure X Y W H",
// "focus in", "focus out", "key CODE", "button BUTTON". Runs until killed
// or closed (WM_DELETE_WINDOW). Connects to $DISPLAY.

use smithay::reexports::x11rb::connection::Connection;
use smithay::reexports::x11rb::protocol::Event;
use smithay::reexports::x11rb::protocol::randr::{ConnectionExt as _, SetConfig};
use smithay::reexports::x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConfigureNotifyEvent, ConnectionExt as _, CreateWindowAux, EventMask, PropMode,
    WindowClass,
};
use smithay::reexports::x11rb::rust_connection::RustConnection;
use smithay::reexports::x11rb::wrapper::ConnectionExt as _;

fn usage() -> ! {
    eprintln!("usage: x11-probe color|fullscreen|mode RRGGBB");
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rgb = |hex: &String| u32::from_str_radix(hex, 16).unwrap_or_else(|_| usage());
    match (args.first().map(String::as_str), args.get(1)) {
        (Some("color"), Some(hex)) => show(rgb(hex), false, None),
        (Some("fullscreen"), Some(hex)) => show(rgb(hex), true, None),
        (Some("mode"), Some(hex)) => show(rgb(hex), false, Some((800, 600))),
        _ => usage(),
    }
}

fn atom(conn: &RustConnection, name: &str) -> u32 {
    conn.intern_atom(false, name.as_bytes()).expect("intern_atom").reply().expect("intern_atom reply").atom
}

// Set `mode` on the first lit output through RandR, as a game changing
// resolution does: Under XWayland a mode the output does not have is
// emulated for this client alone. Returns the output's origin.
fn set_mode(conn: &RustConnection, root: u32, (w, h): (u16, u16)) -> (i16, i16) {
    let res = conn.randr_get_screen_resources_current(root).expect("screen resources").reply().expect("screen resources");
    for &crtc in &res.crtcs {
        let info = conn.randr_get_crtc_info(crtc, res.config_timestamp).expect("crtc").reply().expect("crtc");
        let Some(&output) = info.outputs.first() else { continue };
        let modes = conn.randr_get_output_info(output, res.config_timestamp).expect("output").reply().expect("output").modes;
        let Some(mode) = res.modes.iter().find(|m| m.width == w && m.height == h && modes.contains(&m.id)) else {
            fail(&format!("no {}x{} mode on the output: {:?}", w, h, res.modes.iter().map(|m| (m.width, m.height)).collect::<Vec<_>>()));
        };
        let set = conn
            .randr_set_crtc_config(crtc, 0, res.config_timestamp, info.x, info.y, mode.id, info.rotation, &[output])
            .expect("set crtc")
            .reply()
            .expect("set crtc");
        if set.status != SetConfig::SUCCESS {
            fail(&format!("the {}x{} mode was refused: {:?}", w, h, set.status));
        }
        println!("mode {}x{} set", w, h);
        return (info.x, info.y);
    }
    fail("no lit output");
}

fn fail(why: &str) -> ! {
    println!("{}", why);
    std::process::exit(1);
}

fn show(rgb: u32, fullscreen: bool, mode: Option<(u16, u16)>) {
    let (conn, screen) = RustConnection::connect(None).expect("connect to $DISPLAY");
    let root = conn.setup().roots[screen].root;
    let ((x, y), (w, h)) = match mode {
        Some(size) => (set_mode(&conn, root, size), size),
        None => ((0, 0), (400, 300)),
    };
    let window = conn.generate_id().expect("window id");
    let aux = CreateWindowAux::new().background_pixel(rgb).override_redirect(mode.is_some() as u32).event_mask(
        EventMask::STRUCTURE_NOTIFY | EventMask::FOCUS_CHANGE | EventMask::KEY_PRESS | EventMask::BUTTON_PRESS | EventMask::EXPOSURE,
    );
    conn.create_window(0, window, root, x, y, w, h, 0, WindowClass::INPUT_OUTPUT, 0, &aux)
        .expect("create_window");
    conn.change_property8(PropMode::REPLACE, window, AtomEnum::WM_NAME, AtomEnum::STRING, b"x11-probe")
        .expect("WM_NAME");
    conn.change_property8(PropMode::REPLACE, window, AtomEnum::WM_CLASS, AtomEnum::STRING, b"x11-probe\0x11-probe\0")
        .expect("WM_CLASS");
    let protocols = atom(&conn, "WM_PROTOCOLS");
    let delete = atom(&conn, "WM_DELETE_WINDOW");
    conn.change_property32(PropMode::REPLACE, window, protocols, AtomEnum::ATOM, &[delete])
        .expect("WM_PROTOCOLS");
    if fullscreen {
        let state = atom(&conn, "_NET_WM_STATE");
        let full = atom(&conn, "_NET_WM_STATE_FULLSCREEN");
        conn.change_property32(PropMode::REPLACE, window, state, AtomEnum::ATOM, &[full])
            .expect("_NET_WM_STATE");
    }
    conn.map_window(window).expect("map_window");
    conn.flush().expect("flush");

    loop {
        match conn.wait_for_event().expect("event") {
            Event::ConfigureNotify(ConfigureNotifyEvent { x, y, width, height, .. }) => {
                println!("configure {} {} {} {}", x, y, width, height);
            }
            Event::FocusIn(_) => println!("focus in"),
            Event::FocusOut(_) => println!("focus out"),
            Event::KeyPress(e) => println!("key {}", e.detail),
            Event::ButtonPress(e) => println!("button {}", e.detail),
            Event::ClientMessage(ClientMessageEvent { data, .. }) if data.as_data32()[0] == delete => return,
            _ => {}
        }
    }
}
