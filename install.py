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

# gordian_knot -- VT + X11 screen locker. Binary is setuid-root so the VT
# path can drive VT_LOCKSWITCH. PAM service file tells libpam which stack
# to consult. Two user-level systemd units: daemon (idle watcher) + sleep
# (lock before suspend).
INSTALL_GK_BINARY = Path("/usr/local/bin/gordian_knot")
INSTALL_GK_PAM = Path("/etc/pam.d/gordian_knot")
INSTALL_GK_DAEMON_SERVICE = Path.home() / ".config/systemd/user/gordian_knot-daemon.service"
INSTALL_GK_SLEEP_SERVICE = Path.home() / ".config/systemd/user/gordian_knot-sleep.service"


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
    source_service = source_dir / "demiurge.service"
    source_desktop = source_dir / "demiurge.desktop"
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
    ret = run_cmd_sudo(["install", "-Dm755", str(source_binary), str(INSTALL_BINARY)])
    if ret != 0:
        log_error("Failed to install binary")
        return False

    if source_desktop.exists():
        log_info(f"Installing xsession entry: {INSTALL_DESKTOP}")
        ret = run_cmd_sudo(["install", "-Dm644", str(source_desktop), str(INSTALL_DESKTOP)])
        if ret != 0:
            log_error("Failed to install xsessions entry")
            return False
    else:
        log_warn("demiurge.desktop not found in source")

    if source_service.exists():
        log_info(f"Installing systemd unit: {INSTALL_SERVICE}")
        shutil.copy2(source_service, INSTALL_SERVICE)
        systemctl_user("daemon-reload")
    else:
        log_warn("demiurge.service not found in source")

    # gordian_knot (screen locker). Binary is setuid-root so it can drive
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
    """Install the gordian_knot binary, PAM file, and systemd units."""
    source_gk_binary = BUILD_DIR / "release" / "gordian_knot"
    source_daemon_svc = source_dir / "gordian_knot-daemon.service"
    source_sleep_svc = source_dir / "gordian_knot-sleep.service"

    if not source_gk_binary.exists():
        log_warn("gordian_knot binary not found; skipping locker install")
        return True

    # Binary: setuid-root (mode 4755) so VT ioctls work from an unprivileged
    # invocation. The binary itself drops privs via setresuid before running
    # PAM + UI; root is held only for VT_LOCKSWITCH.
    log_info(f"Installing locker: {INSTALL_GK_BINARY} (setuid-root)")
    ret = run_cmd_sudo(
        ["install", "-Dm4755", "-o", "root", "-g", "root",
         str(source_gk_binary), str(INSTALL_GK_BINARY)]
    )
    if ret != 0:
        log_error("Failed to install gordian_knot binary")
        return False

    # PAM stack file. Minimal -- delegate to system-auth for the real
    # password machinery. Installed via sudo + tee so we control perms.
    log_info(f"Installing PAM stack: {INSTALL_GK_PAM}")
    if not write_pam_file_as_root(INSTALL_GK_PAM, "auth include system-auth\n"):
        log_error("Failed to install PAM stack file")
        return False

    # Daemon service (user). Runs the idle watcher; respawns gordian_knot
    # on threshold crossings.
    if source_daemon_svc.exists():
        log_info(f"Installing idle-daemon unit: {INSTALL_GK_DAEMON_SERVICE}")
        INSTALL_GK_DAEMON_SERVICE.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source_daemon_svc, INSTALL_GK_DAEMON_SERVICE)
    else:
        log_warn("gordian_knot-daemon.service not found in source")

    # Sleep hook (user). Locks before suspend.target so the unlock screen
    # on resume is ours, not the display manager's.
    if source_sleep_svc.exists():
        log_info(f"Installing sleep-hook unit: {INSTALL_GK_SLEEP_SERVICE}")
        shutil.copy2(source_sleep_svc, INSTALL_GK_SLEEP_SERVICE)
    else:
        log_warn("gordian_knot-sleep.service not found in source")

    systemctl_user("daemon-reload")

    # Prompt to enable services unless --yes or non-interactive.
    if getattr(args, "yes", False):
        _enable_gordian_services()
    elif sys.stdin.isatty():
        try:
            resp = input(
                "Enable gordian_knot idle + sleep services now? [Y/n]: "
            ).strip().lower()
            if resp in ("", "y", "yes"):
                _enable_gordian_services()
        except EOFError:
            pass
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


def _enable_gordian_services() -> None:
    """Enable + start the two user units. Silent-best-effort."""
    for unit in ("gordian_knot-daemon.service", "gordian_knot-sleep.service"):
        systemctl_user("enable", "--now", unit)


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
    ret = run_cmd_sudo(["install", "-Dm755", str(source_binary), str(INSTALL_BINARY)])
    if ret != 0:
        log_error("Failed to install binary")
        return False

    # Refresh service file if source is newer
    source_service = source_dir / "demiurge.service"
    if source_service.exists() and INSTALL_SERVICE.exists():
        if source_service.stat().st_mtime > INSTALL_SERVICE.stat().st_mtime:
            log_info(f"Refreshing systemd unit: {INSTALL_SERVICE}")
            shutil.copy2(source_service, INSTALL_SERVICE)
            systemctl_user("daemon-reload")

    log_info("Update complete")
    log_info("Restart your session to pick up the new binary.")
    return True


def cmd_uninstall(args, source_dir: Path) -> bool:
    log_info("Uninstalling DEMIURGE")

    removed = False

    # Stop gordian_knot user units first so daemon-reload picks up removal.
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
