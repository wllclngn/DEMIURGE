# DEMIURGE

ATTENTION: There are currently plans to implement DEMIURGE in Wayland. However, w/ the present state of awesomewm on Arch Linux, DEMIURGE's release was expedited. As of 4/20/2026 X11 is the only implementation of DEMIURGE available.

Minimal X11 window manager in Rust. Drop-in replacement for AwesomeWM. Non-reparenting, single-threaded, zero mutexes. Ships GORDIAN KNOT (screen locker) and a shared rendering module (`torrentius`) in the same Cargo package.

Target: rustc 1.85+ (Rust 2024 edition). Linux only. X11 only.

## Components

DEMIURGE is a Cargo workspace. The window manager itself lives in
`x11/` and `wayland/` with shared logic in `core/`. Two adjacent
daemons that complete the desktop also live here:

| Binary / module | Role |
|---|---|
| `demiurge` | Window manager. TOML config, inotify hot-reload, signalfd shutdown + SIGCHLD reaping, multi-monitor bar, tag-targeted startup spawns, session-scoped child teardown on quit. |
| `demiurge-wl` | Wayland implementation (smithay-based, currently winit dev backend). DSR via offscreen-render + downscale; the rest of the WM features port from `x11/` over the v0.8+ slices. |
| `gordian_knot` | Screen locker + idle daemon + lock-on-suspend hook. Setuid-root with seccomp-bpf + landlock sandbox. PAM authentication via inline FFI. |
| `torrentius` | Rendering subsystem (in `core/`, not a binary). Cairo/Pango primitives, subpixel-AA text on RGB24 intermediate surfaces, PNG capture, framed-panel renderer. Consumed by `demiurge`, `gordian_knot`, eventually `demiurge-wl`. |
| `abraxas` | Color-temperature daemon. Solar grayline (Jean Meeus algorithms) + sigmoid transitions, NOAA cloud-cover weather awareness, four gamma backends (Wayland wlr-gamma, GNOME D-Bus, DRM ioctl, X11 RandR). io_uring event loop. C23 and Rust implementations side-by-side; pick at install time. Replaces redshift. |
| `cherrypie` | Window-matching daemon. TOML rules + regex over WM_CLASS / title / role / process / type. Hot-reload, RandR-aware multi-monitor placement. Replaces devilspie / devilspie2. |

All five binaries share the same workspace version. abraxas and
cherrypie were independent projects before being absorbed; they now
ship under DEMIURGE's release cadence.

## Build

```
CARGO_TARGET_DIR=/tmp/demiurge-build cargo build --release
CARGO_TARGET_DIR=/tmp/demiurge-build cargo test
```

Binaries land in `/tmp/demiurge-build/release/{demiurge, gordian_knot}`.
92 tests pass across 9 targets.

System deps (Arch package names): `cairo pango libpam rust`.

## Install

```
./install.py              # Build, install everything (WM + locker + services)
./install.py -y           # Non-interactive
./install.py status       # Show installation status
./install.py update       # Rebuild and reinstall if source changed
./install.py uninstall    # Remove installed files (config preserved)
```

Install layout:

| Path | Owner | Notes |
|---|---|---|
| `/usr/local/bin/demiurge` | root | WM binary |
| `/usr/local/bin/gordian_knot` | root | Locker, mode 4755 (setuid-root) |
| `/etc/pam.d/gordian_knot` | root | `auth include system-auth` |
| `/usr/share/xsessions/demiurge.desktop` | root | Display-manager entry |
| `~/.config/systemd/user/demiurge.service` | user | WM service |
| `~/.config/systemd/user/gordian_knot-daemon.service` | user | Idle watcher |
| `~/.config/systemd/user/gordian_knot-sleep.service` | user | Pre-suspend hook |
| `~/.config/demiurge/config.toml` | user | Copied from `config.default.toml` only if missing |

If an existing `config.toml` is present at install time, `install.py` runs
`demiurge --check-config` against it. A parse failure offers a backup
(`config.toml.bak`) + replace with the shipped default.

## Run

Session launch (display manager path): the `.desktop` file runs
`systemctl --user start --wait demiurge.service`. Logs to
`journalctl --user -u demiurge`.

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
| `border_width` | `u32` | `0` | Accepted but unused (always 0) |
| `focus_model` | `String` | `"click"` | Accepted but unused (click-to-focus only) |

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
| `lock_seconds` | `u32` | `0` | Idle threshold for GORDIAN KNOT to fire the locker. `0` falls back to legacy `gordian_knot.idle_timeout_seconds` (default 600). Setting this here is the new path. |
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
upgrade; same TOML reaches it). Currently single-output via the
winit dev backend; per-output matching mirrors the X11 path when
the udev/DRM backend lands.

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
tag = "X"

[[startup.spawn]]
cmd = "kitty --class montauk-term -e montauk"
tag = "Z"
class = "montauk-term"
```

`commands` runs each entry via `spawn::spawn()` at WM startup.
Fire-and-forget.

`[[startup.spawn]]` is tag-targeted. `cmd` launches; the next window
whose `WM_CLASS` matches is routed to `tag` and kept unmapped until the
user views that tag. `class` overrides the default derivation (first
whitespace-separated token of `cmd`). FIFO consumption: two pending
entries with the same class are matched in spawn order. Config-load
validation rejects unknown tag names and empty commands.

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
| `view_tag` | `"1"` .. `"N"` | Switch focused monitor to tag (1-indexed). On multi-monitor, swaps with another monitor if it was already showing that tag. |
| `view_prev_tag` | — | Previous tag on focused monitor |
| `view_next_tag` | — | Next tag on focused monitor |
| `move_to_tag` | `"1"` .. `"N"` | Move focused window to tag (visibility follows: stays mapped iff some monitor is showing the destination) |
| `toggle_above` | — | Toggle `_NET_WM_STATE_ABOVE` on focused |
| `toggle_fullscreen` | — | Toggle `_NET_WM_STATE_FULLSCREEN` on focused |
| `toggle_layout` | — | Cycle active tag's layout |
| `run_prompt` | — | Show bar run prompt with `$PATH` tab-completion |
| `screenshot` | — | Write `~/Pictures/screenshot-YYYYmmdd-HHMMSS.png` via `XGetImage` + Cairo PNG |
| `lock` | — | Spawn `gordian_knot` |
| `volume_up` / `volume_down` | — | `wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%±`, read back, fire bar notification |
| `volume_mute` | — | Toggle `@DEFAULT_AUDIO_SINK@` mute, fire bar notification |
| `volume_mic_mute` | — | Toggle `@DEFAULT_AUDIO_SOURCE@` mute, fire bar notification |
| `brightness_up` / `brightness_down` | — | `xbacklight -inc 5` / `-dec 5`, read back, fire bar notification |
| `media_play_pause` / `media_next` / `media_prev` | — | `playerctl`, fire bar notification with current state + track |

Modifiers: `Super`, `Alt`, `Control`, `Shift`. Recognized keysym names
are in `src/keys.rs::name_to_keysym`. Unknown keysyms can be specified
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

RandR hot-plug resizes `active_tags` in lockstep: shrink truncates
and clamps `focused_monitor`; growth appends new entries picking
unique tags so a freshly-plugged screen lands on a previously-hidden
tag rather than mirroring an existing view.

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

## Bar redraw (push-based, per-region atomic)

Each panel owns a persistent Cairo `ImageSurface` allocated at
`Bar::create` time (not per-draw). The panel width is partitioned
into four non-overlapping regions:

| Region | Width | Purpose |
|---|---|---|
| tags | measured: `TAGS_EDGE_PAD + Σ(padded tag widths) + TAGS_EDGE_PAD` | Tag labels, active highlight |
| prompt | `PROMPT_MAX_W = 600` | Run-prompt input (only primary panel) |
| title | remainder | Focused-window title / `NOTIFICATION:` |
| clock | `CLOCK_MAX_W = 400` | Date/time, right-aligned |

The tag strip's width is measured at `Bar::create` time (and again
on any `reload_config`) by running each padded tag name through a
throwaway Pango layout against the active font and summing the
pixel widths. `TAGS_EDGE_PAD = 20` provides a mirrored pad on each
end of the strip; the prompt rect abuts the trailing pad so the
`> ` prefix always lands exactly one edge-pad past the last tag's
right edge, regardless of tag-name length or font choice.

State mutations call `bar.mark_tags_dirty()`, `mark_prompt_dirty()`,
`mark_title_dirty()`, `mark_clock_dirty()`, or `mark_all_dirty()` —
none render directly. `wm.commit_bar()` runs once per event-loop
iteration (in `main.rs::run`), consumes the dirty flags, renders
only the dirty regions into the persistent surface, and
`put_image`s each dirty rect to X. Fast path: no flags set →
`commit` returns immediately.

`mark_clock_dirty` caches the previous `format_clock()` output
(`Bar.last_clock_text`). If the formatted string is unchanged, it
is a no-op — a 1 Hz timer tick that lands inside the same
wall-clock second produces zero work and zero compositor damage.

The net effect: the 1 Hz clock tick pushes only the clock rect to
X (a ~400×40 px region, ~64 KB) instead of the full panel
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

Keybindings can be edited and applied without restarting the session.

## Architecture

Single-threaded, zero mutexes. `x11rb` for X11 (pure Rust, no Xlib).
Cairo + Pango for rendering. `serde` + `toml` for config.

```
src/
  main.rs           CLI + signalfd + poll loop + inotify
  config.rs         TOML schema + validation
  atoms.rs          EWMH/ICCCM atoms
  appearance.rs     XCURSOR env exports + --setup file writer
  wm.rs             Core state: clients, focus, tags, layouts,
                    pending_spawns, mru
  event.rs          X11 event dispatcher
  keys.rs           Keysym resolution, grabs, action dispatch
  ewmh.rs           EWMH property setters
  bar.rs            Multi-monitor status bar. Persistent per-panel
                    Cairo surface, per-region dirty flags (tags/prompt
                    /title/clock), push-driven commit that put_images
                    only dirty rects (see "Bar redraw" section below).
  layout.rs         floating / tile / monocle
  mouse.rs          Super+drag move/resize, _NET_WM_MOVERESIZE
  mru.rs            Cyclical MRU ring
  spawn.rs          posix_spawn with metachar detection; live-child
                    registry + SIGTERM-on-quit (quit_children); resets
                    child signal mask via POSIX_SPAWN_SETSIGMASK so
                    children don't inherit demiurge's blocked signals
  monitor.rs        RandR monitor query
  torrentius.rs     Cairo/Pango primitives shared by demiurge + gordian_knot

  gordian_knot/
    mod.rs
    sysinfo.rs      HOSTNAME/KERNEL/DATE/TIME/UPTIME panels
    pam.rs          Inline libpam.so.0 FFI
    privsep.rs      fork + setresuid + PR_SET_NO_NEW_PRIVS
    seccomp.rs      Raw BPF syscall allowlist
    landlock.rs     Raw landlock_* syscalls
    vt.rs           /dev/ttyN acquisition via VT_OPENQRY + VT_LOCKSWITCH
    x11_lock.rs     Fullscreen override-redirect + grabs + PAM loop
    daemon.rs       XScreenSaver idle polling
    inhibit.rs      logind ListInhibitors watcher (zbus)

  bin/
    gordian_knot.rs Locker dispatcher: --vt, --x11, --daemon

tests/
  config.rs         Config parsing (19 tests)
  bar.rs            Color parsing, PATH scan, completion (13 tests)
  layout.rs         Layout cycle (3 tests)
  mru.rs            Cyclical MRU ring (15 tests)
  torrentius.rs     Hex parsers, font options, surfaces (9 tests)
  unmap_ignore.rs   Unmap ignore counter invariants (5 tests)
  gordian_knot.rs   Locker config + sysinfo (12 tests)
  xephyr.py         Manual nested-X harness
```

## Dependencies

```toml
x11rb = { version = "0.13", features = ["randr", "render", "xkb", "screensaver"] }
cairo-rs = { version = "0.20", features = ["png"] }
pangocairo = "0.20"
pango = "0.20"
serde = { version = "1", features = ["derive"] }
toml = "0.8"
libc = "0.2"
zbus = "4"
```

No Qt. No compositor runtime. No PAM helper crate (inline FFI).

## GORDIAN KNOT

```
gordian_knot                 # X11 in-session lock; VT fallback if X11 grab fails
gordian_knot --vt            # Force VT lock on a fresh /dev/ttyN
gordian_knot --x11           # Force X11 in-session lock
gordian_knot --daemon        # Idle watcher; spawns locker on threshold
gordian_knot --check-config  # Parse [gordian_knot] and exit 0/1
gordian_knot --no-sandbox    # Skip seccomp + landlock (debug only)
```

Security posture:

- Setuid-root binary. `setresuid` + `PR_SET_NO_NEW_PRIVS` drop to the
  real user before PAM + UI.
- seccomp-bpf allowlist via raw BPF (no libseccomp).
  `SECCOMP_RET_KILL_PROCESS` on any syscall outside the allowlist.
- landlock read-only sandbox over `/etc /usr /lib /lib64 /proc /sys
  /dev /run /tmp`. `/home` + `/root` explicitly excluded.
- PAM via inline FFI against `libpam.so.0` (no pam / pam-sys crate).
- Signalfd cleanup path releases the VT on SIGSEGV/SIGBUS/SIGFPE.

Services:

- `gordian_knot-daemon.service` — idle watcher,
  `WantedBy=graphical-session.target`.
- `gordian_knot-sleep.service` — `Before=sleep.target suspend.target
  hibernate.target hybrid-sleep.target`.

Inhibit: zbus watcher on `org.freedesktop.login1.Manager.ListInhibitors`
suppresses auto-lock during video playback and fullscreen games.

## Known limitations

- No Alt+Tab overlay (functional cycling only, no thumbnail preview).
- `_NET_WM_MOVERESIZE` edge-resize directions 0–7 are silently
  dropped. Affects Chromium-family edge drag.
- No urgency hints. `WM_HINTS` urgent flag is ignored.
- `[general] border_width` and `[general] focus_model` parse but have
  no effect. Click-to-focus is hardcoded; borders are always zero.
- GORDIAN KNOT on Wayland: VT path works, X11 in-session lock does not.
- GORDIAN KNOT integration surface (PAM, VT ioctls, seccomp, landlock,
  D-Bus) is not unit-testable without affecting the test-runner
  process state. Logic layers are covered; these are manual-verify.
