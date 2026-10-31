# ABRAXAS

A daemon that smoothly adjusts your screen's color temperature throughout the day based on the sun's position at your location. Part of DEMIURGE: Each of its two stacks runs a binary of its own over this crate.

**Key Features:**

### Solar Grayline Engine
- **Worldwide Usage**: Offline sunrise/sunset from Jean Meeus algorithms based on any latitude/longitude
- **Sigmoid Transitions**: Normalized sigmoid over 90-min dawn and 180-min dusk windows with indoor-aware offsets (dawn midpoint 30 min after sunrise, dusk midpoint 30 min before sunset, k=8)
- **Endpoint Normalization**: Exact [0, 1] output over [-1, 1] domain -- no residual drift at target temperatures
- **Weather Awareness (US, optional)**: NOAA api.weather.gov cloud cover shifts daytime target (6500K clear, 4500K overcast). See [Build & Install](#build--install) for international builds
- **Manual Override**: `--set TEMP MINUTES` with the same sigmoid curve, auto-resumes at next transition window

### Gamma Control (gamma module)
- **One Backend per Stack**: The X11 binary sets gamma through RandR, the Wayland binary through wlr-gamma-control; each follows monitors plugged and pulled
- **Blackbody Ramp**: Planckian locus approximation, 1000K-25000K

### Daemon Reliability
- **PID File Liveness**: Daemon writes PID on start, CLI commands check liveness before reporting success
- **Instant Startup**: Gamma applied before weather init -- screen is correct on first frame
- **io_uring Event Loop**: Raw io_uring syscalls. 1 `io_uring_enter` per 60s tick via multi-shot `IORING_OP_POLL_ADD` (`IORING_POLL_ADD_MULTI`) + `IORING_OP_TIMEOUT`. Polls persist across CQE deliveries via `IORING_CQE_F_MORE` liveness tracking -- no re-registration each tick. Atomic `uint32_t` event bitmask, unified `process_cqe` handler for both main and cancel drains. Weather fetches are non-blocking via `POLL_ADD` on the curl child's stdout pipe -- zero event loop stalls. Requires kernel >= 5.13
- **inotify**: Config file hot-reload via IN_CLOSE_WRITE (no spurious partial-write triggers)
- **signalfd**: Clean SIGTERM/SIGINT shutdown
- **seccomp-bpf**: 91 whitelisted syscalls, KILL_PROCESS on violation. Raw BPF, no libseccomp
- **landlock**: Filesystem sandboxed to config dir, /dev, /proc, /usr, /etc, /lib, /tmp. Raw syscalls, no libc wrappers
- **prctl hardening**: 1ns timer slack, no-new-privs, non-dumpable
- **Temperature Logging**: Every tick logs current mode, temperature, sun position, and cloud cover to stderr
- **Zero Polling**: CPU usage ~180ms over 3 hours

### Build
- **Zero Warnings**: Compiles warning-free
- **Weather via curl(1)**: `Command::new("curl")`. No HTTP crate dependency.

---

## Modernizing redshift + redshift-scheduler

| Feature | redshift | redshift-scheduler | ABRAXAS |
|---------|----------|--------------------|---------|
| Set gamma ramp | Yes | No | Yes |
| Automatic day/night transitions | Manual via `-t` | Cron-based | Built-in sigmoid transitions |
| Sun position calculation | Via geoclue/manual | External | Built-in NOAA ephemeris |
| Weather awareness | No | No | Yes (NOAA api.weather.gov) |
| Smooth transitions | Step function (instant) | N/A | Front-loaded sigmoid over 90-180 min windows |
| Manual override with gradient | No (`-O` is instant) | No | `--set TEMP MINUTES` |
| Cloud cover dark mode | No | No | Yes (60% threshold) |
| Wayland native | No | No | Yes (wlr-gamma-control) |
| NVIDIA support | Via X11 | N/A | X11/RandR, or demiurge-wl's wlr-gamma-control |
| Kernel sandbox | No | No | seccomp-bpf + landlock |
| Dependencies | geoclue, X11 | cron, redshift | libc; x11rb or wayland-client, its stack's |
| Scheduler needed | Yes | Is the scheduler | No |

## Implementation

|                  |                    |
|------------------|--------------------|
| Compiler         | rustc 1.75+        |
| Memory model     | ownership/borrow   |
| Gamma: Wayland   | wayland-client     |
| Gamma: X11       | x11rb (pure Rust)  |
| Weather          | Command::new curl(1) |
| Config parse     | serde_json         |
| Event loop       | io_uring (raw syscall) |
| Sandbox          | seccomp + landlock |
| Hardening        | prctl              |
| LTO              | opt-level=z, lto=true, strip, panic=abort (workspace) |
| Heap allocs/tick | 0                  |
| Binaries         | `abraxas-x11`, `abraxas-wl` (cargo packages), installed as `abraxas` |
| Default features | noaa               |

### Measured Performance

|                     | Rust (glibc)  |
|---------------------|---------------|
| Binary size         | ~604 KB       |
| Linking             | dynamic       |
| Shared libs (ldd)   | 4             |
| Startup (--help)    | 0.9 ms        |
| --status            | 0.9 ms        |
| --set               | 0.8 ms        |
| Syscalls/tick       | 1             |
| Daemon threads      | 1             |
| CPU / hour          | < 1s          |
| Weather             | async (multi-shot POLL_ADD) |

ABRAXAS shipped through v0.7.0 with parallel C23 and Rust implementations; v0.8.0 retired the C23 path after a feature-parity audit confirmed the Rust side covered every subsystem (CLI, config, gamma backends, seccomp allowlist, weather, solar). The workspace is now monoculture-Rust.

## Architecture

```
core/abraxas/       Everything of ABRAXAS that is not one stack's
  src/              cli, daemon, config, solar, sigmoid, weather, zipdb,
                    uring, seccomp, landlock, gamma (the backend trait)
  tests/            Config tests
  us_zipcodes.bin   ZIP code database
x11/abraxas/        The X11 binary: gamma through RandR
wayland/abraxas/    The Wayland binary: gamma through wlr-gamma-control
contrib/systemd/abraxas@.service   The user unit, one instance per stack
```

```
abraxas (one binary per DEMIURGE stack, over core/abraxas)
    |
    +-- NOAA sun ephemeris (offline calculation)
    +-- Weather from api.weather.gov (Command::new curl, async via POLL_ADD)
    +-- Sigmoid transition engine
    +-- Config via serde_json
    +-- io_uring event loop (raw syscalls)
    +-- seccomp-bpf filter (91 whitelisted syscalls)
    +-- landlock filesystem sandbox (raw syscalls)
    +-- prctl hardening (timerslack, no_new_privs, !dumpable)
    |
    +-- gamma backend, the stack's
            |
            +-- X11 binary: x11rb RandR (pure Rust X11 protocol)
            +-- Wayland binary: wayland-client + wayland-protocols-wlr
```

## Sigmoid Transitions

All temperature transitions use a normalized sigmoid function:

```
            1
f(x) = -----------        normalized to exactly [0, 1] over x in [-1, 1]
        1 + e^(-kx)
```

A sigmoid changes slowly at the edges and quickly through the middle. This matches human color perception -- the transition is imperceptible at both ends, with ~80% of the shift happening through the middle third while ambient light is already changing.

A linear ramp has constant rate, which produces visible start and stop artifacts. A step function (the traditional approach) is a single-frame jump.

```
  Temp (K)
  6500 |            ___________
       |           /                          <- sigmoid (dusk)
       |          /
       |        |
  4700 |       |
       |      /
       |    /
       |___/
  2900 |_________________________________________
       sunset-90   sunset    sunset+90     Time

  6500 |__________
       |          |                        <- step function
       |          |
       |          |
  4700 |          |
       |          |
       |          |_________________________
  2900 |_________________________________________
       sunset-90   sunset    sunset+90     Time
```

Both transitions are offset to account for indoor usage -- you don't get direct sunlight the moment the sun crosses the horizon.

**Dusk** (canonical): 180-minute window, front-loaded 30 minutes before sunset (runs from sunset-120min to sunset+60min). The sigmoid midpoint hits at sunset-30min, so the steep k=8 drop happens during golden hour -- by sunset you're already at ~3200K.

**Dawn** (inverse): 90-minute window, back-loaded 30 minutes after sunrise (runs from sunrise-15min to sunrise+75min). The sigmoid midpoint hits at sunrise+30min, so the screen stays warm through early morning when indoor light is still dim. At sunrise you're only ~15% through the transition (~3400K); full daylight temperature arrives ~75 minutes after sunrise.

**Manual overrides** use the same curve. `abraxas --set 3500 30` maps [0, 30 min] to [-1, 1] and interpolates between current temperature and 3500K.

When `--set TEMP MINUTES` is called, the daemon enters manual mode:

1. The sigmoid begins transitioning from current temperature to the target over the specified duration
2. A manual flag is set -- the daemon stops evaluating solar/weather conditions
3. After reaching the target, the daemon holds until the next natural transition point (e.g., 15 minutes before the next sunrise)
4. At that point, the flag clears and solar-based control resumes

The manual call communicates with the running daemon via a control file (watched by inotify via IN_CLOSE_WRITE). The daemon itself drives the gamma -- no second process. If the daemon is not running, the CLI warns the user that the override was saved but won't apply until the daemon starts.

**Endpoint normalization.** A raw sigmoid with k=6 outputs 0.0025 at x=-1 and 0.9975 at x=+1. Over a 3600K range that's 9K of drift at the boundaries. The output is normalized to hit exact 0.0 and 1.0 at window edges -- no residual error at target temperatures.

## Weather Awareness

Every 15 minutes, ABRAXAS fetches the hourly forecast from `api.weather.gov` (NOAA, US only). If cloud cover exceeds 60%, daytime temperature drops from 6500K to 4500K ("dark mode"). This prevents eye strain on overcast days when the ambient light is already dim.

The weather API requires no API key. Rate limits are generous (per User-Agent). curl(1) is invoked via `Command::new` for HTTP requests, with non-blocking I/O -- the curl child's stdout pipe is polled via io_uring `POLL_ADD`, so weather fetches never stall the event loop. No HTTP library dependency.

## Installation

### Dependencies

```bash
# Rust: cargo
pacman -S rustup && rustup default stable

# Optional: curl binary (weather -- usually pre-installed)
pacman -S curl
```

### Build & Install

ABRAXAS installs with either DEMIURGE stack, unasked, from the
repository's root:

```bash
x11/install.py                          # The X11 stack, ABRAXAS among it
wayland/install.py                      # The Wayland stack
<stack>/install.py --non-usa            # International build (no NOAA)
<stack>/install.py --no-abraxas         # Leave ABRAXAS out

# Or by hand, one stack's binary:
cargo build --release -p abraxas-x11    # or -p abraxas-wl
cargo build --release -p abraxas-wl --no-default-features   # without NOAA
```

### Setup

```bash
# Set location (US ZIP code or lat,lon)
abraxas --set-location 60614
abraxas --set-location 41.88,-87.63

# International users: use lat,lon directly
abraxas --set-location 51.51,-0.13     # London
abraxas --set-location 48.86,2.35      # Paris

# Verify
abraxas --status
```

### Autostart

The installer enables the user unit's instance for its stack:
`abraxas@x11.service` runs `/usr/local/lib/demiurge/x11/bin/abraxas` and
is wanted by `demiurge-x11.target`, `abraxas@wayland.service` the Wayland
binary under `demiurge-wayland.target`. Each starts once its session
hands the user manager its display and stops with it, so ABRAXAS always
runs on the session's own display through that stack's backend.

### Migrating from redshift

```bash
systemctl --user disable --now redshift.service redshift-scheduler.service 2>/dev/null
```

No config migration needed. ABRAXAS calculates everything from your location.

## Usage

```
abraxas                       Run daemon (foreground)
abraxas --daemon              Run daemon (explicit)
abraxas --status              Show sun position, weather, current temperature
abraxas --set TEMP [MINUTES]  Transition to TEMP over MINUTES (default 3)
abraxas --resume              Clear manual override, resume solar control
abraxas --set-location LOC    Set location (ZIP code or LAT,LON)
abraxas --refresh             Force weather refresh from NOAA
abraxas --reset               Reset screen to default gamma and exit
```

### Examples

```bash
# Gradually shift to night mode over 30 minutes
abraxas --set 2900 30

# Quick warm shift for a movie
abraxas --set 3500 5

# Back to solar control
abraxas --resume

# Check what's happening
abraxas --status
```

## Configuration

All config lives in `~/.config/abraxas/`:

| File | Purpose |
|------|---------|
| `config.toml` | Location (`[location]` latitude/longitude) |
| `weather_cache.json` | Cached NOAA forecast |
| `override.json` | Manual override state (daemon-managed) |
| `daemon.pid` | PID file for liveness checks |
| `us_zipcodes.bin` | ZIP code database (33k entries, 429 KB) |

The first run writes `config.toml` itself when none exists. The location
comes from the system timezone ($TZ, else the `/etc/localtime` link) looked
up in `/usr/share/zoneinfo/zone1970.tab`, which gives the zone's reference
city: For `America/Los_Angeles` that is Los Angeles. Edit the coordinates or
run `--set-location` to correct it; the daemon reloads the file on write. A
leftover `config.ini` is migrated once and removed. A `config.toml` that
does not parse is never overwritten: The daemon exits with status 78
(EX_CONFIG), which `abraxas@.service` excludes from its restart policy.

### Tuning

Edit the constants in `core/abraxas/src/lib.rs` and rebuild:

```rust
pub const TEMP_DAY_CLEAR: i32 = 6500;      // Clear sky daytime temperature (K)
pub const TEMP_DAY_DARK: i32 = 4500;       // Overcast daytime temperature (K)
pub const TEMP_NIGHT: i32 = 2900;          // Night temperature (K)
pub const CLOUD_THRESHOLD: i32 = 60;       // Cloud cover % to trigger dark mode
pub const CLOUD_TRANSITION_MIN: f64 = 10.0; // Cloud transition (minutes)
pub const TEMP_UPDATE_SEC: i64 = 60;
pub const DAWN_DURATION: f64 = 90.0;       // Dawn window (minutes)
pub const DUSK_DURATION: f64 = 180.0;      // Dusk window (minutes)
pub const DAWN_OFFSET: f64 = 30.0;         // Shift sigmoid midpoint this many min after sunrise
pub const DUSK_OFFSET: f64 = 30.0;         // Shift sigmoid midpoint this many min before sunset
pub const SIGMOID_STEEPNESS: f64 = 8.0;    // Higher = sharper mid-transition
pub const WEATHER_REFRESH_SEC: i64 = 900;  // 15 minutes
```

## Platform Support

- **Linux only**. Requires kernel >= 5.13 (io_uring multi-shot poll).
- **DEMIURGE's Wayland stack**: wlr-gamma-control on each of demiurge-wl's CRTCs, NVIDIA included; the Wayland binary also runs on Sway, Hyprland, river, labwc, wayfire and niri
- **DEMIURGE's X11 stack**: RandR, on any X server
- **International**: Solar calculations work worldwide. Use `<stack>/install.py --non-usa` to skip NOAA weather.
- **US locations**: NOAA weather awareness via api.weather.gov (cloud cover adjusts daytime temperature). Enabled by default.

## License

GPLv2
