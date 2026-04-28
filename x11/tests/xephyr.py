#!/usr/bin/env python3
"""Launch DEMIURGE in a Xephyr nested X session with test windows."""

import subprocess
import time
import signal
import sys
import os

DISPLAY = ":1"
SCREEN = "2560x1440"

# Prefer the persistent install at ~/.local/bin (survives reboots since /tmp
# is tmpfs on most Arch installs). Fall back to the cargo build target for
# the case where the user just rebuilt and hasn't reinstalled yet.
BINARY = os.path.expanduser("~/.local/bin/demiurge")
if not os.path.isfile(BINARY):
    BINARY = "/tmp/demiurge-build/release/demiurge"

# NOTE: kitty does NOT work inside Xephyr (it requires hardware OpenGL,
# Xephyr is software-only). Use xterm/xeyes for nested testing -- kitty
# works fine in real sessions, just not nested.
#
# xeyes is included as a font-free sanity check: it doesn't need bitmap
# fonts, so if you see eyes following your cursor, the X11 mapping path
# is working independently of any font issue. xterm requires the legacy
# misc bitmap fonts (install xorg-fonts-misc) to render visibly.
TEST_APPS = ["xterm", "xeyes"]

procs = []

def cleanup(sig=None, frame=None):
    for p in reversed(procs):
        try:
            p.terminate()
        except OSError:
            pass
    for p in reversed(procs):
        try:
            p.wait(timeout=3)
        except subprocess.TimeoutExpired:
            p.kill()
    sys.exit(0)

signal.signal(signal.SIGINT, cleanup)
signal.signal(signal.SIGTERM, cleanup)

def main():
    if not os.path.isfile(BINARY):
        print(f"Binary not found: {BINARY}")
        print("Run: CARGO_TARGET_DIR=/tmp/demiurge-build cargo build --release")
        sys.exit(1)

    env = os.environ.copy()
    env["DISPLAY"] = DISPLAY

    # Start Xephyr
    xephyr = subprocess.Popen(
        ["Xephyr", DISPLAY, "-screen", SCREEN, "-ac", "-br"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    procs.append(xephyr)
    time.sleep(1)

    if xephyr.poll() is not None:
        print(f"Xephyr failed to start (display {DISPLAY} may be in use)")
        sys.exit(1)

    # Start DEMIURGE
    wm = subprocess.Popen([BINARY], env=env)
    procs.append(wm)
    time.sleep(1)

    if wm.poll() is not None:
        print("DEMIURGE failed to start")
        cleanup()

    # Spawn test windows. Leave stderr connected so launch errors are visible
    # in the parent terminal -- silent failures here would mask the real issue.
    for app in TEST_APPS:
        try:
            p = subprocess.Popen([app], env=env, stdout=subprocess.DEVNULL)
            procs.append(p)
            print(f"Spawned {app} (pid {p.pid})")
        except FileNotFoundError:
            print(f"WARN: '{app}' not found on PATH, skipping")
        time.sleep(0.5)

    print(f"DEMIURGE running on {DISPLAY} ({SCREEN})")
    print(f"Test windows: {', '.join(TEST_APPS)}")
    print()
    print("Ctrl+Shift        grab/ungrab keyboard (required for keybindings)")
    print("Alt+Shift+F4      quit DEMIURGE and tear down session")
    print("Ctrl+C            stop from terminal")

    # Wait for WM to exit
    wm.wait()
    cleanup()

if __name__ == "__main__":
    main()
