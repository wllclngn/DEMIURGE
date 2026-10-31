#!/usr/bin/env python3
"""
DEMIURGE's two installers in the QEMU guest: Both stacks installed side by
side from this tree's release binaries, the Wayland session running its own
ABRAXAS and cherrypie under its own target with its own binaries first on
its children's PATH, and either stack uninstalled leaving the other whole.

    tests/qemu/install_test.py          (after tests/qemu/guest.py provision)
"""

import os
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import guest  # noqa: E402
from guest import BUILD_DIR, REPO, Guest, log  # noqa: E402

PACKAGES = {
    "x11": ["demiurge-x11", "gordian_knot-x11", "abraxas-x11", "cherrypie-x11", "medusa-x11", "gnosis-x11"],
    "wayland": ["demiurge-wl", "gordian_knot-wl", "abraxas-wl", "cherrypie-wl", "medusa-wl", "gnosis-wl"],
}
# What the installers read from the source tree besides the binaries.
SOURCE = ["installer.py", "x11/install.py", "wayland/install.py", "config.default.toml",
          "contrib/systemd/demiurge-x11.target", "contrib/systemd/demiurge-wayland.target",
          "contrib/systemd/abraxas@.service", "contrib/systemd/cherrypie@.service",
          "contrib/greetd/config.toml"]
PREFIX = "/usr/local/lib/demiurge"
UNITS = "~/.config/systemd/user"
BINARIES = {
    "x11": ["demiurge", "gordian_knot", "abraxas", "cherrypie", "medusa", "gnosis"],
    "wayland": ["demiurge-wl-tty", "gordian_knot", "abraxas", "cherrypie", "medusa", "gnosis"],
}


def wait_for(pred, timeout: float, step: float = 0.5) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if pred():
            return True
        time.sleep(step)
    return pred()


def build() -> list[Path]:
    env = dict(os.environ, CARGO_TARGET_DIR=str(BUILD_DIR))
    packages = [arg for p in PACKAGES["x11"] + PACKAGES["wayland"] for arg in ("-p", p)]
    subprocess.run(["cargo", "build", "--release", *packages], cwd=REPO, env=env, check=True)
    release = BUILD_DIR / "release"
    built = ["demiurge", "gordian_knot", "abraxas", "cherrypie", "medusa", "gnosis",
             "demiurge-wl-tty", "gordian_knot-wl", "abraxas-wl", "cherrypie-wl", "medusa-wl", "gnosis-wl"]
    return [release / b for b in built]


class Run:
    def __init__(self, g: Guest):
        self.g = g

    def sh(self, cmd: str) -> subprocess.CompletedProcess:
        return self.g.ssh(cmd, check=False, capture=True)

    def ok(self, cmd: str) -> bool:
        return self.sh(cmd).returncode == 0

    def install(self, stack: str, command: str = "install") -> subprocess.CompletedProcess:
        return self.sh(f"cd /tmp/demiurge-src && python3 {stack}/install.py {command} -y --skip-build --non-usa")


def check_installed(r: Run, stack: str) -> str | None:
    for name in BINARIES[stack]:
        if not r.ok(f"test -x {PREFIX}/{stack}/bin/{name}"):
            return f"{PREFIX}/{stack}/bin/{name} was not installed"
    mode = r.sh(f"stat -c %a {PREFIX}/{stack}/bin/gordian_knot").stdout.strip()
    if mode != "4755":
        return f"{stack}'s gordian_knot is mode {mode}, not setuid root"
    wm = BINARIES[stack][0]
    link = r.sh(f"readlink /usr/local/bin/{wm}").stdout.strip()
    if link != f"{PREFIX}/{stack}/bin/{wm}":
        return f"/usr/local/bin/{wm} links to {link!r}"
    for name in ("abraxas", "cherrypie"):
        if not r.ok(f"systemctl --user is-enabled --quiet {name}@{stack}.service"):
            return f"{name}@{stack}.service was not enabled"
    return None


def test(g: Guest) -> str | None:
    r = Run(g)
    # Lingering keeps the user manager past each ssh command's login, as a
    # logged-in user's is.
    if not r.ok("sudo -n loginctl enable-linger $USER && mkdir -p ~/.config/abraxas"
                " && printf '[location]\\nlatitude = 51.48\\nlongitude = 0.0\\n' > ~/.config/abraxas/config.toml"):
        return "the guest's user manager could not be kept"

    for stack in ("x11", "wayland"):
        out = r.install(stack)
        if out.returncode != 0:
            return f"{stack}/install.py failed:\n{out.stdout}{out.stderr}"
        if (e := check_installed(r, stack)) is not None:
            return e
    if (e := check_installed(r, "x11")) is not None:
        return "the Wayland install disturbed the X11 stack: " + e

    # The Wayland session from its /usr/local/bin link: Its target starts its
    # own ABRAXAS and cherrypie, and its children find its binaries first.
    r.sh("cat > ~/.config/demiurge/config.toml << 'EOF'\n[startup]\n"
         "commands = [\"sh -c 'command -v gordian_knot medusa > /tmp/which'\"]\nEOF")
    r.sh("LIBSEAT_BACKEND=seatd DEMIURGE_WL_NO_XWAYLAND=1 setsid -f demiurge-wl-tty > /tmp/wl.log 2>&1 < /dev/null")
    try:
        for unit in ("abraxas@wayland.service", "cherrypie@wayland.service"):
            if not wait_for(lambda: r.ok(f"systemctl --user is-active --quiet {unit}"), 20):
                return (f"the Wayland session never ran {unit}:\n"
                        + r.sh(f"systemctl --user status demiurge-wayland.target {unit} --no-pager").stdout)
        if r.ok("systemctl --user is-active --quiet abraxas@x11.service"):
            return "the Wayland session ran the X11 stack's ABRAXAS"
        exe = r.sh("readlink /proc/$(systemctl --user show -p MainPID --value cherrypie@wayland.service)/exe").stdout.strip()
        if exe != f"{PREFIX}/wayland/bin/cherrypie":
            return f"cherrypie@wayland runs {exe!r}"
        if not wait_for(lambda: r.ok("test -s /tmp/which"), 10):
            return "the session's startup command never ran"
        which = r.sh("cat /tmp/which").stdout.split()
        if which != [f"{PREFIX}/wayland/bin/gordian_knot", f"{PREFIX}/wayland/bin/medusa"]:
            return f"the Wayland session's children find {which!r} first"
    finally:
        r.sh("pkill -x demiurge-wl-tty; sleep 1")

    out = r.install("x11", "uninstall")
    if out.returncode != 0:
        return f"x11/install.py uninstall failed:\n{out.stdout}{out.stderr}"
    if r.ok(f"test -e {PREFIX}/x11") or r.ok("test -e /usr/local/bin/demiurge"):
        return "the X11 stack's files outlived its uninstall"
    if (e := check_installed(r, "wayland")) is not None:
        return "the X11 uninstall disturbed the Wayland stack: " + e
    for shared in ("/etc/pam.d/gordian_knot", f"{UNITS}/abraxas@.service", f"{UNITS}/cherrypie@.service"):
        if not r.ok(f"test -e {shared}"):
            return f"{shared}, which the Wayland stack still uses, went with the X11 stack"

    out = r.install("wayland", "uninstall")
    if out.returncode != 0:
        return f"wayland/install.py uninstall failed:\n{out.stdout}{out.stderr}"
    for gone in (PREFIX, "/usr/local/bin/demiurge-wl-tty", "/etc/pam.d/gordian_knot",
                 f"{UNITS}/abraxas@.service", f"{UNITS}/cherrypie@.service", f"{UNITS}/demiurge-wayland.target"):
        if r.ok(f"test -e {gone}"):
            return f"{gone} outlived the last stack's uninstall"
    if not r.ok("test -e ~/.config/demiurge/config.toml"):
        return "the config went with the last stack"
    return None


def main() -> int:
    if not guest.PROVISIONED.exists():
        guest.die("no provisioned image; run: tests/qemu/guest.py provision")
    binaries = build()
    guest.overlay(guest.RUN, guest.PROVISIONED)
    with Guest(guest.RUN, gpu="virtio", qmp=True) as g:
        g.ssh("mkdir -p /tmp/demiurge-build/release /tmp/demiurge-src/x11 /tmp/demiurge-src/wayland"
              " /tmp/demiurge-src/contrib/systemd /tmp/demiurge-src/contrib/greetd")
        for b in binaries:
            g.push(b, f"/tmp/demiurge-build/release/{b.name}")
        for f in SOURCE:
            g.push(REPO / f, f"/tmp/demiurge-src/{f}")
        failure = test(g)
    if failure:
        log("ERROR", f"test_both_stacks_install_side_by_side: {failure}")
        log("ERROR", "0/1 passed")
        return 1
    log("INFO", "test_both_stacks_install_side_by_side: ok")
    log("INFO", "1/1 passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
