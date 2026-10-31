# cherrypie

"Diane, if you ever get up this way, that cherry pie is worth a stop."

Window matching daemon for Linux written in Rust. Automatically positions, sizes, and configures windows based on TOML rules with regex matching. Part of DEMIURGE: Each of its two stacks runs a binary of its own over this crate, and its window manager asks it where every new window goes.

Replaces devilspie and devilspie2 with a simpler, faster alternative: TOML config instead of Lua scripting, regex matching instead of string comparison, hot reload via inotify, and sub-frame rule application via `_NET_CLIENT_LIST` diffing.

## Features

- Match windows by WM_CLASS, title, role, process name, or window type
- Regex patterns on all matchers (case-insensitive, anchored, etc.)
- Position windows: absolute coordinates, named anchors (`center`, `top-right`), or percentages
- Size windows: absolute pixels or percentage of the monitor's work area
- Target specific monitors by name or index
- EWMH actions: maximize, fullscreen, pin (sticky), minimize, shade, above/below, focus, opacity, decoration toggle
- Workspace assignment
- Hot config reload on save (inotify `IN_CLOSE_WRITE`)
- X11 via x11rb (pure Rust) in the X11 binary; the Wayland binary serves the API alone, with no X connection
- RandR monitor detection, followed live through mode changes and hot-plug
- Positions and percentages resolve against the work area (`_NET_WORKAREA`),
  so a centered window sits below the window manager's bar
- Fullscreen windows keep their own geometry; no rule moves or resizes them
- Handles reparenting WMs (AwesomeWM, i3, etc.) via `_NET_CLIENT_LIST` diffing
- Applies rules to existing windows on startup
- poll(2) event loop with signalfd for clean shutdown
- 1.8 MB binary, 1 MB resident memory

## Build

Requires Rust 2024 edition (rustc 1.85+).

```
CARGO_TARGET_DIR=/tmp/demiurge-build cargo build --release -p cherrypie-x11   # or -p cherrypie-wl
```

Binaries: `/tmp/demiurge-build/release/cherrypie` (X11) and
`cherrypie-wl` (Wayland), each installed as `cherrypie` in its stack's
directory.

## Install

cherrypie installs with either DEMIURGE stack, unasked, from the
repository's root: `x11/install.py` or `wayland/install.py`
(`--no-cherrypie` leaves it out). The installer enables the user unit's
instance for its stack: `cherrypie@x11.service` runs the X11 binary under
`demiurge-x11.target`, `cherrypie@wayland.service` the Wayland one under
`demiurge-wayland.target`.

## Usage

```
cherrypie                         # Run with default config
cherrypie --config /path/to.toml  # Custom config path
cherrypie --learn-after 30        # Seconds a window must stay open to be learned
cherrypie --dry-run               # Log matches without applying actions
cherrypie --version               # Print version
```

Default config location: `~/.config/cherrypie/config.toml`

## Configuration

Rules are TOML tables. Each rule has matchers (at least one required) and actions.

### Learned rules

The config does not have to exist. cherrypie starts with no rules, and the
first time a normal window maps whose class no rule matches, it applies live
default actions to it at once. If a window of that class is still open 30
seconds later (`--learn-after`), it appends the rule for the class:

```toml
# Learned 2026-10-16 from a new window; edit to taste.
[[rule]]
class = '^Gimp$'
position = "center"
size = ["60%", "70%"]
focus = true
```

Edit the values and save; the file hot-reloads. The class matcher is the
exact WM_CLASS, regex-escaped and anchored. One rule per class: A second
window of the class matches the rule rather than learning another.
Dialogs, transient windows and every other non-`normal` type are never
learned, and neither are untitled windows, fullscreen windows (games) or
classes whose windows all close inside the interval (installers, consoles
and launch helpers). The rule is appended as text, so comments and ordering in the
rest of the file are untouched. A config that exists but does not parse
stops the daemon with status 78 (EX_CONFIG), which `cherrypie@.service`
excludes from its restart policy.

### Window memory

When you finish moving or resizing a window, the window manager reports
where you left it (DEMIURGE does, over the API below), and cherrypie writes
the spot into the rule that placed the window, as the monitor's name plus
percentages of its work area:

```toml
# Learned 2026-10-16 from a new window; edit to taste.
[[rule]]
class = '^Gimp$'
position = ["10%", "3.52%"]
size = ["50%", "50%"]
focus = true
monitor = "HDMI-0"
```

The next window of the class opens there. Learned rules remember on their
own; `remember = false` stops one. A rule you wrote yourself is never
rewritten unless it says `remember = true`. When several rules match, the
spot goes to the last one that sets a size or position. The file is edited
with toml_edit, so your comments, ordering and formatting stay as they are.

### The window-policy API

cherrypie listens on `$XDG_RUNTIME_DIR/cherrypie-<display>.sock` for a
window manager that asks before it maps (DEMIURGE does), its display
`$DISPLAY` for the X11 binary and `$WAYLAND_DISPLAY` for the Wayland one. The protocol is
REST-shaped JSON, one object per line, on one persistent connection:

```
-> {"id":1,"op":"PUT","path":"/monitors","body":[{"name":"HDMI-0","rect":{...},"work_area":{...}}]}
<- {"id":1,"status":204}
-> {"id":2,"op":"POST","path":"/windows","body":{"window":7340035,"class":"kitty","title":"~",...}}
<- {"id":2,"status":200,"body":{"geometry":{"x":512,"y":236,"w":1536,"h":1008},"focus":true}}
-> {"id":3,"op":"PUT","path":"/windows/7340035/geometry","body":{"monitor":"HDMI-0","rect":{"x":256,"y":140,"w":1280,"h":1420}}}
<- {"id":3,"status":204}
-> {"id":4,"op":"DELETE","path":"/windows/7340035"}
<- {"id":4,"status":204}
```

A field left out of a placement is the window manager's to decide. While a
client is connected, the X11 binary's own actions stand down so the two
never act on the same window; they resume when it disconnects. The types
are `cherrypie::api`.

### Matchers

All matchers use Rust regex syntax.

| Field | Matches against |
|-------|----------------|
| `class` | WM_CLASS (e.g. `kitty`, `Chromium`, `libreoffice-writer`) |
| `title` | `_NET_WM_NAME` / `WM_NAME` |
| `role` | `WM_WINDOW_ROLE` |
| `process` | Process name from `/proc/PID/comm` via `_NET_WM_PID` |
| `type` | `_NET_WM_WINDOW_TYPE` (`normal`, `dialog`, `dock`, `toolbar`, `menu`, `utility`, `splash`) |

Multiple matchers on the same rule are AND-ed.

### Actions

| Field | Value | Description |
|-------|-------|-------------|
| `position` | `[x, y]`, `"center"`, `["50%", "25%"]` | Window position (absolute, named anchor, or percentage) |
| `size` | `[w, h]`, `["80%", "60%"]` | Window size (absolute or percentage of monitor) |
| `workspace` | string or integer | Move to a tag by name (`"Z"`, as DEMIURGE's `[general] tags` spell it) or to an EWMH desktop by index (0-indexed) |
| `monitor` | integer or `"HDMI-0"` | Target monitor by index or RandR name |
| `maximize` | bool | Maximize horizontally and vertically |
| `fullscreen` | bool | Set fullscreen state |
| `pin` | bool | Pin to all workspaces (sticky) |
| `minimize` | bool | Minimize (iconify) |
| `shade` | bool | Shade (collapse to titlebar) |
| `above` | bool | Keep above other windows |
| `below` | bool | Keep below other windows |
| `decorate` | bool | Enable/disable window decorations |
| `focus` | bool | Focus the window |
| `opacity` | float (0.0-1.0) | Window opacity |
| `remember` | bool | Save where you leave a matching window into this rule (learned rules default to true) |

### Named positions

`center`, `top-left`, `top-right`, `bottom-left`, `bottom-right`, `left`, `right`, `top`, `bottom`

### Example config

```toml
[[rule]]
class = "(?i)kitty"
position = [0, 38]
size = [2558, 1401]

[[rule]]
class = "^Chromium$"
position = "center"
size = ["90%", "90%"]

[[rule]]
class = "(?i)vlc"
monitor = "HDMI-0"
position = "top-right"
size = [640, 480]
above = true

[[rule]]
process = "firefox"
workspace = 1
maximize = true
```

## Architecture

```
core/cherrypie/src/
  cli.rs        Hand-rolled CLI (--config, --learn-after, --dry-run, --version, --help)
  daemon.rs     poll(2) event loop: signalfd + inotify + the backend's fd
  config.rs     TOML parsing with serde untagged enums for flexible value types
  learn.rs      Learned-rule text, compile-from-text, append to the config
  memory.rs     Window memory: a moved window's spot written into its rule
  rules.rs      Rule compilation: regex, position/size/monitor resolution
  place.rs      Rule geometry, shared by the X11 backend and the API
  api.rs        The window-policy API's requests, responses and types
  server.rs     The API's Unix socket listener and connections
  client.rs     The window manager's half of the API, both stacks'
  backend.rs    The backend trait
x11/cherrypie/src/
  x11.rs        X11 via x11rb: atom_manager, _NET_CLIENT_LIST diffing, RandR, EWMH
wayland/cherrypie/src/
  main.rs       The API with no backend
```

Event flow (the X11 binary): X11 PropertyNotify on root window signals `_NET_CLIENT_LIST` change. cherrypie diffs against the previous list, identifies new window IDs, queries their properties (class, title, role, process, type), matches against compiled rules, and applies actions via `configure_window` and EWMH ClientMessage events. Flush. One poll wake per batch of changes.

Config reload: inotify watches the config directory for `IN_CLOSE_WRITE`. On trigger, TOML is re-parsed and rules re-compiled. No restart needed.

## Dependencies

Runtime: none (statically links everything except libc).

Build:
- x11rb 0.13 (pure Rust X11 protocol, RandR extension), the X11 binary only
- serde + toml 0.8 + serde_json 1
- toml_edit 0.22 (window memory edits the config in place)
- regex 1
- libc 0.2

## Tests

```
CARGO_TARGET_DIR=/tmp/demiurge-build cargo test -p cherrypie-core -p cherrypie-x11 -p cherrypie-wl
```

70 tests: 23 config parsing, 22 rule compilation and matching, 5 rule
learning and 4 window memory (verbatim edits, `remember`, the deciding
rule, pixel-exact percentages) in `core/cherrypie/tests/`; 9 API runs of
the X11 binary with the test as the window manager and 5 end-to-end runs
of it against a headless Xvfb (learning, placement, one rule per class,
transients, untitled, short-lived and fullscreen windows skipped,
work-area placement, and a live mode change through a resizeable Xephyr
nested in Xvfb) in `x11/cherrypie/tests/`; and 2 runs of the Wayland
binary placing and learning with no X display in
`wayland/cherrypie/tests/`. The X11 tests need `xorg-server-xvfb`,
`xorg-server-xephyr` and `xorg-xrandr`.

## License

MIT
