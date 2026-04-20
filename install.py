#!/usr/bin/env python3
"""
DEMIURGE installer

Builds and installs the DEMIURGE X11 window manager.

Usage:
    ./install.py              # Build and install (default)
    ./install.py uninstall    # Remove installed files
    ./install.py status       # Show installation status
    ./install.py update       # Rebuild and install if source is newer
"""

import argparse
import os
import sys
import shutil
import subprocess
from pathlib import Path
from datetime import datetime


# CONFIGURATION

INSTALL_BINARY = Path("/usr/local/bin/demiurge")
INSTALL_DESKTOP = Path("/usr/share/xsessions/demiurge.desktop")
INSTALL_SERVICE = Path.home() / ".config/systemd/user/demiurge.service"
INSTALL_CONFIG_DIR = Path.home() / ".config/demiurge"
INSTALL_CONFIG = INSTALL_CONFIG_DIR / "config.toml"
BUILD_DIR = Path("/tmp/demiurge-build")

# GORDIAN KNOT -- VT + X11 screen locker. Binary is setuid-root so the VT
# path can drive VT_LOCKSWITCH. PAM service file tells libpam which stack
# to consult. Two user-level systemd units: daemon (idle watcher) + sleep
# (lock before suspend).
INSTALL_GK_BINARY = Path("/usr/local/bin/gordian_knot")
INSTALL_GK_PAM = Path("/etc/pam.d/gordian_knot")
INSTALL_GK_DAEMON_SERVICE = Path.home() / ".config/systemd/user/gordian_knot-daemon.service"
INSTALL_GK_SLEEP_SERVICE = Path.home() / ".config/systemd/user/gordian_knot-sleep.service"


# EMBEDDED FILE CONTENTS
#
# .desktop and .service files are small, declarative, and only ever read
# by this installer. Keeping them as separate static files in the source
# tree means every sync / copy / branch switch can desync them -- which
# already bit us once (demiurge.desktop went missing from the deploy
# copy between v0.3.0 and v0.4.0). Inlined here, install.py is the
# single source of truth; the repo loses four ghost files.

DEMIURGE_DESKTOP_CONTENT = """\
[Desktop Entry]
Name=DEMIURGE
Comment=Minimal X11 window manager
Exec=systemctl --user start --wait demiurge.service
TryExec=/usr/local/bin/demiurge
Type=Application
DesktopNames=DEMIURGE
"""

DEMIURGE_SERVICE_CONTENT = """\
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

GK_DAEMON_SERVICE_CONTENT = """\
[Unit]
Description=GORDIAN KNOT idle watcher
PartOf=graphical-session.target
After=graphical-session.target

[Service]
Type=exec
ExecStart=/usr/local/bin/gordian_knot --daemon
Restart=on-failure
RestartSec=2

[Install]
WantedBy=graphical-session.target
"""

GK_SLEEP_SERVICE_CONTENT = """\
[Unit]
Description=Lock the session before suspend (GORDIAN KNOT)
Before=sleep.target suspend.target hibernate.target hybrid-sleep.target

[Service]
Type=forking
Environment=XDG_SESSION_TYPE=x11
ExecStart=/usr/local/bin/gordian_knot
TimeoutStartSec=10

[Install]
WantedBy=sleep.target suspend.target hibernate.target hybrid-sleep.target
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


def run_cmd_capture(cmd: list, cwd: Path | None = None) -> tuple[int, str, str]:
    result = subprocess.run(cmd, capture_output=True, text=True, cwd=cwd)
    return result.returncode, result.stdout, result.stderr


def run_cmd_sudo(cmd: list) -> int:
    return run_cmd(["sudo"] + cmd)


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
    the parent dir if needed. Returns True on success."""
    try:
        dst.parent.mkdir(parents=True, exist_ok=True)
        staging = Path(str(dst) + ".new")
        staging.write_text(content)
        os.replace(staging, dst)
        return True
    except OSError as e:
        log_error(f"write {dst}: {e}")
        return False


def atomic_write_root_file(dst: Path, content: str, mode: str = "0644") -> bool:
    """Write a root-owned file atomically. Uses `sudo install` to place
    the content at <dst>.new with the requested mode, then renames. The
    content is piped through `sudo tee` into the staging path."""
    staging = Path(str(dst) + ".new")
    try:
        run_cmd_sudo(["mkdir", "-p", str(dst.parent)])
        proc = subprocess.Popen(
            ["sudo", "tee", str(staging)],
            stdin=subprocess.PIPE, stdout=subprocess.DEVNULL,
        )
        proc.communicate(input=content.encode("utf-8"))
        if proc.returncode != 0:
            return False
    except OSError as e:
        log_error(f"write {staging}: {e}")
        return False
    if run_cmd_sudo(["chmod", mode, str(staging)]) != 0:
        return False
    if run_cmd_sudo(["mv", "-f", str(staging), str(dst)]) != 0:
        return False
    return True


def systemctl_user(*args: str) -> int:
    ret, _, _ = run_cmd_capture(["systemctl", "--user", *args])
    return ret


# CONFIG VALIDATION

def validate_config(binary: Path, config_path: Path) -> bool:
    """Run the freshly-built binary with --check-config against the given path.
    Returns True if the config parses cleanly."""
    ret, stdout, stderr = run_cmd_capture(
        [str(binary), "-c", str(config_path), "--check-config"]
    )
    if ret != 0:
        if stderr.strip():
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


# BUILD

def build_demiurge(source_dir: Path) -> bool:
    ret, _, _ = run_cmd_capture(["which", "cargo"])
    if ret != 0:
        log_error("cargo not found. Install Rust: https://rustup.rs")
        return False

    log_info("Building demiurge")

    env = os.environ.copy()
    env["CARGO_TARGET_DIR"] = str(BUILD_DIR)

    cargo_cmd = ["cargo", "build", "--release"]
    print(f">>> CARGO_TARGET_DIR={BUILD_DIR} {' '.join(cargo_cmd)}")
    result = subprocess.run(cargo_cmd, cwd=source_dir, env=env)

    if result.returncode != 0:
        log_error("Build failed")
        return False

    binary = BUILD_DIR / "release" / "demiurge"
    if not binary.exists():
        log_error("Build completed but binary not found")
        return False

    size = binary.stat().st_size
    log_info(f"Built: {binary} ({size // 1024} KB)")
    return True


# COMMANDS

def cmd_install(args, source_dir: Path) -> bool:
    log_info("Installing DEMIURGE")

    source_binary = BUILD_DIR / "release" / "demiurge"
    source_config = source_dir / "config.default.toml"

    if not build_demiurge(source_dir):
        return False

    try:
        INSTALL_CONFIG_DIR.mkdir(parents=True, exist_ok=True)
        INSTALL_SERVICE.parent.mkdir(parents=True, exist_ok=True)
    except OSError as e:
        log_error(f"Failed to create directories: {e}")
        return False

    # If a config already exists, validate it against the freshly-built
    # binary before preserving. A config that can't parse will kill the
    # session at login, so catch it here rather than after logout.
    if INSTALL_CONFIG.exists():
        if validate_config(source_binary, INSTALL_CONFIG):
            log_info(f"Config preserved: {INSTALL_CONFIG}")
        else:
            log_warn(f"Existing config fails validation: {INSTALL_CONFIG}")
            if should_replace_config(args):
                backup = INSTALL_CONFIG.with_suffix(".toml.bak")
                log_info(f"Backing up to {backup}")
                shutil.copy2(INSTALL_CONFIG, backup)
                if source_config.exists():
                    log_info(f"Writing fresh default config: {INSTALL_CONFIG}")
                    shutil.copy2(source_config, INSTALL_CONFIG)
                else:
                    log_warn("config.default.toml not found in source; not replaced")
            else:
                log_warn("Keeping broken config per user choice. The WM will fail to start.")
    elif source_config.exists():
        log_info(f"Writing default config: {INSTALL_CONFIG}")
        shutil.copy2(source_config, INSTALL_CONFIG)
    else:
        log_warn("config.default.toml not found in source")

    log_info(f"Installing binary: {INSTALL_BINARY}")
    ret = atomic_sudo_install(["-Dm755"], source_binary, INSTALL_BINARY)
    if ret != 0:
        log_error("Failed to install binary")
        return False

    log_info(f"Installing xsession entry: {INSTALL_DESKTOP}")
    if not atomic_write_root_file(INSTALL_DESKTOP, DEMIURGE_DESKTOP_CONTENT, "0644"):
        log_error("Failed to install xsession entry")
        return False

    log_info(f"Installing systemd unit: {INSTALL_SERVICE}")
    if not atomic_write_user_file(INSTALL_SERVICE, DEMIURGE_SERVICE_CONTENT):
        log_error("Failed to install systemd unit")
        return False
    systemctl_user("daemon-reload")

    # GORDIAN KNOT (screen locker). Binary is setuid-root so it can drive
    # VT_LOCKSWITCH when the X11 grab fallback fires. PAM stack file under
    # /etc/pam.d. Two user systemd units: daemon (idle watcher) + sleep
    # (lock before suspend).
    if not install_gordian_knot(source_dir, args):
        return False

    print()
    log_info("Installation complete")
    log_info(f"Binary:       {INSTALL_BINARY}")
    log_info(f"Session:      {INSTALL_DESKTOP}")
    log_info(f"Service:      {INSTALL_SERVICE}")
    log_info(f"Config:       {INSTALL_CONFIG}")
    log_info(f"Locker:       {INSTALL_GK_BINARY}")
    log_info(f"Locker PAM:   {INSTALL_GK_PAM}")
    log_info(f"Idle daemon:  {INSTALL_GK_DAEMON_SERVICE}")
    log_info(f"Sleep lock:   {INSTALL_GK_SLEEP_SERVICE}")
    print()
    log_info("Log out and select 'DEMIURGE' in your display manager to launch.")
    return True


def install_gordian_knot(source_dir: Path, args) -> bool:
    """Install the GORDIAN KNOT binary, PAM file, and systemd units."""
    source_gk_binary = BUILD_DIR / "release" / "gordian_knot"

    if not source_gk_binary.exists():
        log_warn("GORDIAN KNOT binary not found; skipping locker install")
        return True

    # Binary: setuid-root (mode 4755) so VT ioctls work from an unprivileged
    # invocation. The binary itself drops privs via setresuid before running
    # PAM + UI; root is held only for VT_LOCKSWITCH.
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
    if not write_pam_file_as_root(INSTALL_GK_PAM, "auth include system-auth\n"):
        log_error("Failed to install PAM stack file")
        return False

    # Daemon service (user). Runs the idle watcher; respawns GORDIAN KNOT
    # on threshold crossings.
    log_info(f"Installing idle-daemon unit: {INSTALL_GK_DAEMON_SERVICE}")
    if not atomic_write_user_file(INSTALL_GK_DAEMON_SERVICE, GK_DAEMON_SERVICE_CONTENT):
        log_error("Failed to install idle-daemon unit")
        return False

    # Sleep hook (user). Locks before suspend.target so the unlock screen
    # on resume is ours, not the display manager's.
    log_info(f"Installing sleep-hook unit: {INSTALL_GK_SLEEP_SERVICE}")
    if not atomic_write_user_file(INSTALL_GK_SLEEP_SERVICE, GK_SLEEP_SERVICE_CONTENT):
        log_error("Failed to install sleep-hook unit")
        return False

    # Force-disable the locker services, regardless of their prior state.
    # Reason: GORDIAN KNOT's PAM conversation has a re-entry bug -- after
    # a wrong password, the prompt refuses further input and the user is
    # locked out until a force-reboot. Auto-enabling the idle-watcher
    # has burned the user once already (mid-install auto-lock + lockout
    # + 0-byte binaries after force-reboot). Upgrades from a version
    # where the daemon was previously enabled would otherwise inherit
    # the enabled state silently.
    #
    # Re-enable manually once the PAM bug is fixed:
    #   systemctl --user enable --now gordian_knot-daemon.service
    #   systemctl --user enable --now gordian_knot-sleep.service
    for unit in ("gordian_knot-daemon.service", "gordian_knot-sleep.service"):
        systemctl_user("disable", "--now", unit)
    systemctl_user("daemon-reload")

    log_warn("GORDIAN KNOT services installed but force-disabled.")
    log_warn("See the comment in install.py::install_gordian_knot; re-enable")
    log_warn("manually once the PAM re-entry bug is fixed.")
    return True


def write_pam_file_as_root(path: Path, content: str) -> bool:
    """Write a file under /etc as root with 0644 perms. Uses sudo tee so
    we don't have to shell through a temp-file dance."""
    try:
        proc = subprocess.Popen(
            ["sudo", "tee", str(path)],
            stdin=subprocess.PIPE, stdout=subprocess.DEVNULL,
        )
        proc.communicate(input=content.encode("utf-8"))
        if proc.returncode != 0:
            return False
    except OSError:
        return False
    # Ensure mode 0644 (tee may leave 0666 via umask).
    run_cmd_sudo(["chmod", "0644", str(path)])
    return True


def cmd_update(args, source_dir: Path) -> bool:
    log_info("Checking for updates")

    if not INSTALL_BINARY.exists():
        log_info("Binary not installed -- running full install")
        return cmd_install(args, source_dir)

    needs_rebuild = False
    installed_mtime = INSTALL_BINARY.stat().st_mtime

    for pattern in ["src/**/*.rs", "Cargo.toml"]:
        for src_file in source_dir.glob(pattern):
            if src_file.stat().st_mtime > installed_mtime:
                needs_rebuild = True
                log_info(f"Source updated: {src_file.name}")
                break
        if needs_rebuild:
            break

    if not needs_rebuild:
        log_info("Already up to date")
        return True

    if not build_demiurge(source_dir):
        return False

    source_binary = BUILD_DIR / "release" / "demiurge"
    log_info(f"Installing binary: {INSTALL_BINARY}")
    ret = atomic_sudo_install(["-Dm755"], source_binary, INSTALL_BINARY)
    if ret != 0:
        log_error("Failed to install binary")
        return False

    # Always rewrite the service file so bug fixes / embedded-content
    # edits in this installer take effect on update. Cheap and atomic.
    log_info(f"Refreshing systemd unit: {INSTALL_SERVICE}")
    if not atomic_write_user_file(INSTALL_SERVICE, DEMIURGE_SERVICE_CONTENT):
        log_error("Failed to refresh systemd unit")
        return False
    systemctl_user("daemon-reload")

    log_info("Update complete")
    log_info("Restart your session to pick up the new binary.")
    return True


def cmd_uninstall(args, source_dir: Path) -> bool:
    log_info("Uninstalling DEMIURGE")

    removed = False

    # Stop GORDIAN KNOT user units first so daemon-reload picks up removal.
    for unit in ("gordian_knot-daemon.service", "gordian_knot-sleep.service"):
        systemctl_user("disable", "--now", unit)

    if INSTALL_BINARY.exists():
        log_info(f"Removing {INSTALL_BINARY}")
        if run_cmd_sudo(["rm", "-f", str(INSTALL_BINARY)]) == 0:
            removed = True

    if INSTALL_DESKTOP.exists():
        log_info(f"Removing {INSTALL_DESKTOP}")
        if run_cmd_sudo(["rm", "-f", str(INSTALL_DESKTOP)]) == 0:
            removed = True

    if INSTALL_SERVICE.exists():
        log_info(f"Removing {INSTALL_SERVICE}")
        INSTALL_SERVICE.unlink()
        removed = True

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

    if not removed:
        log_warn("No installed files found")
    else:
        log_info("Uninstall complete")

    if INSTALL_CONFIG_DIR.exists():
        log_info(f"Config directory preserved: {INSTALL_CONFIG_DIR}")

    return True


def cmd_status(args, source_dir: Path) -> bool:
    log_info("DEMIURGE status")
    print()

    binary_ok = INSTALL_BINARY.exists()
    desktop_ok = INSTALL_DESKTOP.exists()
    service_ok = INSTALL_SERVICE.exists()
    config_ok = INSTALL_CONFIG.exists()

    print(f"  Binary:   {INSTALL_BINARY}")
    print(f"            {'installed' if binary_ok else 'NOT INSTALLED'}")
    if binary_ok:
        size = INSTALL_BINARY.stat().st_size
        print(f"            {size // 1024} KB")
    print()

    print(f"  Session:  {INSTALL_DESKTOP}")
    print(f"            {'installed' if desktop_ok else 'NOT INSTALLED'}")
    print()

    print(f"  Service:  {INSTALL_SERVICE}")
    print(f"            {'installed' if service_ok else 'NOT INSTALLED'}")
    print()

    print(f"  Config:   {INSTALL_CONFIG}")
    print(f"            {'exists' if config_ok else 'NOT FOUND'}")
    print()

    gk_binary_ok = INSTALL_GK_BINARY.exists()
    gk_pam_ok = INSTALL_GK_PAM.exists()
    gk_daemon_ok = INSTALL_GK_DAEMON_SERVICE.exists()
    gk_sleep_ok = INSTALL_GK_SLEEP_SERVICE.exists()

    print(f"  Locker:      {INSTALL_GK_BINARY}")
    print(f"               {'installed' if gk_binary_ok else 'NOT INSTALLED'}")
    if gk_binary_ok:
        mode = oct(INSTALL_GK_BINARY.stat().st_mode)[-4:]
        print(f"               mode {mode} (setuid-root expected: 4755)")
    print()
    print(f"  PAM stack:   {INSTALL_GK_PAM}")
    print(f"               {'installed' if gk_pam_ok else 'NOT INSTALLED'}")
    print()
    print(f"  Idle daemon: {INSTALL_GK_DAEMON_SERVICE}")
    print(f"               {'installed' if gk_daemon_ok else 'NOT INSTALLED'}")
    print()
    print(f"  Sleep lock:  {INSTALL_GK_SLEEP_SERVICE}")
    print(f"               {'installed' if gk_sleep_ok else 'NOT INSTALLED'}")
    print()

    print(f"  Overall: {'INSTALLED' if binary_ok else 'NOT INSTALLED'}")
    return binary_ok


# MAIN

def main() -> int:
    if os.geteuid() == 0:
        log_error("Do not run this script as root or with sudo.")
        log_error("The installer calls sudo internally only where needed.")
        log_error("Running as root would poison the build directory with root-owned files.")
        return 1

    parser = argparse.ArgumentParser(
        description="Install DEMIURGE X11 window manager",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Commands:
  (default)   Build and install
  uninstall   Remove installed files (config preserved)
  status      Show installation status
  update      Rebuild and install if source is newer

Examples:
  ./install.py              # Build and install
  ./install.py update       # Rebuild if source changed
  ./install.py status       # Check installation status
  ./install.py uninstall    # Remove installation
"""
    )

    parser.add_argument("command", nargs="?", default="install",
                       choices=["install", "uninstall", "status", "update"],
                       help="Command to run (default: install)")
    parser.add_argument("-y", "--yes", action="store_true",
                       help="Assume yes to prompts (non-interactive installs)")

    args = parser.parse_args()
    source_dir = Path(__file__).parent.resolve()

    print()
    log_info("DEMIURGE installer")
    log_info(f"Source: {source_dir}")
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
