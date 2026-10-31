// Wayland keybind compilation + dispatch. The Action enum lives in
// demiurge-core (server-agnostic verb set); this module is the Wayland
// counterpart to x11/src/keys.rs.
//
// X11 compiles bindings to keycodes and grabs them on the root window.
// Wayland has no grab model for compositor shortcuts: every key flows
// through the seat keyboard, and the compositor's input filter decides
// per-event whether to intercept (consume as a WM action) or forward
// (deliver to the focused client). So instead of keycodes we keep the
// resolved keysym + the modifier predicate, and match against smithay's
// KeysymHandle + ModifiersState inside the filter closure.
//
// Keysym values are X11/XKB keysym constants -- identical numbers on
// both stacks -- so name_to_keysym and parse_action are core's.

use smithay::input::keyboard::{KeysymHandle, ModifiersState};

use demiurge_core::Action;
use demiurge_core::config::Keybind;
use demiurge_core::keys::{name_to_keysym, parse_action};

// A resolved keybinding: keysym + a modifier predicate -> action. The
// four mod bools are exact-match (Super+Shift means Super held AND Shift
// held AND nothing else); lock modifiers (Caps/Num) are not represented
// here because smithay's ModifiersState already excludes them from the
// logo/alt/ctrl/shift fields.
pub struct Binding {
    pub keysym: u32,
    pub sup: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub shift: bool,
    pub action: Action,
}

/// Compile the config's keybind list into resolved bindings. Unknown
/// key names or unparseable actions are logged and skipped rather than
/// aborting startup -- a single bad keybind shouldn't black-hole the
/// whole binding set.
pub fn compile(keybinds: &[Keybind]) -> Vec<Binding> {
    let mut out = Vec::new();
    for (i, kb) in keybinds.iter().enumerate() {
        let keysym = match name_to_keysym(&kb.key) {
            Some(k) => k,
            None => {
                tracing::warn!(idx = i, key = %kb.key, "unknown keysym; skipping keybind");
                continue;
            }
        };
        let action = match parse_action(&kb.action, &kb.args, i) {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!("{}", e);
                continue;
            }
        };
        let (mut sup, mut alt, mut ctrl, mut shift) = (false, false, false, false);
        for m in &kb.mods {
            match m.as_str() {
                "Super" => sup = true,
                "Alt" => alt = true,
                "Control" => ctrl = true,
                "Shift" => shift = true,
                other => tracing::warn!(idx = i, modifier = %other, "unknown modifier; ignoring"),
            }
        }
        out.push(Binding {
            keysym,
            sup,
            alt,
            ctrl,
            shift,
            action,
        });
    }
    out
}

/// Match a key event against the compiled bindings. Returns the action
/// to run, cloned so the caller can drop the immutable borrow on the
/// binding set before mutating the Wm.
///
/// Modifier match is exact on Super/Alt/Control/Shift. Keysym match
/// tests the unmodified layout symbols first (so a `Super+Shift+1`
/// binding fires on the physical `1` key even though Shift turns it
/// into `exclam`), then falls back to the modified symbol for keys
/// whose only meaningful form is the shifted one.
pub fn match_binding(
    bindings: &[Binding],
    mods: &ModifiersState,
    handle: &KeysymHandle<'_>,
) -> Option<Action> {
    for b in bindings {
        if b.sup != mods.logo || b.alt != mods.alt || b.ctrl != mods.ctrl || b.shift != mods.shift
        {
            continue;
        }
        let raw_hit = handle.raw_syms().iter().any(|s| s.raw() == b.keysym);
        if raw_hit || handle.modified_sym().raw() == b.keysym {
            return Some(b.action.clone());
        }
    }
    None
}

/// Fire-and-forget child spawn for the Spawn action, the CLI shellouts
/// (volume / brightness / media / lock) and the startup commands,
/// started as the X11 stack starts it (demiurge_core::spawn): `sh -c` only
/// when it needs a shell, tracked, reaped as SIGCHLD comes and told to
/// quit with the compositor. `env` (the compositor's WAYLAND_DISPLAY,
/// DISPLAY and cursor theme) goes into the child's environment alone:
/// Writing the compositor's own would race the getenv of the threads EGL
/// has started.
pub fn spawn(cmd: &str, env: &[(&str, String)]) {
    match demiurge_core::spawn::spawn_with(cmd, env) {
        Some(pid) => tracing::info!(pid, %cmd, "spawned"),
        None => tracing::warn!(%cmd, "spawn failed"),
    }
}
