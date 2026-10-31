// Action enum lives in demiurge-core (server-agnostic verb set). The
// keycode-based compilation and X11 grab_key wiring below is
// X11-specific and stays here.

use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;

use crate::config::Keybind;
pub use demiurge_core::Action;
use demiurge_core::keys::{name_to_keysym, parse_action};

// A resolved keybinding: keycode + modifier mask -> action
#[derive(Debug)]
pub struct Binding {
    pub keycode: Keycode,
    pub modmask: ModMask,
    pub action: Action,
}

pub use x11_client::keymap::{KeyMap, keymap_keysym, load_keymap};

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
