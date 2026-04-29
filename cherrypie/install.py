#!/usr/bin/env python3
"""
cherrypie installer

Installs cherrypie window matching daemon.

Usage:
    ./install.py              # Install (default)
    ./install.py uninstall    # Remove installed files
    ./install.py status       # Show installation status
    ./install.py enable       # Enable systemd service
    ./install.py disable      # Disable systemd service
    ./install.py update       # Rebuild and install if source is newer
"""

import argparse
import os
import pwd
import sys
import shutil
import subprocess
import time
from pathlib import Path
from datetime import datetime


# CONFIGURATION

_real_user = os.environ.get("SUDO_USER", os.environ.get("USER"))
try:
    _real_home = Path(pwd.getpwnam(_real_user).pw_dir) if _real_user else Path.home()
except KeyError:
    _real_home = Path.home()

INSTALL_BINARY = _real_home / ".local" / "bin" / "cherrypie"
INSTALL_CONFIG_DIR = _real_home / ".config" / "cherrypie"
INSTALL_SERVICE = _real_home / ".config" / "systemd" / "user" / "cherrypie.service"
BUILD_DIR = Path("/tmp/cherrypie-build")


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

def run_cmd(cmd: list, cwd: Path | None = None) -> int:
    print(f">>> {' '.join(cmd)}")
    result = subprocess.run(cmd, cwd=cwd)
    return result.returncode


def run_cmd_capture(cmd: list, cwd: Path | None = None) -> tuple[int, str, str]:
    result = subprocess.run(cmd, capture_output=True, text=True, cwd=cwd)
    return result.returncode, result.stdout, result.stderr


def get_systemctl_cmd() -> list:
    if os.environ.get("SUDO_USER"):
        return ["systemctl", f"--machine={os.environ['SUDO_USER']}@.host", "--user"]
    return ["systemctl", "--user"]


# SERVICE MANAGEMENT

def is_service_enabled() -> bool:
    cmd = get_systemctl_cmd() + ["is-enabled", "cherrypie.service"]
    ret, _, _ = run_cmd_capture(cmd)
    return ret == 0


def is_service_active() -> bool:
    cmd = get_systemctl_cmd() + ["is-active", "cherrypie.service"]
    ret, _, _ = run_cmd_capture(cmd)
    return ret == 0


def enable_service() -> bool:
    log_info("Enabling cherrypie service...")

    base_cmd = get_systemctl_cmd()
    subprocess.run(base_cmd + ["daemon-reload"], capture_output=True)

    ret, _, stderr = run_cmd_capture(base_cmd + ["enable", "--now", "cherrypie.service"])
    if ret == 0:
        log_info("Service enabled and started")
        return True
    else:
        log_error(f"Failed to enable service: {stderr}")
        return False


def disable_service() -> bool:
    log_info("Disabling cherrypie service...")

    cmd = get_systemctl_cmd() + ["disable", "--now", "cherrypie.service"]
    ret, _, stderr = run_cmd_capture(cmd)
    if ret == 0:
        log_info("Service disabled and stopped")
        return True
    else:
        log_error(f"Failed to disable service: {stderr}")
        return False


def stop_all_cherrypie() -> None:
    base_cmd = get_systemctl_cmd()

    try:
        subprocess.run(base_cmd + ["stop", "cherrypie.service"],
                       capture_output=True, timeout=5)
    except subprocess.TimeoutExpired:
        log_warn("systemctl stop timed out, force-killing...")

    subprocess.run(["pkill", "-x", "cherrypie"], capture_output=True)
    time.sleep(0.5)

    ret, _, _ = run_cmd_capture(["pgrep", "-x", "cherrypie"])
    if ret == 0:
        log_warn("Daemon still alive after SIGTERM, sending SIGKILL...")
        subprocess.run(["pkill", "-9", "-x", "cherrypie"], capture_output=True)
        time.sleep(0.5)

        ret, _, _ = run_cmd_capture(["pgrep", "-x", "cherrypie"])
        if ret == 0:
            log_warn("Daemon survived SIGKILL (zombie?)")


def restart_service() -> bool:
    if is_service_enabled():
        log_info("Starting service...")
        cmd = get_systemctl_cmd() + ["start", "cherrypie.service"]
        ret, _, _ = run_cmd_capture(cmd)
        return ret == 0
    return True


# BUILD

def build_cherrypie(source_dir: Path) -> bool:
    ret, _, _ = run_cmd_capture(["which", "cargo"])
    if ret != 0:
        log_error("cargo not found. Install Rust: https://rustup.rs")
        return False

    log_info("Building cherrypie")

    env = os.environ.copy()
    env["CARGO_TARGET_DIR"] = str(BUILD_DIR)

    cargo_cmd = ["cargo", "build", "--release"]
    print(f">>> {' '.join(cargo_cmd)}")
    result = subprocess.run(cargo_cmd, cwd=source_dir, env=env)

    if result.returncode != 0:
        log_error("Build failed!")
        return False

    binary = BUILD_DIR / "release" / "cherrypie"
    if not binary.exists():
        log_error("Build completed but binary not found")
        return False

    size = binary.stat().st_size
    log_info(f"Built: {binary} ({size // 1024} KB)")
    return True


# COMMANDS

def cmd_install(args, source_dir: Path) -> bool:
    log_info("Installing cherrypie")

    source_service = source_dir / "cherrypie.service"
    source_binary = BUILD_DIR / "release" / "cherrypie"

    # Build
    if not build_cherrypie(source_dir):
        return False

    # Create directories
    log_info("Creating directories...")
    try:
        INSTALL_BINARY.parent.mkdir(parents=True, exist_ok=True)
        INSTALL_CONFIG_DIR.mkdir(parents=True, exist_ok=True)
        INSTALL_SERVICE.parent.mkdir(parents=True, exist_ok=True)
    except OSError as e:
        log_error(f"Failed to create directories: {e}")
        return False

    # Stop running processes before overwriting binary
    log_info("Stopping existing cherrypie processes...")
    stop_all_cherrypie()

    # Copy binary
    log_info(f"Installing {INSTALL_BINARY}...")
    try:
        shutil.copy2(source_binary, INSTALL_BINARY)
        INSTALL_BINARY.chmod(0o755)
    except OSError as e:
        log_error(f"Failed to install binary: {e}")
        return False

    # Copy systemd service
    if source_service.exists():
        log_info("Installing systemd service...")
        shutil.copy2(source_service, INSTALL_SERVICE)
        subprocess.run(get_systemctl_cmd() + ["daemon-reload"], capture_output=True)
    else:
        log_warn("Systemd service file not found in source")

    # Fix ownership if running as root
    if os.environ.get("SUDO_USER"):
        try:
            pw = pwd.getpwnam(_real_user)
            uid, gid = pw.pw_uid, pw.pw_gid
            os.chown(INSTALL_BINARY, uid, gid)
            if INSTALL_SERVICE.exists():
                os.chown(INSTALL_SERVICE, uid, gid)
        except (KeyError, OSError) as e:
            log_warn(f"Failed to fix ownership: {e}")

    print()
    log_info("Installation complete")
    log_info(f"Binary:  {INSTALL_BINARY}")
    log_info(f"Config:  {INSTALL_CONFIG_DIR / 'config.toml'}")

    # Offer to enable service
    if not args.no_service and INSTALL_SERVICE.exists():
        if not is_service_enabled():
            print()
            try:
                response = input("Enable cherrypie service? [Y/N]: ").strip().lower()
                if response in ("", "y", "yes"):
                    enable_service()
            except EOFError:
                pass
        else:
            restart_service()

    return True


def cmd_uninstall(args, source_dir: Path) -> bool:
    log_info("Uninstalling cherrypie")

    if is_service_enabled():
        disable_service()

    files = [INSTALL_BINARY, INSTALL_SERVICE]

    removed = False
    for f in files:
        if f.exists():
            log_info(f"Removing {f}")
            f.unlink()
            removed = True

    if not removed:
        log_warn("No installed files found")
    else:
        log_info("Uninstall complete")

    if INSTALL_CONFIG_DIR.exists():
        log_info(f"Config directory preserved: {INSTALL_CONFIG_DIR}")

    return True


def cmd_status(args, source_dir: Path) -> bool:
    log_info("cherrypie status")
    print()

    binary_ok = INSTALL_BINARY.exists()
    service_ok = INSTALL_SERVICE.exists()
    config_ok = (INSTALL_CONFIG_DIR / "config.toml").exists()

    print(f"  Binary:   {INSTALL_BINARY}")
    print(f"            {'installed' if binary_ok else 'NOT INSTALLED'}")
    if binary_ok:
        size = INSTALL_BINARY.stat().st_size
        print(f"            {size // 1024} KB")
    print()

    print(f"  Service:  {INSTALL_SERVICE}")
    print(f"            {'installed' if service_ok else 'NOT INSTALLED'}")
    print()

    print(f"  Config:   {INSTALL_CONFIG_DIR / 'config.toml'}")
    print(f"            {'exists' if config_ok else 'NOT FOUND'}")
    print()

    if service_ok:
        enabled = is_service_enabled()
        active = is_service_active()
        print(f"  Service enabled: {'yes' if enabled else 'no'}")
        print(f"  Service running: {'yes' if active else 'no'}")
        print()

    print(f"  Overall: {'INSTALLED' if binary_ok else 'NOT INSTALLED'}")

    return binary_ok


def cmd_enable(args, source_dir: Path) -> bool:
    if not INSTALL_SERVICE.exists():
        log_error("Service file not installed. Run install first.")
        return False
    return enable_service()


def cmd_disable(args, source_dir: Path) -> bool:
    return disable_service()


def cmd_update(args, source_dir: Path) -> bool:
    log_info("Checking for updates")

    needs_rebuild = False

    if INSTALL_BINARY.exists():
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
    else:
        needs_rebuild = True
        log_info("Binary not installed")

    if not build_cherrypie(source_dir):
        return False

    log_info("Stopping existing cherrypie processes...")
    stop_all_cherrypie()

    log_info("Installing update...")
    shutil.copy2(BUILD_DIR / "release" / "cherrypie", INSTALL_BINARY)
    INSTALL_BINARY.chmod(0o755)

    if os.environ.get("SUDO_USER"):
        try:
            pw = pwd.getpwnam(_real_user)
            os.chown(INSTALL_BINARY, pw.pw_uid, pw.pw_gid)
        except (KeyError, OSError) as e:
            log_warn(f"Failed to fix ownership: {e}")

    restart_service()
    log_info("Update complete")

    return True


# MAIN

def main() -> int:
    parser = argparse.ArgumentParser(
        description="Install cherrypie window matching daemon",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Commands:
  (default)   Install cherrypie
  uninstall   Remove installed files
  status      Show installation status
  enable      Enable systemd service
  disable     Disable systemd service
  update      Rebuild and install if source is newer

Examples:
  ./install.py              # Build, install, prompt for service
  ./install.py --no-service # Install without prompting for service
  ./install.py status       # Check installation status
  ./install.py update       # Rebuild if source changed
  ./install.py enable       # Enable systemd service
  ./install.py uninstall    # Remove installation
"""
    )

    parser.add_argument("command", nargs="?", default="install",
                       choices=["install", "uninstall", "status", "enable", "disable", "update"],
                       help="Command to run (default: install)")
    parser.add_argument("--no-service", action="store_true",
                       help="Don't prompt to enable service after install")

    args = parser.parse_args()
    source_dir = Path(__file__).parent.resolve()

    print()
    log_info("cherrypie installer")
    log_info(f"Source: {source_dir}")
    print()

    commands = {
        "install": cmd_install,
        "uninstall": cmd_uninstall,
        "status": cmd_status,
        "enable": cmd_enable,
        "disable": cmd_disable,
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
