// The Wayland binaries' keyboard: Keys decoded through the keymap the
// compositor hands over, modifiers choosing the level, and a held key
// repeated at the compositor's rate after its delay, as X repeats one.

use std::io::Write;
use std::os::fd::{FromRawFd, OwnedFd};
use std::time::{Duration, Instant};

use wayland_client::WEnum;
use wayland_client::protocol::wl_keyboard::{Event, KeyState, KeymapFormat};
use wl_client::keyboard::Keyboard;
use xkbcommon::xkb;

// evdev's codes, as wl_keyboard sends them.
const KEY_A: u32 = 30;
const KEY_LEFTSHIFT: u32 = 42;

// A US keyboard, its keymap handed over in a memfd as the compositor's is,
// and the Shift modifier's mask.
fn keyboard() -> (Keyboard, u32) {
    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let keymap = xkb::Keymap::new_from_names(&context, "", "", "us", "", None, xkb::KEYMAP_COMPILE_NO_FLAGS)
        .expect("a US keymap");
    let text = keymap.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1);
    let fd = unsafe { libc::memfd_create(c"keymap".as_ptr(), libc::MFD_CLOEXEC) };
    assert!(fd >= 0, "memfd_create");
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    std::fs::File::from(fd.try_clone().unwrap()).write_all(text.as_bytes()).unwrap();
    let mut keyboard = Keyboard::new();
    let event = Event::Keymap { format: WEnum::Value(KeymapFormat::XkbV1), fd, size: text.len() as u32 };
    assert!(keyboard.event(event).is_none());
    (keyboard, 1 << keymap.mod_get_index(xkb::MOD_NAME_SHIFT))
}

fn key(code: u32, pressed: bool) -> Event {
    let state = if pressed { KeyState::Pressed } else { KeyState::Released };
    Event::Key { serial: 0, time: 0, key: code, state: WEnum::Value(state) }
}

fn mods(depressed: u32) -> Event {
    Event::Modifiers { serial: 0, mods_depressed: depressed, mods_latched: 0, mods_locked: 0, group: 0 }
}

#[test]
fn a_press_is_its_keysym_and_text_at_the_level_shift_chooses() {
    let (mut kb, shift) = keyboard();
    let a = kb.event(key(KEY_A, true)).expect("a press is a key");
    assert_eq!((a.sym, a.utf8.as_str(), a.shift, a.ctrl), (0x61, "a", false, false));
    assert!(kb.event(key(KEY_A, false)).is_none(), "a release is no key");

    kb.event(mods(shift));
    let a = kb.event(key(KEY_A, true)).unwrap();
    assert_eq!((a.sym, a.utf8.as_str(), a.shift), (0x41, "A", true));
}

#[test]
fn a_held_key_repeats_at_the_rate_after_the_delay() {
    let (mut kb, _) = keyboard();
    kb.event(Event::RepeatInfo { rate: 10, delay: 300 });
    let pressed = Instant::now();
    kb.event(key(KEY_A, true)).unwrap();
    let due = kb.next_repeat().expect("a held letter repeats");
    assert!(due >= pressed + Duration::from_millis(300) && due < pressed + Duration::from_millis(400));
    assert!(kb.repeat(due - Duration::from_millis(1)).is_none(), "repeated before its delay");

    assert_eq!(kb.repeat(due).expect("repeated once due").sym, 0x61);
    assert_eq!(kb.next_repeat(), Some(due + Duration::from_millis(100)), "the next repeat is a tenth of a second on");
    // Due long ago: One repeat, not the burst a late loop would owe.
    let late = due + Duration::from_secs(2);
    assert!(kb.repeat(late).is_some());
    assert_eq!(kb.next_repeat(), Some(late + Duration::from_millis(100)));
}

#[test]
fn a_release_or_leave_ends_the_repeat_and_modifiers_never_repeat() {
    let (mut kb, _) = keyboard();
    kb.event(key(KEY_A, true));
    kb.event(key(KEY_A, false));
    assert!(kb.next_repeat().is_none(), "the release left a repeat");

    kb.event(key(KEY_A, true));
    kb.event(Event::Leave { serial: 0, surface: dead_surface() });
    assert!(kb.next_repeat().is_none(), "leaving the surface left a repeat");

    kb.event(key(KEY_LEFTSHIFT, true));
    assert!(kb.next_repeat().is_none(), "Shift held repeats");

    kb.event(Event::RepeatInfo { rate: 0, delay: 300 });
    kb.event(key(KEY_A, true));
    assert!(kb.next_repeat().is_none(), "a rate of 0 still repeats");
}

// A wl_surface with no compositor behind it, as a Leave event carries one.
fn dead_surface() -> wayland_client::protocol::wl_surface::WlSurface {
    let (ours, _theirs) = std::os::unix::net::UnixStream::pair().unwrap();
    let conn = wayland_client::Connection::from_socket(ours).unwrap();
    wayland_client::Proxy::inert(conn.backend().downgrade())
}
