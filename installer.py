"""
DEMIURGE's installer: What x11/install.py and wayland/install.py share.

DEMIURGE ships two stacks side by side, each its own window manager and its
own binary of every component:

    /usr/local/lib/demiurge/x11/bin/      demiurge, gordian_knot, abraxas,
                                          cherrypie, medusa, gnosis
    /usr/local/lib/demiurge/wayland/bin/  demiurge-wl-tty, gordian_knot,
                                          abraxas, cherrypie

Each session puts its own directory first on its children's PATH, so a bind
both sessions share (`medusa gui`, the lock) runs that session's binary.
Each session also starts its own target, demiurge-x11.target or
demiurge-wayland.target, which wants that stack's ABRAXAS and cherrypie
through the unit templates (abraxas@x11.service, cherrypie@wayland.service).
/usr/local/bin keeps a symlink to each window manager for starting one by
hand.

Shared by both stacks: the config, GORDIAN KNOT's PAM file, the ABRAXAS and
cherrypie unit templates and greetd's config. Installing, updating or
uninstalling one stack never touches the other's files; the shared ones go
with the last stack left. ABRAXAS and cherrypie install with either stack
unasked; --no-abraxas and --no-cherrypie leave one out.

Running from an Arch install (arch-chroot), where everything is root and
there is no user session yet:

    x11/install.py -y --user <name> --greetd

--user names the account whose $HOME receives the config and the systemd
user units; as root the installer writes them directly and chowns, instead
of shelling out to sudo. --greetd wires the display manager so the machine
boots to a login screen on first boot.
"""

import argparse
import os
import pwd
import sys
import shutil
import subprocess
from dataclasses import dataclass, field
from pathlib import Path
from datetime import datetime


# CONFIGURATION

PREFIX = Path("/usr/local/lib/demiurge")
BIN_LINKS = Path("/usr/local/bin")
BUILD_DIR = Path("/tmp/demiurge-build")

# greetd is the display manager; gordian_knot --login is its greeter.
# DEMIURGE doesn't own /etc, so this is installed only on --greetd.
INSTALL_GREETD_CONFIG = Path("/etc/greetd/config.toml")

# GORDIAN KNOT's PAM service, shared by both stacks' lockers.
INSTALL_GK_PAM = Path("/etc/pam.d/gordian_knot")


@dataclass
class Stack:
    name: str
    label: str
    # The window manager's installed name, also its /usr/local/bin link.
    wm: str
    # Each binary cargo builds, by its name in the build output, and the
    # name it is installed under in the stack's directory.
    binaries: dict[str, str]
    # The packages built for it; ABRAXAS's is built on its own, for NOAA.
    packages: list[str]
    abraxas_package: str
    desktop: Path
    desktop_kind: str
    deps: list = field(default_factory=list)

    @property
    def bin_dir(self) -> Path:
        return PREFIX / self.name / "bin"

    @property
    def target(self) -> str:
        return f"demiurge-{self.name}.target"

    def installed(self, name: str) -> Path:
        return self.bin_dir / name

    def built(self, installed_name: str) -> Path:
        built = next(b for b, i in self.binaries.items() if i == installed_name)
        return BUILD_DIR / "release" / built


# TARGET USER
#
# Everything above is root-owned and absolute. The config and the
# systemd *user* units are not: they live in the home directory of the
# account that will actually log in, which is not necessarily the
# account running this script. Under `arch-chroot` during an Arch
# install there is no such account logged in at all -- we're root, $HOME
# is /root, and the user was created minutes ago by useradd.
#
# resolve_target() binds the home-relative paths below once the target
# user is known: --user wins, then $SUDO_USER, then whoever is running
# us. The values here are placeholders so `import installer` and --help
# work without resolution.

TARGET_USER = ""
TARGET_HOME = Path.home()
TARGET_UID = os.getuid()
TARGET_GID = os.getgid()

USER_UNITS = TARGET_HOME / ".config/systemd/user"
INSTALL_CONFIG_DIR = TARGET_HOME / ".config/demiurge"
INSTALL_CONFIG = INSTALL_CONFIG_DIR / "config.toml"

# True when we hold root directly (arch-chroot, or a `sudo` invocation).
# Privileged writes then skip sudo, and user-owned files get chowned to
# the target user after being written.
IS_ROOT = os.geteuid() == 0


def resolve_target(name: str | None) -> bool:
    """Bind the home-relative install paths to the target account.
    Returns False (with an error logged) if the account can't be
    resolved, which as root is fatal -- installing a user unit into
    /root is never what was meant."""
    global TARGET_USER, TARGET_HOME, TARGET_UID, TARGET_GID
    global USER_UNITS, INSTALL_CONFIG_DIR, INSTALL_CONFIG

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

    USER_UNITS = TARGET_HOME / ".config/systemd/user"
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
# copy between v0.3.0 and v0.4.0). Inlined here, the installer is the
# single source of truth. The units both stacks share live in
# contrib/systemd/, where the session code's tests read them too.

X11_DESKTOP_CONTENT = """\
[Desktop Entry]
Name=DEMIURGE
Comment=Opinionated X11 desktop
Exec=systemctl --user start --wait demiurge.service
TryExec=/usr/local/lib/demiurge/x11/bin/demiurge
Type=Application
DesktopNames=DEMIURGE
"""

X11_SERVICE_CONTENT = """\
[Unit]
Description=DEMIURGE X11 window manager

[Service]
Type=exec
ExecStart=/usr/local/lib/demiurge/x11/bin/demiurge
Restart=no
# Prefer X11 for Chromium/Electron children so they don't try Wayland
# and exit when no $WAYLAND_DISPLAY is present.
Environment=OZONE_PLATFORM=x11
"""

WAYLAND_DESKTOP_CONTENT = """\
[Desktop Entry]
Name=DEMIURGE (Wayland)
Comment=Opinionated Wayland desktop
Exec=/usr/local/lib/demiurge/wayland/bin/demiurge-wl-tty
TryExec=/usr/local/lib/demiurge/wayland/bin/demiurge-wl-tty
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
    ret = run_cmd_sudo(["install", "-D"] + flags + [str(src), str(staging)])
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


def unit_active(unit: str) -> bool:
    return user_bus_available() and systemctl_user("is-active", "--quiet", unit) == 0


# SYSTEM DEPENDENCIES
#
# Rust crates are cargo's problem; these are the C libraries and the
# runtime programs pacman has to have put on the disk first. Split
# because the consequences differ: a missing BUILD library fails the
# cargo build outright, a missing RUNTIME program means the session
# installs and then can't start.
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


X11 = Stack(
    name="x11",
    label="X11",
    wm="demiurge",
    binaries={"demiurge": "demiurge", "gordian_knot": "gordian_knot", "abraxas": "abraxas",
              "cherrypie": "cherrypie", "medusa": "medusa", "gnosis": "gnosis"},
    packages=["demiurge-x11", "gordian_knot-x11", "cherrypie-x11", "medusa-x11", "gnosis-x11"],
    abraxas_package="abraxas-x11",
    desktop=Path("/usr/share/xsessions/demiurge.desktop"),
    desktop_kind="xsession",
    deps=[],
)

WAYLAND = Stack(
    name="wayland",
    label="Wayland",
    wm="demiurge-wl-tty",
    binaries={"demiurge-wl-tty": "demiurge-wl-tty", "gordian_knot-wl": "gordian_knot",
              "abraxas-wl": "abraxas", "cherrypie-wl": "cherrypie", "medusa-wl": "medusa", "gnosis-wl": "gnosis"},
    packages=["demiurge-wl", "gordian_knot-wl", "cherrypie-wl", "medusa-wl", "gnosis-wl"],
    abraxas_package="abraxas-wl",
    desktop=Path("/usr/share/wayland-sessions/demiurge.desktop"),
    desktop_kind="wayland-session",
    deps=WAYLAND_BUILD_DEPS,
)

STACKS = {"x11": X11, "wayland": WAYLAND}


def other(stack: Stack) -> Stack:
    return WAYLAND if stack is X11 else X11


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


def cmd_deps(stack: Stack, args, source_dir: Path) -> bool:
    """Report what the build and the session need from pacman."""
    log_info(f"DEMIURGE {stack.label} system dependencies")
    print()

    if shutil.which("pkg-config") is None:
        log_warn("pkg-config missing; library checks below will all read")
        log_warn("MISSING. Install pkgconf and re-run for a true picture.")
        print()

    missing = report_deps(BUILD_DEPS, "Build")
    if stack is X11:
        missing += [p for p in report_deps(X11_RUNTIME_DEPS, "X11 session") if p not in missing]
    else:
        missing += [p for p in report_deps(WAYLAND_BUILD_DEPS, "Wayland session") if p not in missing]
    missing += [p for p in report_deps(GREETD_DEPS, "Login (greetd)") if p not in missing]

    if missing:
        print("  pacman -S --needed " + " ".join(missing))
    else:
        print("  Everything DEMIURGE needs is present.")
    print()
    return not missing


def preflight_deps(stack: Stack, greetd: bool) -> bool:
    """Check only what this install actually needs. Build libraries are
    fatal; runtime programs warn, since a machine being provisioned may
    install them later in the same pacman transaction."""
    deps = list(BUILD_DEPS) + stack.deps
    missing = [pkg for pc, prog, pkg, _ in deps
               if not dep_present(pc, prog)]
    if missing:
        log_error("Missing build dependencies: " + " ".join(dict.fromkeys(missing)))
        log_error("pacman -S --needed " + " ".join(dict.fromkeys(missing)))
        log_error("Run `install.py deps` for the full picture.")
        return False

    runtime = list(X11_RUNTIME_DEPS) if stack is X11 else []
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

    Either stack's gordian_knot takes `--check-config` and loads the
    unified demiurge_core schema, and it always builds.

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


def phase(name: str) -> None:
    """Print a phase banner. Called between major install steps."""
    print()
    log_info(f"PHASE: {name}")


# BUILD

def noaa(args) -> bool:
    """ABRAXAS's NOAA weather is US-only data: --non-usa leaves it out, -y
    keeps it, and otherwise the one question this installer asks decides."""
    if args.non_usa:
        return False
    if args.yes:
        return True
    print()
    try:
        while True:
            response = input(
                "ABRAXAS allows for utilization of US-specific NOAA data\n"
                "for weather calculations. Do you wish to enable NOAA\n"
                "data calculations? [Y/N]: "
            ).strip()
            if response in ("y", "Y"):
                return True
            if response in ("n", "N"):
                return False
    except EOFError:
        print()
        log_info("No input (non-interactive). Defaulting to non-USA build.")
        return False


def build(stack: Stack, source_dir: Path, args, with_noaa: bool) -> bool:
    """Cargo build the stack's packages, ABRAXAS's on its own for its NOAA
    feature. Only this stack's crates: Building the whole workspace for
    X11 pulled in the Wayland crates, whose libseat / libinput the X11
    preflight never asks for, so a fresh X11 machine failed here."""
    if args.skip_build:
        log_info(f"Using the binaries already in {BUILD_DIR}/release")
    else:
        if shutil.which("cargo") is None:
            log_error("cargo not found. Install Rust: https://rustup.rs")
            return False
        log_info(f"Building DEMIURGE {stack.label}")
        env = os.environ.copy()
        env["CARGO_TARGET_DIR"] = str(BUILD_DIR)
        cargo = ["cargo", "build", "--release"]
        steps = [cargo + [arg for p in stack.packages for arg in ("-p", p)]]
        if not args.no_abraxas:
            abraxas = cargo + ["-p", stack.abraxas_package, "--no-default-features"]
            steps.append(abraxas + (["--features", "noaa"] if with_noaa else []))
        for cmd in steps:
            print(f">>> CARGO_TARGET_DIR={BUILD_DIR} {' '.join(cmd)}")
            if subprocess.run(cmd, cwd=source_dir, env=env).returncode != 0:
                log_error("Build failed")
                return False

    wm = stack.built(stack.wm)
    if not wm.exists():
        log_error(f"{wm} not found")
        return False
    log_info(f"Built: {wm} ({wm.stat().st_size // 1024} KB)")

    # A build run as root leaves a root-owned target dir that the user's
    # own later `install.py update` can't write to. Hand it over now.
    if IS_ROOT and not args.skip_build:
        log_info(f"Handing {BUILD_DIR} to {TARGET_USER}")
        chown_tree_to_target(BUILD_DIR)
    return True


# INSTALL STEPS

def install_config(stack: Stack, source_dir: Path, args) -> bool:
    """The shared config: Written from config.default.toml when there is
    none, and validated against the new build when there is."""
    source_config = source_dir / "config.default.toml"
    try:
        INSTALL_CONFIG_DIR.mkdir(parents=True, exist_ok=True)
        chown_to_target(INSTALL_CONFIG_DIR)
    except OSError as e:
        log_error(f"Failed to create {INSTALL_CONFIG_DIR}: {e}")
        return False

    if not INSTALL_CONFIG.exists():
        if source_config.exists():
            log_info(f"Writing default config: {INSTALL_CONFIG}")
            shutil.copy2(source_config, INSTALL_CONFIG)
            chown_to_target(INSTALL_CONFIG)
        else:
            log_warn("config.default.toml not found in source")
        return True

    # A bad config would kill the session at login.
    if validate_config(stack.built("gordian_knot"), INSTALL_CONFIG):
        log_info(f"Config preserved: {INSTALL_CONFIG}")
        return True
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
    return True


def install_binaries(stack: Stack, args) -> bool:
    """The stack's binaries into its own directory, GORDIAN KNOT setuid
    root: The lock holder becomes root fully (grab / VT, PAM in forked
    children); the renderer, the idle daemon and the greeter drop to the
    user (core/gordian_knot's privsep.rs)."""
    for built_name, name in stack.binaries.items():
        if name == "abraxas" and args.no_abraxas or name == "cherrypie" and args.no_cherrypie:
            continue
        src = BUILD_DIR / "release" / built_name
        if not src.exists():
            log_warn(f"{src} not found in the build output; skipping {name}")
            continue
        dst = stack.installed(name)
        flags = ["-m4755", "-o", "root", "-g", "root"] if name == "gordian_knot" else ["-m755"]
        log_info(f"Installing {dst}" + (" (setuid-root)" if name == "gordian_knot" else ""))
        if atomic_sudo_install(flags, src, dst) != 0:
            log_error(f"Failed to install {dst}")
            return False
    return True


def link_entry_points(stack: Stack) -> bool:
    """/usr/local/bin links into the stack's directory: The window manager,
    for starting one by hand, and any name an older install left there as a
    file, so a session still running the old binaries finds its locker and
    tools until it is restarted."""
    names = [stack.wm] + [n for n in stack.binaries.values()
                          if n != stack.wm and (BIN_LINKS / n).is_file() and not (BIN_LINKS / n).is_symlink()]
    for name in names:
        target = stack.installed(name)
        if not target.exists():
            continue
        log_info(f"Linking {BIN_LINKS / name} -> {target}")
        if run_cmd_sudo(["ln", "-sfn", str(target), str(BIN_LINKS / name)]) != 0:
            log_error(f"Failed to link {BIN_LINKS / name}")
            return False
    return True


def install_session(stack: Stack, source_dir: Path) -> bool:
    """The session entry the display manager lists, the stack's target, and
    the X11 window manager's own unit."""
    content = X11_DESKTOP_CONTENT if stack is X11 else WAYLAND_DESKTOP_CONTENT
    log_info(f"Installing {stack.desktop_kind} entry: {stack.desktop}")
    if not atomic_write_root_file(stack.desktop, content, "0644"):
        log_error(f"Failed to install {stack.desktop_kind} entry")
        return False

    # One target per stack, where an older install had both share one.
    old_target = USER_UNITS / "demiurge-session.target"
    if old_target.exists():
        log_info(f"Retiring {old_target} for {stack.target}")
        old_target.unlink()
    units = {stack.target: (source_dir / "contrib" / "systemd" / stack.target).read_text()}
    if stack is X11:
        units["demiurge.service"] = X11_SERVICE_CONTENT
    for unit, content in units.items():
        log_info(f"Installing {USER_UNITS / unit}")
        if not atomic_write_user_file(USER_UNITS / unit, content):
            log_error(f"Failed to install {unit}")
            return False
    systemctl_user("daemon-reload")
    return True


def install_pam() -> bool:
    """GORDIAN KNOT's PAM service. Minimal -- delegate to system-auth for
    the real password machinery."""
    log_info(f"Installing PAM stack: {INSTALL_GK_PAM}")
    if not atomic_write_root_file(INSTALL_GK_PAM, "auth include system-auth\n", "0644"):
        log_error("Failed to install PAM stack file")
        return False
    return True


# Where an older install put ABRAXAS and cherrypie: Per-user binaries and one
# unit each for whichever session was running.
OLD_SIBLING_UNITS = ("abraxas.service", "cherrypie.service")


def install_siblings(stack: Stack, source_dir: Path, args, with_noaa: bool) -> bool:
    """ABRAXAS and cherrypie for this stack: Their unit templates, the
    stack's instances enabled under its target, ABRAXAS's ZIP database for
    a NOAA build, and the older per-user install retired. An instance this
    replaces while it ran is started at once, so the screen keeps its
    temperature until the next login."""
    wanted = [n for n in ("abraxas", "cherrypie") if not getattr(args, f"no_{n}")]
    if not wanted:
        return True

    replace_now = False
    for unit in OLD_SIBLING_UNITS:
        if (USER_UNITS / unit).exists():
            replace_now |= unit_active(unit)
            log_info(f"Retiring {unit} for its {stack.name} instance")
            systemctl_user("disable", "--now", unit)
            (USER_UNITS / unit).unlink()
    for name in ("abraxas", "cherrypie"):
        old = TARGET_HOME / ".local" / "bin" / name
        if old.exists():
            log_info(f"Removing {old}")
            old.unlink()

    for name in wanted:
        template = f"{name}@.service"
        log_info(f"Installing {USER_UNITS / template}")
        if not atomic_write_user_file(USER_UNITS / template,
                                      (source_dir / "contrib" / "systemd" / template).read_text()):
            log_error(f"Failed to install {template}")
            return False
    systemctl_user("daemon-reload")

    if "abraxas" in wanted:
        abraxas_config = TARGET_HOME / ".config" / "abraxas"
        abraxas_config.mkdir(parents=True, exist_ok=True)
        chown_to_target(abraxas_config)
        zipdb = abraxas_config / "us_zipcodes.bin"
        if with_noaa and not zipdb.exists():
            log_info(f"Installing ABRAXAS's ZIP database: {zipdb}")
            shutil.copy2(source_dir / "core" / "abraxas" / "us_zipcodes.bin", zipdb)
            chown_to_target(zipdb)

    ok = True
    for name in wanted:
        unit = f"{name}@{stack.name}.service"
        if not user_bus_available():
            log_warn(f"No user systemd manager for {TARGET_USER}; after first boot: "
                     f"systemctl --user enable {unit}")
            continue
        if systemctl_user("enable", unit) != 0:
            log_warn(f"Failed to enable {unit}; enable it by hand: systemctl --user enable {unit}")
            ok = False
            continue
        log_info(f"Enabled {unit}")
        if replace_now or unit_active(stack.target):
            systemctl_user("restart", unit)
    return ok


def install_greetd(stack: Stack, source_dir: Path) -> bool:
    """Install the greetd config and enable the service, so the machine
    boots to gordian_knot --login instead of a bare TTY. Opt-in: this is
    the one thing DEMIURGE installs into /etc that isn't its own. Its
    greeter is the stack's own gordian_knot."""
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
    content = source.read_text().replace("/usr/local/bin/gordian_knot", str(stack.installed("gordian_knot")))
    if not atomic_write_root_file(INSTALL_GREETD_CONFIG, content, "0644"):
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

def cmd_install(stack: Stack, args, source_dir: Path) -> bool:
    if not args.skip_build and not preflight_deps(stack, args.greetd):
        return False
    with_noaa = not args.no_abraxas and noaa(args)

    print()
    log_info("Plan:")
    log_info(f"  - Target user: {TARGET_USER} ({TARGET_HOME})")
    log_info(f"  - DEMIURGE {stack.label} into {stack.bin_dir}")
    log_info(f"  - ABRAXAS: {'skip' if args.no_abraxas else 'yes, NOAA ' + ('on' if with_noaa else 'off')}")
    log_info(f"  - cherrypie: {'skip' if args.no_cherrypie else 'yes'}")
    log_info(f"  - greetd login: {'yes' if args.greetd else 'skip'}")
    if not user_bus_available():
        log_info("  - No user systemd manager: units installed, not enabled")

    phase(f"DEMIURGE {stack.label}")
    steps = (
        lambda: build(stack, source_dir, args, with_noaa),
        lambda: install_config(stack, source_dir, args),
        lambda: install_binaries(stack, args),
        lambda: link_entry_points(stack),
        lambda: install_session(stack, source_dir),
        install_pam,
    )
    if not all(step() for step in steps):
        return False

    phase("ABRAXAS and cherrypie")
    siblings_ok = install_siblings(stack, source_dir, args, with_noaa)

    greetd_ok = True
    if args.greetd:
        phase("greetd login")
        greetd_ok = install_greetd(stack, source_dir)
        if not greetd_ok:
            log_warn("greetd wiring failed; the machine will boot to a TTY")

    print()
    log_info(f"DEMIURGE {stack.label} installed in {stack.bin_dir}")
    log_info(f"Session:  {stack.desktop}")
    log_info(f"Config:   {INSTALL_CONFIG}")
    log_info(f"Target:   {USER_UNITS / stack.target}")
    if not (siblings_ok and greetd_ok):
        log_warn("Some steps failed; see the PHASE logs above for details.")
    if args.greetd:
        log_info("Reboot: greetd shows the GORDIAN KNOT login screen.")
    else:
        log_info("Log out and select 'DEMIURGE' in your display manager to launch.")
    return True


def cmd_update(stack: Stack, args, source_dir: Path) -> bool:
    log_info(f"Checking for updates ({stack.label})")
    wm = stack.installed(stack.wm)
    if not wm.exists():
        log_info(f"{wm} not installed -- running a full install")
        return cmd_install(stack, args, source_dir)

    installed_mtime = wm.stat().st_mtime
    changed = next((f for f in [*source_dir.glob("**/src/**/*.rs"), *source_dir.glob("**/Cargo.toml"),
                                *source_dir.glob("contrib/systemd/*")]
                    if f.stat().st_mtime > installed_mtime), None)
    if changed is None and not args.skip_build:
        log_info("Already up to date")
        return True
    if changed is not None:
        log_info(f"Source updated: {changed.relative_to(source_dir)}")
    # The update keeps the NOAA choice the install made: Its ZIP database is
    # there when it chose NOAA.
    with_noaa = (not args.no_abraxas and not args.non_usa
                 and (TARGET_HOME / ".config/abraxas/us_zipcodes.bin").exists())
    steps = (
        lambda: build(stack, source_dir, args, with_noaa),
        lambda: install_binaries(stack, args),
        lambda: link_entry_points(stack),
        lambda: install_session(stack, source_dir),
        install_pam,
        lambda: install_siblings(stack, source_dir, args, with_noaa),
    )
    if not all(step() for step in steps):
        return False

    # An update is exactly when the schema moves out from under a config
    # that was fine yesterday, and the symptom is a session that dies at
    # the next login.
    if INSTALL_CONFIG.exists():
        if validate_config(stack.built("gordian_knot"), INSTALL_CONFIG):
            log_info(f"Config still valid: {INSTALL_CONFIG}")
        else:
            log_warn(f"Config fails validation against the new build: {INSTALL_CONFIG}")
            log_warn("Fix it before logging out, or the session won't start.")

    log_info("Update complete")
    log_info("Log out and back in to pick up the new binaries.")
    return True


def cmd_uninstall(stack: Stack, args, source_dir: Path) -> bool:
    last = not other(stack).installed(other(stack).wm).exists()
    log_info(f"Uninstalling DEMIURGE {stack.label}"
             + ("" if last else f"; the {other(stack).label} stack keeps what both share"))

    for name in ("abraxas", "cherrypie"):
        systemctl_user("disable", "--now", f"{name}@{stack.name}.service")
    if stack is X11:
        systemctl_user("disable", "--now", "demiurge.service")

    removed = False
    for link in BIN_LINKS.iterdir() if BIN_LINKS.exists() else []:
        if link.is_symlink() and Path(os.readlink(link)).parent == stack.bin_dir:
            log_info(f"Removing {link}")
            removed |= run_cmd_sudo(["rm", "-f", str(link)]) == 0
    for path in (stack.desktop, PREFIX / stack.name):
        if path.exists():
            log_info(f"Removing {path}")
            removed |= run_cmd_sudo(["rm", "-rf", str(path)]) == 0
    user_files = [USER_UNITS / stack.target] + ([USER_UNITS / "demiurge.service"] if stack is X11 else [])
    if last:
        user_files += [USER_UNITS / "abraxas@.service", USER_UNITS / "cherrypie@.service"]
        if INSTALL_GK_PAM.exists():
            log_info(f"Removing {INSTALL_GK_PAM}")
            removed |= run_cmd_sudo(["rm", "-f", str(INSTALL_GK_PAM)]) == 0
        if PREFIX.exists() and not any(PREFIX.iterdir()):
            run_cmd_sudo(["rmdir", str(PREFIX)])
    for f in user_files:
        if f.exists():
            log_info(f"Removing {f}")
            f.unlink()
            removed = True
    systemctl_user("daemon-reload")

    if not removed:
        log_warn(f"No DEMIURGE {stack.label} files found")
    else:
        log_info(f"DEMIURGE {stack.label} uninstalled")
    if INSTALL_CONFIG_DIR.exists():
        log_info(f"Config directory preserved: {INSTALL_CONFIG_DIR}")
    if INSTALL_GREETD_CONFIG.exists():
        log_info(f"greetd config left in place: {INSTALL_GREETD_CONFIG}")
    return True


def cmd_status(stack: Stack, args, source_dir: Path) -> bool:
    log_info("DEMIURGE status")
    print()
    print(f"  User:     {TARGET_USER} ({TARGET_HOME})")
    print()
    any_installed = False
    for s in (X11, WAYLAND):
        print(f"  {s.label} stack: {s.bin_dir}")
        for name in dict.fromkeys(s.binaries.values()):
            path = s.installed(name)
            state = "NOT INSTALLED"
            if path.exists():
                state = f"{path.stat().st_size // 1024} KB"
                if name == "gordian_knot":
                    state += f", mode {oct(path.stat().st_mode)[-4:]} (setuid-root expected: 4755)"
            print(f"    {name:<16} {state}")
        print(f"    session          {'installed' if s.desktop.exists() else 'NOT INSTALLED'}: {s.desktop}")
        print(f"    target           {'installed' if (USER_UNITS / s.target).exists() else 'NOT INSTALLED'}: {s.target}")
        for name in ("abraxas", "cherrypie"):
            unit = f"{name}@{s.name}.service"
            enabled = user_bus_available() and systemctl_user("is-enabled", "--quiet", unit) == 0
            print(f"    {unit:<24} {'enabled' if enabled else 'not enabled'}")
        print()
        any_installed |= s.installed(s.wm).exists()

    print(f"  Config:      {INSTALL_CONFIG} ({'exists' if INSTALL_CONFIG.exists() else 'NOT FOUND'})")
    print(f"  PAM stack:   {INSTALL_GK_PAM} ({'installed' if INSTALL_GK_PAM.exists() else 'NOT INSTALLED'})")
    print(f"  greetd:      {INSTALL_GREETD_CONFIG} "
          f"({'installed' if INSTALL_GREETD_CONFIG.exists() else 'NOT INSTALLED'})")
    print("  Idle lock:   [input.idle] lock_seconds > 0 in the config")
    print()
    print(f"  Overall: {'INSTALLED' if any_installed else 'NOT INSTALLED'}")
    return any_installed


# MAIN

def main(stack_name: str) -> int:
    stack = STACKS[stack_name]
    parser = argparse.ArgumentParser(
        prog=f"{stack.name}/install.py",
        description=f"Install DEMIURGE's {stack.label} stack",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=f"""
Commands:
  (default)   Build and install
  uninstall   Remove this stack (the config is preserved; what both
              stacks share goes with the last one)
  status      Show both stacks
  deps        Report system dependencies and the pacman line
  update      Rebuild and install if the source is newer

From an Arch install, inside arch-chroot:
  {stack.name}/install.py -y --user <name> --greetd
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
    parser.add_argument("--no-abraxas", action="store_true", help="Leave ABRAXAS out")
    parser.add_argument("--no-cherrypie", action="store_true", help="Leave cherrypie out")
    parser.add_argument("--non-usa", action="store_true",
                       help="Build ABRAXAS without NOAA's US-only weather data")
    parser.add_argument("--skip-build", action="store_true",
                       help=f"Install the binaries already in {BUILD_DIR}/release")

    args = parser.parse_args()
    source_dir = Path(__file__).parent.resolve()

    print()
    log_info(f"DEMIURGE {stack.label} installer")
    log_info(f"Source: {source_dir}")

    if args.command == "deps":
        print()
        return 0 if cmd_deps(stack, args, source_dir) else 1

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
    return 0 if commands[args.command](stack, args, source_dir) else 1


def run(stack_name: str) -> None:
    try:
        sys.exit(main(stack_name))
    except KeyboardInterrupt:
        print("\nInterrupted by user.")
        sys.exit(130)
