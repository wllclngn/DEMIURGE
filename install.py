#!/usr/bin/env python3
"""
DEMIURGE installer

DEMIURGE ships two session backends: an X11 window manager (`demiurge`,
the daily-driver) and a Wayland compositor (`demiurge-wl-tty`, the
backup). Both can be installed side-by-side; the display manager
shows whichever sessions are present and lets you pick at login.

The two stacks have separate file footprints (different binaries,
different desktop entries, different systemd units) so installing,
updating, or uninstalling one never touches the other.

Usage:
    ./install.py                # Install X11 stack (default)
    ./install.py --wayland      # Install Wayland stack instead
    ./install.py uninstall      # Remove X11 stack
    ./install.py uninstall --wayland   # Remove Wayland stack
    ./install.py status         # Show what's installed
    ./install.py deps           # Report system dependencies + pacman line
    ./install.py update         # Rebuild + reinstall the X11 stack
    ./install.py update --wayland      # Rebuild + reinstall Wayland

To get both: run `./install.py` then `./install.py --wayland`.

Running from an Arch install (arch-chroot), where everything is root
and there is no user session yet:

    ./install.py -y --user <name> --greetd --no-abraxas

--user names the account whose $HOME receives the config and the
systemd user units; as root the installer writes them directly and
chowns, instead of shelling out to sudo. --greetd wires the display
manager so the machine boots to a login screen on first boot.
"""

import argparse
import os
import pwd
import sys
import shutil
import subprocess
from pathlib import Path
from datetime import datetime


# CONFIGURATION

# X11 stack: the daily-driver session.
#   - demiurge binary at /usr/local/bin/demiurge
#   - xsessions desktop entry pointing the display manager at the
#     systemd user unit (which sets OZONE_PLATFORM=x11 for Chromium /
#     Electron children)
#   - user-level systemd unit
INSTALL_X11_BINARY = Path("/usr/local/bin/demiurge")
INSTALL_X11_DESKTOP = Path("/usr/share/xsessions/demiurge.desktop")

# MEDUSA: screenshot capture utility (X11). Plain non-setuid binary;
# installed alongside the X11 stack, invoked from the F12 / Print
# keybinds. Replaces the flameshot dependency.
INSTALL_MEDUSA_BINARY = Path("/usr/local/bin/medusa")

# GNOSIS: file manager (X11). Plain non-setuid binary; installed
# alongside the X11 stack. Replaces the pcmanfm dependency.
INSTALL_GNOSIS_BINARY = Path("/usr/local/bin/gnosis")

# Wayland stack: the backup session.
#   - demiurge-wl-tty binary at /usr/local/bin/demiurge-wl-tty
#     (smithay libseat / udev / drm / libinput backend)
#   - wayland-sessions desktop entry; the desktop file invokes the
#     binary directly, no systemd unit indirection
INSTALL_WL_BINARY = Path("/usr/local/bin/demiurge-wl-tty")
INSTALL_WL_DESKTOP = Path("/usr/share/wayland-sessions/demiurge.desktop")

# Shared across both stacks.
BUILD_DIR = Path("/tmp/demiurge-build")

# greetd is the display manager; gordian_knot --login is its greeter.
# DEMIURGE doesn't own /etc, so this is installed only on --greetd.
INSTALL_GREETD_CONFIG = Path("/etc/greetd/config.toml")

# GORDIAN KNOT -- VT + X11 + Wayland screen locker, plus greetd
# greeter (--login). Binary is setuid-root so the VT path can drive
# VT_LOCKSWITCH; the X11 / Wayland in-session paths drop privs before
# PAM. PAM service file tells libpam which stack to consult. Two
# user-level systemd units: daemon (idle watcher) + sleep (lock
# before suspend).
#
# gordian_knot is shared by both stacks: --x11 in an X11 session,
# --wl in a Wayland session, --vt as a fallback. Always installed
# alongside whichever stack you pick.
INSTALL_GK_BINARY = Path("/usr/local/bin/gordian_knot")
INSTALL_GK_PAM = Path("/etc/pam.d/gordian_knot")


# TARGET USER
#
# Everything above is root-owned and absolute. The config and the
# systemd *user* units are not: they live in the home directory of the
# account that will actually log in, which is not necessarily the
# account running this script. Under `arch-chroot` during an Arch
# install there is no such account logged in at all -- we're root, $HOME
# is /root, and the user was created minutes ago by useradd.
#
# resolve_target() binds the five home-relative paths below once the
# target user is known: --user wins, then $SUDO_USER, then whoever is
# running us. The values here are placeholders so `import install` and
# --help work without resolution.

TARGET_USER = ""
TARGET_HOME = Path.home()
TARGET_UID = os.getuid()
TARGET_GID = os.getgid()

INSTALL_X11_SERVICE = TARGET_HOME / ".config/systemd/user/demiurge.service"
INSTALL_CONFIG_DIR = TARGET_HOME / ".config/demiurge"
INSTALL_CONFIG = INSTALL_CONFIG_DIR / "config.toml"
INSTALL_GK_DAEMON_SERVICE = TARGET_HOME / ".config/systemd/user/gordian_knot-daemon.service"
INSTALL_GK_SLEEP_SERVICE = TARGET_HOME / ".config/systemd/user/gordian_knot-sleep.service"

# True when we hold root directly (arch-chroot, or a `sudo ./install.py`
# invocation). Privileged writes then skip sudo, and user-owned files
# get chowned to the target user after being written.
IS_ROOT = os.geteuid() == 0


def resolve_target(name: str | None) -> bool:
    """Bind the home-relative install paths to the target account.
    Returns False (with an error logged) if the account can't be
    resolved, which as root is fatal -- installing a user unit into
    /root is never what was meant."""
    global TARGET_USER, TARGET_HOME, TARGET_UID, TARGET_GID
    global INSTALL_X11_SERVICE, INSTALL_CONFIG_DIR, INSTALL_CONFIG
    global INSTALL_GK_DAEMON_SERVICE, INSTALL_GK_SLEEP_SERVICE

    candidate = name or os.environ.get("SUDO_USER") or os.environ.get("USER")
    try:
        pw = pwd.getpwnam(candidate) if candidate else pwd.getpwuid(os.getuid())
    except KeyError:
        log_error(f"No such user: {candidate}")
        return False

    if pw.pw_uid == 0 and not name:
        log_error("Refusing to install the session for root.")
        log_error("Pass --user <name> to name the account that will log in.")
        return False

    TARGET_USER = pw.pw_name
    TARGET_HOME = Path(pw.pw_dir)
    TARGET_UID = pw.pw_uid
    TARGET_GID = pw.pw_gid

    user_units = TARGET_HOME / ".config/systemd/user"
    INSTALL_X11_SERVICE = user_units / "demiurge.service"
    INSTALL_GK_DAEMON_SERVICE = user_units / "gordian_knot-daemon.service"
    INSTALL_GK_SLEEP_SERVICE = user_units / "gordian_knot-sleep.service"
    INSTALL_CONFIG_DIR = TARGET_HOME / ".config/demiurge"
    INSTALL_CONFIG = INSTALL_CONFIG_DIR / "config.toml"
    return True


def chown_to_target(path: Path) -> None:
    """Give a file we wrote as root back to the target user. No-op when
    we're not root -- the file is already theirs."""
    if not IS_ROOT:
        return
    try:
        os.chown(path, TARGET_UID, TARGET_GID)
    except OSError as e:
        log_warn(f"chown {path}: {e}")


def chown_tree_to_target(root: Path) -> None:
    """chown -R for a directory we created as root."""
    if not IS_ROOT or not root.exists():
        return
    chown_to_target(root)
    for entry in root.rglob("*"):
        chown_to_target(entry)


# EMBEDDED FILE CONTENTS
#
# .desktop and .service files are small, declarative, and only ever read
# by this installer. Keeping them as separate static files in the source
# tree means every sync / copy / branch switch can desync them -- which
# already bit us once (demiurge.desktop went missing from the deploy
# copy between v0.3.0 and v0.4.0). Inlined here, install.py is the
# single source of truth; the repo loses four ghost files.

DEMIURGE_X11_DESKTOP_CONTENT = """\
[Desktop Entry]
Name=DEMIURGE
Comment=Opinionated X11 desktop
Exec=systemctl --user start --wait demiurge.service
TryExec=/usr/local/bin/demiurge
Type=Application
DesktopNames=DEMIURGE
"""

DEMIURGE_X11_SERVICE_CONTENT = """\
[Unit]
Description=DEMIURGE X11 window manager

[Service]
Type=exec
ExecStart=/usr/local/bin/demiurge
Restart=no
# Prefer X11 for Chromium/Electron children so they don't try Wayland
# and exit when no $WAYLAND_DISPLAY is present.
Environment=OZONE_PLATFORM=x11
"""

DEMIURGE_WL_DESKTOP_CONTENT = """\
[Desktop Entry]
Name=DEMIURGE (Wayland)
Comment=Opinionated Wayland desktop
Exec=/usr/local/bin/demiurge-wl-tty
TryExec=/usr/local/bin/demiurge-wl-tty
Type=Application
DesktopNames=DEMIURGE
"""



# LOGGING

def _timestamp() -> str:
    return datetime.now().strftime("[%H:%M:%S]")


def log_info(msg: str) -> None:
    print(f"{_timestamp()} [INFO]   {msg}")


def log_warn(msg: str) -> None:
    print(f"{_timestamp()} [WARN]   {msg}")


def log_error(msg: str) -> None:
    print(f"{_timestamp()} [ERROR]  {msg}")


# COMMAND EXECUTION

def run_cmd(cmd: list, cwd: Path | None = None, env: dict | None = None) -> int:
    print(f">>> {' '.join(cmd)}")
    result = subprocess.run(cmd, cwd=cwd, env=env)
    return result.returncode


def run_cmd_capture(cmd: list, cwd: Path | None = None,
                    env: dict | None = None) -> tuple[int, str, str]:
    result = subprocess.run(cmd, capture_output=True, text=True, cwd=cwd, env=env)
    return result.returncode, result.stdout, result.stderr


def run_cmd_sudo(cmd: list) -> int:
    """Run a command with root privilege. Already root (arch-chroot, or
    the whole script under sudo) means running it directly -- sudo may
    not even be installed in a freshly pacstrapped system."""
    return run_cmd(cmd if IS_ROOT else ["sudo"] + cmd)


def popen_sudo(cmd: list, **kwargs) -> subprocess.Popen:
    return subprocess.Popen(cmd if IS_ROOT else ["sudo"] + cmd, **kwargs)


def check_sudo_available(interactive: bool) -> bool:
    """Fail fast rather than blocking forever on a password prompt that
    nobody is there to answer. A scripted run (-y, or no tty) needs
    sudo to be passwordless; an interactive one just needs sudo to
    exist."""
    if IS_ROOT:
        return True
    if not shutil.which("sudo"):
        log_error("sudo not found and not running as root.")
        log_error("Run as root and pass --user <name>, or install sudo.")
        return False
    if interactive:
        return True
    ret, _, _ = run_cmd_capture(["sudo", "-n", "true"])
    if ret != 0:
        log_error("Non-interactive install needs passwordless sudo.")
        log_error("Run as root with --user <name>, or authenticate first")
        log_error("with `sudo -v` and re-run.")
        return False
    return True


def atomic_sudo_install(flags: list, src: Path, dst: Path) -> int:
    """Atomic binary install. GNU `install` opens with O_TRUNC and is not
    interrupt-safe: a SIGKILL between open and write leaves a 0-byte stub
    where the working binary used to be. That has burned us (GORDIAN KNOT
    auto-lock fired mid-install, user force-rebooted, returned to empty
    /usr/local/bin/demiurge).

    We write to <dst>.new, then rename. Rename within the same filesystem
    is atomic -- crash between the two steps leaves <dst> untouched and a
    stale <dst>.new alongside, which the next install overwrites."""
    staging = Path(str(dst) + ".new")
    ret = run_cmd_sudo(["install"] + flags + [str(src), str(staging)])
    if ret != 0:
        return ret
    return run_cmd_sudo(["mv", "-f", str(staging), str(dst)])


def atomic_write_user_file(dst: Path, content: str) -> bool:
    """Write a user-owned file atomically via staging + rename. Creates
    the parent dir if needed, chowning anything it had to create back to
    the target user. Returns True on success."""
    try:
        missing = [p for p in (dst.parent, *dst.parent.parents)
                   if not p.exists()]
        dst.parent.mkdir(parents=True, exist_ok=True)
        for created in missing:
            chown_to_target(created)
        staging = Path(str(dst) + ".new")
        staging.write_text(content)
        os.replace(staging, dst)
        chown_to_target(dst)
        return True
    except OSError as e:
        log_error(f"write {dst}: {e}")
        return False


def atomic_write_root_file(dst: Path, content: str, mode: str = "0644") -> bool:
    """Write a root-owned file atomically: `install -m MODE /dev/stdin`
    into <dst>.new, then rename. `install` sets the mode as it creates
    the file, so the staging path is never briefly world-writable the
    way a `tee` + `chmod` pair leaves it -- which matters most for the
    PAM stack, where a truncated or open file is the difference between
    unlocking the session and not."""
    staging = Path(str(dst) + ".new")
    try:
        run_cmd_sudo(["mkdir", "-p", str(dst.parent)])
        proc = popen_sudo(
            ["install", "-m", mode, "/dev/stdin", str(staging)],
            stdin=subprocess.PIPE,
        )
        proc.communicate(input=content.encode("utf-8"))
        if proc.returncode != 0:
            log_error(f"write {staging}: install exited {proc.returncode}")
            return False
    except OSError as e:
        log_error(f"write {staging}: {e}")
        return False
    return run_cmd_sudo(["mv", "-f", str(staging), str(dst)]) == 0


def user_bus_available() -> bool:
    """Whether the target user has a running systemd user manager. In an
    arch-chroot nobody is logged in, so there is no bus and every
    `systemctl --user` call is a guaranteed failure -- worth skipping
    loudly once instead of failing silently a dozen times."""
    if IS_ROOT:
        return False
    return Path(f"/run/user/{TARGET_UID}/bus").exists()


def systemctl_user(*args: str) -> int:
    if not user_bus_available():
        return 0
    ret, _, _ = run_cmd_capture(["systemctl", "--user", *args])
    return ret


def systemctl_system(*args: str) -> int:
    return run_cmd_sudo(["systemctl", *args])


# SYSTEM DEPENDENCIES
#
# Rust crates are cargo's problem; these are the C libraries and the
# runtime programs pacman has to have put on the disk first. Split three
# ways because the consequences differ: a missing BUILD library fails
# the cargo build outright, a missing RUNTIME program means the session
# installs and then can't start, and the Wayland set only matters for
# --wayland.
#
# Each entry: (pkg-config name or None, program name or None, pacman
# package, what it's for).

BUILD_DEPS = [
    ("cairo", None, "cairo", "Cairo: bar, lock screen, greeter, MEDUSA"),
    ("pango", None, "pango", "Pango text shaping"),
    ("pangocairo", None, "pango", "Pango/Cairo glue"),
    ("xkbcommon", None, "libxkbcommon", "keymap handling"),
    (None, "pkg-config", "pkgconf", "used by the cairo/pango build scripts"),
    (None, "cargo", "rust", "a working Rust toolchain (rustup needs a "
                            "`rustup default stable` first)"),
]

X11_RUNTIME_DEPS = [
    (None, "Xorg", "xorg-server", "the X server"),
    (None, "startx", "xorg-xinit", "how the greeter starts an X session"),
]

WAYLAND_BUILD_DEPS = [
    ("wayland-client", None, "wayland", "Wayland client protocol"),
    ("libinput", None, "libinput", "input devices in the TTY backend"),
    ("libseat", None, "seatd", "seat management in the TTY backend"),
    ("libdrm", None, "libdrm", "mode setting"),
    ("gbm", None, "mesa", "buffer allocation"),
    ("libudev", None, "systemd", "device enumeration"),
]

GREETD_DEPS = [
    (None, "greetd", "greetd", "the display manager"),
    (None, "cage", "cage", "kiosk compositor hosting the greeter"),
]


# Programs whose presence on disk isn't proof they work. /usr/bin/cargo
# is a rustup shim: it exists the moment `pacman -S rustup` lands and
# errors on every invocation until `rustup default stable` has run,
# which needs network. A which() check passes and the build then fails
# for a reason that has nothing to do with the build. The `rust` package
# ships a real toolchain instead, which is what provisioning should use.
DEP_PROBES = {
    "cargo": ["cargo", "--version"],
}


def dep_present(pc_name: str | None, program: str | None) -> bool:
    if program is not None:
        if shutil.which(program) is None:
            return False
        probe = DEP_PROBES.get(program)
        if probe is not None:
            ret, _, _ = run_cmd_capture(probe)
            if ret != 0:
                return False
    if pc_name is not None:
        ret, _, _ = run_cmd_capture(["pkg-config", "--exists", pc_name])
        if ret != 0:
            return False
    return True


def report_deps(deps: list, label: str) -> list:
    """Print one group's status. Returns the pacman packages that are
    missing, de-duplicated, in listed order."""
    print(f"  {label}:")
    missing = []
    for pc_name, program, package, purpose in deps:
        ok = dep_present(pc_name, program)
        mark = "present" if ok else "MISSING"
        print(f"    {package:<16} {mark:<8} {purpose}")
        if not ok and package not in missing:
            missing.append(package)
    print()
    return missing


def cmd_deps(args, source_dir: Path) -> bool:
    """Report what the build and the session need from pacman."""
    log_info("DEMIURGE system dependencies")
    print()

    if shutil.which("pkg-config") is None:
        log_warn("pkg-config missing; library checks below will all read")
        log_warn("MISSING. Install pkgconf and re-run for a true picture.")
        print()

    missing = report_deps(BUILD_DEPS, "Build")
    missing += [p for p in report_deps(X11_RUNTIME_DEPS, "X11 session")
                if p not in missing]
    missing += [p for p in report_deps(WAYLAND_BUILD_DEPS, "Wayland session")
                if p not in missing]
    missing += [p for p in report_deps(GREETD_DEPS, "Login (greetd)")
                if p not in missing]

    if missing:
        print("  pacman -S --needed " + " ".join(missing))
    else:
        print("  Everything DEMIURGE needs is present.")
    print()
    return not missing


def preflight_deps(wayland: bool, greetd: bool) -> bool:
    """Check only what this install actually needs. Build libraries are
    fatal; runtime programs warn, since a machine being provisioned may
    install them later in the same pacman transaction."""
    deps = list(BUILD_DEPS)
    if wayland:
        deps += WAYLAND_BUILD_DEPS
    missing = [pkg for pc, prog, pkg, _ in deps
               if not dep_present(pc, prog)]
    if missing:
        log_error("Missing build dependencies: " + " ".join(dict.fromkeys(missing)))
        log_error("pacman -S --needed " + " ".join(dict.fromkeys(missing)))
        log_error("Run `./install.py deps` for the full picture.")
        return False

    runtime = [] if wayland else list(X11_RUNTIME_DEPS)
    if greetd:
        runtime += GREETD_DEPS
    absent = [pkg for pc, prog, pkg, _ in runtime if not dep_present(pc, prog)]
    if absent:
        log_warn("Not installed yet, needed to start a session: "
                 + " ".join(dict.fromkeys(absent)))
    return True


# CONFIG VALIDATION

def validate_config(binary: Path, config_path: Path) -> bool:
    """Validate the user's config against the freshly-built schema.
    Used at install time to catch a stale config that would otherwise
    crash the session at first login.

    `binary` should accept `--check-config` and load the unified
    demiurge_core schema. gordian_knot does (it shares
    demiurge_core::config), and it always builds; the wayland
    compositor binaries don't have that flag. The caller passes
    BUILD_DIR/release/gordian_knot.

    Note: gordian_knot reads $HOME/.config/demiurge/config.toml
    directly and takes no path argument, so `config_path` is steered by
    handing the child a $HOME whose config dir is the one we mean --
    which is also what makes this work when we're root installing for
    somebody else."""
    if not binary.exists():
        # Binary not available yet (build hasn't run). Skip.
        return True
    log_info(f"Validating {config_path}")
    env = os.environ.copy()
    env["HOME"] = str(config_path.parent.parent.parent)
    ret, _, stderr = run_cmd_capture([str(binary), "--check-config"], env=env)
    if ret != 0 and stderr.strip():
        for line in stderr.strip().splitlines():
            print(f"         {line}")
    return ret == 0


def should_replace_config(args) -> bool:
    """Decide whether to back up and replace a broken config. --yes or
    non-interactive stdin both default to yes."""
    if getattr(args, "yes", False):
        return True
    try:
        response = input(
            "Back up the broken config and replace with shipped default? [Y/n]: "
        ).strip().lower()
        return response in ("", "y", "yes")
    except EOFError:
        log_info("Non-interactive -- defaulting to replace.")
        return True


# PROMPTS / PHASE ORCHESTRATION
#
# DEMIURGE ships as a workspace: the WM proper, GORDIAN KNOT, and two
# sibling daemons (ABRAXAS + cherrypie). Each subproject has its own
# install.py with its own concerns (binary paths, systemd units,
# config defaults). Rather than re-derive that logic here, this
# orchestrator prompts up-front for which sub-installs to run, then
# delegates by calling the subproject installers as subprocesses.
# Modeled on montauk/install.py's phase-and-prompt pattern.

def phase(name: str) -> None:
    """Print a phase banner. Called between major install steps."""
    print()
    log_info(f"PHASE: {name}")


def prompt_yn(question: str, default_yes: bool = True) -> bool:
    """Y/N prompt with EOF fallback. default_yes governs both the
    bracket display ([Y/n] vs [y/N]) and the "blank Enter" / EOF
    answer."""
    suffix = "[Y/n]" if default_yes else "[y/N]"
    try:
        while True:
            response = input(f"{question} {suffix}: ").strip().lower()
            if not response:
                return default_yes
            if response in ("y", "yes"):
                return True
            if response in ("n", "no"):
                return False
    except EOFError:
        print()
        return default_yes


def prompt_abraxas() -> bool:
    log_info("ABRAXAS: dynamic color-temperature daemon (replaces redshift).")
    log_info("  - Solar grayline + sigmoid transitions across the day")
    log_info("  - Optional NOAA cloud-cover weather awareness")
    log_info("  - Per-output gamma via X11 RandR / DRM / Wayland / GNOME D-Bus")
    print()
    return prompt_yn("Install ABRAXAS?", default_yes=True)


def prompt_cherrypie() -> bool:
    log_info("cherrypie: window-matching daemon (replaces devilspie / devilspie2).")
    log_info("  - TOML rules + regex over WM_CLASS / title / role / process")
    log_info("  - RandR-aware multi-monitor placement; hot-reload")
    print()
    return prompt_yn("Install cherrypie?", default_yes=True)


def resolve_abraxas(args) -> bool:
    """--abraxas: yes; --no-abraxas: no; -y: yes; otherwise prompt."""
    if args.no_abraxas:
        return False
    if args.abraxas:
        return True
    if args.yes:
        return True
    return prompt_abraxas()


def resolve_cherrypie(args) -> bool:
    if args.no_cherrypie:
        return False
    if args.cherrypie:
        return True
    if args.yes:
        return True
    return prompt_cherrypie()


def delegation_args(args) -> list[str]:
    """Flags every subproject installer understands, derived from how we
    were invoked. Without these a `-y` orchestration still stops dead on
    the subproject's own prompts, and a root install writes the
    subproject's files into /root."""
    extra = ["--user", TARGET_USER, "--target-dir", str(BUILD_DIR)]
    if getattr(args, "yes", False):
        extra.append("-y")
    if not user_bus_available():
        # No user manager to enable anything with. The units still get
        # installed; enabling them is the user's first-boot business.
        extra.append("--no-service")
    return extra


def delegate_install(installer: Path, extra_args: list[str]) -> bool:
    """Run a subproject installer as a subprocess. The subproject
    knows its own binary paths + systemd units; we just point at
    the installer and let it do its work."""
    if not installer.exists():
        log_error(f"Subproject installer not found: {installer}")
        return False
    log_info(f"Delegating to {installer}")
    cmd = [sys.executable, str(installer), "install"] + extra_args
    ret = run_cmd(cmd)
    return ret == 0


def install_abraxas(source_dir: Path, args) -> bool:
    # ABRAXAS is Rust-only as of v0.7.0 (the C23 source tree under
    # ABRAXAS/c23/ stays in place but is unwired from the install
    # path). The subproject installer handles its own NOAA prompt
    # and service setup.
    return delegate_install(source_dir / "ABRAXAS" / "install.py",
                            delegation_args(args))


def install_cherrypie(source_dir: Path, args) -> bool:
    return delegate_install(source_dir / "cherrypie" / "install.py",
                            delegation_args(args))


def update_siblings(source_dir: Path, args) -> bool:
    """Run each installed sibling daemon's own `update`, which rebuilds,
    reinstalls and restarts it when its sources are newer than its binary.
    A sibling that was never installed stays that way."""
    ok = True
    for name, installer in (("abraxas", "ABRAXAS/install.py"), ("cherrypie", "cherrypie/install.py")):
        if not (TARGET_HOME / ".local" / "bin" / name).exists():
            continue
        phase(f"{name} update")
        cmd = [sys.executable, str(source_dir / installer), "update"] + delegation_args(args)
        if run_cmd(cmd) != 0:
            log_warn(f"{name} update failed")
            ok = False
    return ok


def delegate_uninstall(installer: Path) -> bool:
    if not installer.exists():
        log_warn(f"Subproject installer not found (skipping): {installer}")
        return True
    log_info(f"Delegating uninstall to {installer}")
    ret = run_cmd([sys.executable, str(installer), "uninstall",
                   "--user", TARGET_USER])
    return ret == 0


# BUILD

def build_demiurge(source_dir: Path, expected_binary_name: str, wayland: bool) -> bool:
    """Cargo build the stack being installed. `expected_binary_name` is
    the binary we'll actually install (`demiurge` or `demiurge-wl-tty`);
    it's used for the post-build sanity check. The X11 stack builds
    demiurge-x11 (demiurge, gordian_knot, medusa, gnosis), abraxas and
    cherrypie; --wayland adds demiurge-wl. Building the whole workspace
    for X11 pulled in the Wayland crates, whose libseat / libinput the
    X11 preflight never asks for, so a fresh X11 machine failed here."""
    ret, _, _ = run_cmd_capture(["which", "cargo"])
    if ret != 0:
        log_error("cargo not found. Install Rust: https://rustup.rs")
        return False

    log_info("Building DEMIURGE workspace")

    env = os.environ.copy()
    env["CARGO_TARGET_DIR"] = str(BUILD_DIR)

    packages = ["demiurge-x11", "abraxas", "cherrypie"] + (["demiurge-wl"] if wayland else [])
    cargo_cmd = ["cargo", "build", "--release"] + [arg for p in packages for arg in ("-p", p)]
    print(f">>> CARGO_TARGET_DIR={BUILD_DIR} {' '.join(cargo_cmd)}")
    result = subprocess.run(cargo_cmd, cwd=source_dir, env=env)

    if result.returncode != 0:
        log_error("Build failed")
        return False

    binary = BUILD_DIR / "release" / expected_binary_name
    if not binary.exists():
        log_error(f"Build completed but {binary.name} not found")
        return False

    size = binary.stat().st_size
    log_info(f"Built: {binary} ({size // 1024} KB)")

    # A build run as root leaves a root-owned target dir that the user's
    # own later `./install.py update` can't write to. Hand it over now.
    # The subproject installers build into this same directory, so they
    # reuse every compiled dependency instead of paying for the whole
    # graph again.
    if IS_ROOT:
        log_info(f"Handing {BUILD_DIR} to {TARGET_USER}")
        chown_tree_to_target(BUILD_DIR)
    return True


def install_greetd(source_dir: Path, args) -> bool:
    """Install the greetd config and enable the service, so the machine
    boots to gordian_knot --login instead of a bare TTY. Opt-in: this is
    the one thing DEMIURGE installs into /etc that isn't its own."""
    source = source_dir / "contrib" / "greetd" / "config.toml"
    if not source.exists():
        log_error(f"greetd config not found in source: {source}")
        return False

    if not shutil.which("greetd"):
        log_warn("greetd is not installed; writing the config anyway.")
        log_warn("pacman -S greetd cage, then: systemctl enable greetd.service")

    if INSTALL_GREETD_CONFIG.exists():
        backup = Path(str(INSTALL_GREETD_CONFIG) + ".bak")
        log_info(f"Backing up existing greetd config to {backup}")
        if run_cmd_sudo(["cp", "-a", str(INSTALL_GREETD_CONFIG), str(backup)]) != 0:
            log_error("Failed to back up the existing greetd config")
            return False

    log_info(f"Installing greetd config: {INSTALL_GREETD_CONFIG}")
    if not atomic_write_root_file(INSTALL_GREETD_CONFIG,
                                  source.read_text(), "0644"):
        log_error("Failed to install greetd config")
        return False

    # greetd's own package creates the `greeter` user; without it the
    # daemon starts and then fails to drop privileges for the greeter.
    try:
        pwd.getpwnam("greeter")
    except KeyError:
        log_warn("No `greeter` user; greetd will fail to start its greeter.")
        log_warn("The greetd package creates it -- install greetd first.")

    log_info("Enabling greetd.service")
    if systemctl_system("enable", "greetd.service") != 0:
        log_warn("Failed to enable greetd.service; enable it by hand.")
        return True

    log_info("The session greetd starts on login comes from")
    log_info(f"[gordian_knot].session_cmd in {INSTALL_CONFIG}")
    return True


# COMMANDS

def cmd_install(args, source_dir: Path) -> bool:
    wayland = bool(getattr(args, "wayland", False))
    stack_label = "Wayland" if wayland else "X11"

    # Resolve sub-component choices up front, before any work begins,
    # so the user can walk away once the prompts are answered. The
    # phases below run unattended.
    if not preflight_deps(wayland, args.greetd):
        return False

    do_abraxas = resolve_abraxas(args)
    do_cherrypie = resolve_cherrypie(args)
    print()
    log_info("Plan:")
    log_info(f"  - Target user: {TARGET_USER} ({TARGET_HOME})")
    log_info(f"  - DEMIURGE {stack_label} + GORDIAN KNOT: yes")
    log_info(f"  - ABRAXAS: {'yes' if do_abraxas else 'skip'}")
    log_info(f"  - cherrypie: {'yes' if do_cherrypie else 'skip'}")
    log_info(f"  - greetd login: {'yes' if args.greetd else 'skip'}")
    if not user_bus_available():
        log_info("  - No user systemd manager: units installed, not enabled")

    phase(f"DEMIURGE {stack_label} + GORDIAN KNOT")

    source_gk_binary = BUILD_DIR / "release" / "gordian_knot"
    source_config = source_dir / "config.default.toml"

    if wayland:
        source_binary = BUILD_DIR / "release" / "demiurge-wl-tty"
        install_binary = INSTALL_WL_BINARY
        install_desktop = INSTALL_WL_DESKTOP
        desktop_content = DEMIURGE_WL_DESKTOP_CONTENT
        session_kind = "wayland-session"
    else:
        source_binary = BUILD_DIR / "release" / "demiurge"
        install_binary = INSTALL_X11_BINARY
        install_desktop = INSTALL_X11_DESKTOP
        desktop_content = DEMIURGE_X11_DESKTOP_CONTENT
        session_kind = "xsession"

    if not build_demiurge(source_dir, source_binary.name, wayland):
        return False

    try:
        INSTALL_CONFIG_DIR.mkdir(parents=True, exist_ok=True)
        chown_to_target(INSTALL_CONFIG_DIR)
        if not wayland:
            # X11 path uses a systemd user unit; ensure parent dir exists.
            INSTALL_X11_SERVICE.parent.mkdir(parents=True, exist_ok=True)
    except OSError as e:
        log_error(f"Failed to create directories: {e}")
        return False

    # If a config already exists, validate it against gordian_knot
    # (which shares the demiurge_core schema and always has
    # --check-config) before preserving. A bad config would kill the
    # session at login.
    if INSTALL_CONFIG.exists():
        if validate_config(source_gk_binary, INSTALL_CONFIG):
            log_info(f"Config preserved: {INSTALL_CONFIG}")
        else:
            log_warn(f"Existing config fails validation: {INSTALL_CONFIG}")
            if should_replace_config(args):
                backup = INSTALL_CONFIG.with_suffix(".toml.bak")
                log_info(f"Backing up to {backup}")
                shutil.copy2(INSTALL_CONFIG, backup)
                chown_to_target(backup)
                if source_config.exists():
                    log_info(f"Writing fresh default config: {INSTALL_CONFIG}")
                    shutil.copy2(source_config, INSTALL_CONFIG)
                    chown_to_target(INSTALL_CONFIG)
                else:
                    log_warn("config.default.toml not found in source; not replaced")
            else:
                log_warn("Keeping broken config per user choice. The session will fail to start.")
    elif source_config.exists():
        log_info(f"Writing default config: {INSTALL_CONFIG}")
        shutil.copy2(source_config, INSTALL_CONFIG)
        chown_to_target(INSTALL_CONFIG)
    else:
        log_warn("config.default.toml not found in source")

    log_info(f"Installing binary: {install_binary}")
    ret = atomic_sudo_install(["-Dm755"], source_binary, install_binary)
    if ret != 0:
        log_error("Failed to install binary")
        return False

    log_info(f"Installing {session_kind} entry: {install_desktop}")
    if not atomic_write_root_file(install_desktop, desktop_content, "0644"):
        log_error(f"Failed to install {session_kind} entry")
        return False

    if not wayland:
        # X11 stack: install the systemd user unit that the xsession
        # entry's Exec= line invokes. Wayland skips this -- the
        # wayland-sessions desktop file calls the binary directly.
        log_info(f"Installing systemd unit: {INSTALL_X11_SERVICE}")
        if not atomic_write_user_file(INSTALL_X11_SERVICE, DEMIURGE_X11_SERVICE_CONTENT):
            log_error("Failed to install systemd unit")
            return False
        systemctl_user("daemon-reload")

        # MEDUSA: screenshot utility for the X11 session. Plain 0755
        # binary; the F12 / Print keybinds invoke it.
        source_medusa = BUILD_DIR / "release" / "medusa"
        if source_medusa.exists():
            log_info(f"Installing screenshot tool: {INSTALL_MEDUSA_BINARY}")
            if atomic_sudo_install(["-Dm755"], source_medusa, INSTALL_MEDUSA_BINARY) != 0:
                log_warn("Failed to install medusa; F12/Print keybinds won't work")
        else:
            log_warn("medusa binary not found in build output; skipping")

        # GNOSIS: file manager for the X11 session. Plain 0755 binary.
        source_gnosis = BUILD_DIR / "release" / "gnosis"
        if source_gnosis.exists():
            log_info(f"Installing file manager: {INSTALL_GNOSIS_BINARY}")
            if atomic_sudo_install(["-Dm755"], source_gnosis, INSTALL_GNOSIS_BINARY) != 0:
                log_warn("Failed to install gnosis; file manager won't be available")
        else:
            log_warn("gnosis binary not found in build output; skipping")

    # GORDIAN KNOT (screen locker + login greeter). Shared by both
    # stacks; gordian_knot's auto-detect picks --x11 / --wl / --vt at
    # invocation time based on $WAYLAND_DISPLAY / $DISPLAY.
    if not install_gordian_knot(source_dir, args):
        return False

    print()
    log_info(f"DEMIURGE {stack_label} + GORDIAN KNOT installed")
    log_info(f"Binary:       {install_binary}")
    log_info(f"Session:      {install_desktop}")
    if not wayland:
        log_info(f"Service:      {INSTALL_X11_SERVICE}")
        log_info(f"Screenshot:   {INSTALL_MEDUSA_BINARY}")
        log_info(f"File manager: {INSTALL_GNOSIS_BINARY}")
    log_info(f"Config:       {INSTALL_CONFIG}")
    log_info(f"Locker:       {INSTALL_GK_BINARY}")
    log_info(f"Locker PAM:   {INSTALL_GK_PAM}")
    log_info("Idle lock:    [input.idle] lock_seconds (demiurge starts the daemon)")

    # Sibling daemons. If the sub-install fails, we log+continue
    # rather than aborting the whole orchestration -- a partial
    # desktop is more useful than rolling everything back.
    abraxas_ok = True
    cherrypie_ok = True
    greetd_ok = True

    if do_abraxas:
        phase("ABRAXAS color-temperature daemon")
        abraxas_ok = install_abraxas(source_dir, args)
        if not abraxas_ok:
            log_warn("ABRAXAS install failed; continuing with rest")

    if do_cherrypie:
        phase("cherrypie window-matching daemon")
        cherrypie_ok = install_cherrypie(source_dir, args)
        if not cherrypie_ok:
            log_warn("cherrypie install failed; continuing with rest")

    if args.greetd:
        phase("greetd login")
        greetd_ok = install_greetd(source_dir, args)
        if not greetd_ok:
            log_warn("greetd wiring failed; the machine will boot to a TTY")

    print()
    log_info("Installation complete")
    if not (abraxas_ok and cherrypie_ok and greetd_ok):
        log_warn("Some sub-installs failed; see PHASE logs above for details.")
    if args.greetd:
        log_info("Reboot: greetd shows the GORDIAN KNOT login screen.")
    else:
        log_info("Log out and select 'DEMIURGE' in your display manager to launch.")
    return True


def install_gordian_knot(source_dir: Path, args) -> bool:
    """Install the GORDIAN KNOT binary, PAM file, and systemd units."""
    source_gk_binary = BUILD_DIR / "release" / "gordian_knot"

    if not source_gk_binary.exists():
        log_warn("GORDIAN KNOT binary not found; skipping locker install")
        return True

    # Binary: setuid-root (mode 4755). The lock holder becomes root fully
    # (grab / VT, PAM in forked children); the X11 renderer, the idle
    # daemon and the greeter drop to the user (see gordian_knot/privsep.rs).
    log_info(f"Installing locker: {INSTALL_GK_BINARY} (setuid-root)")
    ret = atomic_sudo_install(
        ["-Dm4755", "-o", "root", "-g", "root"],
        source_gk_binary, INSTALL_GK_BINARY,
    )
    if ret != 0:
        log_error("Failed to install GORDIAN KNOT binary")
        return False

    # PAM stack file. Minimal -- delegate to system-auth for the real
    # password machinery. Installed via sudo + tee so we control perms.
    log_info(f"Installing PAM stack: {INSTALL_GK_PAM}")
    if not atomic_write_root_file(INSTALL_GK_PAM, "auth include system-auth\n", "0644"):
        log_error("Failed to install PAM stack file")
        return False

    # The idle daemon (with lock-before-suspend through a logind delay
    # inhibitor) is now a session child demiurge starts when [input.idle]
    # lock_seconds is set. The two user units it replaces could not work:
    # graphical-session.target never starts under startx, and a user unit
    # cannot hook sleep.target. Retire them where an older install left them.
    retired = False
    for unit, path in (("gordian_knot-daemon.service", INSTALL_GK_DAEMON_SERVICE),
                       ("gordian_knot-sleep.service", INSTALL_GK_SLEEP_SERVICE)):
        if path.exists():
            log_info(f"Retiring {unit} (demiurge starts the daemon itself)")
            systemctl_user("disable", "--now", unit)
            path.unlink()
            retired = True
    if retired:
        systemctl_user("daemon-reload")

    log_info("Idle lock + lock-before-suspend: set [input.idle] lock_seconds in")
    log_info(f"{INSTALL_CONFIG}; demiurge starts gordian_knot --daemon at session start.")
    return True


def cmd_update(args, source_dir: Path) -> bool:
    wayland = bool(getattr(args, "wayland", False))
    install_binary = INSTALL_WL_BINARY if wayland else INSTALL_X11_BINARY
    stack_label = "Wayland" if wayland else "X11"
    log_info(f"Checking for updates ({stack_label})")

    if not install_binary.exists():
        log_info("Binary not installed -- running full install")
        return cmd_install(args, source_dir)

    needs_rebuild = False
    installed_mtime = install_binary.stat().st_mtime

    # Workspace-wide source check. Every member crate counts: ABRAXAS
    # and cherrypie ship binaries this installer is responsible for too,
    # and leaving them out meant edits there never triggered a rebuild.
    for crate in ("wayland", "core", "x11", "ABRAXAS/rust", "cherrypie"):
        crate_dir = source_dir / crate
        if not crate_dir.exists():
            continue
        for src_file in crate_dir.glob("**/*.rs"):
            if src_file.stat().st_mtime > installed_mtime:
                needs_rebuild = True
                log_info(f"Source updated: {src_file.relative_to(source_dir)}")
                break
        if needs_rebuild:
            break
    if not needs_rebuild:
        for cargo_toml in source_dir.glob("**/Cargo.toml"):
            if cargo_toml.stat().st_mtime > installed_mtime:
                needs_rebuild = True
                log_info(f"Source updated: {cargo_toml.relative_to(source_dir)}")
                break

    # The sibling daemons answer to their own binaries' age, not the WM's:
    # A stale cherrypie can sit behind an up-to-date demiurge.
    siblings_ok = update_siblings(source_dir, args)

    if not needs_rebuild:
        log_info("Already up to date")
        return siblings_ok

    if wayland:
        source_binary = BUILD_DIR / "release" / "demiurge-wl-tty"
        install_desktop = INSTALL_WL_DESKTOP
        desktop_content = DEMIURGE_WL_DESKTOP_CONTENT
        session_kind = "wayland-session"
    else:
        source_binary = BUILD_DIR / "release" / "demiurge"
        install_desktop = INSTALL_X11_DESKTOP
        desktop_content = DEMIURGE_X11_DESKTOP_CONTENT
        session_kind = "xsession"

    if not build_demiurge(source_dir, source_binary.name, wayland):
        return False

    log_info(f"Installing binary: {install_binary}")
    ret = atomic_sudo_install(["-Dm755"], source_binary, install_binary)
    if ret != 0:
        log_error("Failed to install binary")
        return False

    # Always rewrite the desktop entry so bug fixes / embedded-content
    # edits in this installer take effect on update. Cheap and
    # atomic.
    log_info(f"Refreshing {session_kind} entry: {install_desktop}")
    if not atomic_write_root_file(install_desktop, desktop_content, "0644"):
        log_error(f"Failed to refresh {session_kind} entry")
        return False

    if not wayland:
        log_info(f"Refreshing systemd unit: {INSTALL_X11_SERVICE}")
        if not atomic_write_user_file(INSTALL_X11_SERVICE, DEMIURGE_X11_SERVICE_CONTENT):
            log_error("Failed to refresh systemd unit")
            return False
        systemctl_user("daemon-reload")

        # Refresh the X11 utilities (medusa, gnosis) so update picks up
        # their new builds too. Best-effort: a missing build artifact
        # warns but doesn't fail the update.
        for util_name, dest in (("medusa", INSTALL_MEDUSA_BINARY), ("gnosis", INSTALL_GNOSIS_BINARY)):
            src = BUILD_DIR / "release" / util_name
            if src.exists():
                log_info(f"Refreshing {util_name}: {dest}")
                if atomic_sudo_install(["-Dm755"], src, dest) != 0:
                    log_warn(f"Failed to refresh {util_name}")
            else:
                log_warn(f"{util_name} binary not found in build output; skipping")

    # GORDIAN KNOT is shared by both stacks and was rebuilt by the
    # workspace build above. Reinstall it here or the locker stays
    # frozen at whatever the first install put on disk, forever.
    if not install_gordian_knot(source_dir, args):
        return False

    # Validate the config against the binary that just got built. An
    # update is exactly when the schema moves out from under a config
    # that was fine yesterday, and the symptom is a session that dies
    # at the next login.
    if INSTALL_CONFIG.exists():
        if validate_config(BUILD_DIR / "release" / "gordian_knot", INSTALL_CONFIG):
            log_info(f"Config still valid: {INSTALL_CONFIG}")
        else:
            log_warn(f"Config fails validation against the new build: {INSTALL_CONFIG}")
            log_warn("Fix it before logging out, or the session won't start.")

    log_info("Update complete")
    log_info("Log out and back in to pick up the new binary.")
    return siblings_ok


def cmd_uninstall(args, source_dir: Path) -> bool:
    wayland = bool(getattr(args, "wayland", False))
    stack_label = "Wayland" if wayland else "X11"

    # Resolve sibling-daemon choices up front. Same flag semantics as
    # install: --no-X skips, --X explicit yes, -y yes-to-all,
    # otherwise prompt. "Yes" here means "uninstall it"; we let the
    # subproject's own uninstaller handle its bits.
    do_abraxas = resolve_abraxas(args)
    do_cherrypie = resolve_cherrypie(args)

    log_info(f"Uninstalling DEMIURGE {stack_label}")
    log_info("Plan:")
    log_info(f"  - DEMIURGE {stack_label}: yes")
    log_info(f"  - ABRAXAS: {'yes' if do_abraxas else 'skip'}")
    log_info(f"  - cherrypie: {'yes' if do_cherrypie else 'skip'}")

    phase(f"DEMIURGE {stack_label}")

    removed = False

    if wayland:
        if INSTALL_WL_BINARY.exists():
            log_info(f"Removing {INSTALL_WL_BINARY}")
            if run_cmd_sudo(["rm", "-f", str(INSTALL_WL_BINARY)]) == 0:
                removed = True
        if INSTALL_WL_DESKTOP.exists():
            log_info(f"Removing {INSTALL_WL_DESKTOP}")
            if run_cmd_sudo(["rm", "-f", str(INSTALL_WL_DESKTOP)]) == 0:
                removed = True
    else:
        # Stop the X11 systemd unit before removing it.
        systemctl_user("disable", "--now", "demiurge.service")
        if INSTALL_X11_BINARY.exists():
            log_info(f"Removing {INSTALL_X11_BINARY}")
            if run_cmd_sudo(["rm", "-f", str(INSTALL_X11_BINARY)]) == 0:
                removed = True
        if INSTALL_X11_DESKTOP.exists():
            log_info(f"Removing {INSTALL_X11_DESKTOP}")
            if run_cmd_sudo(["rm", "-f", str(INSTALL_X11_DESKTOP)]) == 0:
                removed = True
        if INSTALL_X11_SERVICE.exists():
            log_info(f"Removing {INSTALL_X11_SERVICE}")
            INSTALL_X11_SERVICE.unlink()
            removed = True
        if INSTALL_MEDUSA_BINARY.exists():
            log_info(f"Removing {INSTALL_MEDUSA_BINARY}")
            if run_cmd_sudo(["rm", "-f", str(INSTALL_MEDUSA_BINARY)]) == 0:
                removed = True
        if INSTALL_GNOSIS_BINARY.exists():
            log_info(f"Removing {INSTALL_GNOSIS_BINARY}")
            if run_cmd_sudo(["rm", "-f", str(INSTALL_GNOSIS_BINARY)]) == 0:
                removed = True
        systemctl_user("daemon-reload")

    # GORDIAN KNOT is shared. Only remove it if NEITHER stack is left
    # installed; otherwise the surviving stack still needs the locker.
    other_stack_present = (
        INSTALL_X11_BINARY.exists() if wayland else INSTALL_WL_BINARY.exists()
    )
    if not other_stack_present:
        log_info("Both DEMIURGE stacks gone; removing shared GORDIAN KNOT")
        for unit in ("gordian_knot-daemon.service", "gordian_knot-sleep.service"):
            systemctl_user("disable", "--now", unit)
        if INSTALL_GK_BINARY.exists():
            log_info(f"Removing {INSTALL_GK_BINARY}")
            if run_cmd_sudo(["rm", "-f", str(INSTALL_GK_BINARY)]) == 0:
                removed = True
        if INSTALL_GK_PAM.exists():
            log_info(f"Removing {INSTALL_GK_PAM}")
            if run_cmd_sudo(["rm", "-f", str(INSTALL_GK_PAM)]) == 0:
                removed = True
        for svc in (INSTALL_GK_DAEMON_SERVICE, INSTALL_GK_SLEEP_SERVICE):
            if svc.exists():
                log_info(f"Removing {svc}")
                svc.unlink()
                removed = True
        systemctl_user("daemon-reload")
    else:
        log_info("Other stack still installed; keeping shared GORDIAN KNOT")

    if not removed:
        log_warn(f"No DEMIURGE {stack_label} files found")
    else:
        log_info(f"DEMIURGE {stack_label} uninstalled")

    if INSTALL_CONFIG_DIR.exists():
        log_info(f"Config directory preserved: {INSTALL_CONFIG_DIR}")
    if INSTALL_GREETD_CONFIG.exists():
        log_info(f"greetd config left in place: {INSTALL_GREETD_CONFIG}")

    if do_abraxas:
        phase("ABRAXAS color-temperature daemon")
        if not delegate_uninstall(source_dir / "ABRAXAS" / "install.py"):
            log_warn("ABRAXAS uninstall reported issues; see PHASE log above")

    if do_cherrypie:
        phase("cherrypie window-matching daemon")
        if not delegate_uninstall(source_dir / "cherrypie" / "install.py"):
            log_warn("cherrypie uninstall reported issues; see PHASE log above")

    print()
    log_info("Uninstall complete")
    return True


def cmd_status(args, source_dir: Path) -> bool:
    log_info("DEMIURGE status")
    print()
    print(f"  User:     {TARGET_USER} ({TARGET_HOME})")
    print()

    x11_binary_ok = INSTALL_X11_BINARY.exists()
    x11_desktop_ok = INSTALL_X11_DESKTOP.exists()
    x11_service_ok = INSTALL_X11_SERVICE.exists()

    print("  X11 stack:")
    print(f"    Binary:   {INSTALL_X11_BINARY}")
    print(f"              {'installed' if x11_binary_ok else 'NOT INSTALLED'}")
    if x11_binary_ok:
        size = INSTALL_X11_BINARY.stat().st_size
        print(f"              {size // 1024} KB")
    print(f"    Session:  {INSTALL_X11_DESKTOP}")
    print(f"              {'installed' if x11_desktop_ok else 'NOT INSTALLED'}")
    print(f"    Service:  {INSTALL_X11_SERVICE}")
    print(f"              {'installed' if x11_service_ok else 'NOT INSTALLED'}")
    medusa_ok = INSTALL_MEDUSA_BINARY.exists()
    print(f"    Medusa:   {INSTALL_MEDUSA_BINARY}")
    print(f"              {'installed' if medusa_ok else 'NOT INSTALLED'}")
    gnosis_ok = INSTALL_GNOSIS_BINARY.exists()
    print(f"    Gnosis:   {INSTALL_GNOSIS_BINARY}")
    print(f"              {'installed' if gnosis_ok else 'NOT INSTALLED'}")
    print()

    wl_binary_ok = INSTALL_WL_BINARY.exists()
    wl_desktop_ok = INSTALL_WL_DESKTOP.exists()

    print("  Wayland stack:")
    print(f"    Binary:   {INSTALL_WL_BINARY}")
    print(f"              {'installed' if wl_binary_ok else 'NOT INSTALLED'}")
    if wl_binary_ok:
        size = INSTALL_WL_BINARY.stat().st_size
        print(f"              {size // 1024} KB")
    print(f"    Session:  {INSTALL_WL_DESKTOP}")
    print(f"              {'installed' if wl_desktop_ok else 'NOT INSTALLED'}")
    print()

    config_ok = INSTALL_CONFIG.exists()
    print(f"  Config:   {INSTALL_CONFIG}")
    print(f"            {'exists' if config_ok else 'NOT FOUND'}")
    print()

    gk_binary_ok = INSTALL_GK_BINARY.exists()
    gk_pam_ok = INSTALL_GK_PAM.exists()

    print(f"  Locker:      {INSTALL_GK_BINARY}")
    print(f"               {'installed' if gk_binary_ok else 'NOT INSTALLED'}")
    if gk_binary_ok:
        mode = oct(INSTALL_GK_BINARY.stat().st_mode)[-4:]
        print(f"               mode {mode} (setuid-root expected: 4755)")
    print()
    print(f"  PAM stack:   {INSTALL_GK_PAM}")
    print(f"               {'installed' if gk_pam_ok else 'NOT INSTALLED'}")
    print()
    print("  Idle lock:   [input.idle] lock_seconds > 0 in the config;")
    print("               demiurge starts gordian_knot --daemon at session start")
    print()
    greetd_ok = INSTALL_GREETD_CONFIG.exists()
    print(f"  greetd:      {INSTALL_GREETD_CONFIG}")
    print(f"               {'installed' if greetd_ok else 'NOT INSTALLED'}")
    print()

    overall_installed = x11_binary_ok or wl_binary_ok
    print(f"  Overall: {'INSTALLED' if overall_installed else 'NOT INSTALLED'}")
    return overall_installed


# MAIN

def main() -> int:
    parser = argparse.ArgumentParser(
        description="Install DEMIURGE (X11 default; --wayland for the Wayland stack)",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Commands:
  (default)   Build and install
  uninstall   Remove installed files (config preserved)
  status      Show installation status (both stacks)
  deps        Report system dependencies and the pacman line
  update      Rebuild and install if source is newer

Stack selection:
  By default acts on the X11 stack. Pass --wayland to act on the
  Wayland stack instead. status always shows both.

Examples:
  ./install.py                       # Install X11 stack
  ./install.py --wayland             # Install Wayland stack
  ./install.py deps                  # What pacman still owes us
  ./install.py update                # Rebuild + reinstall X11
  ./install.py update --wayland      # Rebuild + reinstall Wayland
  ./install.py status                # Show both stacks
  ./install.py uninstall             # Remove X11 stack only
  ./install.py uninstall --wayland   # Remove Wayland stack only

From an Arch install, inside arch-chroot:
  ./install.py -y --user <name> --greetd --no-abraxas

To get both stacks installed: run install twice (default + --wayland).
GORDIAN KNOT is shared and only removed when both stacks are gone.
"""
    )

    parser.add_argument("command", nargs="?", default="install",
                       choices=["install", "uninstall", "status", "deps", "update"],
                       help="Command to run (default: install)")
    parser.add_argument("--user", metavar="NAME",
                       help="Account whose $HOME receives the config and the "
                            "systemd user units. Required when running as "
                            "root; otherwise defaults to $SUDO_USER or you.")
    parser.add_argument("--greetd", action="store_true",
                       help="Install contrib/greetd/config.toml to /etc and "
                            "enable greetd.service, so the machine boots to "
                            "the GORDIAN KNOT login screen")
    parser.add_argument("-y", "--yes", action="store_true",
                       help="Assume yes to prompts (non-interactive installs)")
    parser.add_argument("--wayland", action="store_true",
                       help="Act on the Wayland stack (demiurge-wl-tty) "
                            "instead of the default X11 stack (demiurge)")

    # Per-component flags. Each pair is mutex; absence = prompt
    # interactively. -y treats absence as yes.
    abraxas_grp = parser.add_mutually_exclusive_group()
    abraxas_grp.add_argument("--abraxas", action="store_true",
                             help="Install ABRAXAS (no prompt)")
    abraxas_grp.add_argument("--no-abraxas", action="store_true",
                             help="Skip ABRAXAS (no prompt)")

    cherrypie_grp = parser.add_mutually_exclusive_group()
    cherrypie_grp.add_argument("--cherrypie", action="store_true",
                               help="Install cherrypie (no prompt)")
    cherrypie_grp.add_argument("--no-cherrypie", action="store_true",
                               help="Skip cherrypie (no prompt)")

    args = parser.parse_args()
    source_dir = Path(__file__).parent.resolve()

    print()
    log_info("DEMIURGE installer")
    log_info(f"Source: {source_dir}")

    if args.command == "deps":
        print()
        return 0 if cmd_deps(args, source_dir) else 1

    if not resolve_target(args.user):
        return 1
    log_info(f"Target user: {TARGET_USER} ({TARGET_HOME})")
    if IS_ROOT:
        log_info("Running as root; privileged writes skip sudo.")

    # -y also means "nobody is watching", which is the case that has to
    # fail fast instead of parking on an unanswerable sudo prompt.
    if args.command in ("install", "update", "uninstall"):
        if not check_sudo_available(interactive=not args.yes and sys.stdin.isatty()):
            return 1
    print()

    commands = {
        "install": cmd_install,
        "uninstall": cmd_uninstall,
        "status": cmd_status,
        "update": cmd_update,
    }

    success = commands[args.command](args, source_dir)
    return 0 if success else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        print("\nInterrupted by user.")
        sys.exit(130)
