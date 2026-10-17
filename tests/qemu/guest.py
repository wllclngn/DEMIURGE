#!/usr/bin/env python3
"""
QEMU guest for the DEMIURGE tests that must never run on the host: anything
that grabs input, takes a VT, runs PAM, runs setuid or installs a sandbox
(the GORDIAN KNOT suite). A failure in here strands a throwaway VM, never
the host session.

Disk layers, all under ~/.cache/demiurge-qemu/:
  Arch-Linux-x86_64-cloudimg.qcow2  the official Arch cloud image,
                                    checksum-verified, never written
  provisioned.qcow2                 overlay on it: packages, the test
                                    user, the PAM service
  run.qcow2                         overlay on provisioned, recreated for
                                    every run, so each run starts clean

Nothing on the host needs root: KVM, user-mode networking (ssh forwarded
to a free localhost port) and cloud-init's NoCloud seed served over HTTP
to the guest at 10.0.2.2.

Usage:
  tests/qemu/guest.py fetch       download + verify the cloud image
  tests/qemu/guest.py provision   build provisioned.qcow2 (once)
  tests/qemu/guest.py test [-k NAME]
                                  build, push, run tests/qemu/gk_suite.py
  tests/qemu/guest.py shell       fresh run overlay + an interactive ssh
"""

import argparse
import functools
import hashlib
import http.server
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
from pathlib import Path

CACHE = Path.home() / ".cache" / "demiurge-qemu"
IMAGE_URL = "https://geo.mirror.pkgbuild.com/images/latest/Arch-Linux-x86_64-cloudimg.qcow2"
BASE = CACHE / "Arch-Linux-x86_64-cloudimg.qcow2"
SUMS = CACHE / "Arch-Linux-x86_64-cloudimg.qcow2.SHA256"
PROVISIONED = CACHE / "provisioned.qcow2"
RUN = CACHE / "run.qcow2"
KEY = CACHE / "id_ed25519"
SERIAL_LOG = CACHE / "serial.log"

REPO = Path(__file__).resolve().parents[2]
BUILD_DIR = Path("/tmp/demiurge-build")
SUITE = Path(__file__).resolve().parent / "gk_suite.py"

GUEST_USER = "arch"
TEST_USER = "gk"
TEST_PASSWORD = "knot-test-pw"
PACKAGES = [
    "python", "pango", "cairo", "libxkbcommon", "ttf-dejavu",
    "xorg-server-xvfb", "xorg-xauth",
    # logind refuses a non-root sleep inhibitor without polkit, as on any
    # desktop that has it; the cloud image ships without.
    "polkit",
]

MEMORY = "4096"
CPUS = "4"
BOOT_TIMEOUT = 240


def log(level: str, msg: str) -> None:
    print(f"[{time.strftime('%H:%M:%S')}] {'[' + level + ']':<8} {msg}", flush=True)


def die(msg: str) -> None:
    log("ERROR", msg)
    sys.exit(1)


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def ensure_key() -> str:
    if not KEY.exists():
        CACHE.mkdir(parents=True, exist_ok=True)
        subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "demiurge-qemu",
                        "-f", str(KEY)], check=True)
    return KEY.with_suffix(".pub").read_text().strip()


def fetch() -> None:
    CACHE.mkdir(parents=True, exist_ok=True)
    if not SUMS.exists():
        urllib.request.urlretrieve(IMAGE_URL + ".SHA256", SUMS)
    want = SUMS.read_text().split()[0]
    if BASE.exists() and sha256(BASE) == want:
        log("INFO", f"cloud image present and verified: {BASE}")
        return
    part = BASE.with_suffix(".part")
    log("INFO", f"downloading {IMAGE_URL}")
    urllib.request.urlretrieve(IMAGE_URL, part)
    if sha256(part) != want:
        part.unlink()
        die("cloud image checksum mismatch; download discarded")
    part.rename(BASE)
    log("INFO", f"verified: {BASE}")


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def overlay(path: Path, backing: Path) -> None:
    path.unlink(missing_ok=True)
    subprocess.run(["qemu-img", "create", "-q", "-f", "qcow2", "-b", str(backing), "-F", "qcow2",
                    str(path)], check=True)


class _QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *args) -> None:
        pass


class SeedServer:
    """cloud-init NoCloud seed: The guest fetches user-data and meta-data
    from http://10.0.2.2:PORT/, which slirp maps to this host loopback."""

    def __init__(self, pubkey: str):
        self.dir = tempfile.TemporaryDirectory(prefix="demiurge-seed-")
        root = Path(self.dir.name)
        (root / "meta-data").write_text("instance-id: demiurge-qemu\nlocal-hostname: knot\n")
        (root / "user-data").write_text(f"#cloud-config\nssh_authorized_keys:\n  - {pubkey}\n")
        (root / "vendor-data").write_text("")
        handler = functools.partial(_QuietHandler, directory=str(root))
        self.httpd = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
        self.port = self.httpd.server_address[1]
        threading.Thread(target=self.httpd.serve_forever, daemon=True).start()

    def close(self) -> None:
        self.httpd.shutdown()
        self.dir.cleanup()


class Guest:
    def __init__(self, disk: Path, seed: SeedServer | None = None):
        self.disk = disk
        self.seed = seed
        self.port = free_port()
        self.proc: subprocess.Popen | None = None

    def __enter__(self) -> "Guest":
        cmd = [
            "qemu-system-x86_64", "-enable-kvm", "-cpu", "host",
            "-m", MEMORY, "-smp", CPUS,
            "-drive", f"file={self.disk},if=virtio,format=qcow2",
            "-nic", f"user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:{self.port}-:22",
            "-display", "none", "-vga", "std",
            "-serial", f"file:{SERIAL_LOG}",
            "-no-reboot",
        ]
        if self.seed:
            cmd += ["-smbios", f"type=1,serial=ds=nocloud;s=http://10.0.2.2:{self.seed.port}/"]
        self.proc = subprocess.Popen(cmd, stdin=subprocess.DEVNULL)
        self.wait_ssh()
        return self

    def __exit__(self, *exc) -> None:
        if self.proc and self.proc.poll() is None:
            self.ssh("sudo systemctl poweroff", check=False, timeout=15)
            try:
                self.proc.wait(timeout=60)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()

    def ssh_base(self) -> list[str]:
        return ["ssh", "-i", str(KEY), "-p", str(self.port),
                "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
                "-o", "LogLevel=ERROR", "-o", "ConnectTimeout=3", "-o", "BatchMode=yes",
                f"{GUEST_USER}@127.0.0.1"]

    def ssh(self, command: str, check: bool = True, timeout: float | None = None,
            capture: bool = False) -> subprocess.CompletedProcess:
        return subprocess.run(self.ssh_base() + [command], check=check, timeout=timeout,
                              text=True, capture_output=capture)

    def push(self, local: Path, remote: str) -> None:
        subprocess.run(["scp", "-q", "-i", str(KEY), "-P", str(self.port),
                        "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
                        "-o", "LogLevel=ERROR", "-o", "BatchMode=yes",
                        str(local), f"{GUEST_USER}@127.0.0.1:{remote}"], check=True)

    def wait_ssh(self) -> None:
        deadline = time.monotonic() + BOOT_TIMEOUT
        while time.monotonic() < deadline:
            if self.proc.poll() is not None:
                die(f"qemu exited ({self.proc.returncode}); see {SERIAL_LOG}")
            r = subprocess.run(self.ssh_base() + ["true"], capture_output=True)
            if r.returncode == 0:
                log("INFO", f"guest up, ssh on 127.0.0.1:{self.port}")
                return
            time.sleep(2)
        die(f"guest never answered ssh within {BOOT_TIMEOUT}s; see {SERIAL_LOG}")


def provision() -> None:
    if not BASE.exists():
        fetch()
    pubkey = ensure_key()
    overlay(PROVISIONED, BASE)
    subprocess.run(["qemu-img", "resize", "-q", str(PROVISIONED), "20G"], check=True)
    seed = SeedServer(pubkey)
    try:
        with Guest(PROVISIONED, seed) as g:
            g.ssh("sudo cloud-init status --wait >/dev/null")
            log("INFO", "upgrading and installing: " + " ".join(PACKAGES))
            g.ssh("sudo pacman -Syu --noconfirm --needed " + " ".join(PACKAGES))
            g.ssh(f"sudo useradd -m {TEST_USER} && echo '{TEST_USER}:{TEST_PASSWORD}' | sudo chpasswd")
            g.ssh("printf 'auth include system-auth\\n' | sudo tee /etc/pam.d/gordian_knot >/dev/null")
            g.ssh("sudo touch /etc/cloud/cloud-init.disabled")
            log("INFO", "provisioned")
    finally:
        seed.close()
    log("INFO", f"wrote {PROVISIONED}")


def build() -> tuple[Path, Path]:
    env = dict(os.environ, CARGO_TARGET_DIR=str(BUILD_DIR))
    subprocess.run(["cargo", "build", "--release", "-p", "demiurge-x11", "--bin", "gordian_knot",
                    "--example", "gk_probe"], cwd=REPO, env=env, check=True)
    return BUILD_DIR / "release" / "gordian_knot", BUILD_DIR / "release" / "examples" / "gk_probe"


def run_tests(selector: str | None) -> int:
    if not PROVISIONED.exists():
        die("no provisioned image; run: tests/qemu/guest.py provision")
    locker, probe = build()
    overlay(RUN, PROVISIONED)
    with Guest(RUN) as g:
        g.push(locker, "/tmp/gordian_knot")
        g.push(probe, "/tmp/gk-probe")
        g.push(SUITE, "/tmp/gk_suite.py")
        g.ssh("sudo install -o root -g root -m 4755 /tmp/gordian_knot /usr/local/bin/gordian_knot"
              " && sudo install -m 0755 /tmp/gk-probe /usr/local/bin/gk-probe")
        args = f" -k {selector}" if selector else ""
        r = g.ssh(f"sudo python3 /tmp/gk_suite.py --user {TEST_USER} --password {TEST_PASSWORD}{args}",
                  check=False)
        return r.returncode


def shell() -> None:
    if not PROVISIONED.exists():
        die("no provisioned image; run: tests/qemu/guest.py provision")
    overlay(RUN, PROVISIONED)
    with Guest(RUN) as g:
        subprocess.run(g.ssh_base())


def main() -> None:
    for tool in ("qemu-system-x86_64", "qemu-img", "ssh", "scp", "ssh-keygen"):
        if not shutil.which(tool):
            die(f"{tool} not found")
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("command", choices=["fetch", "provision", "test", "shell"])
    p.add_argument("-k", metavar="NAME", help="run only tests whose name contains NAME")
    a = p.parse_args()
    if a.command == "fetch":
        fetch()
    elif a.command == "provision":
        provision()
    elif a.command == "test":
        sys.exit(run_tests(a.k))
    else:
        shell()


if __name__ == "__main__":
    main()
