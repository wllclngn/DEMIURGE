// The keyboard as a Wayland client reads it: The compositor's keymap
// through xkbcommon, each press as its keysym, text and modifiers, and a
// held key repeated at the rate and after the delay the compositor names,
// as the X server repeats one for an X11 client.

use std::time::{Duration, Instant};

use wayland_client::WEnum;
use wayland_client::protocol::wl_keyboard::{self, KeyState, KeymapFormat};
use xkbcommon::xkb;

/// A key pressed, or repeated while it is held.
#[derive(Clone, Debug)]
pub struct Key {
    /// The keysym at the level the modifiers choose: `A` with Shift held.
    pub sym: u32,
    /// The text it types, empty for none.
    pub utf8: String,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

pub struct Keyboard {
    context: xkb::Context,
    keymap: Option<xkb::Keymap>,
    state: Option<xkb::State>,
    // The key held, and when it next repeats.
    held: Option<(xkb::Keycode, Instant)>,
    // Repeats a second, none at 0, and the wait before the first.
    rate: u32,
    delay: Duration,
}

impl Default for Keyboard {
    fn default() -> Self {
        Self::new()
    }
}

impl Keyboard {
    pub fn new() -> Self {
        // X's own default until the compositor names its own.
        Keyboard {
            context: xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
            keymap: None,
            state: None,
            held: None,
            rate: 25,
            delay: Duration::from_millis(600),
        }
    }

    /// One wl_keyboard event: The key it presses, if it presses one.
    pub fn event(&mut self, event: wl_keyboard::Event) -> Option<Key> {
        match event {
            wl_keyboard::Event::Keymap { format: WEnum::Value(KeymapFormat::XkbV1), fd, size } => {
                // SAFETY: The compositor's keymap fd is read only here, through
                // a private copy-on-write map.
                let map = unsafe { memmap2::MmapOptions::new().len(size as usize).map_copy_read_only(&fd) }.ok()?;
                // The blob may carry a trailing NUL.
                let text = std::str::from_utf8(&map).unwrap_or("").trim_end_matches('\0').to_string();
                let keymap = xkb::Keymap::new_from_string(&self.context, text, xkb::KEYMAP_FORMAT_TEXT_V1, xkb::KEYMAP_COMPILE_NO_FLAGS)?;
                self.state = Some(xkb::State::new(&keymap));
                self.keymap = Some(keymap);
                None
            }
            wl_keyboard::Event::Key { key, state, .. } => {
                let code = xkb::Keycode::new(key + 8);
                if state != WEnum::Value(KeyState::Pressed) {
                    if self.held.is_some_and(|(held, _)| held == code) {
                        self.held = None;
                    }
                    return None;
                }
                let pressed = self.decode(code)?;
                let repeats = self.rate > 0 && self.keymap.as_ref().is_some_and(|keymap| keymap.key_repeats(code));
                self.held = repeats.then(|| (code, Instant::now() + self.delay));
                Some(pressed)
            }
            wl_keyboard::Event::Modifiers { mods_depressed, mods_latched, mods_locked, group, .. } => {
                if let Some(state) = self.state.as_mut() {
                    state.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }
                None
            }
            wl_keyboard::Event::RepeatInfo { rate, delay } => {
                self.rate = rate.max(0) as u32;
                self.delay = Duration::from_millis(delay.max(0) as u64);
                None
            }
            wl_keyboard::Event::Leave { .. } => {
                self.held = None;
                None
            }
            _ => None,
        }
    }

    /// When the key held next repeats.
    pub fn next_repeat(&self) -> Option<Instant> {
        self.held.map(|(_, at)| at)
    }

    /// The key held, again, when its repeat is due by `now`.
    pub fn repeat(&mut self, now: Instant) -> Option<Key> {
        let (code, at) = self.held?;
        if now < at {
            return None;
        }
        // From now, not from when it was due: A loop held up late repeats
        // once, as X does, rather than in a burst.
        self.held = Some((code, now + Duration::from_secs(1) / self.rate));
        self.decode(code)
    }

    fn decode(&self, code: xkb::Keycode) -> Option<Key> {
        let state = self.state.as_ref()?;
        let active = |name| state.mod_name_is_active(name, xkb::STATE_MODS_EFFECTIVE);
        Some(Key {
            sym: state.key_get_one_sym(code).raw(),
            utf8: state.key_get_utf8(code),
            ctrl: active(xkb::MOD_NAME_CTRL),
            shift: active(xkb::MOD_NAME_SHIFT),
            alt: active(xkb::MOD_NAME_ALT),
        })
    }
}
