// Action enum lives in demiurge-core (server-agnostic verb set). The
// keycode-based compilation and X11 grab_key wiring below is
// X11-specific and stays here.

use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;

use crate::config::Keybind;
pub use demiurge_core::Action;

// A resolved keybinding: keycode + modifier mask -> action
#[derive(Debug)]
pub struct Binding {
    pub keycode: Keycode,
    pub modmask: ModMask,
    pub action: Action,
}

// Cached keyboard mapping. Built once at startup and rebuilt on MappingNotify.
// Replaces per-call get_keyboard_mapping roundtrips, which previously fired
// once per keystroke in the run prompt and once per init for find_alt_keycodes.
pub struct KeyMap {
    pub min_keycode: u8,
    pub syms_per_code: usize,
    pub keysyms: Vec<u32>,
}

pub fn load_keymap(conn: &RustConnection) -> KeyMap {
    let setup = conn.setup();
    let min_keycode = setup.min_keycode;
    let max_keycode = setup.max_keycode;
    let count = (max_keycode - min_keycode) as u32 + 1;

    let (syms_per_code, keysyms) = conn
        .get_keyboard_mapping(min_keycode, count as u8)
        .ok()
        .and_then(|c| c.reply().ok())
        .map(|m| (m.keysyms_per_keycode as usize, m.keysyms))
        .unwrap_or((0, Vec::new()));

    KeyMap {
        min_keycode,
        syms_per_code,
        keysyms,
    }
}

pub fn keymap_keysym(map: &KeyMap, keycode: Keycode, state: u16) -> u32 {
    if map.syms_per_code == 0 {
        return 0;
    }
    let idx = (keycode - map.min_keycode) as usize;
    let base = idx * map.syms_per_code;
    if base >= map.keysyms.len() {
        return 0;
    }
    let shifted = state & u16::from(ModMask::SHIFT) != 0;
    let col = if shifted && map.syms_per_code > 1 { 1 } else { 0 };
    map.keysyms.get(base + col).copied().unwrap_or(0)
}

pub fn keymap_alt_keycodes(map: &KeyMap) -> Vec<Keycode> {
    let mut keycodes = Vec::new();
    // Alt_L = 0xffe9, Alt_R = 0xffea
    for target in [0xffe9u32, 0xffea] {
        if let Some(kc) = find_keycode(&map.keysyms, map.syms_per_code, map.min_keycode, target) {
            keycodes.push(kc);
        }
    }
    keycodes
}

pub fn compile(
    keybinds: &[Keybind],
    keymap: &KeyMap,
) -> Result<Vec<Binding>, String> {
    let mut bindings = Vec::new();

    for (i, kb) in keybinds.iter().enumerate() {
        let target_sym = name_to_keysym(&kb.key)
            .ok_or_else(|| format!("keybind[{}]: unknown key '{}'", i, kb.key))?;

        let keycode = find_keycode(&keymap.keysyms, keymap.syms_per_code, keymap.min_keycode, target_sym);
        let keycode = match keycode {
            Some(kc) => kc,
            None => {
                eprintln!("[keys] warning: no keycode for '{}', skipping", kb.key);
                continue;
            }
        };

        let modmask = mods_to_mask(&kb.mods);
        let action = parse_action(&kb.action, &kb.args, i)?;

        bindings.push(Binding {
            keycode,
            modmask,
            action,
        });
    }

    Ok(bindings)
}

pub fn grab_all(conn: &RustConnection, root: Window, bindings: &[Binding]) {
    // Ungrab everything first
    let _ = conn.ungrab_key(Grab::ANY, root, ModMask::ANY);

    for binding in bindings {
        // Grab with all lock modifier combinations
        for lock_mask in &[
            ModMask::from(0u16),
            ModMask::LOCK,                           // CapsLock
            ModMask::from(u16::from(ModMask::M2)),   // NumLock (Mod2)
            ModMask::LOCK | ModMask::from(u16::from(ModMask::M2)), // Both
        ] {
            let combined = ModMask::from(u16::from(binding.modmask) | u16::from(*lock_mask));
            let _ = conn.grab_key(
                false,
                root,
                combined,
                binding.keycode,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            );
        }
    }

    let _ = conn.flush();
}

pub fn lookup(bindings: &[Binding], keycode: Keycode, state: u16) -> Option<&Action> {
    // Strip lock modifiers for matching
    let clean_state = state & !(u16::from(ModMask::LOCK) | u16::from(ModMask::M2));
    bindings
        .iter()
        .find(|b| b.keycode == keycode && u16::from(b.modmask) == clean_state)
        .map(|b| &b.action)
}

fn find_keycode(
    keysyms: &[Keysym],
    syms_per_code: usize,
    min_keycode: Keycode,
    target: u32,
) -> Option<Keycode> {
    for (idx, chunk) in keysyms.chunks(syms_per_code).enumerate() {
        // Check first two columns (unshifted and shifted)
        for sym in chunk.iter().take(2.min(syms_per_code)) {
            if *sym == target {
                return Some(min_keycode + idx as u8);
            }
        }
        // Also check all columns for media keys
        if target >= 0x1008_0000 {
            for sym in chunk {
                if *sym == target {
                    return Some(min_keycode + idx as u8);
                }
            }
        }
    }
    None
}

fn mods_to_mask(mods: &[String]) -> ModMask {
    let mut mask = 0u16;
    for m in mods {
        mask |= match m.as_str() {
            "Super" => u16::from(ModMask::M4),
            "Alt" => u16::from(ModMask::M1),
            "Control" => u16::from(ModMask::CONTROL),
            "Shift" => u16::from(ModMask::SHIFT),
            _ => 0,
        };
    }
    ModMask::from(mask)
}

fn parse_action(action: &str, args: &Option<String>, idx: usize) -> Result<Action, String> {
    match action {
        "spawn" => {
            let cmd = args
                .as_ref()
                .ok_or_else(|| format!("keybind[{}]: spawn requires args", idx))?;
            Ok(Action::Spawn(cmd.clone()))
        }
        "close_window" => Ok(Action::CloseWindow),
        "quit" => Ok(Action::Quit),
        "mru_next" => Ok(Action::MruNext),
        "mru_prev" => Ok(Action::MruPrev),
        "mru_next_global" => Ok(Action::MruNextGlobal),
        "mru_prev_global" => Ok(Action::MruPrevGlobal),
        "view_tag" => {
            let n: usize = args
                .as_ref()
                .ok_or_else(|| format!("keybind[{}]: view_tag requires args", idx))?
                .parse()
                .map_err(|_| format!("keybind[{}]: view_tag args must be a number", idx))?;
            Ok(Action::ViewTag(n.saturating_sub(1)))
        }
        "view_prev_tag" => Ok(Action::ViewPrevTag),
        "view_next_tag" => Ok(Action::ViewNextTag),
        "move_to_tag" => {
            let n: usize = args
                .as_ref()
                .ok_or_else(|| format!("keybind[{}]: move_to_tag requires args", idx))?
                .parse()
                .map_err(|_| format!("keybind[{}]: move_to_tag args must be a number", idx))?;
            Ok(Action::MoveToTag(n.saturating_sub(1)))
        }
        "toggle_above" => Ok(Action::ToggleAbove),
        "toggle_fullscreen" => Ok(Action::ToggleFullscreen),
        "toggle_layout" => Ok(Action::ToggleLayout),
        "run_prompt" => Ok(Action::RunPrompt),
        "screenshot" => Ok(Action::Screenshot),
        "lock" => Ok(Action::Lock),
        "volume_up" => Ok(Action::VolumeUp),
        "volume_down" => Ok(Action::VolumeDown),
        "volume_mute" => Ok(Action::VolumeMute),
        "volume_mic_mute" => Ok(Action::VolumeMicMute),
        "brightness_up" => Ok(Action::BrightnessUp),
        "brightness_down" => Ok(Action::BrightnessDown),
        "media_play_pause" => Ok(Action::MediaPlayPause),
        "media_next" => Ok(Action::MediaNext),
        "media_prev" => Ok(Action::MediaPrev),
        _ => Err(format!("keybind[{}]: unknown action '{}'", idx, action)),
    }
}

// Keysym name resolution
// Covers the user's approximately 25 keybindings
fn name_to_keysym(name: &str) -> Option<u32> {
    match name {
        // Letters
        "a" | "A" => Some(0x0061),
        "b" | "B" => Some(0x0062),
        "c" | "C" => Some(0x0063),
        "d" | "D" => Some(0x0064),
        "e" | "E" => Some(0x0065),
        "f" | "F" => Some(0x0066),
        "g" | "G" => Some(0x0067),
        "h" | "H" => Some(0x0068),
        "i" | "I" => Some(0x0069),
        "j" | "J" => Some(0x006a),
        "k" | "K" => Some(0x006b),
        "l" | "L" => Some(0x006c),
        "m" | "M" => Some(0x006d),
        "n" | "N" => Some(0x006e),
        "o" | "O" => Some(0x006f),
        "p" | "P" => Some(0x0070),
        "q" | "Q" => Some(0x0071),
        "r" | "R" => Some(0x0072),
        "s" | "S" => Some(0x0073),
        "t" | "T" => Some(0x0074),
        "u" | "U" => Some(0x0075),
        "v" | "V" => Some(0x0076),
        "w" | "W" => Some(0x0077),
        "x" | "X" => Some(0x0078),
        "y" | "Y" => Some(0x0079),
        "z" | "Z" => Some(0x007a),

        // Numbers
        "1" => Some(0x0031),
        "2" => Some(0x0032),
        "3" => Some(0x0033),
        "4" => Some(0x0034),
        "5" => Some(0x0035),
        "6" => Some(0x0036),
        "7" => Some(0x0037),
        "8" => Some(0x0038),
        "9" => Some(0x0039),
        "0" => Some(0x0030),

        // Function keys
        "F1" => Some(0xffbe),
        "F2" => Some(0xffbf),
        "F3" => Some(0xffc0),
        "F4" => Some(0xffc1),
        "F5" => Some(0xffc2),
        "F6" => Some(0xffc3),
        "F7" => Some(0xffc4),
        "F8" => Some(0xffc5),
        "F9" => Some(0xffc6),
        "F10" => Some(0xffc7),
        "F11" => Some(0xffc8),
        "F12" => Some(0xffc9),

        // Navigation
        "Return" | "Enter" => Some(0xff0d),
        "Escape" => Some(0xff1b),
        "Tab" => Some(0xff09),
        "BackSpace" => Some(0xff08),
        "Delete" => Some(0xffff),
        "space" | "Space" => Some(0x0020),
        "Up" => Some(0xff52),
        "Down" => Some(0xff54),
        "Left" => Some(0xff51),
        "Right" => Some(0xff53),
        "Home" => Some(0xff50),
        "End" => Some(0xff57),
        "Page_Up" => Some(0xff55),
        "Page_Down" => Some(0xff56),
        "Print" => Some(0xff61),
        "grave" => Some(0x0060),

        // XF86 media keys
        "XF86AudioMute" => Some(0x1008ff12),
        "XF86AudioLowerVolume" => Some(0x1008ff11),
        "XF86AudioRaiseVolume" => Some(0x1008ff13),
        "XF86AudioMicMute" => Some(0x1008ffb2),
        "XF86MonBrightnessUp" => Some(0x1008ff02),
        "XF86MonBrightnessDown" => Some(0x1008ff03),
        "XF86AudioPlay" => Some(0x1008ff14),
        "XF86AudioNext" => Some(0x1008ff17),
        "XF86AudioPrev" => Some(0x1008ff16),

        // Raw hex keysym
        s if s.starts_with("0x") => u32::from_str_radix(&s[2..], 16).ok(),

        _ => None,
    }
}
