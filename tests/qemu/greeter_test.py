#!/usr/bin/env python3
"""
The greeter in the QEMU guest, each stack's in turn: greetd starts cage and
that stack's gordian_knot --login on vt1, from the greetd config its
installer writes; a wrong password shows greetd's error and asks again; the
right one starts that stack's session as the user who logged in,
demiurge-wl-tty for the Wayland stack and startx with demiurge for the X11
one.

    tests/qemu/greeter_test.py          (after tests/qemu/guest.py provision)
"""

import os
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import guest  # noqa: E402
from guest import BUILD_DIR, CACHE, REPO, TEST_PASSWORD, TEST_USER, Guest, log  # noqa: E402

PREFIX = "/usr/local/lib/demiurge"
# Each stack's session, as its greeter starts it: The process to look for.
SESSIONS = {"wayland": "demiurge-wl-tty", "x11": "demiurge"}
SHOT = CACHE / "greeter.ppm"
BG = (0x12, 0x12, 0x12)
KEYS = {"-": "minus", " ": "spc"}


def wait_for(pred, timeout: float, step: float = 0.5) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if pred():
            return True
        time.sleep(step)
    return pred()


# Each stack's greeter and window manager: (built, installed) pairs.
def build() -> list[tuple[Path, str]]:
    env = dict(os.environ, CARGO_TARGET_DIR=str(BUILD_DIR))
    subprocess.run(["cargo", "build", "--release", "-p", "gordian_knot-wl", "-p", "demiurge-wl",
                    "-p", "gordian_knot-x11", "-p", "demiurge-x11", "--bin", "gordian_knot-wl",
                    "--bin", "demiurge-wl-tty", "--bin", "gordian_knot", "--bin", "demiurge"],
                   cwd=REPO, env=env, check=True)
    release = BUILD_DIR / "release"
    return [(release / "gordian_knot-wl", f"{PREFIX}/wayland/bin/gordian_knot"),
            (release / "demiurge-wl-tty", f"{PREFIX}/wayland/bin/demiurge-wl-tty"),
            (release / "gordian_knot", f"{PREFIX}/x11/bin/gordian_knot"),
            (release / "demiurge", f"{PREFIX}/x11/bin/demiurge")]


def typed(g: Guest, text: str, enter: bool = True) -> None:
    for c in text:
        g.qmp.keys(KEYS.get(c, c))
        time.sleep(0.05)
    time.sleep(0.2)
    if enter:
        g.qmp.keys("ret")


# The screen's lower half, where the USER panel is: What the greeter asks,
# what is typed and the status, without the SYSTEM panel's ticking clock.
def user_half(g: Guest) -> bytes:
    w, h, data = g.qmp.screendump(SHOT)
    return data[(h // 2) * w * 3:]


# The rows of pixels where two of those halves differ.
def changed_rows(a: bytes, b: bytes, w: int) -> list[int]:
    row = w * 3
    return [y for y in range(len(a) // row) if a[y * row:(y + 1) * row] != b[y * row:(y + 1) * row]]


def test(g: Guest, stack: str) -> str | None:
    def sh(cmd: str) -> subprocess.CompletedProcess:
        return g.ssh(cmd, check=False, capture=True)

    def journal() -> str:
        return sh("sudo journalctl -b -u greetd --no-pager -o cat | tail -n 30").stdout

    # greetd's config as the installer writes it, the greeter the stack's own.
    config = CACHE / "greetd.toml"
    config.write_text((REPO / "contrib" / "greetd" / "config.toml").read_text()
                      .replace("/usr/local/bin/gordian_knot", f"{PREFIX}/{stack}/bin/gordian_knot"))
    g.push(config, "/tmp/greetd.toml")
    g.ssh("sudo install -D -m 0644 /tmp/greetd.toml /etc/greetd/config.toml")
    sh("sudo systemctl restart greetd")
    if not wait_for(lambda: sh("pgrep -x cage && pgrep -f -- 'gordian_knot --login'").returncode == 0, 20):
        return f"greetd never started cage and the greeter:\n{journal()}"
    if sh("cat /sys/class/tty/tty0/active").stdout.strip() != "tty1":
        return f"the greeter is not on vt1 but {sh('cat /sys/class/tty/tty0/active').stdout.strip()}"
    # The panels drawn: The background at a corner, more than it in the middle.
    if not wait_for(lambda: len(set(user_half(g)[::3])) > 2, 10):
        return "the greeter drew no panels"
    w, h, data = g.qmp.screendump(SHOT)
    if tuple(data[:3]) != BG:
        return f"the greeter's background is {tuple(data[:3])}, not the lock's {BG}"
    asking = user_half(g)
    # Enter on no username: The STATUS row alone changes. The wrong
    # password's error must be another text than this one.
    g.qmp.keys("ret")
    if not wait_for(lambda: user_half(g) != asking, 5):
        return "Enter on no username showed nothing"
    time.sleep(0.5)
    required = user_half(g)

    # A wrong password: The username asked for again, the screen as it
    # first was but for greetd's error in STATUS, the row below those the
    # username and the password are typed into.
    typed(g, TEST_USER)
    time.sleep(0.5)
    typed(g, "not-the-password", enter=False)
    fields = changed_rows(user_half(g), asking, w)
    if not fields:
        return "the typed username and password did not show"
    g.qmp.keys("ret")

    def error_shown() -> bool:
        shown = user_half(g)
        rows = changed_rows(shown, asking, w)
        return bool(rows) and min(rows) > max(fields) and shown != required
    if not wait_for(error_shown, 15):
        return f"the greeter did not ask again with greetd's error shown:\n{journal()}"
    # Nothing comes after the error, as a key held through PAM's delay and
    # repeated once it answered would bring.
    time.sleep(2)
    if user_half(g) == required:
        return "the greeter took a key nobody pressed after the wrong password"
    session = SESSIONS[stack]
    if sh(f"pgrep -u {TEST_USER} -x {session}").returncode == 0:
        return "a wrong password started the session"

    # The right one starts the stack's session as the user.
    typed(g, TEST_USER)
    time.sleep(0.5)
    typed(g, TEST_PASSWORD)
    if not wait_for(lambda: sh(f"pgrep -u {TEST_USER} -x {session}").returncode == 0, 30):
        return f"the right password started no {session} as {TEST_USER}:\n{journal()}"
    if not wait_for(lambda: sh("pgrep -x cage").returncode != 0, 10):
        return "the greeter outlived the login"
    # Gone again for the next stack's turn.
    sh(f"sudo systemctl stop greetd; sudo loginctl terminate-user {TEST_USER}; sleep 2")
    return None


def main() -> int:
    if not guest.PROVISIONED.exists():
        guest.die("no provisioned image; run: tests/qemu/guest.py provision")
    binaries = build()
    guest.overlay(guest.RUN, guest.PROVISIONED)
    failures = []
    with Guest(guest.RUN, gpu="virtio", qmp=True) as g:
        for built, installed in binaries:
            g.push(built, f"/tmp/{built.name}")
            mode = "-o root -g root -m 4755" if installed.endswith("gordian_knot") else "-m 0755"
            g.ssh(f"sudo install -D {mode} /tmp/{built.name} {installed}")
        # The sessions' entry points, as the installers link them.
        g.ssh(f"sudo ln -sfn {PREFIX}/wayland/bin/demiurge-wl-tty /usr/local/bin/demiurge-wl-tty"
              f" && sudo ln -sfn {PREFIX}/x11/bin/demiurge /usr/local/bin/demiurge")
        for stack in ("wayland", "x11"):
            name = f"test_the_{stack}_greeter_logs_in_through_greetd_in_cage"
            failure = test(g, stack)
            if failure:
                log("ERROR", f"{name}: {failure}")
                failures.append(name)
            else:
                log("INFO", f"{name}: ok")
    log("INFO" if not failures else "ERROR", f"{2 - len(failures)}/2 passed")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
