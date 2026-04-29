# cherrypie

"Diane, if you ever get up this way, that cherry pie is worth a stop."

Window matching daemon for Linux written in Rust. Automatically positions, sizes, and configures windows based on TOML rules with regex matching.

Replaces devilspie and devilspie2 with a simpler, faster alternative: TOML config instead of Lua scripting, regex matching instead of string comparison, hot reload via inotify, and sub-frame rule application via `_NET_CLIENT_LIST` diffing.

## Features

- Match windows by WM_CLASS, title, role, process name, or window type
- Regex patterns on all matchers (case-insensitive, anchored, etc.)
- Position windows: absolute coordinates, named anchors (`center`, `top-right`), or percentages
- Size windows: absolute pixels or percentage of monitor
- Target specific monitors by name or index
- EWMH actions: maximize, fullscreen, pin (sticky), minimize, shade, above/below, focus, opacity, decoration toggle
- Workspace assignment
- Hot config reload on save (inotify `IN_CLOSE_WRITE`)
- X11 via x11rb (pure Rust)
- RandR monitor detection
- Handles reparenting WMs (AwesomeWM, i3, etc.) via `_NET_CLIENT_LIST` diffing
- Applies rules to existing windows on startup
- poll(2) event loop with signalfd for clean shutdown
- 1.8 MB binary, 1 MB resident memory

## Build

Requires Rust 2024 edition (rustc 1.85+).

```
CARGO_TARGET_DIR=/tmp/cherrypie-build cargo build --release
```

Binary: `/tmp/cherrypie-build/release/cherrypie`

## Install

```
./install.py              # Build, install to ~/.local/bin, install systemd service
./install.py status       # Show installation status
./install.py update       # Rebuild if source changed, reinstall
./install.py uninstall    # Remove binary and service, preserve config
./install.py enable       # Enable and start systemd user service
./install.py disable      # Disable and stop service
```

## Usage

```
cherrypie                         # Run with default config
cherrypie --config /path/to.toml  # Custom config path
cherrypie --dry-run               # Log matches without applying actions
cherrypie --version               # Print version
```

Default config location: `~/.config/cherrypie/config.toml`

## Configuration

Rules are TOML tables. Each rule has matchers (at least one required) and actions.

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
| `workspace` | integer | Move to workspace (0-indexed) |
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
src/
  main.rs       Hand-rolled CLI (--config, --dry-run, --version, --help)
  daemon.rs     poll(2) event loop: signalfd + inotify + X11 fd
  config.rs     TOML parsing with serde untagged enums for flexible value types
  rules.rs      Rule compilation: regex, position/size/monitor resolution
  backend/
    mod.rs      Backend enum dispatch (feature-gated)
    x11.rs      X11 via x11rb: atom_manager, _NET_CLIENT_LIST diffing, RandR, EWMH
```

Event flow: X11 PropertyNotify on root window signals `_NET_CLIENT_LIST` change. cherrypie diffs against the previous list, identifies new window IDs, queries their properties (class, title, role, process, type), matches against compiled rules, and applies actions via `configure_window` and EWMH ClientMessage events. Flush. One poll wake per batch of changes.

Config reload: inotify watches the config directory for `IN_CLOSE_WRITE`. On trigger, TOML is re-parsed and rules re-compiled. No restart needed.

## Dependencies

Runtime: none (statically links everything except libc).

Build:
- x11rb 0.13 (pure Rust X11 protocol, RandR extension)
- serde + toml 0.8
- regex 1
- libc 0.2

## Tests

```
CARGO_TARGET_DIR=/tmp/cherrypie-build cargo test --release
```

43 tests: 21 config parsing, 22 rule compilation and matching. Tests live in `tests/` (external test crates).

## License

MIT
