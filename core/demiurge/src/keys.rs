// Keybind config read the same by both stacks: Each `[[keybind]]`'s
// action and its args as an Action, and a key's name as its X11/XKB
// keysym, the number space the X11 server and xkbcommon share. Turning
// a keysym into what a key press matches against stays per stack.

use crate::Action;

pub fn parse_action(action: &str, args: &Option<String>, idx: usize) -> Result<Action, String> {
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
        "zoom_in" => Ok(Action::ZoomIn),
        "zoom_out" => Ok(Action::ZoomOut),
        "minimize" => Ok(Action::Minimize),
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

// Keysym name resolution. Values are X11/XKB keysym constants.
pub fn name_to_keysym(name: &str) -> Option<u32> {
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
        "equal" => Some(0x003d),
        "minus" => Some(0x002d),

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
