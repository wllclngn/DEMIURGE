# DEMIURGE

Minimal X11 window manager in Rust, purpose-built to replace AwesomeWM. Named for the Gnostic craftsman-creator who fashions the physical world from the abstract: the WM takes running processes and gives them visible form on screen.

AwesomeWM is ~87K LOC (29K C + 58K Lua); most of it was unused. DEMIURGE does exactly what the user needs -- 3 tags, ~18 keybindings, a top bar, Alt+Tab MRU cycling, click-to-focus, Super+drag move/resize, no titlebars, no decorations, no compositor -- in ~5,500 LOC of Rust including a full screen locker.

## Orchestration

DEMIURGE is two binaries that share the same Cargo package, plus a rendering module they both consume.

| Component | Role |
|---|---|
| `demiurge` | The window manager. Non-reparenting, single-threaded, zero mutexes. TOML config, inotify hot-reload, signalfd shutdown + SIGCHLD reaping, multi-monitor bar. |
| `torrentius` | Rendering subsystem (not a binary). Named for Johannes Torrentius and his camera obscura. Owns all Cairo/Pango primitives, PNG capture, and framed-panel rendering. Used by both binaries. |
| `gordian_knot` | VT + X11 screen locker. Named for the Phrygian knot that only the destined could untie. PAM authentication, privilege separation, seccomp-bpf + landlock sandboxing, idle-watcher daemon, lock-on-suspend hook, inhibit respect via D-Bus. |

## Features

### Window manager

- Non-reparenting (dwm/xmonad/pgwm pattern; no decoration management)
- TOML config with inotify hot-reload (edit during a session, bindings + colors update live)
- Three layouts: floating, tile (master-stack), monocle; per-tag layout state
- MRU Alt+Tab with keyboard grab cycling
- Status bar with subpixel-AA text (Cairo RGB24 + Pango); per-monitor panels
- Run prompt with `$PATH` tab-completion and ghost text
- Super+drag move/resize, CSD `_NET_WM_MOVERESIZE`
- EWMH: dialog/transient auto-float, fullscreen save/restore, initial `_NET_WM_STATE`, `WM_TAKE_FOCUS`, `_NET_WM_ALLOWED_ACTIONS`
- RandR multi-monitor bar
- Screenshot capture via the `screenshot` action -> `~/Pictures/screenshot-YYYYmmdd-HHMMSS.png`
- `SIGCHLD` reaping so spawned apps don't accumulate as zombies in the service cgroup
- `OZONE_PLATFORM=x11` in the service unit so Chromium/Electron children launch cleanly
- `demiurge --check-config` validates an existing config before the installer preserves it across reinstalls

### Screen locker (gordian_knot)

- Real PAM authentication via inline FFI (no pam-sys / pam crate; direct `libpam.so.0` link)
- Privilege separation: setuid-root binary, `setresuid` + `PR_SET_NO_NEW_PRIVS` drop to user before PAM + UI
- seccomp-bpf syscall allowlist (raw BPF, no libseccomp), `SECCOMP_RET_KILL_PROCESS` on anything outside the whitelist
- landlock filesystem sandbox (raw syscalls, no helper crate), read-only over `/etc /usr /lib /lib64 /proc /sys /dev /run /tmp`, `/home` + `/root` explicitly excluded
- X11 in-session lock: fullscreen override-redirect window, exclusive keyboard + pointer grab (with retry), monospace SYSTEM + USER panels rendered via `torrentius::draw_panel`, dot-echo password input
- VT fallback: when X11 grab fails or `$DISPLAY` is unset, acquires a fresh `/dev/ttyN` via `VT_OPENQRY` + `VT_LOCKSWITCH`, text-mode UI on the console
- Idle auto-lock (`--daemon`): XScreenSaver polling, configurable threshold (`idle_timeout_seconds`)
- Inhibit respect: zbus D-Bus watcher on `org.freedesktop.login1.Manager.ListInhibitors`, suppresses auto-lock during video playback and fullscreen games
- Lock before suspend: `gordian_knot-sleep.service` with `Before=sleep.target suspend.target hibernate.target hybrid-sleep.target`
- Signalfd-driven cleanup (no `atexit`): SIGSEGV/SIGBUS/SIGFPE still release the VT

## Build

Requires Rust 2024 edition (rustc 1.85+), cairo, pango, pangocairo, libpam (runtime + headers).

```
CARGO_TARGET_DIR=/tmp/demiurge-build cargo build --release
CARGO_TARGET_DIR=/tmp/demiurge-build cargo test
```

Binaries: `/tmp/demiurge-build/release/{demiurge, gordian_knot}`

## Install

```
./install.py              # Build, install everything (WM + locker + services)
./install.py -y           # Non-interactive: assume yes to all prompts
./install.py status       # Show installation status
./install.py update       # Rebuild and reinstall if source changed
./install.py uninstall    # Remove all installed files (config preserved)
```

Install layout:

| Path | Notes |
|---|---|
| `/usr/local/bin/demiurge` | WM binary (sudo) |
| `/usr/local/bin/gordian_knot` | Locker binary, mode 4755 (setuid-root, sudo) |
| `/etc/pam.d/gordian_knot` | PAM stack: `auth include system-auth` (sudo) |
| `/usr/share/xsessions/demiurge.desktop` | Display-manager entry (sudo) |
| `~/.config/systemd/user/demiurge.service` | WM systemd unit |
| `~/.config/systemd/user/gordian_knot-daemon.service` | Idle-watcher service |
| `~/.config/systemd/user/gordian_knot-sleep.service` | Lock-before-suspend hook |
| `~/.config/demiurge/config.toml` | User config (only written if missing) |

Session launch: the `.desktop` runs `systemctl --user start --wait demiurge.service`, which runs the binary. Everything goes to `journalctl --user -u demiurge`.

On install, if an existing config is present, the installer runs the freshly-built binary with `--check-config` against it. If it fails to parse, the installer offers to back it up to `config.toml.bak` and drop in the shipped default. This is the fix for the "stale config silently kills the session across reinstalls" foot-gun we hit during v0.2.0 deployment.

## Testing in nested X (Xephyr)

```
Xephyr :1 -screen 2560x1440 &
DISPLAY=:1 /tmp/demiurge-build/release/demiurge
```

## Architecture

Single-threaded, zero mutexes in the WM. `x11rb` for X11, Cairo + Pango for rendering. TOML config via serde.

```
src/
  main.rs           WM entry. CLI args (-c, --check-config, -v, -h).
                    signalfd (SIGINT/SIGTERM/SIGCHLD), poll(), inotify.
  config.rs         TOML config: tags, keybindings, bar, startup,
                    [gordian_knot]. ENOENT -> Config::default().
  atoms.rs          EWMH/ICCCM atom table
  wm.rs             State machine: manage/unmanage, focus, tags, layout.
                    Client.unmap_ignore counter for tag-switch handling.
  event.rs          X11 event dispatcher (unmap handler consumes
                    unmap_ignore to distinguish WM self-unmaps from
                    client withdraws).
  keys.rs           Keysym resolution, grabs, dispatch. Actions include
                    spawn, close_window, quit, mru_next/prev,
                    view_tag, view_prev/next_tag, move_to_tag,
                    toggle_above, toggle_fullscreen, toggle_layout,
                    run_prompt, screenshot, lock.
  ewmh.rs           EWMH property setters, allowed actions, WM check
  bar.rs            Multi-monitor status bar. Uses torrentius primitives.
  layout.rs         Floating, tile, monocle
  mouse.rs          Super+drag move/resize, CSD _NET_WM_MOVERESIZE
  mru.rs            Alt+Tab MRU cycling with keyboard grab
  spawn.rs          posix_spawn for startup + keybind commands
  monitor.rs        RandR monitor query
  torrentius.rs     Rendering subsystem. Cairo/Pango primitives,
                    framed-panel renderer, PNG capture.
  lib.rs            Module declarations

  gordian_knot/     Locker subsystem
    mod.rs
    sysinfo.rs      HOSTNAME, KERNEL, DATE, TIME, UPTIME -- libc + /proc
    pam.rs          Inline FFI + safe pam::authenticate(user, password)
    privsep.rs      fork_child + drop_to_real_user (setresuid +
                    PR_SET_NO_NEW_PRIVS) + wait_child
    seccomp.rs      Raw BPF syscall allowlist
    landlock.rs     Raw landlock_* syscalls; read-only fs sandbox
    vt.rs           /dev/console + VT_OPENQRY/VT_LOCKSWITCH + prompt loop
    x11_lock.rs     Fullscreen override-redirect grab + torrentius panels
                    + PAM conversation
    daemon.rs       XScreenSaver idle polling, spawns locker on threshold
    inhibit.rs      zbus watcher on logind ListInhibitors

  bin/
    gordian_knot.rs Dispatcher: --vt, --x11, --daemon, --check-config,
                    --no-sandbox. X11 first, VT fallback.

tests/
  config.rs         Config parsing (16 tests)
  bar.rs            Color parsing, PATH scan, completion (13 tests)
  layout.rs         Layout cycle (3 tests)
  mru.rs            MRU list behavior (9 tests)
  torrentius.rs     Hex parsers, font options, surfaces (9 tests)
  unmap_ignore.rs   Counter invariants, saturation (5 tests)
  gordian_knot.rs   [gordian_knot] config + sysinfo uptime formatter (12 tests)
  xephyr.py         Manual test harness
```

83 tests pass across 9 targets on the current build.

## Config

See `config.default.toml` for the shipped default. Key sections:

```toml
[general]
tags = ["X", "Y", "Z"]
default_layout = "floating"
master_ratio = 0.5

[bar]
height = 30
font = "monospace 9"
bg = "#121212"
fg = "#888888"
clock_format = "%a %b %d %Y   %I:%M:%S %p %Z"
tag_focused_bg = "#4A4881"
tag_focused_fg = "#ffffff"
tag_occupied_fg = "#888888"
tag_empty_fg = "#555555"

[startup]
commands = []

[gordian_knot]
bg = "#121212"
fg = "#c8c8c8"
accent = "#e5a93d"
font = "monospace 12"
idle_timeout_seconds = 600
prompt = "PASSWORD"

[[keybind]]
mods = ["Super"]
key = "Return"
action = "spawn"
args = "kitty"

[[keybind]]
mods = ["Super"]
key = "l"
action = "lock"

[[keybind]]
mods = ["Super"]
key = "Print"
action = "screenshot"
```

Valid actions: `spawn`, `close_window`, `quit`, `mru_next`, `mru_prev`, `view_tag`, `view_prev_tag`, `view_next_tag`, `move_to_tag`, `toggle_above`, `toggle_fullscreen`, `toggle_layout`, `run_prompt`, `screenshot`, `lock`.

Valid modifiers: `Super`, `Alt`, `Control`, `Shift`.

`view_tag` and `move_to_tag` take a 1-indexed numeric `args` string (e.g. `"1"` for the first tag), not the tag name.

## Screenshot

Bind the `screenshot` action to any keybind; pressing it writes `~/Pictures/screenshot-YYYYmmdd-HHMMSS.png`. Uses `XGetImage` via x11rb and Cairo's PNG encoder. No subprocess, no Qt, no external tool.

## gordian_knot usage

```
gordian_knot                 # X11 in-session lock; falls back to VT
gordian_knot --vt            # Force VT lock (text-mode on fresh TTY)
gordian_knot --x11           # Force X11 in-session lock
gordian_knot --daemon        # Idle watcher; spawns locker on threshold
gordian_knot --check-config  # Parse config and exit 0/1
gordian_knot --no-sandbox    # Skip seccomp + landlock (debug only)
```

Services (installed to `~/.config/systemd/user/`):
- `gordian_knot-daemon.service` -- idle watcher, `WantedBy=graphical-session.target`
- `gordian_knot-sleep.service` -- fires before suspend, `Before=sleep.target suspend.target hibernate.target hybrid-sleep.target`

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

## Known limitations / future work

- **Multi-monitor: bar-only.** The bar creates one panel per RandR output, but fullscreen, tile, and monocle layouts still use `monitors[0]`. Invisible on single-monitor machines. Fix is a `client_monitor(client)` helper routed through the affected sites in `wm.rs`, `layout.rs`, `ewmh.rs`.
- **gordian_knot on Wayland.** Phase 5 item. VT path still works on Wayland; X11 in-session lock does not.
- **Alt+Tab visual feedback.** Cycling works but has no overlay preview. Torrentius + Alt+Tab overlay is a future expansion.
- **RandR hot-plug.** `monitor.rs::query()` runs once at init.
- **CSD edge resize directions 0-7.** `_NET_WM_MOVERESIZE` edge cases silently dropped. Affects Chromium-family edge resize.
- **Urgency hints.** No `Client.urgent` field; would need WM_HINTS reads + tag-flash render.
- **Config keys with no effect.** `[general] border_width` and `[general] focus_model` are accepted by the TOML parser but currently have no effect. Click-to-focus is always on; border width is always zero.
- **gordian_knot manual verify surface.** PAM auth, X11 grabs, VT ioctls, privilege drop, seccomp, landlock, daemon loop, and D-Bus inhibit watcher cannot be unit-tested without affecting the test runner's process state. All other logic is covered.

## Style

- No decorative separators in comments or output (no `===`, `---`, `***`)
- Log format: `[HH:MM:SS] [LEVEL]   message`
- snake_case for locals/functions, PascalCase for types
- Comments only where logic isn't self-evident
