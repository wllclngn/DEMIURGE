// The verb set the WM exposes to the user via keybindings. Server-
// agnostic by construction -- "view tag 2" / "spawn kitty" / "toggle
// fullscreen" mean the same thing on X11 and Wayland; only the
// dispatch (compile to keycodes + grab_keyboard vs xkb keymap +
// per-seat bindings) is protocol-specific. That dispatch lives in
// each implementation's keys module; the variants here stay shared.

#[derive(Debug, Clone)]
pub enum Action {
    Spawn(String),
    CloseWindow,
    Quit,
    MruNext,
    MruPrev,
    MruNextGlobal,
    MruPrevGlobal,
    ViewTag(usize),
    ViewPrevTag,
    ViewNextTag,
    MoveToTag(usize),
    ToggleAbove,
    ToggleFullscreen,
    ToggleLayout,
    RunPrompt,
    Screenshot,
    Lock,
    // System controls. Each one shells out to its CLI (wpctl, xbacklight,
    // playerctl), reads the new state, and fires a bar notification.
    VolumeUp,
    VolumeDown,
    VolumeMute,
    VolumeMicMute,
    BrightnessUp,
    BrightnessDown,
    MediaPlayPause,
    MediaNext,
    MediaPrev,
}
