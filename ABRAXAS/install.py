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
    if os.environ.get("SUDO_USER"):
        return ["systemctl", f"--machine={os.environ['SUDO_USER']}@.host", "--user"]
    return ["systemctl", "--user"]


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
    cmd = get_systemctl_cmd() + ["is-enabled", "abraxas.service"]
    ret, _, _ = run_cmd_capture(cmd)
    return ret == 0


def is_service_active() -> bool:
    """Check if abraxas systemd service is running."""
    cmd = get_systemctl_cmd() + ["is-active", "abraxas.service"]
    ret, _, _ = run_cmd_capture(cmd)
    return ret == 0


def enable_service() -> bool:
    """Enable and start abraxas service."""
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

def build_abraxas_c23(source_dir: Path, force: bool = False,
                      non_usa: bool = False) -> bool:
    """Build abraxas C23 binary (statically links libmeridian)."""
    c23_dir = source_dir / "c23"
    binary = c23_dir / "abraxas"

    # Skip build if already exists (unless forced)
    if binary.exists() and not force:
        log_info("abraxas (C23) already built (use --rebuild to force)")
        return True

    if non_usa:
        log_info("BUILDING ABRAXAS [C23] (non-USA: weather disabled)")
    else:
        log_info("BUILDING ABRAXAS [C23]")

    # Check for make
    ret, _, _ = run_cmd_capture(["which", "make"])
    if ret != 0:
        log_error("make not found. Install build tools: sudo pacman -S base-devel")
        return False

    # Check for gcc
    ret, _, _ = run_cmd_capture(["which", "gcc"])
    if ret != 0:
        log_error("gcc not found. Install build tools: sudo pacman -S base-devel")
        return False

    # Build (top-level make builds libmeridian.a then daemon)
    make_cmd = ["make", "-C", str(c23_dir)]
    if non_usa:
        make_cmd.append("NOAA=0")
    ret = run_cmd(make_cmd + ["clean"])
    ret = run_cmd(make_cmd)

    if ret != 0:
        log_error("C23 build failed!")
        return False

    if not binary.exists():
        log_error("Build completed but abraxas binary not found")
        return False

    log_info(f"Built: {binary}")
    return True


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

    ret = run_cmd(cargo_cmd, cwd=rust_dir)

    if ret != 0:
        log_error("Rust build failed!")
        return False

    binary = rust_dir / "target" / "release" / "abraxas"
    if not binary.exists():
        log_error("Build completed but binary not found")
        return False

    log_info(f"Built: {binary}")
    return True


# =============================================================================
# COMMANDS
# =============================================================================

def cmd_install(args, source_dir: Path) -> bool:
    """Install abraxas."""
    log_info("INSTALLING ABRAXAS")

    source_service = source_dir / "abraxas.service"
    source_zipdb = source_dir / "us_zipcodes.bin"

    # Rust is the only implementation path now. The C23 source tree
    # under c23/ is preserved (build_abraxas_c23 still defined) but
    # the install path no longer offers it -- DEMIURGE is monoculture-
    # Rust. Restoring the C23 install path is a contained change to
    # this function plus the argparse table at the bottom.

    # Determine NOAA mode: --non-usa flag overrides, otherwise prompt
    if args.non_usa:
        non_usa = True
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
    source_binary = source_dir / "rust" / "target" / "release" / "abraxas"

    # Create directories
    log_info("Creating directories...")
    try:
        INSTALL_BINARY.parent.mkdir(parents=True, exist_ok=True)
        INSTALL_CONFIG_DIR.mkdir(parents=True, exist_ok=True)
        INSTALL_SERVICE.parent.mkdir(parents=True, exist_ok=True)
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

    # Fix ownership if running as root
    if os.environ.get("SUDO_USER"):
        try:
            pw = pwd.getpwnam(_real_user)
            uid, gid = pw.pw_uid, pw.pw_gid

            os.chown(INSTALL_BINARY, uid, gid)
            wl_dest = INSTALL_BINARY.parent / "meridian_wl.so"
            if wl_dest.exists():
                os.chown(wl_dest, uid, gid)
            if INSTALL_SERVICE.exists():
                os.chown(INSTALL_SERVICE, uid, gid)

            for f in INSTALL_CONFIG_DIR.iterdir():
                os.chown(f, uid, gid)
        except (KeyError, OSError) as e:
            log_warn(f"Failed to fix ownership: {e}")

    print()
    log_info("SUCCESS. Installation complete.")
    log_info(f"Binary: {INSTALL_BINARY}")

    # Offer to enable service
    if not args.no_service and INSTALL_SERVICE.exists():
        if not is_service_enabled():
            print()
            response = input("Enable abraxas service? [Y/N]: ").strip().lower()
            if response in ("", "y", "yes"):
                enable_service()
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

    # Implementation is fixed at Rust now; the impl_file marker may
    # still exist on systems upgraded from a C23-era install but is
    # no longer authoritative. Show binary size for sanity instead.
    if binary_ok:
        size = INSTALL_BINARY.stat().st_size
        print(f"  Language: RUST ({size // 1024} KB)")
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


def cmd_test(args, source_dir: Path) -> bool:
    """Show implementation comparison table."""
    print()
    print("  ════════════════════════════════════════════════════════════")
    print("                  MAIN EVENT: C23 vs. Rust")
    print("  ════════════════════════════════════════════════════════════")
    print("  ┌──────────────────┬───────────────────┬───────────────────┐")
    print("  │                  │ C23               │ Rust              │")
    print("  ├──────────────────┼───────────────────┼───────────────────┤")
    print("  │ Compiler         │ GCC 15 (-std=c2x) │ rustc 1.75+       │")
    print("  │ Binary size      │ ~69 KB            │ ~598 KB           │")
    print("  │ Shared libs      │ 3                 │ 4                 │")
    print("  │ Memory model     │ manual alloc/free │ ownership/borrow  │")
    print("  │ Gamma: DRM       │ raw ioctl         │ raw ioctl         │")
    print("  │ Gamma: Wayland   │ dlopen plugin     │ wayland-client    │")
    print("  │ Gamma: X11       │ dlopen            │ x11rb             │")
    print("  │ Gamma: GNOME     │ dlopen sd-bus     │ libsystemd/sd-bus │")
    print("  │ Weather          │ curl(1) via spawn │ curl(1) via spawn │")
    print("  │ Config parse     │ hand-rolled JSON  │ serde_json        │")
    print("  │ Event loop       │ io_uring          │ io_uring          │")
    print("  │ Sandbox          │ seccomp + landlock │ seccomp + landlock│")
    print("  │ Hardening        │ prctl             │ prctl             │")
    print("  │ LTO              │ yes (-flto=auto)  │ yes (cargo)       │")
    print("  │ Static build     │ make static (musl)│ musl static-pie   │")
    print("  └──────────────────┴───────────────────┴───────────────────┘")

    # Show actual binary sizes if built
    c23_bin = source_dir / "c23" / "abraxas"
    rust_bin = source_dir / "rust" / "target" / "release" / "abraxas"
    print()
    if c23_bin.exists():
        size = c23_bin.stat().st_size
        log_info(f"C23 binary:  {c23_bin} ({size // 1024} KB)")
    else:
        log_info("C23 binary:  not built")
    if rust_bin.exists():
        size = rust_bin.stat().st_size
        log_info(f"Rust binary: {rust_bin} ({size // 1024} KB)")
    else:
        log_info("Rust binary: not built")

    print()
    return True


def cmd_update(args, source_dir: Path) -> bool:
    """Update abraxas if source is newer."""
    log_info("CHECKING FOR UPDATES")
    log_info("Implementation: rust")

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
    source_binary = source_dir / "rust" / "target" / "release" / "abraxas"

    # Stop ALL running abraxas processes before overwriting binary
    log_info("Stopping existing abraxas processes...")
    stop_all_abraxas()

    log_info("Installing updates...")
    shutil.copy2(source_binary, INSTALL_BINARY)
    INSTALL_BINARY.chmod(0o755)

    # Fix ownership if running as root
    if os.environ.get("SUDO_USER"):
        try:
            pw = pwd.getpwnam(_real_user)
            os.chown(INSTALL_BINARY, pw.pw_uid, pw.pw_gid)
            wl_dest = INSTALL_BINARY.parent / "meridian_wl.so"
            if wl_dest.exists():
                os.chown(wl_dest, pw.pw_uid, pw.pw_gid)
        except (KeyError, OSError) as e:
            log_warn(f"Failed to fix ownership: {e}")

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
  test        Show C23 vs Rust comparison table

Examples:
  ./install.py                    # Install (prompts for NOAA)
  ./install.py --no-service       # Install without prompting for service
  ./install.py --non-usa          # Install without NOAA weather support
  ./install.py test               # Show implementation comparison
  ./install.py status             # Check installation status
  ./install.py enable             # Enable service
  ./install.py uninstall          # Remove installation
"""
    )

    parser.add_argument("command", nargs="?", default="install",
                       choices=["install", "uninstall", "status", "enable", "disable", "update", "test"],
                       help="Command to run (default: install)")
    parser.add_argument("--no-service", action="store_true",
                       help="Don't prompt to enable service after install")
    parser.add_argument("--rebuild", action="store_true",
                       help="Force rebuild even if already built")
    parser.add_argument("--non-usa", action="store_true",
                       help="Disable NOAA weather support")
    # --impl: accepted for backward compat, ignored. The C23 path is
    # unwired; only the Rust implementation is built and installed.
    parser.add_argument("--impl", dest="impl_choice", choices=["c23", "rust"],
                       default=None,
                       help=argparse.SUPPRESS)
    parser.add_argument("--test", action="store_true",
                       help="Show C23 vs Rust comparison table")

    args = parser.parse_args()

    # --test flag overrides command
    if args.test:
        args.command = "test"

    source_dir = Path(__file__).parent.resolve()

    print()
    log_info("abraxas installer")
    log_info(f"Source: {source_dir}")
    print()

    # Run command
    commands = {
        "install": cmd_install,
        "uninstall": cmd_uninstall,
        "status": cmd_status,
        "enable": cmd_enable,
        "disable": cmd_disable,
        "update": cmd_update,
        "test": cmd_test,
    }

    success = commands[args.command](args, source_dir)
    return 0 if success else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        print("\nInterrupted by user.")
        sys.exit(130)
