#!/usr/bin/env python3
"""
abraxas installer

Installs ABRAXAS dynamic color temperature daemon.

Usage:
    ./install.py              # Install (default)
    ./install.py uninstall    # Remove installed files
    ./install.py status       # Show installation status
    ./install.py enable       # Enable systemd service
    ./install.py disable      # Disable systemd service
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


# =============================================================================
# CONFIGURATION
# =============================================================================

# Use SUDO_USER's home if running under sudo, otherwise current user's home
_real_user = os.environ.get("SUDO_USER", os.environ.get("USER"))
try:
    _real_home = Path(pwd.getpwnam(_real_user).pw_dir) if _real_user else Path.home()
except KeyError:
    _real_home = Path.home()

INSTALL_BINARY = _real_home / ".local" / "bin" / "abraxas"
INSTALL_CONFIG_DIR = _real_home / ".config" / "abraxas"
INSTALL_SERVICE = _real_home / ".config" / "systemd" / "user" / "abraxas.service"

# Cargo target directory. ABRAXAS/rust is a member of the DEMIURGE
# workspace, so a bare `cargo build` there writes to the workspace root's
# target/, not rust/target/ -- which is why this has to be pinned rather
# than assumed. --target-dir points it at the orchestrator's shared build
# so the dependency graph is compiled once for the whole install.
BUILD_DIR = Path("/tmp/abraxas-build")


def retarget(name: str | None, target_dir: str | None) -> bool:
    """Rebind the install paths to an explicitly named account, and the
    build to a shared target directory. --user matters inside an
    arch-chroot, where $SUDO_USER doesn't exist and $HOME is /root."""
    global _real_user, _real_home, INSTALL_BINARY, INSTALL_CONFIG_DIR
    global INSTALL_SERVICE, BUILD_DIR

    if target_dir:
        BUILD_DIR = Path(target_dir)
    if not name:
        return True
    try:
        pw = pwd.getpwnam(name)
    except KeyError:
        log_error(f"No such user: {name}")
        return False
    _real_user = pw.pw_name
    _real_home = Path(pw.pw_dir)
    INSTALL_BINARY = _real_home / ".local" / "bin" / "abraxas"
    INSTALL_CONFIG_DIR = _real_home / ".config" / "abraxas"
    INSTALL_SERVICE = _real_home / ".config" / "systemd" / "user" / "abraxas.service"
    return True


def chown_to_real_user(path: Path) -> None:
    """Hand a file we wrote as root to the account that will use it."""
    if os.geteuid() != 0:
        return
    try:
        pw = pwd.getpwnam(_real_user)
        os.chown(path, pw.pw_uid, pw.pw_gid)
    except (KeyError, OSError) as e:
        log_warn(f"chown {path}: {e}")



# =============================================================================
# LOGGING
# =============================================================================

def _timestamp() -> str:
    """Get current timestamp in [HH:MM:SS] format."""
    return datetime.now().strftime("[%H:%M:%S]")


def log_info(msg: str) -> None:
    print(f"{_timestamp()} [INFO]   {msg}")


def log_warn(msg: str) -> None:
    print(f"{_timestamp()} [WARN]   {msg}")


def log_error(msg: str) -> None:
    print(f"{_timestamp()} [ERROR]  {msg}")


# =============================================================================
# COMMAND EXECUTION
# =============================================================================

def run_cmd(cmd: list, cwd: Path | None = None) -> int:
    """Run a command with real-time output to terminal."""
    print(f">>> {' '.join(cmd)}")
    result = subprocess.run(cmd, cwd=cwd)
    return result.returncode


def run_cmd_capture(cmd: list, cwd: Path | None = None) -> tuple[int, str, str]:
    """Run a command and capture output."""
    result = subprocess.run(cmd, capture_output=True, text=True, cwd=cwd)
    return result.returncode, result.stdout, result.stderr


def get_systemctl_cmd() -> list:
    """Get base systemctl command, handling sudo user context."""
    if os.geteuid() == 0:
        return ["systemctl", f"--machine={_real_user}@.host", "--user"]
    return ["systemctl", "--user"]


def user_manager_running() -> bool:
    """Whether _real_user has a systemd user manager to talk to. There
    isn't one inside an arch-chroot, and every systemctl --user call
    there is a slow guaranteed failure."""
    try:
        uid = pwd.getpwnam(_real_user).pw_uid
    except KeyError:
        return False
    return Path(f"/run/user/{uid}/bus").exists()


# =============================================================================
# DEPENDENCY CHECKS
# =============================================================================

def check_x11_libs() -> bool:
    """Check if X11 libraries are available (for NVIDIA fallback)."""
    ret, _, _ = run_cmd_capture(["pkg-config", "--exists", "x11", "xrandr"])
    return ret == 0


# =============================================================================
# SERVICE MANAGEMENT
# =============================================================================

def is_service_enabled() -> bool:
    """Check if abraxas systemd service is enabled."""
    if not user_manager_running():
        return False
    cmd = get_systemctl_cmd() + ["is-enabled", "abraxas.service"]
    ret, _, _ = run_cmd_capture(cmd)
    return ret == 0


def is_service_active() -> bool:
    """Check if abraxas systemd service is running."""
    if not user_manager_running():
        return False
    cmd = get_systemctl_cmd() + ["is-active", "abraxas.service"]
    ret, _, _ = run_cmd_capture(cmd)
    return ret == 0


def enable_service() -> bool:
    """Enable and start abraxas service."""
    if not user_manager_running():
        log_warn(f"No systemd user manager for {_real_user}; enable "
                 "abraxas.service after first boot:")
        log_warn("  systemctl --user enable --now abraxas.service")
        return True

    log_info("Enabling abraxas service...")

    base_cmd = get_systemctl_cmd()

    # Disable old redshift service if present
    subprocess.run(
        base_cmd + ["disable", "--now", "redshift-scheduler.service"],
        capture_output=True
    )

    # Reload daemon
    subprocess.run(base_cmd + ["daemon-reload"], capture_output=True)

    # Enable new service
    ret, _, stderr = run_cmd_capture(base_cmd + ["enable", "--now", "abraxas.service"])

    if ret == 0:
        log_info("Service enabled and started")
        return True
    else:
        log_error(f"Failed to enable service: {stderr}")
        return False


def disable_service() -> bool:
    """Disable and stop abraxas service."""
    log_info("Disabling abraxas service...")

    cmd = get_systemctl_cmd() + ["disable", "--now", "abraxas.service"]
    ret, _, stderr = run_cmd_capture(cmd)

    if ret == 0:
        log_info("Service disabled and stopped")
        return True
    else:
        log_error(f"Failed to disable service: {stderr}")
        return False


def stop_all_abraxas() -> None:
    """Stop service, kill all abraxas processes, and verify they're dead.

    Prevents duplicate daemons: the old process must be fully dead before
    a new binary is installed and the service restarted.
    """
    if not user_manager_running():
        # Nothing is running on a machine that isn't booted yet.
        return

    base_cmd = get_systemctl_cmd()

    # Stop the service (5s timeout -- old binaries may not respond to SIGTERM)
    try:
        subprocess.run(base_cmd + ["stop", "abraxas.service"],
                       capture_output=True, timeout=5)
    except subprocess.TimeoutExpired:
        log_warn("systemctl stop timed out, force-killing...")

    # Kill any remaining abraxas processes (including strays)
    subprocess.run(["pkill", "-x", "abraxas"], capture_output=True)
    time.sleep(0.5)

    # Verify dead -- escalate to SIGKILL if needed
    ret, _, _ = run_cmd_capture(["pgrep", "-x", "abraxas"])
    if ret == 0:
        log_warn("Daemon still alive after SIGTERM, sending SIGKILL...")
        subprocess.run(["pkill", "-9", "-x", "abraxas"], capture_output=True)
        time.sleep(0.5)

        ret, _, _ = run_cmd_capture(["pgrep", "-x", "abraxas"])
        if ret == 0:
            log_warn("Daemon survived SIGKILL (zombie?)")


def restart_service() -> bool:
    """Start abraxas service if enabled."""
    if is_service_enabled():
        log_info("Starting service...")
        cmd = get_systemctl_cmd() + ["start", "abraxas.service"]
        ret, _, _ = run_cmd_capture(cmd)
        return ret == 0
    return True


# =============================================================================
# BUILD
# =============================================================================

def build_abraxas_rust(source_dir: Path, non_usa: bool = False) -> bool:
    """Build abraxas via cargo."""
    rust_dir = source_dir / "rust"

    if not rust_dir.exists():
        log_error("Rust source not found at rust/")
        return False

    # Check for cargo
    ret, _, _ = run_cmd_capture(["which", "cargo"])
    if ret != 0:
        log_error("cargo not found. Install Rust: https://rustup.rs")
        return False

    log_info("BUILDING ABRAXAS [Rust]")

    # Detect available backends via pkg-config
    features = []

    ret, _, _ = run_cmd_capture(["pkg-config", "--exists", "wayland-client"])
    if ret == 0:
        features.append("wayland")
        log_info("  Wayland backend: enabled")
    else:
        log_info("  Wayland backend: disabled (no wayland-client)")

    ret, _, _ = run_cmd_capture(["pkg-config", "--exists", "libsystemd"])
    if ret == 0:
        features.append("gnome")
        log_info("  GNOME backend:   enabled")
    else:
        log_info("  GNOME backend:   disabled (no libsystemd)")

    # X11 backend doesn't need pkg-config (x11rb is pure Rust protocol impl)
    # but check if X11 libs exist as a signal the user wants X11 support
    if check_x11_libs():
        features.append("x11")
        log_info("  X11 backend:     enabled")
    else:
        log_info("  X11 backend:     disabled (no libx11/libxrandr)")

    log_info("  DRM backend:     always enabled")

    if not non_usa:
        features.append("noaa")

    cargo_cmd = ["cargo", "build", "--release", "--no-default-features"]
    if features:
        cargo_cmd.extend(["--features", ",".join(features)])

    env = os.environ.copy()
    env["CARGO_TARGET_DIR"] = str(BUILD_DIR)
    print(f">>> CARGO_TARGET_DIR={BUILD_DIR} {' '.join(cargo_cmd)}")
    result = subprocess.run(cargo_cmd, cwd=rust_dir, env=env)

    if result.returncode != 0:
        log_error("Rust build failed!")
        return False

    binary = abraxas_binary()
    if not binary.exists():
        log_error("Build completed but binary not found")
        return False

    log_info(f"Built: {binary}")
    return True


def abraxas_binary() -> Path:
    return BUILD_DIR / "release" / "abraxas"


# =============================================================================
# COMMANDS
# =============================================================================

def cmd_install(args, source_dir: Path) -> bool:
    """Install abraxas."""
    log_info("INSTALLING ABRAXAS")

    source_service = source_dir / "abraxas.service"
    source_zipdb = source_dir / "us_zipcodes.bin"

    # ABRAXAS is Rust-only. The C23 implementation that shipped
    # alongside through v0.7 is gone as of v0.8.0 -- the audit
    # confirmed feature parity, the install path was already Rust,
    # and the c23/ source tree was deleted to drop ~6900 LOC of
    # dead weight from the repo.

    # Determine NOAA mode: --non-usa flag overrides, otherwise prompt
    if args.non_usa:
        non_usa = True
    elif args.yes:
        non_usa = False
    else:
        print()
        try:
            while True:
                response = input(
                    "ABRAXAS allows for utilization of US-specific NOAA data\n"
                    "for weather calculations. Do you wish to enable NOAA\n"
                    "data calculations? [Y/N]: "
                ).strip()
                if response in ("y", "Y"):
                    non_usa = False
                    break
                elif response in ("n", "N"):
                    non_usa = True
                    break
        except EOFError:
            print()
            log_info("No input (non-interactive). Defaulting to non-USA build.")
            non_usa = True
        print()

    if not build_abraxas_rust(source_dir, non_usa=non_usa):
        return False
    source_binary = abraxas_binary()

    # Create directories
    log_info("Creating directories...")
    try:
        for d in (INSTALL_BINARY.parent, INSTALL_CONFIG_DIR,
                  INSTALL_SERVICE.parent):
            d.mkdir(parents=True, exist_ok=True)
            chown_to_real_user(d)
    except OSError as e:
        log_error(f"Failed to create directories: {e}")
        return False

    # Stop ALL running abraxas processes before overwriting binary.
    # Prevents duplicate daemons and ETXTBSY.
    log_info("Stopping existing abraxas processes...")
    stop_all_abraxas()

    # Copy binary
    log_info(f"Installing {INSTALL_BINARY} [rust]...")
    try:
        shutil.copy2(source_binary, INSTALL_BINARY)
        INSTALL_BINARY.chmod(0o755)
    except OSError as e:
        log_error(f"Failed to install binary: {e}")
        return False

    # Copy ZIP database (skip for non-USA builds)
    if not non_usa:
        if source_zipdb.exists():
            dest_zipdb = INSTALL_CONFIG_DIR / "us_zipcodes.bin"
            if not dest_zipdb.exists():
                log_info("Installing ZIP database...")
                shutil.copy2(source_zipdb, dest_zipdb)
            else:
                log_info("ZIP database already installed")
        else:
            log_warn("ZIP database not found in source")
    else:
        log_info("Skipping ZIP database (non-USA build)")

    # Copy systemd service
    if source_service.exists():
        log_info("Installing systemd service...")
        shutil.copy2(source_service, INSTALL_SERVICE)

        # Reload systemd
        subprocess.run(get_systemctl_cmd() + ["daemon-reload"], capture_output=True)
    else:
        log_warn("Systemd service file not found in source")

    chown_to_real_user(INSTALL_BINARY)
    wl_dest = INSTALL_BINARY.parent / "meridian_wl.so"
    if wl_dest.exists():
        chown_to_real_user(wl_dest)
    if INSTALL_SERVICE.exists():
        chown_to_real_user(INSTALL_SERVICE)
    for f in INSTALL_CONFIG_DIR.iterdir():
        chown_to_real_user(f)

    print()
    log_info("SUCCESS. Installation complete.")
    log_info(f"Binary: {INSTALL_BINARY}")

    # Offer to enable service
    if not args.no_service and INSTALL_SERVICE.exists():
        if not is_service_enabled():
            if args.yes:
                enable_service()
            else:
                print()
                try:
                    response = input("Enable abraxas service? [Y/N]: ").strip().lower()
                    if response in ("", "y", "yes"):
                        enable_service()
                except EOFError:
                    print()
        else:
            # Restart if already running
            restart_service()

    return True


def cmd_uninstall(args, source_dir: Path) -> bool:
    """Remove installed files."""
    log_info("UNINSTALLING ABRAXAS")

    # Disable service first
    if is_service_enabled():
        disable_service()

    files = [
        INSTALL_BINARY,
        INSTALL_BINARY.parent / "meridian_wl.so",
        INSTALL_SERVICE,
    ]

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

    # Note: Leave config directory alone (user data)
    if INSTALL_CONFIG_DIR.exists():
        log_info(f"Config directory preserved: {INSTALL_CONFIG_DIR}")

    return True


def cmd_status(args, source_dir: Path) -> bool:
    """Show installation status."""
    log_info("ABRAXAS STATUS")
    print()

    # Check installation
    binary_ok = INSTALL_BINARY.exists()
    service_ok = INSTALL_SERVICE.exists()
    config_ok = INSTALL_CONFIG_DIR.exists()
    zipdb_ok = (INSTALL_CONFIG_DIR / "us_zipcodes.bin").exists()

    print(f"  Binary:    {INSTALL_BINARY}")
    print(f"             {'installed' if binary_ok else 'NOT INSTALLED'}")
    print()
    print(f"  Service:   {INSTALL_SERVICE}")
    print(f"             {'installed' if service_ok else 'NOT INSTALLED'}")
    print()
    print(f"  Config:    {INSTALL_CONFIG_DIR}")
    print(f"             {'exists' if config_ok else 'NOT FOUND'}")
    print()
    print(f"  ZIP DB:    {INSTALL_CONFIG_DIR / 'us_zipcodes.bin'}")
    print(f"             {'installed' if zipdb_ok else 'NOT INSTALLED'}")
    print()

    # Service status
    if service_ok:
        enabled = is_service_enabled()
        active = is_service_active()
        print(f"  Service enabled: {'yes' if enabled else 'no'}")
        print(f"  Service running: {'yes' if active else 'no'}")
        print()

    # Check X11 libs
    x11_ok = check_x11_libs()
    print(f"  X11 fallback: {'available' if x11_ok else 'not available (install libx11 libxrandr)'}")
    print()

    if binary_ok:
        size = INSTALL_BINARY.stat().st_size
        print(f"  Binary: {size // 1024} KB")
        print()

    print(f"  Overall: {'INSTALLED' if binary_ok else 'NOT INSTALLED'}")

    return binary_ok


def cmd_enable(args, source_dir: Path) -> bool:
    """Enable systemd service."""
    if not INSTALL_SERVICE.exists():
        log_error("Service file not installed. Run install first.")
        return False

    return enable_service()


def cmd_disable(args, source_dir: Path) -> bool:
    """Disable systemd service."""
    return disable_service()


def cmd_update(args, source_dir: Path) -> bool:
    """Update abraxas if source is newer."""
    log_info("CHECKING FOR UPDATES")

    needs_rebuild = False

    # Check if any source files are newer than installed binary
    if INSTALL_BINARY.exists():
        installed_mtime = INSTALL_BINARY.stat().st_mtime

        for pattern in ["rust/src/**/*.rs", "rust/Cargo.toml"]:
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
        log_info("Binary: not installed")

    # Rebuild
    non_usa = getattr(args, 'non_usa', False)
    if not build_abraxas_rust(source_dir, non_usa=non_usa):
        return False
    source_binary = abraxas_binary()

    # Stop ALL running abraxas processes before overwriting binary
    log_info("Stopping existing abraxas processes...")
    stop_all_abraxas()

    log_info("Installing updates...")
    shutil.copy2(source_binary, INSTALL_BINARY)
    INSTALL_BINARY.chmod(0o755)

    chown_to_real_user(INSTALL_BINARY)
    wl_dest = INSTALL_BINARY.parent / "meridian_wl.so"
    if wl_dest.exists():
        chown_to_real_user(wl_dest)

    restart_service()
    log_info("Update complete")

    return True


# =============================================================================
# MAIN
# =============================================================================

def main() -> int:
    parser = argparse.ArgumentParser(
        description="Install ABRAXAS dynamic color temperature daemon",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Commands:
  (default)   Install abraxas
  uninstall   Remove installed files
  status      Show installation status
  enable      Enable systemd service
  disable     Disable systemd service
  update      Update if source is newer

Examples:
  ./install.py                    # Install (prompts for NOAA)
  ./install.py --no-service       # Install without prompting for service
  ./install.py --non-usa          # Install without NOAA weather support
  ./install.py status             # Check installation status
  ./install.py enable             # Enable service
  ./install.py uninstall          # Remove installation
"""
    )

    parser.add_argument("command", nargs="?", default="install",
                       choices=["install", "uninstall", "status", "enable", "disable", "update"],
                       help="Command to run (default: install)")
    parser.add_argument("--no-service", action="store_true",
                       help="Don't prompt to enable service after install")
    parser.add_argument("--rebuild", action="store_true",
                       help="Force rebuild even if already built")
    parser.add_argument("--non-usa", action="store_true",
                       help="Disable NOAA weather support")
    parser.add_argument("-y", "--yes", action="store_true",
                       help="Assume yes to prompts (non-interactive installs)")
    parser.add_argument("--user", metavar="NAME",
                       help="Account whose $HOME receives the binary, config "
                            "and user unit (default: $SUDO_USER, else you)")
    parser.add_argument("--target-dir", metavar="DIR",
                       help="Cargo target directory to build into. Point it "
                            "at a shared workspace build to reuse its "
                            "compiled dependencies.")

    args = parser.parse_args()

    source_dir = Path(__file__).parent.resolve()

    print()
    log_info("abraxas installer")
    log_info(f"Source: {source_dir}")

    if not retarget(args.user, args.target_dir):
        return 1
    print()

    # Run command
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
