# DEMIURGE

A Linux desktop in Rust, on two stacks: An X11 window manager, a drop-in
replacement for AwesomeWM, and a Wayland compositor, each with a binary of
its own for every component of the desktop. X11 is frozen and stays on
offer, as solid as it is. The Wayland stack carries the same desktop,
implemented in Wayland rather than borrowed through XWayland, which it
keeps only for X11 applications.

Target: rustc 1.85+ (Rust 2024 edition). Linux only.

## Components

DEMIURGE is one Cargo workspace, laid out by stack:

- `core/` holds what both stacks build on: `core/demiurge` (the config,
  the layout, the bar's painter, the spawner, the clock, the session and
  `torrentius`, the Cairo + Pango primitives every component draws with)
  and each component's crate, everything of it that is not one stack's.
- `x11/` holds the window manager, the X11 binary of each component and
  `x11/client`, what those binaries share (the X keymap, screen capture).
- `wayland/` holds the compositor (`wayland/demiurge`, with its `screen`
  and `window` backends), the Wayland binary of each component and
  `wayland/client`, what those share (the keyboard through xkbcommon with
  key repeat, drawing into `wl_shm`, the event loop, screen capture).
  Nothing under `wayland/` but the compositor speaks X11, and it only to
  XWayland's clients.

| Component | Role |
|---|---|
| DEMIURGE | The window manager: TOML config with inotify hot-reload, tags per monitor, a bar per monitor, title bars, minimize and the tag menu, zoom, Alt+Tab, the run prompt, the idle lock. On Wayland, `demiurge-wl-tty` runs on the real screen from a VT (libseat, udev, DRM, libinput) and `demiurge-wl` is the same compositor in a test window. |
| GORDIAN KNOT | Screen locker, idle and lock-before-suspend daemon, and greetd greeter. Setuid-root, with a seccomp-bpf + landlock sandboxed renderer and PAM through inline FFI. |
| MEDUSA | Screenshot capture and annotation (arrow, rectangle, ellipse, line, marker, pencil, text, blur, numbered steps), pin-to-screen and a clipboard holder of its own. Replaces flameshot. |
| GNOSIS | Keyboard-first file manager: Directory view, folder previews, image thumbnails (the freedesktop cache, shared with other apps), a sidebar of bookmarks and drives (`s`; udisks2 mount on Enter, unmount on `u`), type-to-filter, bookmarks, yank / paste / XDG trash. Replaces pcmanfm. |
| ABRAXAS | Color-temperature daemon: The solar grayline (Jean Meeus's algorithms) with sigmoid transitions and NOAA cloud cover, its gamma set through RandR on X11 and wlr-gamma-control on Wayland, on an io_uring event loop. Replaces redshift. |
| cherrypie | Window policy: TOML rules and regex over class, title, role, process and type, learned rules and window memory. Each window manager asks it over a socket where every new window goes. Replaces devilspie. |

Every binary shares the workspace's version. ABRAXAS and cherrypie were
independent projects before being absorbed; they keep their own READMEs
and ship as part of DEMIURGE.

## Build

```
CARGO_TARGET_DIR=/tmp/demiurge-build cargo build --release
CARGO_TARGET_DIR=/tmp/demiurge-build cargo test
```

Binaries land in `/tmp/demiurge-build/release/`: The X11 stack's under
their own names (`demiurge`, `gordian_knot`, `medusa`, `gnosis`,
`abraxas`, `cherrypie`), the Wayland stack's beside them with a `-wl`
suffix (`demiurge-wl-tty`, `demiurge-wl`, `gordian_knot-wl`, `medusa-wl`,
`gnosis-wl`, `abraxas-wl`, `cherrypie-wl`). Installed, each goes into its
stack's directory under its own name.

426 tests pass across 61 test targets, all of them in each crate's
`tests/` directory. The window manager's integration tests run demiurge
inside nested Xephyr servers (inside a private Xvfb for the RandR mode
tests), cherrypie's end-to-end tests run against a headless Xvfb, and
demiurge-wl's protocol tests run its test window in whatever X server
`$DISPLAY` names (`xvfb-run` for a headless run).

Everything that needs a real screen, a VT, PAM, setuid or greetd runs in a
QEMU guest, never on the host: `tests/qemu/guest.py provision` once, then
`tests/qemu/wayland_suite.py` (demiurge-wl-tty on virtio-gpu, the
protocol suite, MEDUSA, GNOSIS and the clipboard among it, with XWayland
off), `tests/qemu/session_test.py` (a real `startx` session),
`tests/qemu/guest.py test` (the VT lock), `tests/qemu/install_test.py`
(both stacks installed side by side) and `tests/qemu/greeter_test.py`
(greetd, cage and the Wayland greeter).

System deps (Arch package names): `cairo pango libxkbcommon pkgconf
rustup xorg-server xorg-xinit`, plus `greetd cage` for the login screen
and `wayland libinput seatd libdrm mesa` for the Wayland stack.
`x11/install.py deps` and `wayland/install.py deps` check them against
the running system and print the `pacman -S` line for whatever is
missing.

## Install

Each stack has an installer of its own, over one shared module
(`installer.py`):

```
x11/install.py              # Build and install the X11 stack and what both share
wayland/install.py          # The same for the Wayland stack
<stack>/install.py -y       # Non-interactive
<stack>/install.py deps     # Report system dependencies + pacman line
<stack>/install.py status   # Show installation status
<stack>/install.py update   # Rebuild and reinstall if source changed
<stack>/install.py uninstall  # Remove the stack (config preserved)
```

Both stacks install side by side, and uninstalling one leaves the other
whole: What they share (GORDIAN KNOT's PAM file, the unit templates) goes
with the last one, and the config and greetd's config stay. ABRAXAS and cherrypie, part of
DEMIURGE, install with either stack unasked and are enabled for that
stack's session; `--no-abraxas` and `--no-cherrypie` leave one out, and
`--non-usa` skips ABRAXAS's NOAA weather.

### During an Arch install

The installer runs inside `arch-chroot`, so DEMIURGE lands on the disk in
the same pass as the base system:

```
arch-chroot /mnt
x11/install.py -y --user <name> --greetd
```

`--user` names the account that will log in — inside a chroot `$HOME`
is `/root`, and the config, the systemd user units and the sibling
daemons all belong to somebody else. Holding root directly, the
installer writes files itself rather than shelling out to `sudo`
(which a freshly pacstrapped system may not have) and chowns them to
that account afterwards.

`--greetd` installs `contrib/greetd/config.toml`, its greeter the stack's
own `gordian_knot --login` inside cage, and enables `greetd.service`, so
the machine boots to the GORDIAN KNOT login screen instead of a bare TTY.
Each stack's greeter starts its own session unless
`[gordian_knot].session_cmd` names another.

Idle lock and lock-before-suspend need no unit to enable. Set
`[input.idle] lock_seconds` and demiurge starts `gordian_knot --daemon`
with the session; demiurge-wl locks on its own timer.

Install layout:

| Path | Owner | Notes |
|---|---|---|
| `/usr/local/lib/demiurge/x11/bin/` | root | The X11 stack's binaries; `gordian_knot` mode 4755 (setuid-root) |
| `/usr/local/lib/demiurge/wayland/bin/` | root | The Wayland stack's, `demiurge-wl-tty` among them; `gordian_knot` mode 4755 |
| `/usr/local/bin/demiurge`, `/usr/local/bin/demiurge-wl-tty` | root | Links to each stack's window manager, the sessions' entry points |
| `/etc/pam.d/gordian_knot` | root | `auth include system-auth` |
| `/usr/share/xsessions/demiurge.desktop` | root | The X11 session's display-manager entry |
| `/usr/share/wayland-sessions/demiurge.desktop` | root | The Wayland session's |
| `~/.config/systemd/user/demiurge.service` | user | The X11 window manager's service |
| `~/.config/systemd/user/demiurge-{x11,wayland}.target` | user | Each stack's session, which both window managers start; binds `graphical-session.target` |
| `~/.config/systemd/user/{abraxas,cherrypie}@.service` | user | Templates: `abraxas@x11` runs the X11 stack's binary and is wanted by its target, `abraxas@wayland` the Wayland one's |
| `~/.config/demiurge/config.toml` | user | Copied from `config.default.toml` only if missing |
| `/etc/greetd/config.toml` | root | Only with `--greetd`; existing one backed up |

`~` is the target account's home, which `--user` sets; it is not
necessarily the account running the installer. Each session puts its own
stack's directory first on the PATH of everything it starts, so a key
bound to `medusa`, or the lock, runs that stack's binary.

If an existing `config.toml` is present, the installer runs
`gordian_knot --check-config` against it — on install, where a parse
failure offers a backup (`config.toml.bak`) + replace with the shipped
default, and on update, where it warns that the schema has moved out
from under a config that used to work.

## Run

Session launch (display manager path): The X11 session's `.desktop`
file runs `systemctl --user start --wait demiurge.service`, which logs to
`journalctl --user -u demiurge`; the Wayland session's runs
`demiurge-wl-tty`, as greetd or a VT login does.

Under `startx` from a TTY login (the auto-startx and greetd paths),
demiurge's stderr is the virtual console it started on, which nobody
reads once X owns the screen. demiurge detects that and moves its output
onto journald's stream socket: `journalctl -t demiurge`. demiurge-wl-tty,
started from a VT, does the same: `journalctl -t demiurge-wl-tty`.

Nested X (Xephyr):

```
Xephyr :1 -screen 2560x1440 &
DISPLAY=:1 /tmp/demiurge-build/release/demiurge
```

## CLI flags

```
demiurge [-c config.toml] [--check-config] [--setup] [-v] [-h]
```

| Flag | Effect |
|---|---|
| `-c` / `--config PATH` | Override config path (default `~/.config/demiurge/config.toml`) |
| `--check-config` | Parse config, print `demiurge: config ok (PATH)` on success, exit 0/1. No WM startup. |
| `--setup` | lxappearance replacement: regenerate `~/.gtkrc-2.0`, `~/.config/gtk-3.0/settings.ini`, `~/.icons/default/index.theme` from `[cursor]` + `[font]`. Exit without starting the WM. |
| `-v` / `--version` | Print version |
| `-h` / `--help` | Usage |

## Config reference

Path: `~/.config/demiurge/config.toml`. ENOENT falls back to
`Config::default()`. Hot-reloaded via inotify on `IN_CLOSE_WRITE`.

### `[general]`

| Key | Type | Default | Notes |
|---|---|---|---|
| `tags` | `[String]` | `["X", "Y", "Z"]` | Must be non-empty |
| `default_layout` | `String` | `"floating"` | `floating`, `tile`, or `monocle` |
| `master_ratio` | `f64` | `0.5` | `(0.0, 1.0)` exclusive |
| `title_bars` | `bool` | `false` | Title bars on floating windows (see below) |
| `alt_tab_preview` | `bool` | `true` | demiurge-wl's overlay of the windows cycled through while Alt is held |

Title bars: With `title_bars = true`, every floating normal window and
dialog sits in a frame under a 30 px bar with its title and macOS traffic
lights. Close asks the window to close, minimize hides it on its tag, and
zoom does what Super+= and Super+- do; dragging the bar moves the window.
The lights go grey on an unfocused window. Tiled windows stay borderless,
and a tag switched to a tiled layout drops its frames until it floats
again. A window that asks for no decorations through `_MOTIF_WM_HINTS`
(Chromium's tab strip, GTK header bars) gets no frame, and fullscreen
hides the bar. GNOSIS draws the same bar itself when nothing frames it.
Frames are in the save-set, so if DEMIURGE goes away the server hands
every window back to the root. Under demiurge-wl the same bar goes on every
floating window that asks xdg-decoration for server-side decorations.

The layout toggle tiles every window on the tag except those floating on
their own (dialogs, transients, and windows dragged or zoomed out of the
tiles).

### `[bar]`

| Key | Type | Default | Notes |
|---|---|---|---|
| `height` | `u32` | `40` | Panel height in px; hot-reloads |
| `font` | `String` | `"Noto Sans 9"` | Pango font string |
| `bg` / `fg` | `String` | `#121212` / `#888888` | Hex `#RRGGBB` |
| `clock_format` | `String` | `"%a %b %d %Y   %I:%M:%S %p %Z"` | `strftime` syntax. Rendered with `font_features="tnum"` for stable digit widths. |
| `tag_focused_bg` | `String` | `#4A4881` | |
| `tag_focused_fg` | `String` | `#ffffff` | |
| `tag_occupied_fg` | `String` | `#888888` | |
| `tag_empty_fg` | `String` | `#555555` | |
| `notification_fg` | `String` | `#e5a93d` | Color for transient `NOTIFICATION:` text in the bar center |

### `[cursor]` and `[font]`

lxappearance replacement. `[cursor] theme` is exported as `XCURSOR_THEME`
and `[cursor] size` as `XCURSOR_SIZE` at WM init so every spawned child
inherits. `demiurge --setup` writes the equivalent GTK + icon defaults
files.

`[cursor] auto_hide` is the unclutter replacement: when true, the cursor
hides after `auto_hide_seconds` of no pointer motion and reappears the
moment you move the mouse. `auto_hide_seconds = 0` with `auto_hide =
true` gives "always hidden except while actively moving" — keyboard-
forward extreme. The auto-hide is force-suspended during a Super+drag
operation so the cursor stays visible while you're dragging a window.
Hot-reloadable along with the rest of the config; toggling
`auto_hide = false` immediately force-shows.

| Key | Type | Default |
|---|---|---|
| `cursor.theme` | `String` | `"default"` |
| `cursor.size` | `u32` | `24` |
| `cursor.auto_hide` | `bool` | `false` |
| `cursor.auto_hide_seconds` | `u32` | `5` |
| `font.default` | `String` | `"Noto Sans 9"` |

### `[input]`

Settings DEMIURGE owns directly. The point: external tools (`xset`,
`setxkbmap`) get clobbered by X server resets, MappingNotify echoes,
USB keyboard hot-plug, etc., so a one-shot startup command silently
becomes wrong over the course of a session. Putting these in the
config means DEMIURGE re-applies them on every event that would
otherwise reset them, and pulls the surface into one place that
hot-reloads with the rest of the config.

#### `[input.keyboard]`

| Key | Type | Default | Notes |
|---|---|---|---|
| `repeat_delay` | `u32` ms | `0` | Milliseconds before auto-repeat starts. xset r rate's first arg. `0` means "leave server default in place" — DEMIURGE doesn't touch the setting. |
| `repeat_rate` | `u32` per sec | `0` | Repeats per second after the delay. xset r rate's second arg. `0` means "leave server default". |
| `layout` | `String` | `""` | XKB layout (e.g. `"us"`, `"us,de"`). Empty = leave default. |
| `variant` | `String` | `""` | XKB variant (e.g. `"dvorak"`, `"colemak"`). |
| `options` | `[String]` | `[]` | XKB options pass-through. Common values: `"caps:escape"`, `"ctrl:nocaps"`, `"compose:menu"`, `"altwin:swap_lalt_lwin"`. List is the full set; existing options are cleared before applying. |

If `repeat_delay` or `repeat_rate` is non-zero, DEMIURGE applies the
values via XKB SetControls at startup, re-applies on every
MappingNotify event (USB hot-plug, layout switch, external
setxkbmap), and re-applies on config hot-reload.

If any of `layout` / `variant` / `options` is non-empty, DEMIURGE
applies the layout via XKB at startup and re-applies on hot-reload.
Implementation note: the X server takes RMLVO via xkbcomp's rules
parser; DEMIURGE invokes `setxkbmap` internally to push the values
(setxkbmap is part of the standard X11 stack). Replaces
`setxkbmap -layout … -option …` from startup.commands.

#### `[input.idle]`

Single source of truth for "when does the session go dark"
thresholds. GORDIAN KNOT's idle daemon reads `lock_seconds` from
here; DEMIURGE's session applies the DPMS and screensaver timeouts
to the X server.

| Key | Type | Default | Notes |
|---|---|---|---|
| `lock_seconds` | `u32` | `0` | Idle threshold for GORDIAN KNOT. Above `0`, demiurge starts `gordian_knot --daemon` with the session: Idle lock plus lock-before-suspend. `0` = no daemon (read at session start). |
| `screensaver_seconds` | `u32` | `0` | X11 SetScreenSaver timeout. `0` = leave default. |
| `dpms_standby_seconds` | `u32` | `0` | DPMS standby timer (monitor low-power). |
| `dpms_suspend_seconds` | `u32` | `0` | DPMS suspend timer (deeper low-power). |
| `dpms_off_seconds` | `u32` | `0` | DPMS off timer (monitor fully off). |

DPMS is enabled when any of the three timers is non-zero; if all are
zero, DEMIURGE leaves DPMS state alone (does not enable, does not
disable). The three timer fields can be set independently — fields
left at `0` retain whatever the X server currently has for that
slot.

#### `[input.bell]`

X11 audible bell. Most keyboard-forward setups want this off
permanently; this is the place to do it once.

| Key | Type | Default | Notes |
|---|---|---|---|
| `enabled` | `Option<bool>` | unset | `false` silences (volume forced to 0). `true` enables; combine with `volume` for level. Unset = leave default. |
| `volume` | `Option<u8>` | unset | 0–100. Server clamps. |
| `pitch_hz` | `Option<u16>` | unset | Bell pitch in Hz. |
| `duration_ms` | `Option<u16>` | unset | Bell duration in milliseconds. |

Applied via `ChangeKeyboardControl`. The four fields use `Option`
because "explicitly opted out" is meaningfully different from "left
the default in place" — TOML omitting the field means the latter.

### `[[display]]`

Per-output mode + Dynamic Super Resolution (DSR) + DPI ownership.
Each entry matches one output by connector name (or EDID model
substring); first match wins. Outputs not matching any entry keep
their server-default state.

```toml
[[display]]
match = "DP-1"           # output connector name or EDID substring
mode = "2560x1440"       # native panel mode
dsr_multiplier = 2.0     # render at 5120x2880, downscale to 2560x1440
dpi = 138
filter = "bilinear"      # X11: "bilinear" | "nearest"
                         # Wayland: "bilinear" (default), Lanczos shader follow-on
```

| Key | Type | Default | Notes |
|---|---|---|---|
| `match` | `String` | required | Connector name (`"DP-1"`, `"HDMI-A-0"`) or EDID model substring (case-insensitive). |
| `mode` | `String` | `""` | Panel mode (`"WIDTHxHEIGHT"`). Empty = leave current mode. |
| `dsr_multiplier` | `f64` | `1.0` | Framebuffer is rendered at this multiple of the panel size and downscaled for scanout. `2.0` = render 4× the pixels. |
| `dpi` | `u32` | `0` | X server reported DPI. `0` = leave default. Subsumes `xrandr --dpi`. |
| `filter` | `String` | `"bilinear"` | Downscale filter. X11 only honors bilinear/nearest; Wayland defaults bilinear with Lanczos as a future quality option. |

**X11 implementation:** Applied via RandR's CRTC transform matrix
(internally invokes `xrandr` from inside DEMIURGE). The driver's
scanout pipe does the GPU-side downscale; the wire signal stays at
native bandwidth. Replaces `xrandr -s`, `xrandr --scale`, and
`xrandr --dpi` lines from startup.commands.

**Wayland implementation:** Applied as a compositor-side render
pass in demiurge-wl. Clients render into an offscreen GLES texture
sized at `panel_size × dsr_multiplier`; a downscale pass blits that
texture to the scanout target with bilinear sampling. Lower-quality
filter than what's possible (Lanczos compute shader is a future
upgrade; same TOML reaches it). The pass runs on both backends; the
first `[[display]]` entry's multiplier applies to the one monitor driven
today, and per-output matching comes with multi-output.

**No `[[display]]` entry = no DSR.** Both implementations skip the
pass entirely when `dsr_multiplier == 1.0`, so the cost when
disabled is zero.

### `[startup]`

```toml
[startup]
commands = [
    "xrdb -merge ~/.Xresources",
]

[[startup.spawn]]
cmd = "kitty"

[[startup.spawn]]
cmd = "kitty --class montauk-term -e montauk"
```

`commands` runs each entry via `spawn::spawn()` at WM startup.
Fire-and-forget.

`[[startup.spawn]]` launches `cmd` at startup. Which tag a window lands
on is cherrypie's to answer: A rule's `workspace = "Z"` names the tag. An
entry still carrying `tag` (the form before v0.9.0) is moved once, on the
first start of v0.9.0, into a cherrypie rule for its class (`class`, or the
first token of `cmd`), and the `tag` line leaves this file; until then it
routes the next window of that class to the tag, kept unmapped until the
tag is viewed. Config-load validation rejects unknown tag names and empty
commands.

### `[[keybind]]`

```toml
[[keybind]]
mods = ["Super", "Shift"]    # Any subset of Super/Alt/Control/Shift
key = "Return"               # Keysym name or 0x... hex
action = "spawn"             # See action table below
args = "kitty"               # Optional; required for some actions
```

Actions:

| Action | `args` | Effect |
|---|---|---|
| `spawn` | command string | `posix_spawn` the command; metacharacters trigger `sh -c` wrapping |
| `close_window` | — | Send `WM_DELETE_WINDOW` to focus |
| `quit` | — | Clean WM shutdown |
| `mru_next` | — | Alt+Tab forward through the MRU ring, **restricted to the active tag** |
| `mru_prev` | — | Alt+`/Alt+Shift+Tab backward through the MRU ring, active tag only |
| `mru_next_global` | — | Super+Tab forward across **all tags** (switches tag as needed) |
| `mru_prev_global` | — | Super+Shift+Tab backward across all tags |
| `view_tag` | `"1"` .. `"N"` | Switch focused monitor to tag (1-indexed). On multi-monitor, swaps with another monitor if it was already showing that tag. The tag already in view opens the tag menu instead (see below). |
| `view_prev_tag` | — | Previous tag on focused monitor |
| `view_next_tag` | — | Next tag on focused monitor |
| `move_to_tag` | `"1"` .. `"N"` | Move focused window to tag (visibility follows: stays mapped iff some monitor is showing the destination) |
| `toggle_above` | — | Toggle `_NET_WM_STATE_ABOVE` on focused |
| `toggle_fullscreen` | — | Toggle `_NET_WM_STATE_FULLSCREEN` on focused |
| `zoom_in` | — | Super+=: Fill the work area below the bar with the focused window (not fullscreen; the bar stays). A tiled window floats out for it. Advertised as maximized, and an application's own maximize request zooms the same way. A drag ends the zoom; a mode change refits it. |
| `zoom_out` | — | Super+-: Return the zoomed window to its geometry before the zoom, or to its tile |
| `minimize` | — | Super+h: Hide the focused window on its tag. It leaves the tiles, focus and Alt+Tab until the tag menu or a pager restores it; an application's own minimize button (ICCCM IconicState) does the same. |
| `toggle_layout` | — | Cycle active tag's layout |
| `run_prompt` | — | Show bar run prompt with `$PATH` tab-completion |
| `screenshot` | — | Write `~/Pictures/screenshot-YYYYmmdd-HHMMSS.png` via `XGetImage` + Cairo PNG |
| `lock` | — | Spawn `gordian_knot` |
| `volume_up` / `volume_down` | — | `wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%±`, read back, fire bar notification |
| `volume_mute` | — | Toggle `@DEFAULT_AUDIO_SINK@` mute, fire bar notification |
| `volume_mic_mute` | — | Toggle `@DEFAULT_AUDIO_SOURCE@` mute, fire bar notification |
| `brightness_up` / `brightness_down` | — | `xbacklight -inc 5` / `-dec 5`, read back, fire bar notification |
| `media_play_pause` / `media_next` / `media_prev` | — | `playerctl`, fire bar notification with current state + track |

The tag menu: Pressing the key of the tag already in view (Super+2 on
tag Y), or clicking that tag on the bar, drops a list of the tag's windows
under its label, minimized ones marked with a diamond. From the key the
first entry is selected; Up and Down move, Enter picks, Escape closes.
With the mouse, hovering selects, a click picks and a click anywhere else
closes. Picking a window restores it if minimized, then focuses and raises
it.

Modifiers: `Super`, `Alt`, `Control`, `Shift`. Recognized keysym names
are in `core/demiurge/src/keys.rs::name_to_keysym`. Unknown keysyms can be specified
as raw hex (`0xff09` for Tab). Lock/NumLock are stripped from incoming
state before matching.

### `[gordian_knot]`

Consumed by the locker binary. `demiurge` itself ignores it.

| Key | Type | Default |
|---|---|---|
| `bg` | `String` | `#121212` |
| `fg` | `String` | `#c8c8c8` |
| `accent` | `String` | `#e5a93d` |
| `font` | `String` | `"monospace 12"` |
| `idle_timeout_seconds` | `u64` | `600` |
| `prompt` | `String` | `"PASSWORD"` |
| `session_cmd` | `[String]` | The greeter's own stack's session (see GORDIAN KNOT) |
| `session_env` | `[String]` | Likewise |

## Window cycling (cyclical MRU ring)

`wm.mru: Vec<Window>` ordered most-recently-used-first; each window
appears exactly once. Two cycling scopes:

- **Alt+Tab / Alt+` / Alt+Esc** (`mru_next`, `mru_prev`) — cycle
  windows on the **active tag only**. Tag stays put. Classic
  same-desktop window switcher.
- **Super+Tab / Super+Shift+Tab** (`mru_next_global`,
  `mru_prev_global`) — cycle across **all tags**. The tag switches
  as needed while you hold the modifier; on release, you land on the
  target's tag with the target focused.

On first press, `mru.rs` grabs the keyboard, freezes the candidate
list under the requested scope, and sets a `cycle_index` pointing at
the visually-raised target. Further presses advance the index modulo
the candidate count. On modifier release, `finish_cycle` commits
`candidates[index]` to position 0 of `wm.mru` via the normal
`on_focus` path.

`focus_window` dedupes: any focus change removes the target from the
ring and re-inserts at position 0. Destroyed windows are scrubbed
from the ring via `on_unmanage`.

## Multi-monitor

Each RandR output gets its own independent active tag. `wm.active_tags`
is a `Vec<usize>` parallel to `wm.monitors`; entry `i` is the tag
visible on monitor `i`. `wm.focused_monitor` is the index of the
monitor receiving the next user action — view_tag, new-window spawn,
toggle_layout all route to it.

A tag can be visible on at most one monitor at a time. If you
`view_tag T` on monitor A while monitor B is already showing tag T,
the views swap: B takes the tag A was on, A takes T. This is
XMonad-style behavior; it's the only sensible answer that doesn't
duplicate one tag's clients across two monitors.

`focused_monitor` updates implicitly via click-to-focus (the clicked
window's monitor becomes focused) and via bar tag clicks (the clicked
panel's monitor becomes focused, then the requested tag is applied
there).

`arrange()` iterates monitors and tiles each one's active tag's
clients within that monitor's bar-reserved work area. Layouts stay
keyed on tag (one `Layout` per tag, not per monitor-tag pair) — since
a tag is on at most one monitor at a time, there's no ambiguity.

`_NET_CURRENT_DESKTOP` reports the focused monitor's tag (the EWMH
spec doesn't have a per-monitor slot). `_NET_WM_DESKTOP` on a client
is its tag; the client is mapped iff some monitor is showing that tag.

RandR hot-plug resizes `active_tags` in lockstep: Shrink truncates
and clamps `focused_monitor`; growth appends new entries picking
unique tags so a freshly-plugged screen lands on a previously-hidden
tag rather than mirroring an existing view.

### Output modes

X scans out whatever mode an output was last given, so DEMIURGE takes
two decisions the server leaves open:

- **The panel changes:** When an output's advertised mode list changes
  (hot-plug, or a monitor switching its input layout, as an LG DualUp
  does leaving picture-by-picture), DEMIURGE sets the new preferred
  mode. An output a `[[display]]` entry gives a `mode` keeps that mode.
- **A fullscreen client leaves its mode behind:** X does not revert a
  RandR mode set when the client that made it dies. A mode change made
  while a fullscreen client exists belongs to that client, and so does one
  made before it maps, however long before (Wine sets the mode before
  mapping its window; DOOM five seconds before), while the output still
  sits on that mode. When no fullscreen client remains, the output returns
  to its preferred mode (or its `[[display]]` mode). A game killed
  mid-session no longer strands the desktop at its resolution, and a game
  that restores its own mode is never second-guessed. A window that asks
  for exactly a monitor's size is fullscreen whether or not it says so, as
  DOOM's does not.

A mode set by hand (`xrandr -s`) stays while no fullscreen client comes;
the accepted cost is that a fullscreen video played afterwards takes it
back to the preferred mode when it closes. A mode changed and changed back
by hand leaves nothing to claim. Every mode change re-arranges the tiles
against the new geometry. Both decisions run on RandR notify and window
events; nothing polls.

## New-window placement

A floating window that asks for no position of its own (no USPosition in
WM_NORMAL_HINTS) opens centered in the work area of the monitor showing
its tag, below the bar, its size clamped to that area. Most clients ask
for 0,0, which would put them under the bar. A user-specified position
(`-geometry`) is kept. With cherrypie running, DEMIURGE asks it where
each window goes before mapping, and this placement is the fallback. When
a move or resize ends, DEMIURGE reports where the window was left, and
cherrypie's window memory saves the spot in the rule for that class.
demiurge-wl asks the same of each window: An xdg toplevel at its initial
commit, with the app_id and title it set, its first configure waiting for
the answer, and an X11 window at its map request. It reaches the Wayland
stack's cherrypie on the socket named for its own Wayland display, with
no XWayland needed.

## Notifications

Volume, brightness, and media keybinds fire a transient `NOTIFICATION:`
message that replaces the focused-window title in the bar center for
1500 ms, rendered in `bar.notification_fg`. Format:

```
NOTIFICATION: System Volume, 50%.
NOTIFICATION: System Volume, MUTED.
NOTIFICATION: Microphone, MUTED.
NOTIFICATION: Brightness, 75%.
NOTIFICATION: Playing, Title — Artist.
NOTIFICATION: Paused.
```

Expiry is checked on every `redraw_bar` call; since the 1-second
timerfd tick drives redraws, notifications clear with at most ~1 s
overshoot past TTL.

Runtime deps for the action handlers (not enforced at build time):
`wpctl` (wireplumber), `xbacklight`, `playerctl`.

## Bar redraw (per region, by comparison)

Both stacks' bars are drawn by one painter, `core/demiurge/src/bar.rs`. Each
panel, one per monitor, owns a persistent Cairo `ImageSurface`, its width
split into four regions side by side:

| Region | Width | Purpose |
|---|---|---|
| tags | measured: `TAGS_EDGE_PAD + Σ(padded tag widths) + TAGS_EDGE_PAD` | Tag labels, the active highlight, urgent and occupied colors |
| prompt | `PROMPT_MAX_W = 600` | Run-prompt input (primary panel only) |
| title | remainder | Focused-window title or notification (primary panel only) |
| clock | `CLOCK_MAX_W = 400` | Date/time, right-aligned in tabular figures |

On a bar too narrow for all four, the clock keeps its room first, then
the tags, then the prompt. The regions never overlap, and each region's
drawing is clipped to it, so drawing one never touches another.

The tag strip's width is measured when the painter is made (at startup
and on any `reload_config`) by running each padded tag name through a
throwaway Pango layout against the active font and summing the pixel
widths. `TAGS_EDGE_PAD = 20` provides a mirrored pad on each end of the
strip; the prompt region abuts the trailing pad so the `> ` prefix always
lands exactly one edge-pad past the last tag's right edge, regardless of
tag-name length or font choice.

Each panel remembers what each region was last drawn showing.
`Painter::paint` takes what the bar shows now, draws only the regions
whose part of it changed (every region the first time) and returns them;
the stack puts those rects on its screen. On X11, `wm.commit_bar()` runs
once per event-loop iteration (in `main.rs::run`) and `put_image`s each
rect drawn, and an Expose puts the panels whole; demiurge-wl uploads each
into the panel's texture and damages it. With nothing changed, nothing is
drawn or sent.

The clock is read on its 1 Hz tick (`tick_clock`). A tick that lands
inside the same wall-clock second leaves its text as it was, so it draws
nothing and makes no damage.

The net effect: the 1 Hz clock tick pushes only the clock rect (a
400 px wide region, ~64 KB at 40 px tall) instead of the full panel
(~400 KB). Title/notification/prompt updates similarly ship only
their region's pixels. On NVIDIA X11 with
`ForceFullCompositionPipeline = On`, the compositor's per-event
recomposition cost scales with damage rect size, so shrinking the
damage rect directly reduces GPU load.

## Hot-reload

inotify watches the config file on `IN_CLOSE_WRITE`. On change,
`wm::reload_config` applies:

- Keybindings: recompiled, ungrabbed, regrabbed
- Colors, font, clock format: reapplied to bar
- Bar height: panel windows reconfigured, `_NET_WM_STRUT_PARTIAL`
  re-emitted, `_NET_WORKAREA` updated, `arrange()` triggered
- `master_ratio`: reapplied
- `title_bars`: frames added or removed at once

Keybindings can be edited and applied without restarting the session.

## Architecture

Each window manager is single-threaded: X11 through `x11rb` (pure Rust, no
Xlib), Wayland through smithay. Cairo + Pango draw everything, `serde` +
`toml` read the config. Every component is a core crate of what is not one
stack's, with an X11 and a Wayland binary over it.

```
core/
  demiurge/       config, action, layout, monitor, mru, keys, bar (the
                  painter), titlebar, tag_menu, drag, prompt, notify,
                  spawn, clock, journal, session, xsettings, torrentius
  gordian_knot/   PAM (inline libpam FFI, a forked child per attempt),
                  privsep, the secret, keyinput, the renderer and its
                  sandbox (ui, seccomp, landlock), the VT lock (vt, fb,
                  warden), greetd and login (the greeters' conversation),
                  greeter (inside cage, either stack's), screen (the
                  panels it and the Wayland lock draw), sysinfo
  medusa/         overlay (the frozen desktop, the selection, every key),
                  tools (the annotations), cli
  gnosis/         browser (the window's contents and keys), fs, thumbs,
                  drives, cli
  abraxas/        solar, sigmoid, weather, zipdb, config, the io_uring
                  daemon loop and its sandbox, gamma (the backend trait)
  cherrypie/      rules, learn, memory, place, the API and its server,
                  the window manager's client, the daemon and its
                  backend trait

x11/
  demiurge/       The window manager: wm, event, keys, mouse, frame,
                  layout, monitor, modes, display, bar, tag_menu, mru,
                  ewmh, atoms, cursor, idle, migrate, xsettings,
                  appearance
  client/         The X keymap and screen capture its binaries share
  gordian_knot/   x11_lock (the holder; never draws), window (the
                  renderer's X window), decoder, daemon (idle and
                  lock-before-suspend), inhibit (logind's inhibitors)
  medusa/         The overlay as an override-redirect window, the pin,
                  the CLIPBOARD holder
  gnosis/         The browser in an X window, its corners through SHAPE
  abraxas/        Gamma through RandR
  cherrypie/      The X11 backend: placement and learning on X

wayland/
  demiurge/       The compositor: wm, render, input, focus, layer
                  (wlr-layer-shell), capture (ext-image-copy-capture),
                  xwm (XWayland), policy (cherrypie), cycle, titlebar,
                  tag_menu, drag, idle, ime, activation, color, gamma,
                  steam, bar, cursor, dsr
    screen/       The real screen: libseat, udev, DRM, libinput
    window/       The test window (winit)
  client/         The keyboard (xkbcommon, key repeat), drawing into
                  wl_shm, the event loop, screen capture
  gordian_knot/   wl_lock (ext-session-lock)
  medusa/         The overlay on each monitor's overlay layer, the pin,
                  the clipboard holder (ext-data-control)
  gnosis/         The browser in an xdg toplevel
  abraxas/        Gamma through wlr-gamma-control
  cherrypie/      The API with no X connection
```

Each crate's tests sit in its own `tests/`: core's unit-level suites, the
X11 window manager's integration tests under Xephyr (MEDUSA and GNOSIS
end to end among them), demiurge-wl's protocol suite, and
`tests/qemu/` for everything that needs the guest.

## Dependencies

```toml
# Both stacks
cairo-rs = { version = "0.20", features = ["png"] }
pangocairo = "0.20"
pango = "0.20"
serde = { version = "1", features = ["derive"] }
toml = "0.8"
libc = "0.2"
zbus = "4"                      # GNOSIS's drives, X11 GORDIAN KNOT's inhibitors
xkbcommon = "0.8"               # The locks' and the Wayland binaries' keys

# X11
x11rb = { version = "0.13", features = ["randr", "render", "xkb", "screensaver"] }

# Wayland
smithay = "0.7"                 # Pinned to upstream's 253c1545 (Cargo.toml)
smithay-client-toolkit = { version = "0.19", default-features = false }
wayland-client = "0.31"
wayland-protocols = { version = "0.32", features = ["client", "staging"] }
```

No Qt. No GTK. No PAM helper crate (inline FFI).

## GORDIAN KNOT

The X11 binary:

```
gordian_knot                 # Auto: The VT lock; --x11 only if no VT can be taken
gordian_knot --vt            # Force the VT lock on a fresh /dev/ttyN
gordian_knot --x11           # Force the X11 in-session lock
gordian_knot --daemon        # Idle + lock-before-suspend watcher (demiurge starts it)
gordian_knot --login         # greetd greeter inside cage; the TTY greeter with
                             #   no Wayland display
gordian_knot --check-config  # Parse [gordian_knot] and exit 0/1
gordian_knot --no-sandbox    # Skip the renderer's seccomp + landlock (debug only)
```

The Wayland binary:

```
gordian_knot                 # Lock this session (ext-session-lock-v1); also --wl
gordian_knot --vt            # The VT lock, as on X11
gordian_knot --login         # greetd greeter inside cage; the TTY greeter with
                             #   no Wayland display
gordian_knot --check-config  # Parse [gordian_knot] and exit 0/1
```

One credential surface, three states: login / lock / idle-watch. The
SYSTEM + USER panels, the keypress state machine, and the
[appearance].accent theming are shared; login and lock match by
construction. The states differ only in: prompt for username (login
yes, lock no), auth backend (login delegates to greetd, lock calls PAM
directly), and success action (login starts a session, lock unlocks).

### Login screen (`--login`)

`gordian_knot --login` is a greetd greeter, replacing
`lightdm` + `lightdm-gtk-greeter` (and the GTK theme they needed to
draw the login box). It reads `$GREETD_SOCK`, asks for the username
and whatever greetd's PAM asks next, and delegates authentication to
greetd; a wrong password shows greetd's error and asks again. Either
stack's binary draws the lock's SYSTEM and USER panels fullscreen inside
cage, a tiny kiosk Wayland compositor and greetd's usual companion,
whatever session it then starts; with no Wayland display it is the TTY
greeter.

```
pacman -S greetd cage
<stack>/install.py --greetd          # or by hand:
cp contrib/greetd/config.toml /etc/greetd/config.toml
systemctl enable greetd.service
systemctl disable lightdm.service    # if migrating
```

`contrib/greetd/config.toml` names the greeter and nothing else. What
greetd starts on a successful login is `[gordian_knot].session_cmd` and
`session_env`, which greetd's `greeter` user normally leaves unset: Each
stack's greeter then starts its own stack's session.

```toml
[gordian_knot]
#   The X11 stack's greeter:
# session_cmd = ["startx", "/usr/local/bin/demiurge"]
# session_env = ["XDG_SESSION_TYPE=x11", "OZONE_PLATFORM=x11"]
#
#   The Wayland stack's:
# session_cmd = ["/usr/local/bin/demiurge-wl-tty"]
# session_env = ["XDG_SESSION_TYPE=wayland"]
```

`OZONE_PLATFORM` is set for X11 because the greetd path never touches the
xsessions entry or the `demiurge.service` user unit that otherwise carry
it.

### How the lock is built

The binary is setuid-root, and each mode settles its privilege first.

- The lock HOLDER (X11 and VT) becomes root fully, real uid included, so
  the user's own processes cannot signal it; it is undumpable, has no
  core limit and sits at `oom_score_adj -1000`. It holds the grab or the
  VT, decodes keys (X11 through xkbcommon with the server's layout, so
  Caps Lock, AltGr, keypad digits and non-ASCII passwords work) and never
  draws or loads a font.
- PAM runs in a forked child per attempt, so a crashing module costs one
  attempt, not the lock. PAM's own messages are shown, pam_faillock's
  lockout notice among them. Nothing that runs PAM sets
  `PR_SET_NO_NEW_PRIVS`, which would make the kernel ignore
  `unix_chkpwd`'s setuid bit and reject every password.
- The password lives in one page mapped once: `mlock`ed,
  `MADV_DONTDUMP`, wiped with `explicit_bzero` after every attempt.
- The X11 RENDERER is a separate child: The holder re-execs it as the
  user with `PR_SET_NO_NEW_PRIVS`, and it learns only how many characters
  were typed. Each panel is a small persistent surface pushed with a
  checked `put_image`, so no screen size can exceed the server's request
  limit. After its first accepted frame it sandboxes itself, landlock
  (read-only fonts, `/etc`, `/usr`, `/proc`) and seccomp (no exec, no
  fork; an unlisted syscall is refused with `EPERM` and logged, never a
  kill). If it dies, the holder keeps the grab and restarts it.
- The holder takes the grab only once the renderer reports its first
  frame accepted: A locker that cannot show its prompt never holds
  input.
- A session lock takes a VT: A fresh console with switching locked
  (`VT_LOCKSWITCH`), so X, no longer on the active VT, receives no input
  and no X client can watch the password. The same renderer draws the
  same panels into `/dev/fb0` (32-bit XRGB) with the console in graphics
  mode, sandboxed the same way; without such a framebuffer the VT shows a
  text prompt. The holder reads keys from the tty. The in-X lock is the
  fallback only where no VT can be taken (a nested or remote X server).
- `VT_LOCKSWITCH` outlives the process that set it, so before setting it
  the holder forks a warden: Root, in a session of its own, deaf to
  hangups and terminal signals, on a pipe from the holder. An unlock
  tells it the holder is done. If the holder dies instead, the warden
  gives the console back (text mode, switching unlocked, X's VT active,
  the taken VT freed) and locks the session again in X, so a dead holder
  neither strands the machine nor leaves it open.
- The VT lock opens `/dev/tty0` for its ioctls (falling back to
  `/dev/console`, a serial port wherever `console=ttyS0`).

The daemon runs as the user. It spawns the locker past the idle
threshold, never a second while one lives, honors logind "idle"
inhibitors (video playback, fullscreen games), and holds a logind sleep
delay inhibitor: On suspend it locks, waits for the locker to report the
lock is up, then lets the machine sleep.

Every test that grabs input, takes a VT, runs PAM or runs setuid runs in
a QEMU guest only, never on the host: `tests/qemu/guest.py provision`
once, then `tests/qemu/guest.py test`.

## Known limitations

- No Alt+Tab overlay on the X11 stack (functional cycling only, no
  thumbnail preview); demiurge-wl draws one.
- GORDIAN KNOT's in-X lock (`--x11`, and the fallback where no VT can be
  taken) cannot hide typing from other X clients: With the grab held, any
  client selecting XInput2 raw key events still sees every key (measured
  in the QEMU suite: 6 of 6). The session lock takes a VT and gives X no
  input at all: Against a real Xorg on vt1 in QEMU, a spy counted 7 of 7
  keys typed with X active and 0 of the password typed into the lock.
