#!/usr/bin/env python3
"""
demiurge-wl on a real screen, in the QEMU guest of tests/qemu/guest.py:
virtio-gpu with two heads drives demiurge-wl-tty through DRM, keys reach
it through QMP and the guest's libinput, and QMP's screendump reads back
what a head shows.

  - the protocol suite (wayland/tests/smoke.rs, policy.rs and gamma.rs)
    against demiurge-wl-tty
  - the bar draws over the windows, a tiled client fills the rest
  - a client's GBM buffer, handed over through linux-dmabuf, is what
    reaches the screen; a fullscreen one, told the primary plane's
    formats, goes to the screen as it is, with no copy
  - such a window described as PQ over BT.2020 is asked HDR10 for, and
    the monitor, which does not take it, stays SDR
  - a window locking the pointer for mouse-look has it locked, and the
    mouse's motion reaches it as relative motion while the pointer stays;
    one confining the pointer keeps it inside, the mouse pushing it out
  - a window's small buffer, scaled through its viewport, fills its tile
  - explicit sync: A GPU buffer is held until its acquire point is
    signalled, and released through its release point once replaced
  - a window mapping under a pointer that does not move is entered
  - the pointer enters the window under it, where the compositor draws a
    cursor, and a click focuses the window it lands on
  - a menu opens in front of its window, and a click outside closes it
  - the lock covers the screen and takes the keyboard: A key typed there
    reaches the locker alone, the quit bind does nothing, and the unlock
    gives the keyboard back to the window
  - an X11 window, through the compositor's XWayland, tiles beside a
    Wayland one and takes the keyboard; one asking for fullscreen before
    it maps fills the screen
  - an X11 game's 800x600 mode, set through RandR, is emulated by
    XWayland on its surface through a viewport, filling the screen while
    the output keeps its mode
  - the clipboard crosses from one Wayland client to another, and from
    X11 to Wayland and back
  - a monitor swapped for a smaller one is lit again at its new mode, and
    one unplugged and plugged back in is dropped and lit again, the head
    resized and unplugged through QEMU's VNC server
  - a second monitor, plugged into the second head, is lit beside the
    first under a bar of its own, takes the pointer and a window of its
    own, and is dropped when unplugged
  - quark's research runtime runs its windowing probe in demiurge-wl-tty:
    Its window drawn, keys, clicks and a wheel notch arriving as the
    window messages Windows sends, keys waking a thread asleep in
    GetMessage
  - Ctrl+Alt+F<n> leaves the session for a text VT, and switching back
    brings it back drawn
  - the session hands the user manager its display and starts what
    graphical-session.target wants on it; SIGTERM ends the session, the
    services with it, and takes the display back

Usage:
  tests/qemu/wayland_suite.py [-k NAME] [--tty PATH]
      after `tests/qemu/guest.py provision`. --tty runs the suite against
      another demiurge-wl-tty build, such as one from before a fix.
"""

import argparse
import base64
import hashlib
import json
import os
import re
import struct
import subprocess
import sys
import time
import zlib
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import guest  # noqa: E402
from guest import BUILD_DIR, CACHE, REPO, Guest, log  # noqa: E402

RUNTIME = "/tmp/wl-run"
SOCKET = f"{RUNTIME}/wayland-1"
SESSION = f"XDG_RUNTIME_DIR={RUNTIME} LIBSEAT_BACKEND=seatd"
ENV = f"{SESSION} DEMIURGE_WL_NO_XWAYLAND=1"
SHOT = CACHE / "head0.ppm"

BAR = (255, 0, 0)
GREEN = (0, 255, 0)
BLUE = (0, 0, 255)
MAGENTA = (255, 0, 255)
CYAN = (0, 255, 255)
BLACK = (0, 0, 0)

# quark's runtime, quark-wayland, a sibling project in the monorepo, and the
# checks its windowing probe makes of quark's own headless test compositor
# (its output's size, make and name), which demiurge-wl-tty is not.
QUARK = REPO.parent / "quark"
QUARK_HARNESS_ONLY = ("1280x720", "(make 'quark')", "'Headless-1'")
BAR_HEIGHT = 30
CONFIG = f"""
[general]
default_layout = "tile"
tags = ["1", "2", "3"]

[bar]
height = {BAR_HEIGHT}
bg = "#ff0000"
notification_fg = "#00ffff"

[[keybind]]
mods = ["Alt", "Shift"]
key = "F4"
action = "quit"

[[keybind]]
mods = ["Super"]
key = "r"
action = "run_prompt"

[[keybind]]
mods = ["Super"]
key = "F1"
action = "volume_up"

[[keybind]]
mods = ["Super"]
key = "m"
action = "minimize"

[[keybind]]
mods = ["Super"]
key = "s"
action = "screenshot"

[[keybind]]
mods = ["Alt"]
key = "Tab"
action = "mru_next"
"""


def wait_for(pred, timeout: float, step: float = 0.5) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if pred():
            return True
        time.sleep(step)
    return pred()


# The protocol tests (wayland/tests/<name>.rs) run against demiurge-wl-tty.
PROTOCOL = ("smoke", "policy", "gamma")


def build() -> tuple[Path, Path, Path, dict[str, Path]]:
    env = dict(os.environ, CARGO_TARGET_DIR=str(BUILD_DIR))
    subprocess.run(["cargo", "build", "--release", "-p", "demiurge-wl", "--bin", "demiurge-wl-tty",
                    "--example", "wl_probe", "--example", "x11_probe"], cwd=REPO, env=env, check=True)
    subprocess.run(["cargo", "build", "--release", "-p", "cherrypie-wl", "-p", "medusa-wl", "-p", "gnosis-wl"],
                   cwd=REPO, env=env, check=True)
    subprocess.run(["cargo", "build", "--release", "-p", "gnosis-core", "--example", "fake_udisks"],
                   cwd=REPO, env=env, check=True)
    targets = [arg for name in PROTOCOL for arg in ("--test", name)]
    out = subprocess.run(["cargo", "test", "--release", "-p", "demiurge-wl", *targets, "--no-run",
                          "--message-format=json"], cwd=REPO, env=env, check=True,
                         stdout=subprocess.PIPE, text=True).stdout
    protocol = {}
    for line in out.splitlines():
        msg = json.loads(line)
        if msg.get("reason") == "compiler-artifact" and msg["target"]["name"] in PROTOCOL and msg.get("executable"):
            protocol[msg["target"]["name"]] = Path(msg["executable"])
    missing = [name for name in PROTOCOL if name not in protocol]
    if missing:
        raise RuntimeError(f"cargo built no {', '.join(missing)} test binary")
    examples = BUILD_DIR / "release" / "examples"
    return BUILD_DIR / "release" / "demiurge-wl-tty", examples / "wl_probe", examples / "x11_probe", protocol


def build_quark() -> tuple[Path, Path]:
    """quark's runtime as it stands, into a target directory of the suite's
    own, and its windowing probe, win_surface.c, with the flags quark's
    tests/rust/wayland/probes.rs builds a probe with."""
    target = BUILD_DIR / "quark"
    env = dict(os.environ, CARGO_TARGET_DIR=str(target))
    subprocess.run(["cargo", "build", "--release", "-p", "quark-wayland"], cwd=QUARK, env=env, check=True)
    exe = target / "win_surface.exe"
    subprocess.run(["x86_64-w64-mingw32-gcc", "-O2", "-nostdlib", "-fno-stack-protector", "-e", "probe_main",
                    "-o", str(exe), str(QUARK / "tests" / "research" / "wayland" / "win_surface.c"),
                    "-lkernel32", "-lwinmm", "-ladvapi32", "-lsynchronization", "-luser32", "-lgdi32", "-lole32",
                    "-loleaut32", "-lshlwapi", "-lopengl32", "-lshell32", "-lws2_32", "-lntdll", "-lsetupapi",
                    "-lversion", "-lhid", "-ldwmapi", "-limm32", "-ldinput8", "-ldxguid", "-lwinhttp"], check=True)
    return target / "release" / "quark-wayland", exe


class Session:
    """demiurge-wl-tty and its clients in the guest, run as the ssh user
    with the seat from seatd."""

    def __init__(self, g: Guest):
        self.g = g

    def sh(self, cmd: str, timeout: float = 60) -> subprocess.CompletedProcess:
        try:
            return self.g.ssh(cmd, check=False, timeout=timeout, capture=True)
        except subprocess.TimeoutExpired:
            return subprocess.CompletedProcess(cmd, 124, "", "timeout")

    def start(self, xwayland: bool = False, config: str = CONFIG) -> bool:
        self.stop()
        self.sh(f"mkdir -p -m 0700 {RUNTIME} && mkdir -p ~/.config/demiurge"
                f" && cat > ~/.config/demiurge/config.toml << 'EOF'\n{config}\nEOF")
        self.sh(f"{SESSION if xwayland else ENV} setsid -f demiurge-wl-tty > /tmp/wl.log 2>&1 < /dev/null")
        if not wait_for(lambda: self.sh(f"test -S {SOCKET}").returncode == 0, 20):
            return False
        return not xwayland or wait_for(lambda: self.x11_display() is not None, 20)

    # The compositor's XWayland, once it is ready: ":N".
    def x11_display(self) -> str | None:
        m = re.search(r"display_number=(\d+)", self.sh("cat /tmp/wl.log").stdout)
        return f":{m.group(1)}" if m else None

    def x11_client(self, color: str, log: str = "/tmp/x11.log", kind: str = "color") -> None:
        self.sh(f"DISPLAY={self.x11_display()} setsid -f x11-probe {kind} {color} > {log} 2>&1 < /dev/null")

    def client(self, color: str, log: str = "/tmp/probe.log", kind: str = "color") -> None:
        self.sh(f"{ENV} WAYLAND_DISPLAY=wayland-1 setsid -f wl-probe {kind} {color} > {log} 2>&1 < /dev/null")

    def gpu_client(self, color: str, kind: str = "dmabuf") -> None:
        # The guest's GPU is virtio-gpu without 3D, whose software GBM
        # allocates only on the card node, which needs root.
        self.sh(f"sudo -n env WAYLAND_DISPLAY={SOCKET} setsid -f wl-probe {kind} {color} /dev/dri/card0"
                " > /tmp/probe.log 2>&1 < /dev/null")

    def probe_log(self, log: str = "/tmp/probe.log") -> str:
        return self.sh(f"cat {log}").stdout

    def stop(self) -> None:
        self.sh("sudo -n pkill -x wl-probe; pkill -x x11-probe; pkill -x quark-wayland; pkill -x xclip; "
                "pkill -x wl-copy; pkill -x medusa-wl; pkill -x gnosis-wl; pkill -x fake-udisks; pkill -x demiurge-wl-tty; "
                "for i in $(seq 50); do pgrep -x demiurge-wl-tty >/dev/null || break; sleep 0.1; done")

    def log(self) -> str:
        return self.sh("tail -n 20 /tmp/wl.log").stdout

    def running(self) -> bool:
        return self.sh("pgrep -x demiurge-wl-tty").returncode == 0

    def active_vt(self) -> str:
        return self.sh("cat /sys/class/tty/tty0/active").stdout.strip()


def pixel(shot: tuple[int, int, bytes], x: int, y: int) -> tuple[int, int, int]:
    w, _, data = shot
    i = (y * w + x) * 3
    return tuple(data[i:i + 3])


def drawn(g: Guest, client: tuple[int, int, int] = GREEN) -> str | None:
    """None if head 0 shows the bar on top and the client tiled below it,
    else what it shows instead. The center is left out: The pointer starts
    there, under its cursor."""
    shot = g.qmp.screendump(SHOT)
    w, h, _ = shot
    want = {(w // 2, 2): BAR, (w // 2, BAR_HEIGHT - 1): BAR, (w // 2, BAR_HEIGHT + 1): client,
            (w // 3, h * 2 // 3): client, (w - 2, h - 2): client}
    wrong = {p: pixel(shot, *p) for p, c in want.items() if pixel(shot, *p) != c}
    return None if not wrong else f"{w}x{h} head shows {wrong} where it wants {want}"


def test_protocol_suite(g: Guest, s: Session) -> str | None:
    s.stop()
    failed = []
    for name in PROTOCOL:
        r = s.sh(f"{ENV} DEMIURGE_WL_TEST_BIN=/usr/local/bin/demiurge-wl-tty DEMIURGE_WL_TEST_GPU=1"
                 f" /tmp/wl-{name} --test-threads 1", timeout=600)
        summary = [l for l in r.stdout.splitlines() if l.startswith(("test ", "test result"))]
        for line in summary:
            log("INFO", f"  {name}: {line}")
        if r.returncode != 0:
            failed += [f"{name}: {l}" for l in r.stdout.splitlines() if "panicked" in l or "FAILED" in l]
    if failed:
        return "the protocol suite failed on the real screen:\n" + "\n".join(failed)
    return None


def test_the_bar_draws_over_a_tiled_client(g: Guest, s: Session) -> str | None:
    if not s.start():
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    s.client("00ff00")
    if not wait_for(lambda: drawn(g) is None, 10):
        return f"{drawn(g)}\n{s.log()}"
    return None


def test_a_gpu_buffer_reaches_the_screen(g: Guest, s: Session) -> str | None:
    if not s.start():
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    s.gpu_client("0000ff")
    if not wait_for(lambda: drawn(g, BLUE) is None, 10):
        return f"{drawn(g, BLUE)}\nprobe: {s.probe_log()}\n{s.log()}"
    return None


def test_a_fullscreen_gpu_window_goes_straight_to_the_screen(g: Guest, s: Session) -> str | None:
    if not s.start():
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    s.gpu_client("0000ff", kind="scanout")
    if not wait_for(lambda: "feedback scanout" in s.probe_log(), 10):
        return f"the fullscreen window was never told the primary plane's formats:\n{s.probe_log()}\n{s.log()}"
    # The pointer onto the game, which hides the cursor over it. Off the
    # center, where the pointer test expects the tablet's first move.
    g.qmp.point(0.75, 0.75)
    if not wait_for(lambda: "pointer enter" in s.probe_log(), 5):
        return f"the pointer never entered the window:\n{s.probe_log()}"
    if not wait_for(lambda: "presented zero-copy" in s.probe_log(), 10):
        return f"the window's buffer never went to the screen as it is:\n{s.probe_log()}\n{s.log()}"
    w, h, _ = g.qmp.screendump(SHOT)
    covered = [(w // 2, 2), (w // 3, h * 2 // 3), (w - 2, h - 2)]
    shown = [pixel(g.qmp.screendump(SHOT), *p) for p in covered]
    if any(p != BLUE for p in shown):
        return f"the screen does not show the window's buffer: {shown}"
    return None


def test_an_hdr_window_on_an_sdr_monitor_stays_sdr(g: Guest, s: Session) -> str | None:
    """A fullscreen window described as PQ over BT.2020, its buffer going to
    the screen as it is, is what HDR10 is asked for. virtio-gpu's monitor
    does not take HDR10, so it stays SDR and the log says why."""
    if not s.start():
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    s.gpu_client("0000ff", kind="hdr")
    if not wait_for(lambda: "hdr description ready" in s.probe_log(), 10):
        return f"the HDR description was never made:\n{s.probe_log()}\n{s.log()}"
    # The pointer onto the window, which hides the cursor over it: A spot
    # of its own, as the tablet sends nothing for a move to where the last
    # test left it.
    g.qmp.point(0.6, 0.6)
    if not wait_for(lambda: "pointer enter" in s.probe_log(), 5):
        return f"the pointer never entered the window:\n{s.probe_log()}"
    if not wait_for(lambda: "presented zero-copy" in s.probe_log(), 10):
        return f"the window's buffer never went to the screen as it is:\n{s.probe_log()}\n{s.log()}"

    def wl_log() -> str:
        return re.sub(r"\x1b\[[0-9;]*m", "", s.sh("cat /tmp/wl.log").stdout)

    if "surface image description: tf=st2084_pq primaries=bt2020" not in wl_log():
        return f"the window's description never reached the compositor:\n{s.log()}"
    if not wait_for(lambda: "HDR10 not shown" in wl_log(), 5):
        return f"HDR10 was never asked for the window going straight to the screen:\n{s.log()}"
    lit = [l for l in wl_log().splitlines() if "monitor lit" in l]
    if not lit or "hdr10=false" not in lit[0]:
        return f"the monitor was not read as SDR: {lit}"
    if "HDR10 on" in wl_log():
        return f"an SDR monitor was put in HDR10:\n{s.log()}"
    log("INFO", "  " + next(l for l in wl_log().splitlines() if "HDR10 not shown" in l).split("INFO")[-1].strip())
    return None


def test_mouse_look_locks_the_pointer_and_hears_the_mouse(g: Guest, s: Session) -> str | None:
    """A window locking the pointer, as a game in mouse-look does, has it
    locked once the pointer enters it. The mouse's motion then reaches it
    as relative motion while the pointer itself stays where it was."""
    if not s.start():
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    s.client("00ff00", kind="look")
    if not wait_for(lambda: drawn(g) is None, 10):
        return f"{drawn(g)}\n{s.log()}"
    g.qmp.point(0.4, 0.6)
    if not wait_for(lambda: "pointer locked" in s.probe_log(), 5):
        return f"the lock never took hold:\n{s.probe_log()}\n{s.log()}"
    g.qmp.move(40, -25)
    if not wait_for(lambda: "relative " in s.probe_log(), 5):
        return f"the mouse's motion never reached the window as relative motion:\n{s.probe_log()}"
    after = s.probe_log().split("pointer locked", 1)[1].splitlines()
    rel = [tuple(map(int, l.split()[1:3])) for l in after if l.startswith("relative ")]
    dx, dy = sum(r[0] for r in rel), sum(r[1] for r in rel)
    if not (dx > 0 and dy < 0):
        return f"the relative motion went ({dx}, {dy}), not right and up"
    moved = [l for l in after if l.startswith("pointer motion")]
    if moved:
        return f"the locked pointer moved: {moved}"
    return None


def test_a_confined_pointer_stays_in_its_window(g: Guest, s: Session) -> str | None:
    """A window confining the pointer to itself has it confined once the
    pointer enters it, and the mouse cannot take it out, past the bar
    above the window."""
    if not s.start():
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    s.client("00ff00", kind="confine")
    if not wait_for(lambda: drawn(g) is None, 10):
        return f"{drawn(g)}\n{s.log()}"
    g.qmp.point(0.4, 0.6)
    if not wait_for(lambda: "pointer confined" in s.probe_log(), 5):
        return f"the confinement never took hold:\n{s.probe_log()}\n{s.log()}"
    g.qmp.move(40, 25)
    if not wait_for(lambda: "pointer motion" in s.probe_log().split("pointer confined", 1)[1], 5):
        return f"the confined pointer did not move inside the window:\n{s.probe_log()}"
    for _ in range(5):
        g.qmp.move(0, -300)
    time.sleep(1)
    after = s.probe_log().split("pointer confined", 1)[1].splitlines()
    if "pointer leave" in after:
        return f"the confined pointer left the window:\n{s.probe_log()}"
    ys = [int(l.split()[3]) for l in after if l.startswith("pointer motion")]
    if any(y < 0 for y in ys):
        return f"the confined pointer went above the window: {ys}"
    return None


def test_a_viewport_scales_a_small_buffer_to_the_window(g: Guest, s: Session) -> str | None:
    """A window drawing a 64x48 buffer through wp_viewporter at its tile's
    size fills the tile, as XWayland shows a game's emulated mode."""
    if not s.start():
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    s.client("00ff00", kind="viewport")
    if not wait_for(lambda: "viewport 64x48 to" in s.probe_log(), 10):
        return f"the window never drew through its viewport:\n{s.probe_log()}\n{s.log()}"
    if not wait_for(lambda: drawn(g) is None, 10):
        return f"{drawn(g)}\n{s.probe_log()}"
    return None


def test_explicit_sync_holds_a_frame_until_its_acquire_point(g: Guest, s: Session) -> str | None:
    """A GPU buffer committed with an acquire point not yet signalled is not
    shown until the client signals it; once a second buffer replaces it,
    its release point is signalled."""
    if not s.start():
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    s.gpu_client("00ff00", kind="sync")
    if not wait_for(lambda: "release point signalled" in s.probe_log(), 15):
        return f"explicit sync did not hold and release the frames:\n{s.probe_log()}\n{s.log()}"
    if "held until its acquire point" not in s.probe_log():
        return f"the frame was not held for its acquire point:\n{s.probe_log()}"
    return None


def test_the_pointer_enters_the_window_under_it(g: Guest, s: Session) -> str | None:
    if not s.start():
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    s.client("00ff00")
    if not wait_for(lambda: drawn(g) is None, 10):
        return f"never drawn: {drawn(g)}"
    g.qmp.point(0.5, 0.5)
    if not wait_for(lambda: "pointer enter" in s.probe_log(), 5):
        return f"the client under the pointer saw no pointer:\n{s.probe_log()}\n{s.log()}"
    shot = g.qmp.screendump(SHOT)
    w, h, _ = shot
    cx, cy = w // 2, h // 2
    # The cursor's tip sits on the hotspot, covering pixels just below it.
    tip = [pixel(shot, cx + dx, cy + dy) for dx in range(1, 3) for dy in range(4, 8)]
    if all(p == GREEN for p in tip):
        return f"no cursor drawn at ({cx}, {cy}): {tip}"
    if pixel(shot, cx + 120, cy + 120) != GREEN:
        return f"the window is not drawn around the cursor: {pixel(shot, cx + 120, cy + 120)}"
    m = re.search(r"pointer enter (\d+) (\d+)", s.probe_log())
    local = (int(m.group(1)), int(m.group(2)))
    want = (cx, cy - BAR_HEIGHT)
    if abs(local[0] - want[0]) > 2 or abs(local[1] - want[1]) > 2:
        return f"the pointer entered at {local} in the window, not {want}"
    return None


def test_a_window_mapping_under_a_still_pointer_is_entered(g: Guest, s: Session) -> str | None:
    """A window that maps under the pointer is entered without the mouse
    moving, as a game launching under the cursor must be to take its
    pointer lock."""
    if not s.start():
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    g.qmp.point(0.3, 0.7)
    time.sleep(0.5)
    s.client("00ff00")
    if not wait_for(lambda: drawn(g) is None, 10):
        return f"{drawn(g)}\n{s.log()}"
    if not wait_for(lambda: "pointer enter" in s.probe_log(), 5):
        return f"the window under the still pointer was never entered:\n{s.probe_log()}"
    return None


def test_a_click_focuses_the_window_under_it(g: Guest, s: Session) -> str | None:
    if not s.start():
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    a, b = "/tmp/probe-a.log", "/tmp/probe-b.log"
    s.client("00ff00", a)
    if not wait_for(lambda: "keyboard enter" in s.probe_log(a), 5):
        return f"the first window never got the keyboard:\n{s.probe_log(a)}"
    s.client("0000ff", b)
    if not wait_for(lambda: "keyboard enter" in s.probe_log(b) and "keyboard leave" in s.probe_log(a), 5):
        return f"the second window never took the keyboard:\n{s.probe_log(a)}\n{s.probe_log(b)}"
    # Tiled: The first window is the master on the left.
    g.qmp.point(0.25, 0.5)
    g.qmp.click()
    if not wait_for(lambda: s.probe_log(a).count("keyboard enter") == 2, 5):
        return f"a click on the first window did not focus it:\n{s.probe_log(a)}\n{s.log()}"
    if "pointer button 272 pressed" not in s.probe_log(a):
        return f"the click never reached the window:\n{s.probe_log(a)}"
    return None


def test_a_menu_opens_in_front_and_a_click_outside_closes_it(g: Guest, s: Session) -> str | None:
    if not s.start():
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    s.client("00ff00", kind="menu")
    if not wait_for(lambda: drawn(g) is None, 10):
        return f"never drawn: {drawn(g)}"
    g.qmp.point(0.5, 0.5)
    g.qmp.click()
    if not wait_for(lambda: "popup configured" in s.probe_log(), 5):
        return f"the menu was never configured:\n{s.probe_log()}\n{s.log()}"
    w, h, _ = g.qmp.screendump(SHOT)
    inside = (w // 2 + 60, h // 2 + 45)
    if not wait_for(lambda: pixel(g.qmp.screendump(SHOT), *inside) == MAGENTA, 5):
        return f"the menu is not drawn at {inside}: {pixel(g.qmp.screendump(SHOT), *inside)}\n{s.probe_log()}"
    # A press outside the client, on the bar, dismisses the grabbed menu.
    g.qmp.point(0.5, 0.005)
    g.qmp.click()
    if not wait_for(lambda: "popup done" in s.probe_log(), 5):
        return f"a click outside never dismissed the menu:\n{s.probe_log()}"
    if not wait_for(lambda: pixel(g.qmp.screendump(SHOT), *inside) == GREEN, 5):
        return f"the dismissed menu is still drawn: {pixel(g.qmp.screendump(SHOT), *inside)}"
    return None


def test_the_lock_takes_the_keyboard_and_keybinds_stop(g: Guest, s: Session) -> str | None:
    if not s.start():
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    window, lock = "/tmp/probe-a.log", "/tmp/lock.log"
    s.client("00ff00", window)
    if not wait_for(lambda: "keyboard enter" in s.probe_log(window), 5):
        return f"the window never got the keyboard:\n{s.probe_log(window)}"
    s.client("0000ff", lock, kind="lock")
    if not wait_for(lambda: "lock configured" in s.probe_log(lock), 5):
        return f"the lock never came up:\n{s.probe_log(lock)}\n{s.log()}"
    w, h, _ = g.qmp.screendump(SHOT)
    covered = [(w // 2, 2), (w // 3, h * 2 // 3), (w - 2, h - 2)]
    if not wait_for(lambda: all(pixel(g.qmp.screendump(SHOT), *p) == BLUE for p in covered), 5):
        return f"the lock does not cover the screen: {[pixel(g.qmp.screendump(SHOT), *p) for p in covered]}"

    g.qmp.keys("a")
    if not wait_for(lambda: "key 30 pressed" in s.probe_log(lock), 5):
        return f"a key typed at the lock never reached the locker:\n{s.probe_log(lock)}"
    if "key 30" in s.probe_log(window):
        return f"a key typed at the lock reached the window behind it:\n{s.probe_log(window)}"
    if "keyboard leave" not in s.probe_log(window):
        return f"the window behind the lock kept the keyboard:\n{s.probe_log(window)}"

    g.qmp.keys("alt", "shift", "f4")
    time.sleep(1)
    if not s.running():
        return "the quit bind ended the session from under the lock"

    g.qmp.keys("ret")
    if not wait_for(lambda: "unlocked" in s.probe_log(lock), 5):
        return f"Enter never unlocked:\n{s.probe_log(lock)}"
    if not wait_for(lambda: s.probe_log(window).count("keyboard enter") == 2, 5):
        return f"the keyboard did not come back to the window after the unlock:\n{s.probe_log(window)}"
    if not wait_for(lambda: drawn(g) is None, 5):
        return f"the desktop did not come back after the unlock: {drawn(g)}"
    return None


def typed(g: Guest, text: str) -> None:
    names = {" ": "spc", "/": "slash", "-": "minus"}
    for c in text:
        g.qmp.keys(names.get(c, c))


def bar_left(g: Guest) -> list[tuple[int, int, int]]:
    """The left half of the bar's middle rows: The tag strip and whatever
    is drawn after it, away from the clock."""
    shot = g.qmp.screendump(SHOT)
    return [pixel(shot, x, y) for y in range(8, BAR_HEIGHT - 8) for x in range(shot[0] // 2)]


def test_super_r_runs_what_is_typed_into_the_bar(g: Guest, s: Session) -> str | None:
    if not s.start():
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    window, mark = "/tmp/probe-a.log", "/tmp/prompt-ran"
    s.client("00ff00", window)
    if not wait_for(lambda: "keyboard enter" in s.probe_log(window), 5):
        return f"the window never got the keyboard:\n{s.probe_log(window)}"
    s.sh(f"rm -f {mark}")
    before = bar_left(g)
    g.qmp.keys("meta_l", "r")
    if not wait_for(lambda: bar_left(g) != before, 5):
        return f"Super+R drew no prompt in the bar:\n{s.log()}"
    typed(g, f"touch {mark}")
    g.qmp.keys("ret")
    if not wait_for(lambda: s.sh(f"test -e {mark}").returncode == 0, 5):
        return f"the command typed into the prompt never ran:\n{s.log()}"
    if "key 30 pressed" in s.probe_log(window):
        return f"a key typed into the prompt reached the window:\n{s.probe_log(window)}"
    if not wait_for(lambda: bar_left(g) == before, 5):
        return "the prompt stayed in the bar after Return"
    return None


def bar_title_area(g: Guest) -> list[tuple[int, int, int]]:
    """The bar's middle rows from its midpoint to the clock's room, where the
    title and notifications are drawn."""
    shot = g.qmp.screendump(SHOT)
    w = shot[0]
    return [pixel(shot, x, y) for y in range(6, BAR_HEIGHT - 6) for x in range(w // 2, w - 400)]


def cyan(p: tuple[int, int, int]) -> bool:
    return p[0] < 100 and p[1] > 150 and p[2] > 150


def test_the_bar_shows_the_title_and_a_notification_over_it(g: Guest, s: Session) -> str | None:
    if not s.start():
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    window = "/tmp/probe-a.log"
    s.client("00ff00", window)
    if not wait_for(lambda: "keyboard enter" in s.probe_log(window), 5):
        return f"the window never got the keyboard:\n{s.probe_log(window)}"
    if not wait_for(lambda: any(p != BAR for p in bar_title_area(g)), 5):
        return f"the focused window's title never reached the bar:\n{s.log()}"
    g.qmp.keys("meta_l", "f1")
    if not wait_for(lambda: any(cyan(p) for p in bar_title_area(g)), 5, step=0.2):
        return f"the volume bind's notification never reached the bar:\n{s.log()}"
    title_back = lambda: (area := bar_title_area(g)) and not any(cyan(p) for p in area) and any(p != BAR for p in area)
    if not wait_for(title_back, 5):
        return "the notification never gave the bar back to the title"
    return None


def test_super_drag_moves_a_window_out_of_its_tile(g: Guest, s: Session) -> str | None:
    if not s.start():
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    s.client("00ff00")
    if not wait_for(lambda: drawn(g) is None, 10):
        return f"{drawn(g)}\n{s.log()}"
    shot = g.qmp.screendump(SHOT)
    w, h, _ = shot
    if window_origin(shot, GREEN) != (0, BAR_HEIGHT):
        return f"the window is not tiled at the work area's corner: {window_origin(shot, GREEN)}"
    # Super held, the left button drags the window by (100, 60).
    g.qmp.place(w // 3, h * 2 // 3, w, h)
    g.qmp.hold("meta_l", True)
    # The keyboard and the mouse are separate devices: Super is pressed
    # before the button only if it has arrived first.
    time.sleep(0.2)
    g.qmp.button("left", True)
    g.qmp.place(w // 3 + 100, h * 2 // 3 + 60, w, h)
    g.qmp.button("left", False)
    g.qmp.hold("meta_l", False)
    want = (100, BAR_HEIGHT + 60)
    if not wait_for(lambda: window_origin(g.qmp.screendump(SHOT), GREEN) == want, 5):
        return f"Super+drag left the window's corner at {window_origin(g.qmp.screendump(SHOT), GREEN)}, not {want}:\n{s.log()}"
    return None


def cursor_shown(g: Guest, at: tuple[int, int]) -> bool:
    """Whether the cursor is drawn at `at` over the green window: Anything
    but green in the box right of and below its hotspot."""
    shot = g.qmp.screendump(SHOT)
    x, y = at
    return any(pixel(shot, x + dx, y + dy) != GREEN for dx in range(1, 10) for dy in range(1, 16))


def test_the_cursor_hides_while_the_pointer_is_still(g: Guest, s: Session) -> str | None:
    if not s.start(config=CONFIG + "\n[cursor]\nauto_hide = true\nauto_hide_seconds = 1\n"):
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    s.client("00ff00")
    if not wait_for(lambda: drawn(g) is None, 10):
        return f"{drawn(g)}\n{s.log()}"
    w, h, _ = g.qmp.screendump(SHOT)
    at = (w // 3, h * 2 // 3)
    g.qmp.place(*at, w, h)
    if not wait_for(lambda: cursor_shown(g, at), 2, step=0.1):
        return "no cursor was drawn where the pointer moved"
    if not wait_for(lambda: not cursor_shown(g, at), 4):
        return "the cursor never hid with the pointer still"
    at = (at[0] + 40, at[1] + 20)
    g.qmp.place(*at, w, h)
    if not wait_for(lambda: cursor_shown(g, at), 2, step=0.1):
        return "the cursor did not come back as the pointer moved"
    return None


def test_a_minimized_window_comes_back_from_the_tag_menu(g: Guest, s: Session) -> str | None:
    if not s.start():
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    s.client("00ff00")
    if not wait_for(lambda: drawn(g) is None, 10):
        return f"{drawn(g)}\n{s.log()}"
    w, h, _ = g.qmp.screendump(SHOT)
    g.qmp.keys("meta_l", "m")
    if not wait_for(lambda: pixel(g.qmp.screendump(SHOT), w // 3, h * 2 // 3) != GREEN, 5):
        return f"Super+M left the window on the screen:\n{s.log()}"
    # The tag in view, clicked on the bar, drops its menu under it in the
    # bar's red; a click on the window's entry brings it back.
    g.qmp.place(30, BAR_HEIGHT // 2, w, h)
    g.qmp.click()
    if not wait_for(lambda: pixel(g.qmp.screendump(SHOT), 120, BAR_HEIGHT + 4) == BAR, 5):
        return f"a click on the tag in view dropped no menu: {pixel(g.qmp.screendump(SHOT), 120, BAR_HEIGHT + 4)}"
    g.qmp.place(120, BAR_HEIGHT + 8, w, h)
    g.qmp.click()
    if not wait_for(lambda: drawn(g) is None, 5):
        return f"picking the window from the menu did not bring it back: {drawn(g)}"
    return None


def test_the_tag_menu_shows_a_windows_icon(g: Guest, s: Session) -> str | None:
    if not s.start():
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    s.client("00ff00", kind="icon")
    if not wait_for(lambda: drawn(g) is None, 10):
        return f"{drawn(g)}\n{s.log()}"
    w, h, _ = g.qmp.screendump(SHOT)
    g.qmp.place(30, BAR_HEIGHT // 2, w, h)
    g.qmp.click()

    # The probe's icon is solid magenta, drawn in the first entry's row.
    def icon_shown() -> bool:
        shot = g.qmp.screendump(SHOT)
        return any(pixel(shot, x, y) == MAGENTA for x in range(20, 260) for y in range(BAR_HEIGHT, BAR_HEIGHT + 30))

    if not wait_for(icon_shown, 5):
        return f"the tag menu shows no icon for the window:\n{s.probe_log()}\n{s.log()}"
    g.qmp.keys("esc")
    return None


def png_rows(data: bytes) -> tuple[int, int, int, list[bytes]]:
    """An 8-bit truecolor PNG, as Cairo writes one: Its width, height,
    bytes a pixel and rows."""
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError("not a PNG")
    pos, idat, w, h, bpp = 8, b"", 0, 0, 0
    while pos < len(data):
        (n,) = struct.unpack(">I", data[pos:pos + 4])
        kind, body = data[pos + 4:pos + 8], data[pos + 8:pos + 8 + n]
        pos += 12 + n
        if kind == b"IHDR":
            w, h, _, color = struct.unpack(">IIBB", body[:10])
            bpp = {2: 3, 6: 4}[color]
        elif kind == b"IDAT":
            idat += body
    raw = zlib.decompress(idat)
    stride, rows, prev, i = w * bpp, [], bytearray(w * bpp), 0
    for _ in range(h):
        kind, row = raw[i], bytearray(raw[i + 1:i + 1 + stride])
        i += 1 + stride
        for x in range(stride):
            a = row[x - bpp] if x >= bpp else 0
            b = prev[x]
            c = prev[x - bpp] if x >= bpp else 0
            if kind == 1:
                row[x] = (row[x] + a) & 255
            elif kind == 2:
                row[x] = (row[x] + b) & 255
            elif kind == 3:
                row[x] = (row[x] + (a + b) // 2) & 255
            elif kind == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                row[x] = (row[x] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 255
        rows.append(bytes(row))
        prev = row
    return w, h, bpp, rows


def test_the_screenshot_bind_writes_the_screen(g: Guest, s: Session) -> str | None:
    if not s.start():
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    s.sh("rm -rf ~/Pictures && mkdir -p ~/Pictures")
    s.client("00ff00")
    if not wait_for(lambda: drawn(g) is None, 10):
        return f"{drawn(g)}\n{s.log()}"
    w, h, _ = g.qmp.screendump(SHOT)
    g.qmp.keys("meta_l", "s")
    if not wait_for(lambda: s.sh("ls ~/Pictures/screenshot-*.png").returncode == 0, 5):
        return f"the screenshot bind wrote no file:\n{s.log()}"
    name = s.sh("ls ~/Pictures/screenshot-*.png").stdout.split()[0]
    pw, ph, bpp, rows = png_rows(base64.b64decode(s.sh(f"base64 -w0 {name}").stdout))
    if (pw, ph) != (w, h):
        return f"the screenshot is {pw}x{ph}, the screen {w}x{h}"
    want = {(w // 2, 2): BAR, (w // 3, h * 2 // 3): GREEN, (w - 2, h - 2): GREEN}
    got = {p: tuple(rows[p[1]][p[0] * bpp:p[0] * bpp + 3]) for p in want}
    if got != want:
        return f"the screenshot shows {got} where the screen shows {want}"
    return None


def test_a_display_entry_sets_the_monitors_mode(g: Guest, s: Session) -> str | None:
    if not s.start(config=CONFIG + '\n[[display]]\nmatch = "virtual"\nmode = "1024x768"\n'):
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    s.client("00ff00")
    if not wait_for(lambda: drawn(g) is None, 10):
        return f"{drawn(g)}\n{s.log()}"
    w, h, _ = g.qmp.screendump(SHOT)
    if (w, h) != (1024, 768):
        return f"the head runs at {w}x{h}, not the [[display]] entry's 1024x768:\n{s.log()}"
    return None


def test_a_floating_window_wears_its_title_bar(g: Guest, s: Session) -> str | None:
    floating = CONFIG.replace('default_layout = "tile"', 'default_layout = "floating"\ntitle_bars = true')
    if not s.start(config=floating):
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    s.client("00ff00", kind="titled")
    w, h, _ = g.qmp.screendump(SHOT)
    # The probe's 400 by 300, its 30-pixel bar above it, centered below the bar.
    x, y = (w - 400) // 2, BAR_HEIGHT + (h - BAR_HEIGHT - 330) // 2

    def worn() -> bool:
        shot = g.qmp.screendump(SHOT)
        r, gr, b = pixel(shot, x + 18, y + 15)
        return r > 200 and gr < 140 and b < 140 and pixel(shot, x + 200, y + 40) == GREEN and pixel(shot, x + 200, y + 10) != GREEN

    if not wait_for(worn, 10):
        shot = g.qmp.screendump(SHOT)
        return (f"no title bar over the window: close light {pixel(shot, x + 18, y + 15)}, bar {pixel(shot, x + 200, y + 10)},"
                f" client {pixel(shot, x + 200, y + 40)}\n{s.log()}")
    g.qmp.place(x + 18, y + 15, w, h)
    g.qmp.click()
    if not wait_for(lambda: s.sh("pgrep -x wl-probe").returncode != 0, 5):
        return "the close light did not close the window"
    return None


def test_alt_tab_shows_the_windows_while_held(g: Guest, s: Session) -> str | None:
    def two_windows(config: str) -> tuple[int, int] | str:
        if not s.start(config=config):
            return "demiurge-wl-tty never opened its socket:\n" + s.log()
        s.client("00ff00", "/tmp/probe-a.log")
        s.client("0000ff", "/tmp/probe-b.log")
        w, h, _ = g.qmp.screendump(SHOT)
        tiled = lambda: {pixel(g.qmp.screendump(SHOT), x, h * 2 // 3) for x in (w // 4, w * 3 // 4)} == {GREEN, BLUE}
        if not wait_for(tiled, 10):
            return f"the two windows never tiled:\n{s.log()}"
        return w, h

    got = two_windows(CONFIG)
    if isinstance(got, str):
        return got
    w, h = got
    # Two tiles of 240 by 150, a 26-pixel title under each and 16 around.
    pw, ph = 2 * (240 + 16) + 16, 150 + 26 + 16 + 16
    px, py = (w - pw) // 2, (h - ph) // 2
    corner = (px + 6, py + 6)
    centers = [(px + 16 + 120, py + 16 + 75), (px + 16 + 256 + 120, py + 16 + 75)]
    g.qmp.hold("alt", True)
    g.qmp.keys("tab")
    shown = wait_for(lambda: pixel(g.qmp.screendump(SHOT), *corner) == BAR, 5)
    shot = g.qmp.screendump(SHOT)
    thumbs = {pixel(shot, *c) for c in centers}
    g.qmp.hold("alt", False)
    if not shown:
        return f"no overlay while Alt was held: {pixel(shot, *corner)}\n{s.log()}"
    if thumbs != {GREEN, BLUE}:
        return f"the overlay's tiles show {thumbs}, not the two windows"
    if not wait_for(lambda: pixel(g.qmp.screendump(SHOT), *corner) != BAR, 5):
        return "the overlay stayed after Alt was let go"

    got = two_windows(CONFIG.replace('default_layout = "tile"', 'default_layout = "tile"\nalt_tab_preview = false'))
    if isinstance(got, str):
        return got
    g.qmp.hold("alt", True)
    g.qmp.keys("tab")
    time.sleep(1)
    drawn_off = pixel(g.qmp.screendump(SHOT), *corner) == BAR
    g.qmp.hold("alt", False)
    if drawn_off:
        return "an overlay was drawn with alt_tab_preview off"
    return None


def test_an_idle_session_blanks_powers_off_and_wakes(g: Guest, s: Session) -> str | None:
    if not s.start(config=CONFIG + "\n[input.idle]\nscreensaver_seconds = 2\ndpms_off_seconds = 4\n"):
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    s.client("00ff00")
    w, h, _ = g.qmp.screendump(SHOT)
    black = lambda: all(pixel(g.qmp.screendump(SHOT), *p) == BLACK for p in ((w // 2, 2), (w // 3, h * 2 // 3)))
    if not wait_for(black, 6):
        return f"the idle session never blanked:\n{s.log()}"
    if not wait_for(lambda: "idle; outputs off" in s.log(), 6):
        return f"the idle session never powered its monitor off:\n{s.log()}"
    g.qmp.place(w // 2, h // 2, w, h)
    if not wait_for(lambda: drawn(g) is None, 2, step=0.2):
        return f"a pointer motion did not bring the monitor back: {drawn(g)}"
    return None


def test_the_bar_clock_shows_every_second(g: Guest, s: Session) -> str | None:
    if not s.start():
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    w, _, _ = g.qmp.screendump(SHOT)
    last, changes, samples = None, [], []
    deadline = time.monotonic() + 8
    while time.monotonic() < deadline:
        w, _, rgb = g.qmp.screendump(SHOT)
        at = time.monotonic()
        samples.append(at)
        region = b"".join(rgb[(y * w + w - 400) * 3:(y * w + w) * 3] for y in range(BAR_HEIGHT))
        digest = hashlib.blake2b(region, digest_size=8).digest()
        if last is not None and digest != last:
            changes.append(at)
        last = digest
    gaps = [round(b - a, 3) for a, b in zip(changes, changes[1:])]
    if len(changes) < 6 or any(gap > 1.5 for gap in gaps):
        # The screen is read only as often as a screendump returns: A
        # second it went unread is no evidence against the clock.
        pause = max(b - a for a, b in zip(samples, samples[1:]))
        return f"the clock changed {len(changes)} times in 8 s, {gaps} apart; the screen went {pause:.3f} s unread at most"
    # The region the clock's own repaint uploads holds the clock and the bar
    # behind it: Its rows above and below the text are the bar's red.
    edges = [y for y in range(BAR_HEIGHT) if y < 4 or y >= BAR_HEIGHT - 4]
    stray = {bytes(rgb[(y * w + x) * 3:(y * w + x) * 3 + 3]) for y in edges for x in range(w - 400, w)}
    stray = [tuple(c) for c in stray if tuple(c) != BAR]
    if stray:
        return f"the clock's region is garbled: {stray[:8]}"
    return None


# What each X11 window's surface holds is let go with it: smithay 0.7.0 kept
# every XWayland surface paired by serial, about one window-sized texture
# per X11 window ever mapped.
def test_closed_x11_windows_give_their_memory_back(g: Guest, s: Session) -> str | None:
    if not s.start(xwayland=True):
        return "demiurge-wl-tty or its XWayland never came up:\n" + s.log()

    def rss() -> int:
        out = s.sh("grep '^VmRSS:' /proc/$(pgrep -x demiurge-wl-tty)/status").stdout.split()
        return int(out[1])

    def churn(n: int) -> None:
        for i in range(n):
            s.x11_client("0000ff", f"/tmp/churn{i}.log")
            time.sleep(0.4)
            s.sh("pkill -x x11-probe")
            time.sleep(0.2)

    churn(15)
    before = rss()
    churn(30)
    grown = rss() - before
    if grown > 20_000:
        return f"30 X11 windows mapped and closed grew the compositor by {grown} KiB"
    return None


# The idle session's whole cost: Every thread of the compositor, a client
# showing, its voluntary wakeups over five seconds.
def test_an_idle_session_stays_under_its_wakeup_budget(g: Guest, s: Session) -> str | None:
    if not s.start():
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    s.client("00ff00")
    if not wait_for(lambda: drawn(g) is None, 10):
        return f"the client never showed: {drawn(g)}"

    def wakeups() -> int:
        out = s.sh("grep -h '^voluntary_ctxt_switches:' /proc/$(pgrep -x demiurge-wl-tty)/task/*/status").stdout
        return sum(int(line.split()[1]) for line in out.splitlines())

    time.sleep(2)
    before = wakeups()
    time.sleep(5)
    woke = wakeups() - before
    if woke > 150:
        return f"the idle compositor woke {woke} times in 5 s"
    return None


# cherrypie places demiurge-wl's windows with nothing of X11 anywhere:
# XWayland is off, the Wayland cherrypie serves its API on the compositor's
# own socket name, and its rule sends wl-probe's window to tag 2, out of
# sight. demiurge-wl had reached cherrypie only through XWayland's display.
def test_the_wayland_cherrypie_places_a_window_with_no_xwayland(g: Guest, s: Session) -> str | None:
    s.sh("cat > /tmp/cherrypie.toml << 'EOF'\n[[rule]]\nclass = \"wl-probe\"\nworkspace = 2\nEOF")
    config = CONFIG.replace(f"height = {BAR_HEIGHT}\n", f"height = {BAR_HEIGHT}\nclock_format = \"%H:%M\"\n"
                            "tag_empty_fg = \"#0000ff\"\ntag_occupied_fg = \"#00ff00\"\n")
    config += '\n[startup]\ncommands = ["cherrypie-wl --config /tmp/cherrypie.toml"]\n'
    if not s.start(config=config):
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    if not wait_for(lambda: s.sh(f"test -S {RUNTIME}/cherrypie-wayland_1.sock").returncode == 0, 10):
        return "cherrypie-wl never served its API on the compositor's socket name:\n" + s.log()
    if s.sh("pgrep -x Xwayland").returncode == 0:
        return "XWayland ran"
    s.client("00ff00")

    def occupied_drawn() -> bool:
        return any(gr > r and gr > b for r, gr, b in bar_left(g))

    if not wait_for(occupied_drawn, 10, step=0.2):
        return f"cherrypie's rule never put the window on tag 2:\n{s.log()}"
    if drawn(g) is None:
        return "the window showed on tag 1, where cherrypie's rule did not put it"
    return None


# A tag's look follows its windows at once, with nothing else behind it:
# The clock shows minutes, no window shows to put a title in the bar, and a
# window maps onto tag 2, out of sight, three seconds in. Only tag 2's
# occupied mark changes.
def test_a_window_mapped_on_a_hidden_tag_marks_it_at_once(g: Guest, s: Session) -> str | None:
    config = CONFIG.replace(f"height = {BAR_HEIGHT}\n", f"height = {BAR_HEIGHT}\nclock_format = \"%H:%M\"\n"
                            "tag_empty_fg = \"#0000ff\"\ntag_occupied_fg = \"#00ff00\"\n")
    config += ('\n[[startup.spawn]]\ncmd = "sh -c \'sleep 3; exec wl-probe color 00ff00\'"\n'
               'class = "wl-probe"\ntag = "2"\n')
    if not s.start(config=config):
        return "demiurge-wl-tty never opened its socket:\n" + s.log()

    # A tag's text in the occupied color: Green over the red bar, its small
    # glyphs' edges blended with it.
    def occupied_drawn() -> bool:
        return any(gr > r and gr > b for r, gr, b in bar_left(g))

    time.sleep(1)
    if occupied_drawn():
        return "a tag was drawn occupied before any window mapped"
    if not wait_for(occupied_drawn, 8, step=0.1):
        return f"tag 2 was not drawn occupied once its window mapped:\n{s.log()}"
    return None


def test_an_x11_window_tiles_and_takes_the_keyboard(g: Guest, s: Session) -> str | None:
    if not s.start(xwayland=True):
        return "demiurge-wl-tty or its XWayland never came up:\n" + s.log()
    wayland, x11 = "/tmp/probe-a.log", "/tmp/x11.log"
    s.client("00ff00", wayland)
    if not wait_for(lambda: "keyboard enter" in s.probe_log(wayland), 5):
        return f"the Wayland window never got the keyboard:\n{s.probe_log(wayland)}"
    s.x11_client("0000ff", x11)
    if not wait_for(lambda: "focus in" in s.probe_log(x11), 10):
        return f"the X11 window never got the keyboard:\n{s.probe_log(x11)}\n{s.log()}"
    if not wait_for(lambda: "keyboard leave" in s.probe_log(wayland), 5):
        return f"the Wayland window kept the keyboard:\n{s.probe_log(wayland)}"
    # Tiled: The Wayland window the master on the left, the X11 one the
    # stack on the right, each its own color.
    w, h, _ = g.qmp.screendump(SHOT)
    sides = {(w // 8, h // 2): GREEN, (w * 7 // 8, h // 2): BLUE}
    if not wait_for(lambda: all(pixel(g.qmp.screendump(SHOT), *p) == c for p, c in sides.items()), 10):
        return f"not tiled side by side: {[pixel(g.qmp.screendump(SHOT), *p) for p in sides]}\n{s.probe_log(x11)}"
    g.qmp.keys("a")
    if not wait_for(lambda: "key 38" in s.probe_log(x11), 5):
        return f"a key typed never reached the focused X11 window:\n{s.probe_log(x11)}"
    if "key 30" in s.probe_log(wayland):
        return f"a key meant for the X11 window reached the Wayland one:\n{s.probe_log(wayland)}"
    return None


def test_an_x11_game_asking_fullscreen_fills_the_screen(g: Guest, s: Session) -> str | None:
    if not s.start(xwayland=True):
        return "demiurge-wl-tty or its XWayland never came up:\n" + s.log()
    s.x11_client("ff00ff", kind="fullscreen")
    w, h, _ = g.qmp.screendump(SHOT)
    if not wait_for(lambda: f"configure 0 0 {w} {h}" in s.probe_log("/tmp/x11.log"), 10):
        return f"the window was never configured to the whole {w}x{h} screen:\n{s.probe_log('/tmp/x11.log')}\n{s.log()}"
    covered = [(w // 2, 2), (w // 3, h * 2 // 3), (w - 2, h - 2)]
    if not wait_for(lambda: all(pixel(g.qmp.screendump(SHOT), *p) == MAGENTA for p in covered), 10):
        return f"the fullscreen window does not cover the screen and the bar: {[pixel(g.qmp.screendump(SHOT), *p) for p in covered]}"
    return None


def test_an_x11_games_mode_change_is_emulated_on_its_surface(g: Guest, s: Session) -> str | None:
    """An X11 game setting 800x600 through RandR has XWayland emulate the
    mode: Its 800x600 window, at the output's origin, is scaled through a
    viewport to fill the 1280x800 screen, and the output keeps its mode."""
    if not s.start(xwayland=True):
        return "demiurge-wl-tty or its XWayland never came up:\n" + s.log()
    w, h, _ = g.qmp.screendump(SHOT)
    s.x11_client("ff00ff", kind="mode")
    if not wait_for(lambda: "mode 800x600 set" in s.probe_log("/tmp/x11.log"), 10):
        return f"the mode was never set:\n{s.probe_log('/tmp/x11.log')}\n{s.log()}"
    covered = [(2, 2), (w // 2, h // 2 + 40), (w - 2, h - 2)]
    if not wait_for(lambda: all(pixel(g.qmp.screendump(SHOT), *p) == MAGENTA for p in covered), 10):
        return f"the 800x600 window does not fill the screen: {[pixel(g.qmp.screendump(SHOT), *p) for p in covered]}"
    sw, sh, _ = g.qmp.screendump(SHOT)
    if (sw, sh) != (w, h):
        return f"the output changed from {w}x{h} to {sw}x{sh}"
    return None


def medusa_png(s: Session, d: str) -> tuple[int, int, int, list[bytes]] | str:
    """The one screenshot MEDUSA wrote into `d`, or what is wrong."""
    names = s.sh(f"ls {d}/medusa-*.png").stdout.split()
    if len(names) != 1:
        return f"MEDUSA wrote {names} into {d}:\n{s.sh(f'cat {d}.log').stdout}"
    return png_rows(base64.b64decode(s.sh(f"base64 -w0 {names[0]}").stdout))


def test_medusa_freezes_saves_copies_pins_and_writes_the_screen(g: Guest, s: Session) -> str | None:
    if not s.start():
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    s.client("00ff00")
    if not wait_for(lambda: drawn(g) is None, 10):
        return f"{drawn(g)}\n{s.log()}"
    w, h, _ = g.qmp.screendump(SHOT)
    wayland = f"{ENV} WAYLAND_DISPLAY=wayland-1"

    def medusa(d: str, *args: str) -> None:
        s.sh(f"rm -rf {d} && mkdir -p {d} && {wayland} setsid -f medusa-wl {' '.join(args)} -p {d}"
             f" > {d}.log 2>&1 < /dev/null")

    def at(x: int, y: int) -> tuple[int, int, int]:
        return pixel(g.qmp.screendump(SHOT), x, y)

    def dimmed(p: tuple[int, int, int]) -> bool:
        return p[0] < 20 and 110 <= p[1] <= 140 and p[2] < 20

    # A drag selecting (x1, y1)-(x2, y2), done once the overlay shows the
    # region's middle at full brightness: The keyboard is another device,
    # whose keys could otherwise reach MEDUSA before the drag does. Each step
    # waits a moment, or QEMU's tablet hands libinput the press with the
    # motions after it in one frame, a drag of nothing.
    def drag(x1: int, y1: int, x2: int, y2: int) -> bool:
        for step in (lambda: g.qmp.place(x1, y1, w, h), lambda: g.qmp.button("left", True),
                     lambda: g.qmp.place((x1 + x2) // 2, (y1 + y2) // 2, w, h),
                     lambda: g.qmp.place(x2, y2, w, h), lambda: g.qmp.button("left", False)):
            step()
            time.sleep(0.1)
        return wait_for(lambda: at((x1 + x2) // 2, (y1 + y2) // 2) == GREEN, 5)

    # The overlay shows the screen frozen and dimmed: A window mapped under
    # it does not show through.
    medusa("/tmp/medusa-save", "--no-copy")
    corner = (w - 2, h - 2)
    if not wait_for(lambda: dimmed(at(*corner)), 10):
        return f"the overlay never showed the screen dimmed: {at(*corner)}\n{s.sh('cat /tmp/medusa-save.log').stdout}"
    s.client("0000ff", "/tmp/probe-b.log")
    time.sleep(1.5)
    if not dimmed(at(*corner)):
        return f"a window mapped under the overlay showed through it ({at(*corner)}): the screen is not frozen"
    region = (100, 200, 300, 300)
    if not drag(*region):
        return f"the drag selected nothing:\n{s.sh('cat /tmp/medusa-save.log').stdout}\n{s.log()}"
    g.qmp.keys("ctrl", "s")
    if not wait_for(lambda: s.sh("pgrep -x medusa-wl").returncode != 0, 10):
        return "Ctrl+S did not close MEDUSA"
    if not wait_for(lambda: at(*corner) == BLUE, 5):
        return f"the window mapped under the overlay is not where it was looked for: {at(*corner)}"
    shot = medusa_png(s, "/tmp/medusa-save")
    if isinstance(shot, str):
        return shot
    pw, ph, bpp, rows = shot
    if (pw, ph) != (200, 100):
        return (f"the region saved is {pw}x{ph}, not the 200x100 dragged:\n"
                f"{s.sh('cat /tmp/medusa-save.log').stdout}\n{s.log()}")
    wrong = {(x, y): tuple(rows[y][x * bpp:x * bpp + 3]) for x in (0, 99, 199) for y in (0, 49, 99)}
    wrong = {p: c for p, c in wrong.items() if c != GREEN}
    if wrong:
        return f"the region saved is not the window's pixels: {wrong}"

    # Enter saves the region and puts the same PNG on the clipboard.
    left = (w // 4, h * 2 // 3)
    medusa("/tmp/medusa-copy")
    if not wait_for(lambda: dimmed(at(*left)), 10):
        return f"the overlay never came up again: {at(*left)}\n{s.sh('cat /tmp/medusa-copy.log').stdout}"
    if not drag(*region):
        return f"the drag selected nothing:\n{s.sh('cat /tmp/medusa-copy.log').stdout}"
    g.qmp.keys("ret")
    if not wait_for(lambda: s.sh("ls /tmp/medusa-copy/medusa-*.png").returncode == 0, 10):
        return f"Enter saved nothing:\n{s.sh('cat /tmp/medusa-copy.log').stdout}"
    saved = s.sh("sha256sum /tmp/medusa-copy/medusa-*.png").stdout.split()[0]
    pasted = lambda: s.sh(f"{wayland} timeout 5 wl-paste -t image/png | sha256sum").stdout.split()[0]  # noqa: E731
    if not wait_for(lambda: pasted() == saved, 10):
        return f"the clipboard does not hold the PNG saved:\n{s.sh('cat /tmp/medusa-copy.log').stdout}"

    # Ctrl+P pins the region in place, a white border around it, until a key.
    medusa("/tmp/medusa-pin", "--no-copy")
    if not wait_for(lambda: dimmed(at(*left)), 10):
        return f"the overlay never came up for the pin: {at(*left)}"
    if not drag(*region):
        return f"the drag selected nothing:\n{s.sh('cat /tmp/medusa-pin.log').stdout}"
    g.qmp.keys("ctrl", "p")
    border = (region[0] - 1, region[1] - 1)
    if not wait_for(lambda: at(*border) == (255, 255, 255) and at(region[0] + 10, region[1] + 10) == GREEN, 10):
        return f"no pin at the region: {at(*border)} on its border\n{s.sh('cat /tmp/medusa-pin.log').stdout}\n{s.log()}"
    g.qmp.keys("a")
    if not wait_for(lambda: at(*border) == GREEN, 5):
        return f"a key did not dismiss the pin: {at(*border)}"

    # `medusa full` writes the whole screen as it shows.
    s.sh(f"rm -rf /tmp/medusa-full && mkdir -p /tmp/medusa-full && {wayland} timeout 20 medusa-wl full --no-copy"
         " -p /tmp/medusa-full > /tmp/medusa-full.log 2>&1")
    shot = medusa_png(s, "/tmp/medusa-full")
    if isinstance(shot, str):
        return shot
    pw, ph, bpp, rows = shot
    if (pw, ph) != (w, h):
        return f"medusa full wrote {pw}x{ph}, the screen is {w}x{h}"
    screen = g.qmp.screendump(SHOT)
    points = [(w // 2, 2), left, corner, (w * 3 // 4, h // 3)]
    wrong = {p: (tuple(rows[p[1]][p[0] * bpp:p[0] * bpp + 3]), pixel(screen, *p)) for p in points}
    wrong = {p: c for p, c in wrong.items() if c[0] != c[1]}
    if wrong:
        return f"medusa full differs from the screen (written, shown): {wrong}"
    return None


def test_gnosis_lists_trashes_and_mounts_a_drive(g: Guest, s: Session) -> str | None:
    if not s.start():
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    home, media = "/tmp/gnosis-home", "/tmp/gnosis-media"
    # Its background cyan, from a config of its own.
    s.sh(f"rm -rf {home} {media} && mkdir -p {home}/.config/demiurge {media} && echo doomed > {home}/doomed.txt"
         f" && printf '[bar]\\nbg = \"#00ffff\"\\n' > {home}/.config/demiurge/config.toml")
    s.sh(f"setsid -f fake-udisks {media} > /tmp/udisks.log 2>&1 < /dev/null")
    if not wait_for(lambda: s.sh("head -n 1 /tmp/udisks.log").stdout.startswith("unix:"), 10):
        return f"the fake udisks2 never came up:\n{s.sh('cat /tmp/udisks.log').stdout}"
    bus = s.sh("head -n 1 /tmp/udisks.log").stdout.strip()
    mapped = lambda: s.sh("grep -c 'toplevel mapped' /tmp/wl.log").stdout.strip()  # noqa: E731
    before = mapped()
    s.sh(f"{ENV} WAYLAND_DISPLAY=wayland-1 HOME={home} XDG_CONFIG_HOME={home}/.config"
         f" XDG_CACHE_HOME={home}/.cache DBUS_SYSTEM_BUS_ADDRESS='{bus}'"
         f" setsid -f gnosis-wl {home} > /tmp/gnosis.log 2>&1 < /dev/null")
    if not wait_for(lambda: mapped() != before, 10):
        return f"GNOSIS never mapped:\n{s.sh('cat /tmp/gnosis.log').stdout}\n{s.log()}"
    w, h, _ = g.qmp.screendump(SHOT)
    body = (w * 2 // 3, h * 2 // 3)
    if not wait_for(lambda: pixel(g.qmp.screendump(SHOT), *body) == CYAN, 5):
        return f"GNOSIS's window does not show: {pixel(g.qmp.screendump(SHOT), *body)} in its body"

    # The listing's one entry is selected; x trashes it.
    g.qmp.keys("x")
    trashed = f"{home}/.local/share/Trash/files/doomed.txt"
    if not wait_for(lambda: s.sh(f"test -e {trashed} && ! test -e {home}/doomed.txt").returncode == 0, 5):
        return f"x did not trash the file GNOSIS listed:\n{s.sh('cat /tmp/gnosis.log').stdout}"

    # Bookmarks Home and Root, then the drives by label: USB STICK, data.
    for key in ("s", "down", "down", "ret"):
        g.qmp.keys(key)
        time.sleep(0.1)
    stick = "Mount /org/freedesktop/UDisks2/block_devices/sdb1 true"
    if not wait_for(lambda: stick in s.sh("cat /tmp/udisks.log").stdout, 5):
        return (f"Enter on the sidebar's stick asked for no mount:\n{s.sh('cat /tmp/udisks.log').stdout}"
                f"\n{s.sh('cat /tmp/gnosis.log').stdout}")
    return None


def test_a_wayland_client_pastes_what_another_copied(g: Guest, s: Session) -> str | None:
    if not s.start():
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    wayland = f"{ENV} WAYLAND_DISPLAY=wayland-1"
    s.sh(f"printf copied | {wayland} setsid -f wl-copy > /dev/null 2>&1")
    paste = lambda: s.sh(f"{wayland} timeout 5 wl-paste -n").stdout  # noqa: E731
    if not wait_for(lambda: paste() == "copied", 10):
        return f"wl-paste got {paste()!r}, not what wl-copy copied\n{s.log()}"
    return None


def test_the_clipboard_crosses_between_x11_and_wayland(g: Guest, s: Session) -> str | None:
    if not s.start(xwayland=True):
        return "demiurge-wl-tty or its XWayland never came up:\n" + s.log()
    x11 = "/tmp/x11.log"
    s.x11_client("0000ff", x11)
    if not wait_for(lambda: "focus in" in s.probe_log(x11), 10):
        return f"the X11 window never got the keyboard:\n{s.probe_log(x11)}\n{s.log()}"
    display = s.x11_display()
    wayland = f"{SESSION} WAYLAND_DISPLAY=wayland-1"
    paste_wayland = lambda: s.sh(f"{wayland} timeout 5 wl-paste -n").stdout  # noqa: E731
    paste_x11 = lambda: s.sh(f"DISPLAY={display} timeout 5 xclip -o -selection clipboard").stdout  # noqa: E731
    # Copied in X11 while its window has the keyboard, pasted in Wayland.
    s.sh(f"printf from-x11 | DISPLAY={display} setsid -f xclip -selection clipboard > /dev/null 2>&1")
    if not wait_for(lambda: paste_wayland() == "from-x11", 10):
        return f"a Wayland client pasted {paste_wayland()!r}, not what X11 copied\n{s.log()}"
    # Copied in Wayland, pasted in X11 once its window has the keyboard back.
    s.sh(f"printf from-wayland | {wayland} setsid -f wl-copy > /dev/null 2>&1")
    if not wait_for(lambda: paste_x11() == "from-wayland", 10):
        return f"an X11 client pasted {paste_x11()!r}, not what Wayland copied\n{s.probe_log(x11)}\n{s.log()}"
    return None


def test_a_monitor_resized_unplugged_and_plugged_back_is_followed(g: Guest, s: Session) -> str | None:
    if not s.start():
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    s.client("00ff00")
    if not wait_for(lambda: drawn(g) is None, 10):
        return f"never drawn: {drawn(g)}"
    size = lambda: g.qmp.screendump(SHOT)[:2]  # noqa: E731
    # The monitor swapped for a smaller one: Lit again at the mode it now
    # prefers, the bar and the window laid out across it.
    g.vnc.head(1024, 768)
    if not wait_for(lambda: "output mode 1024 768" in s.probe_log(), 10):
        return f"the client never saw the monitor at 1024x768:\n{s.probe_log()}\n{s.log()}"
    if not wait_for(lambda: size() == (1024, 768) and drawn(g) is None, 10):
        return f"the head is not lit and drawn at 1024x768: {size()} {drawn(g)}\n{s.log()}"
    # Unplugged: Its output goes away, the one before it having gone with
    # the swap.
    g.vnc.head(0, 0)
    if not wait_for(lambda: s.probe_log().count("output removed") == 2, 10):
        return f"the unplugged monitor's output stayed:\n{s.probe_log()}\n{s.log()}"
    # Plugged back in: Lit and drawn again.
    g.vnc.head(1280, 800)
    if not wait_for(lambda: s.probe_log().count("output mode 1280 800") == 2, 10):
        return f"the client never saw the monitor plugged back in:\n{s.probe_log()}\n{s.log()}"
    if not wait_for(lambda: size() == (1280, 800) and drawn(g) is None, 10):
        return f"the monitor plugged back in is not lit and drawn: {size()} {drawn(g)}\n{s.log()}"
    return None


def window_origin(shot: tuple[int, int, bytes], color: tuple[int, int, int]) -> tuple[int, int] | None:
    """The top-left corner of the window of `color` under (w/3, 2h/3),
    clear of the cursor at the center, if one is there."""
    w, h, _ = shot
    x, y = w // 3, h * 2 // 3
    if pixel(shot, x, y) != color:
        return None
    top, left = y, x
    while top > 0 and pixel(shot, x, top - 1) == color:
        top -= 1
    while left > 0 and pixel(shot, left - 1, y) == color:
        left -= 1
    return left, top


def test_a_second_monitor_gets_a_tag_and_bar_of_its_own(g: Guest, s: Session) -> str | None:
    """A monitor plugged into the second head is lit to the right of the
    first, under a bar of its own; the mouse carries the pointer across
    onto it, and a window opened there tiles on it. Unplugged, it is
    dropped, and the first monitor's window is untouched."""
    if not s.start():
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    s.client("00ff00", log="/tmp/a.log")
    if not wait_for(lambda: drawn(g) is None, 10):
        return f"{drawn(g)}\n{s.log()}"

    def wl_log() -> str:
        return re.sub(r"\x1b\[[0-9;]*m", "", s.sh("cat /tmp/wl.log").stdout)

    g.vnc1.head(1024, 768)
    if not wait_for(lambda: len([l for l in wl_log().splitlines() if "monitor lit" in l]) == 2, 10):
        return f"the second monitor was never lit:\n{s.log()}"
    g.qmp.point(0.99, 0.5)
    time.sleep(0.3)
    g.qmp.move(300, 0)
    time.sleep(0.3)
    s.client("0000ff", log="/tmp/b.log")
    want = {(512, 2): BAR, (1024 // 3, 768 * 2 // 3): BLUE}
    if not wait_for(lambda: all(pixel(g.qmp.screendump(SHOT, head=1), *p) == c for p, c in want.items()), 10):
        shot = g.qmp.screendump(SHOT, head=1)
        return f"the second monitor shows {[pixel(shot, *p) for p in want]} where it wants {list(want.values())}\n{s.log()}"
    if drawn(g) is not None:
        return f"the first monitor lost its window: {drawn(g)}"
    g.vnc1.head(0, 0)
    if not wait_for(lambda: "monitor dropped" in wl_log(), 10):
        return f"the unplugged monitor was never dropped:\n{s.log()}"
    if not wait_for(lambda: drawn(g) is None, 5):
        return f"the first monitor lost its window when the second went: {drawn(g)}"
    return None


def test_quarks_runtime_draws_and_takes_the_keyboard_and_pointer(g: Guest, s: Session) -> str | None:
    try:
        runtime, exe = build_quark()
    except subprocess.CalledProcessError as e:
        return f"quark's runtime or its windowing probe did not build: {e}"
    g.push(runtime, "/tmp/quark-wayland")
    g.push(exe, "/tmp/win_surface.exe")
    if not s.start():
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    out = "/tmp/quark.log"
    s.sh(f"{ENV} WAYLAND_DISPLAY=wayland-1 STEAM_COMPAT_DATA_PATH=/tmp/quark-compat"
         f" setsid -f /tmp/quark-wayland /tmp/win_surface.exe > {out} 2>&1 < /dev/null")
    # Drawn: The window's frame, black, tiled under the bar.
    if not wait_for(lambda: window_origin(g.qmp.screendump(SHOT), BLACK) is not None, 20):
        return f"the window never reached the screen:\n{s.probe_log(out)}\n{s.log()}"
    shot = g.qmp.screendump(SHOT)
    w, h, _ = shot
    ox, oy = window_origin(shot, BLACK)
    # Phase a: Every kind of input until the probe has it all; the click
    # lands on window pixel (100, 50), which the probe checks.
    deadline = time.monotonic() + 20
    while "phase b" not in s.probe_log(out):
        if time.monotonic() > deadline:
            return f"the window never took the keyboard and pointer:\n{s.probe_log(out)}\n{s.log()}"
        g.qmp.keys("a")
        g.qmp.place(ox + 100, oy + 50, w, h)
        g.qmp.click()
        time.sleep(0.5)
    # Phase b: Keys that must wake a thread asleep in GetMessage.
    deadline = time.monotonic() + 20
    while "phase c" not in s.probe_log(out):
        if time.monotonic() > deadline:
            return f"keys never woke the window's GetMessage:\n{s.probe_log(out)}"
        g.qmp.keys("b")
        time.sleep(0.3)
    # Phase c, once: Shift+A, C held past the repeat delay, a right click
    # on window pixel (200, 120) and one wheel notch down.
    g.qmp.keys("shift", "a")
    g.qmp.keys("c", hold_ms=600)
    g.qmp.place(ox + 200, oy + 120, w, h)
    g.qmp.click("right")
    g.qmp.click("wheel-down")
    if not wait_for(lambda: s.sh("pgrep -x quark-wayland").returncode != 0, 30):
        return f"the probe never finished:\n{s.probe_log(out)}"
    text = s.probe_log(out)
    # The probe's checks, logged as quark logs: `[INFO]` passed, `[ERROR]`
    # failed (quark's tests/research/wayland/probe.h).
    checks = [l for l in text.splitlines() if "] [INFO]" in l or "] [ERROR]" in l]
    failed = [l for l in checks if "] [ERROR]" in l and not any(k in l for k in QUARK_HARNESS_ONLY)]
    if not checks or failed:
        return f"the windowing probe failed under demiurge-wl-tty:\n" + "\n".join(failed or [text])
    harness = [l for l in checks if "] [ERROR]" in l]
    log("INFO", f"  windowing probe: {len(checks) - len(harness)} checks ok; {len(harness)} name quark's own"
                " headless compositor:")
    for line in harness:
        log("INFO", f"    {line}")
    return None


def test_a_vt_switch_leaves_and_comes_back_drawn(g: Guest, s: Session) -> str | None:
    if not s.start():
        return "demiurge-wl-tty never opened its socket:\n" + s.log()
    s.client("00ff00")
    if not wait_for(lambda: drawn(g) is None, 10):
        return f"never drawn before the switch: {drawn(g)}"
    home = s.active_vt()
    away = "tty3" if home != "tty3" else "tty4"
    log("INFO", f"  session on {home}; switching to {away}")
    g.qmp.keys("ctrl", "alt", f"f{away[3:]}")
    if not wait_for(lambda: s.active_vt() == away, 10):
        return f"Ctrl+Alt+F{away[3:]} left the VT at {s.active_vt()}\n{s.log()}"
    if drawn(g) is None:
        return f"{away} still shows the session"
    g.qmp.keys("ctrl", "alt", f"f{home[3:]}")
    if not wait_for(lambda: s.active_vt() == home, 10):
        return f"Ctrl+Alt+F{home[3:]} left the VT at {s.active_vt()}"
    if not wait_for(lambda: drawn(g) is None, 10):
        return f"back on {home} but not drawn: {drawn(g)}\n{s.log()}"
    return None


# The Wayland session's target as its installer installs it, and a service
# standing in for ABRAXAS, wanted by graphical-session.target as ABRAXAS's
# instance is.
SESSION_TARGET = REPO / "contrib" / "systemd" / "demiurge-wayland.target"
STAND_IN = """[Unit]
PartOf=graphical-session.target
After=graphical-session.target

[Service]
ExecStart=/usr/bin/sleep infinity

[Install]
WantedBy=graphical-session.target
"""


def test_the_session_starts_its_services_on_its_display_and_stops_them_with_it(g: Guest, s: Session) -> str | None:
    s.stop()
    units = "~/.config/systemd/user"
    g.push(SESSION_TARGET, "/tmp/demiurge-wayland.target")
    # Lingering keeps the user manager, and the runtime directory the
    # compositor's socket is in, past each ssh command's login.
    s.sh(f"sudo -n loginctl enable-linger $USER && mkdir -p {units} ~/.config/demiurge"
         f" && mv /tmp/demiurge-wayland.target {units}/"
         f" && cat > {units}/stand-in.service << 'EOF'\n{STAND_IN}EOF\n"
         f"cat > ~/.config/demiurge/config.toml << 'EOF'\n{CONFIG}\nEOF\n"
         "systemctl --user daemon-reload && systemctl --user enable stand-in.service")

    def environment() -> dict[str, str]:
        out = s.sh("systemctl --user show-environment").stdout
        return dict(line.split("=", 1) for line in out.splitlines() if "=" in line)

    def active(unit: str) -> bool:
        return s.sh(f"systemctl --user is-active --quiet {unit}").returncode == 0

    try:
        # The ssh login's own runtime directory, the user manager's.
        s.sh("LIBSEAT_BACKEND=seatd DEMIURGE_WL_NO_XWAYLAND=1 setsid -f demiurge-wl-tty > /tmp/wl.log 2>&1 < /dev/null")
        if not wait_for(lambda: "WAYLAND_DISPLAY" in environment(), 20):
            return "the session never handed the user manager its display:\n" + s.log()
        display = environment()["WAYLAND_DISPLAY"]
        if s.sh(f'test -S "$XDG_RUNTIME_DIR/{display}"').returncode != 0:
            return f"the user manager was handed {display}, which is not the compositor's socket"
        if not wait_for(lambda: active("stand-in.service"), 10):
            return ("graphical-session.target never started the service it wants:\n"
                    + s.sh("systemctl --user status demiurge-wayland.target graphical-session.target"
                           " stand-in.service").stdout)
        pid = s.sh("systemctl --user show -p MainPID --value stand-in.service").stdout.strip()
        started = s.sh(f"cat /proc/{pid}/environ").stdout.split("\0")
        if f"WAYLAND_DISPLAY={display}" not in started:
            return f"the service started without the session's display: {[e for e in started if 'DISPLAY' in e]}"
        s.sh("pkill -TERM -x demiurge-wl-tty")
        if not wait_for(lambda: not s.running(), 10):
            return "SIGTERM did not end the session:\n" + s.log()
        if not wait_for(lambda: not active("stand-in.service"), 10):
            return "the service outlived the session"
        if active("graphical-session.target"):
            return "graphical-session.target outlived the session"
        if "WAYLAND_DISPLAY" in environment():
            return f"the session left its display in the user manager: {environment()['WAYLAND_DISPLAY']}"
        return None
    finally:
        s.sh(f"systemctl --user disable --now stand-in.service; rm -f {units}/stand-in.service"
             f" {units}/demiurge-wayland.target; systemctl --user daemon-reload;"
             " systemctl --user unset-environment WAYLAND_DISPLAY; sudo -n loginctl disable-linger $USER")


TESTS = [test_protocol_suite, test_the_bar_draws_over_a_tiled_client, test_a_gpu_buffer_reaches_the_screen,
         test_a_fullscreen_gpu_window_goes_straight_to_the_screen, test_an_hdr_window_on_an_sdr_monitor_stays_sdr,
         test_mouse_look_locks_the_pointer_and_hears_the_mouse, test_a_confined_pointer_stays_in_its_window,
         test_a_viewport_scales_a_small_buffer_to_the_window,
         test_explicit_sync_holds_a_frame_until_its_acquire_point, test_the_pointer_enters_the_window_under_it,
         test_a_window_mapping_under_a_still_pointer_is_entered, test_a_click_focuses_the_window_under_it,
         test_a_menu_opens_in_front_and_a_click_outside_closes_it, test_the_lock_takes_the_keyboard_and_keybinds_stop,
         test_super_r_runs_what_is_typed_into_the_bar, test_the_bar_shows_the_title_and_a_notification_over_it,
         test_super_drag_moves_a_window_out_of_its_tile, test_the_cursor_hides_while_the_pointer_is_still,
         test_a_minimized_window_comes_back_from_the_tag_menu, test_the_tag_menu_shows_a_windows_icon,
         test_the_screenshot_bind_writes_the_screen, test_a_display_entry_sets_the_monitors_mode,
         test_a_floating_window_wears_its_title_bar, test_alt_tab_shows_the_windows_while_held,
         test_an_idle_session_blanks_powers_off_and_wakes, test_the_bar_clock_shows_every_second,
         test_closed_x11_windows_give_their_memory_back, test_an_idle_session_stays_under_its_wakeup_budget,
         test_a_window_mapped_on_a_hidden_tag_marks_it_at_once,
         test_the_wayland_cherrypie_places_a_window_with_no_xwayland,
         test_an_x11_window_tiles_and_takes_the_keyboard, test_an_x11_game_asking_fullscreen_fills_the_screen,
         test_an_x11_games_mode_change_is_emulated_on_its_surface,
         test_medusa_freezes_saves_copies_pins_and_writes_the_screen,
         test_gnosis_lists_trashes_and_mounts_a_drive,
         test_a_wayland_client_pastes_what_another_copied, test_the_clipboard_crosses_between_x11_and_wayland,
         test_a_monitor_resized_unplugged_and_plugged_back_is_followed,
         test_a_second_monitor_gets_a_tag_and_bar_of_its_own, test_quarks_runtime_draws_and_takes_the_keyboard_and_pointer,
         test_a_vt_switch_leaves_and_comes_back_drawn,
         test_the_session_starts_its_services_on_its_display_and_stops_them_with_it]


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("-k", metavar="NAME", help="run only tests whose name contains NAME")
    p.add_argument("--tty", type=Path, help="another demiurge-wl-tty build to run against")
    a = p.parse_args()
    if not guest.PROVISIONED.exists():
        guest.die("no provisioned image; run: tests/qemu/guest.py provision")
    tty, probe, x11_probe, protocol = build()
    tty = a.tty or tty
    guest.overlay(guest.RUN, guest.PROVISIONED)
    failures = []
    with Guest(guest.RUN, gpu="virtio", qmp=True) as g:
        g.push(tty, "/tmp/demiurge-wl-tty")
        g.push(probe, "/tmp/wl-probe")
        g.push(x11_probe, "/tmp/x11-probe")
        g.push(BUILD_DIR / "release" / "cherrypie-wl", "/tmp/cherrypie-wl")
        g.push(BUILD_DIR / "release" / "medusa-wl", "/tmp/medusa-wl")
        g.push(BUILD_DIR / "release" / "gnosis-wl", "/tmp/gnosis-wl")
        g.push(BUILD_DIR / "release" / "examples" / "fake_udisks", "/tmp/fake-udisks")
        for name, binary in protocol.items():
            g.push(binary, f"/tmp/wl-{name}")
        g.ssh("sudo install -m 0755 /tmp/demiurge-wl-tty /usr/local/bin/demiurge-wl-tty"
              " && sudo install -m 0755 /tmp/wl-probe /usr/local/bin/wl-probe"
              " && sudo install -m 0755 /tmp/x11-probe /usr/local/bin/x11-probe"
              " && sudo install -m 0755 /tmp/cherrypie-wl /usr/local/bin/cherrypie-wl"
              " && sudo install -m 0755 /tmp/medusa-wl /usr/local/bin/medusa-wl"
              " && sudo install -m 0755 /tmp/gnosis-wl /usr/local/bin/gnosis-wl"
              " && sudo install -m 0755 /tmp/fake-udisks /usr/local/bin/fake-udisks"
              f" && chmod 0755 {' '.join(f'/tmp/wl-{name}' for name in protocol)}")
        s = Session(g)
        for test in TESTS:
            if a.k and a.k not in test.__name__:
                continue
            log("INFO", f"{test.__name__}")
            try:
                err = test(g, s)
            except Exception as e:  # a broken harness step fails the test, not the run
                err = f"{type(e).__name__}: {e}"
            if err and SHOT.exists():
                kept = CACHE / f"{test.__name__}.ppm"
                SHOT.replace(kept)
                err += f"\nlast screendump: {kept}"
            s.stop()
            if err:
                log("ERROR", f"{test.__name__}: {err}")
                failures.append(test.__name__)
            else:
                log("INFO", f"{test.__name__}: ok")
    total = sum(1 for t in TESTS if not a.k or a.k in t.__name__)
    log("INFO" if not failures else "ERROR", f"{total - len(failures)}/{total} passed")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
