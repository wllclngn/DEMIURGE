// gk_probe: The QEMU suite's instrument for GORDIAN KNOT (tests/qemu/).
// Built as an example so it is never installed with the product. It runs
// inside the guest only:
//
//   gk-probe grab-state           "held" if another client holds the
//                                 keyboard grab, "free" otherwise
//   gk-probe painted              distinct pixel values in the screen's
//                                 center region (1 = nothing drawn)
//   gk-probe type TEXT            XTest key events for printable ASCII
//   gk-probe key NAME             Return, BackSpace, Escape, Num_Lock, ctrl+u,
//                                 super+Escape, KP_0..KP_9
//   gk-probe xi2-raw SECONDS      XInput2 raw key presses seen on the root
//   gk-probe uinput-type TEXT     real kernel key events through /dev/uinput
//   gk-probe uinput-key NAME      enter, backspace (root only)
//
// X subcommands use $DISPLAY / $XAUTHORITY.

use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::xinput::{self, ConnectionExt as _};
use x11rb::protocol::xproto::*;
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize| args.get(i).map(String::as_str).unwrap_or_else(|| usage());
    match arg(0) {
        "grab-state" => println!("{}", grab_state()),
        "painted" => println!("{}", painted()),
        "type" => x_type(arg(1)),
        "key" => x_key(arg(1)),
        "xi2-raw" => println!("{}", xi2_raw(arg(1).parse().unwrap_or_else(|_| usage()))),
        "uinput-type" => uinput::type_text(arg(1)),
        "uinput-key" => uinput::key(arg(1)),
        _ => usage(),
    }
}

fn usage() -> ! {
    eprintln!("usage: gk-probe grab-state | painted | type TEXT | key NAME | xi2-raw SECONDS");
    eprintln!("       gk-probe uinput-type TEXT | uinput-key NAME");
    std::process::exit(2);
}

fn connect() -> (RustConnection, Window) {
    let (conn, screen) = RustConnection::connect(None).unwrap_or_else(|e| {
        eprintln!("gk-probe: connect: {e}");
        std::process::exit(1);
    });
    let root = conn.setup().roots[screen].root;
    (conn, root)
}

fn grab_state() -> &'static str {
    let (conn, root) = connect();
    let status = conn
        .grab_keyboard(false, root, x11rb::CURRENT_TIME, GrabMode::ASYNC, GrabMode::ASYNC)
        .unwrap()
        .reply()
        .unwrap()
        .status;
    if status == GrabStatus::SUCCESS {
        let _ = conn.ungrab_keyboard(x11rb::CURRENT_TIME);
        let _ = conn.flush();
        "free"
    } else {
        "held"
    }
}

// Distinct pixel values over the center of the screen, where the lock
// panels sit. A solid fill is 1; drawn panels and text are many.
fn painted() -> usize {
    let (conn, root) = connect();
    let g = conn.get_geometry(root).unwrap().reply().unwrap();
    let (w, h) = (g.width.min(640), g.height.min(360));
    let (x, y) = ((g.width - w) / 2, (g.height - h) / 2);
    let img = conn
        .get_image(ImageFormat::Z_PIXMAP, root, x as i16, y as i16, w, h, !0)
        .unwrap()
        .reply()
        .unwrap();
    let mut seen: Vec<u32> = img
        .data
        .chunks_exact(4)
        .map(|p| u32::from_le_bytes([p[0], p[1], p[2], 0]))
        .collect();
    seen.sort_unstable();
    seen.dedup();
    seen.len()
}

struct KeyMap {
    min: u8,
    per: usize,
    syms: Vec<u32>,
}

fn keymap(conn: &RustConnection) -> KeyMap {
    let s = conn.setup();
    let (min, max) = (s.min_keycode, s.max_keycode);
    let m = conn.get_keyboard_mapping(min, max - min + 1).unwrap().reply().unwrap();
    KeyMap { min, per: m.keysyms_per_keycode as usize, syms: m.keysyms }
}

// (keycode, needs_shift) for a keysym, from the server's core map.
fn find(map: &KeyMap, sym: u32) -> Option<(u8, bool)> {
    map.syms.chunks(map.per).enumerate().find_map(|(i, row)| {
        let code = map.min + i as u8;
        if row.first() == Some(&sym) {
            Some((code, false))
        } else if row.get(1) == Some(&sym) {
            Some((code, true))
        } else {
            None
        }
    })
}

fn fake(conn: &RustConnection, code: u8, press: bool) {
    let kind = if press { KEY_PRESS_EVENT } else { KEY_RELEASE_EVENT };
    conn.xtest_fake_input(kind, code, x11rb::CURRENT_TIME, x11rb::NONE, 0, 0, 0).unwrap();
}

fn tap(conn: &RustConnection, map: &KeyMap, sym: u32, mods: &[u32]) {
    let (code, shift) = find(map, sym).unwrap_or_else(|| {
        eprintln!("gk-probe: no keycode for keysym {sym:#x}");
        std::process::exit(1);
    });
    let mut held: Vec<u8> = mods.iter().map(|&m| find(map, m).unwrap().0).collect();
    if shift {
        held.push(find(map, SHIFT_L).unwrap().0);
    }
    for &m in &held {
        fake(conn, m, true);
    }
    fake(conn, code, true);
    fake(conn, code, false);
    for &m in held.iter().rev() {
        fake(conn, m, false);
    }
    conn.flush().unwrap();
    std::thread::sleep(Duration::from_millis(15));
}

const SHIFT_L: u32 = 0xffe1;
const CONTROL_L: u32 = 0xffe3;
const SUPER_L: u32 = 0xffeb;

fn x_type(text: &str) {
    let (conn, _) = connect();
    let map = keymap(&conn);
    for c in text.chars() {
        if !(' '..='~').contains(&c) {
            eprintln!("gk-probe: type takes printable ASCII");
            std::process::exit(2);
        }
        tap(&conn, &map, c as u32, &[]);
    }
    let _ = conn.sync();
}

fn x_key(name: &str) {
    let (conn, _) = connect();
    let map = keymap(&conn);
    match name {
        "Return" => tap(&conn, &map, 0xff0d, &[]),
        "BackSpace" => tap(&conn, &map, 0xff08, &[]),
        "Escape" => tap(&conn, &map, 0xff1b, &[]),
        "Num_Lock" => tap(&conn, &map, 0xff7f, &[]),
        "ctrl+u" => tap(&conn, &map, 'u' as u32, &[CONTROL_L]),
        "super+Escape" => tap(&conn, &map, 0xff1b, &[SUPER_L]),
        kp if kp.starts_with("KP_") => {
            let d: u32 = kp[3..].parse().unwrap_or_else(|_| usage());
            // The keypad keycode, found by its NumLock-off keysym row; the
            // locker decides what it means from the modifier state.
            let sym = [0xff9e, 0xff9c, 0xff99, 0xff9b, 0xff96, 0xff9d, 0xff98, 0xff95, 0xff97, 0xff9a]
                [d as usize % 10];
            tap(&conn, &map, sym, &[])
        }
        _ => usage(),
    }
    let _ = conn.sync();
}

fn xi2_raw(seconds: u64) -> usize {
    let (conn, root) = connect();
    conn.xinput_xi_query_version(2, 2).unwrap().reply().unwrap();
    conn.xinput_xi_select_events(
        root,
        &[xinput::EventMask {
            deviceid: 1, // XIAllMasterDevices
            mask: vec![xinput::XIEventMask::RAW_KEY_PRESS],
        }],
    )
    .unwrap();
    conn.flush().unwrap();
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut count = 0;
    while Instant::now() < deadline {
        while let Some(ev) = conn.poll_for_event().unwrap() {
            if let Event::XinputRawKeyPress(_) = ev {
                count += 1;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    count
}

mod uinput {
    use std::time::Duration;

    const UI_SET_EVBIT: libc::c_ulong = 0x4004_5564;
    const UI_SET_KEYBIT: libc::c_ulong = 0x4004_5565;
    const UI_DEV_SETUP: libc::c_ulong = 0x405c_5503;
    const UI_DEV_CREATE: libc::c_ulong = 0x5501;
    const UI_DEV_DESTROY: libc::c_ulong = 0x5502;
    const EV_SYN: u16 = 0;
    const EV_KEY: u16 = 1;
    const KEY_LEFTSHIFT: u16 = 42;
    const KEY_ENTER: u16 = 28;
    const KEY_BACKSPACE: u16 = 14;

    #[repr(C)]
    struct InputId {
        bustype: u16,
        vendor: u16,
        product: u16,
        version: u16,
    }

    #[repr(C)]
    struct UinputSetup {
        id: InputId,
        name: [u8; 80],
        ff_effects_max: u32,
    }

    #[repr(C)]
    struct InputEvent {
        time: libc::timeval,
        kind: u16,
        code: u16,
        value: i32,
    }

    // Linux key codes for the US layout; (code, shift).
    fn code(c: char) -> Option<(u16, bool)> {
        const ROW_QWERTY: &str = "qwertyuiop";
        const ROW_ASDF: &str = "asdfghjkl";
        const ROW_ZXCV: &str = "zxcvbnm";
        let lower = c.to_ascii_lowercase();
        let shift = c.is_ascii_uppercase();
        if let Some(i) = ROW_QWERTY.find(lower) {
            return Some((16 + i as u16, shift));
        }
        if let Some(i) = ROW_ASDF.find(lower) {
            return Some((30 + i as u16, shift));
        }
        if let Some(i) = ROW_ZXCV.find(lower) {
            return Some((44 + i as u16, shift));
        }
        match c {
            '1'..='9' => Some((2 + (c as u16 - '1' as u16), false)),
            '0' => Some((11, false)),
            '-' => Some((12, false)),
            '_' => Some((12, true)),
            ' ' => Some((57, false)),
            _ => None,
        }
    }

    struct Device(i32);

    impl Device {
        fn create() -> Device {
            let path = std::ffi::CString::new("/dev/uinput").unwrap();
            let fd = unsafe { libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK) };
            assert!(fd >= 0, "open /dev/uinput: {}", std::io::Error::last_os_error());
            unsafe {
                libc::ioctl(fd, UI_SET_EVBIT, EV_KEY as libc::c_int);
                for k in 1..=120 {
                    libc::ioctl(fd, UI_SET_KEYBIT, k as libc::c_int);
                }
                let mut setup = UinputSetup {
                    id: InputId { bustype: 0x06, vendor: 0x1234, product: 0x5678, version: 1 },
                    name: [0; 80],
                    ff_effects_max: 0,
                };
                setup.name[..12].copy_from_slice(b"gk-probe-kbd");
                assert_eq!(libc::ioctl(fd, UI_DEV_SETUP, &setup), 0, "UI_DEV_SETUP");
                assert_eq!(libc::ioctl(fd, UI_DEV_CREATE), 0, "UI_DEV_CREATE");
            }
            // Let the kernel's keyboard handler and logind attach.
            std::thread::sleep(Duration::from_millis(800));
            Device(fd)
        }

        fn emit(&self, kind: u16, code: u16, value: i32) {
            let ev = InputEvent {
                time: libc::timeval { tv_sec: 0, tv_usec: 0 },
                kind,
                code,
                value,
            };
            let n = unsafe {
                libc::write(
                    self.0,
                    &ev as *const InputEvent as *const libc::c_void,
                    std::mem::size_of::<InputEvent>(),
                )
            };
            assert_eq!(n as usize, std::mem::size_of::<InputEvent>(), "uinput write");
        }

        fn tap(&self, code: u16, shift: bool) {
            if shift {
                self.emit(EV_KEY, KEY_LEFTSHIFT, 1);
                self.emit(EV_SYN, 0, 0);
            }
            self.emit(EV_KEY, code, 1);
            self.emit(EV_SYN, 0, 0);
            self.emit(EV_KEY, code, 0);
            self.emit(EV_SYN, 0, 0);
            if shift {
                self.emit(EV_KEY, KEY_LEFTSHIFT, 0);
                self.emit(EV_SYN, 0, 0);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    impl Drop for Device {
        fn drop(&mut self) {
            std::thread::sleep(Duration::from_millis(200));
            unsafe {
                libc::ioctl(self.0, UI_DEV_DESTROY);
                libc::close(self.0);
            }
        }
    }

    pub fn type_text(text: &str) {
        let dev = Device::create();
        for c in text.chars() {
            let (code, shift) = code(c).unwrap_or_else(|| {
                eprintln!("gk-probe: uinput-type has no key for {c:?}");
                std::process::exit(2);
            });
            dev.tap(code, shift);
        }
    }

    pub fn key(name: &str) {
        let dev = Device::create();
        match name {
            "enter" => dev.tap(KEY_ENTER, false),
            "backspace" => dev.tap(KEY_BACKSPACE, false),
            _ => super::usage(),
        }
    }
}
