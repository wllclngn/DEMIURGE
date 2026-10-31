#!/usr/bin/env python3
"""
QEMU guest for the DEMIURGE tests that must never run on the host: anything
that grabs input, takes a VT, runs PAM, runs setuid or installs a sandbox
(the GORDIAN KNOT suite), and demiurge-wl on a real screen (the Wayland
suite, tests/qemu/wayland_suite.py). A failure in here strands a throwaway
VM, never the host session.

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
import json
import os
import shutil
import socket
import struct
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
QMP_SOCKET = CACHE / "qmp.sock"
VNC_SOCKET = CACHE / "vnc.sock"
VNC1_SOCKET = CACHE / "vnc1.sock"
GPU_ID = "gpu"

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
    # demiurge-wl-tty: GBM and EGL on virtio-gpu, the keyboard and mouse,
    # a seat it can take from an ssh session, and XWayland for its X11
    # clients.
    "mesa", "libinput", "seatd", "xorg-xwayland",
    # quark's research runtime, run in demiurge-wl-tty: Its PipeWire audio
    # and, when built with Khronos's registry, its Vulkan loader.
    "libpipewire", "vulkan-icd-loader",
    # The clipboard from each side of XWayland's bridge.
    "xclip", "wl-clipboard",
    # The greeter: greetd starts gordian_knot --login inside cage, which
    # starts either stack's session, the X11 one through startx.
    "greetd", "cage", "xorg-server", "xorg-xinit",
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


class Qmp:
    """QEMU's machine protocol: Key events, screendumps, power."""

    def __init__(self, path: Path):
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.connect(str(path))
        self.buf = b""
        self.read()  # greeting
        self.call("qmp_capabilities")

    def read(self) -> dict:
        while b"\n" not in self.buf:
            chunk = self.sock.recv(4096)
            if not chunk:
                raise ConnectionError("QMP closed")
            self.buf += chunk
        line, self.buf = self.buf.split(b"\n", 1)
        return json.loads(line)

    def call(self, cmd: str, **arguments) -> dict:
        msg = {"execute": cmd}
        if arguments:
            msg["arguments"] = arguments
        self.sock.sendall(json.dumps(msg).encode() + b"\n")
        while True:
            reply = self.read()
            if "return" in reply or "error" in reply:
                return reply

    def keys(self, *qcodes: str, hold_ms: int | None = None) -> None:
        """Press the keys together, then release them, after `hold_ms`."""
        args = {"keys": [{"type": "qcode", "data": q} for q in qcodes]}
        if hold_ms is not None:
            args["hold-time"] = hold_ms
        self.call("send-key", **args)

    def point(self, x: float, y: float) -> None:
        """Move the tablet's pointer to a fraction of the screen."""
        self.call("input-send-event", events=[
            {"type": "abs", "data": {"axis": "x", "value": round(x * 32767)}},
            {"type": "abs", "data": {"axis": "y", "value": round(y * 32767)}},
        ])

    def place(self, x: int, y: int, width: int, height: int) -> None:
        """Move the tablet's pointer onto pixel (x, y) of a width x height
        screen exactly: libinput scales its 0..32767 by size / 32768."""
        self.call("input-send-event", events=[
            {"type": "abs", "data": {"axis": "x", "value": -(-x * 32768 // width)}},
            {"type": "abs", "data": {"axis": "y", "value": -(-y * 32768 // height)}},
        ])

    def move(self, dx: int, dy: int) -> None:
        """Relative motion, from the PS/2 mouse beside the tablet."""
        self.call("input-send-event", events=[
            {"type": "rel", "data": {"axis": "x", "value": dx}},
            {"type": "rel", "data": {"axis": "y", "value": dy}},
        ])

    def click(self, button: str = "left") -> None:
        for down in (True, False):
            self.button(button, down)
        # libinput takes a release and a press inside 25 ms of each other
        # for a bouncing contact and drops both: The next click waits it out.
        time.sleep(0.05)

    def button(self, button: str, down: bool) -> None:
        """A mouse button pressed, held until let go."""
        self.call("input-send-event", events=[{"type": "btn", "data": {"down": down, "button": button}}])

    def hold(self, qcode: str, down: bool) -> None:
        """A key pressed, held until let go."""
        self.call("input-send-event", events=[{"type": "key", "data": {"down": down, "key": {"type": "qcode", "data": qcode}}}])

    def screendump(self, path: Path, device: str = GPU_ID, head: int = 0) -> tuple[int, int, bytes]:
        """What `head` of `device` shows, as (width, height, RGB bytes)."""
        path.unlink(missing_ok=True)
        reply = self.call("screendump", filename=str(path), device=device, head=head)
        if "error" in reply:
            raise RuntimeError(f"screendump: {reply['error']}")
        data = path.read_bytes()
        # P6 header: magic, width height, maxval, one whitespace byte.
        fields, end = [], 0
        while len(fields) < 4:
            while data[end:end + 1].isspace():
                end += 1
            start = end
            while not data[end:end + 1].isspace():
                end += 1
            fields.append(data[start:end])
        if fields[0] != b"P6" or fields[3] != b"255":
            raise RuntimeError(f"screendump is not 8-bit P6: {fields}")
        return int(fields[1]), int(fields[2]), data[end + 1:]


class Vnc:
    """One of QEMU's VNC servers, on head 0 or head 1, for the one thing QMP
    has no command for: RFB's SetDesktopSize reaches the guest through the
    console's UI info, as a QEMU window's resize does, so the head's
    monitor changes size, at 0x0 is unplugged, and on head 1, which starts
    unplugged, is plugged in."""

    def __init__(self, path: Path):
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.connect(str(path))
        self.recv(12)  # the server's protocol version
        self.sock.sendall(b"RFB 003.008\n")
        types = self.recv(self.recv(1)[0])
        if 1 not in types:
            raise RuntimeError(f"VNC offers no unauthenticated access: {list(types)}")
        self.sock.sendall(bytes([1]))
        if struct.unpack(">I", self.recv(4))[0] != 0:
            raise RuntimeError("VNC refused the connection")
        self.sock.sendall(bytes([1]))  # ClientInit, shared
        # ServerInit: Size and pixel format, then the desktop's name.
        self.recv(struct.unpack(">20xI", self.recv(24))[0])
        # SetEncodings: ExtendedDesktopSize, the extension SetDesktopSize
        # belongs to.
        self.sock.sendall(struct.pack(">BxHi", 2, 1, -308))

    def recv(self, n: int) -> bytes:
        data = b""
        while len(data) < n:
            chunk = self.sock.recv(n - len(data))
            if not chunk:
                raise ConnectionError("VNC closed")
            data += chunk
        return data

    def head(self, width: int, height: int) -> None:
        """The head's monitor at width x height; 0 x 0 unplugs it."""
        self.sock.sendall(struct.pack(">BxHHBx", 251, width, height, 1)
                          + struct.pack(">IHHHHI", 0, 0, 0, width, height, 0))


class Guest:
    """`gpu` is "std" (the GORDIAN KNOT suite's framebuffer console) or
    "virtio" (virtio-gpu with two heads, for demiurge-wl-tty); `qmp`
    opens the machine protocol as self.qmp, and with virtio-gpu QEMU's
    VNC server as self.vnc."""

    def __init__(self, disk: Path, seed: SeedServer | None = None, gpu: str = "std", qmp: bool = False):
        self.disk = disk
        self.seed = seed
        self.gpu = gpu
        self.port = free_port()
        self.proc: subprocess.Popen | None = None
        self.qmp: Qmp | None = None
        self.vnc: Vnc | None = None
        self.vnc1: Vnc | None = None
        self.qmp_wanted = qmp

    def __enter__(self) -> "Guest":
        cmd = [
            "qemu-system-x86_64", "-enable-kvm", "-cpu", "host",
            "-m", MEMORY, "-smp", CPUS,
            "-drive", f"file={self.disk},if=virtio,format=qcow2",
            "-nic", f"user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:{self.port}-:22",
            "-display", "none",
            "-serial", f"file:{SERIAL_LOG}",
            "-no-reboot",
        ]
        if self.gpu == "virtio":
            # An absolute tablet beside the PS/2 mouse, so QMP can place
            # the pointer exactly, and the PS/2 mouse a relative one for
            # QMP's relative motion: With the VMware port, QEMU's VMMouse,
            # the guest drives it as absolute.
            cmd += ["-machine", "vmport=off", "-vga", "none",
                    "-device", f"virtio-gpu-pci,max_outputs=2,id={GPU_ID}",
                    "-device", "qemu-xhci,id=xhci", "-device", "usb-tablet,bus=xhci.0"]
        else:
            cmd += ["-vga", "std"]
        if self.qmp_wanted:
            QMP_SOCKET.unlink(missing_ok=True)
            cmd += ["-qmp", f"unix:{QMP_SOCKET},server=on,wait=off"]
            if self.gpu == "virtio":
                VNC_SOCKET.unlink(missing_ok=True)
                VNC1_SOCKET.unlink(missing_ok=True)
                cmd += ["-vnc", f"unix:{VNC_SOCKET}",
                        "-vnc", f"unix:{VNC1_SOCKET},id=head1,display={GPU_ID},head=1"]
        if self.seed:
            cmd += ["-smbios", f"type=1,serial=ds=nocloud;s=http://10.0.2.2:{self.seed.port}/"]
        self.proc = subprocess.Popen(cmd, stdin=subprocess.DEVNULL)
        self.wait_ssh()
        if self.qmp_wanted:
            self.qmp = Qmp(QMP_SOCKET)
            if self.gpu == "virtio":
                self.vnc = Vnc(VNC_SOCKET)
                self.vnc1 = Vnc(VNC1_SOCKET)
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
            g.ssh(f"sudo systemctl enable seatd && sudo usermod -aG seat {GUEST_USER}")
            # quark's runtime takes its Windows sync objects from ntsync.
            g.ssh("echo ntsync | sudo tee /etc/modules-load.d/ntsync.conf >/dev/null")
            g.ssh("sudo touch /etc/cloud/cloud-init.disabled")
            log("INFO", "provisioned")
    finally:
        seed.close()
    log("INFO", f"wrote {PROVISIONED}")


def build() -> tuple[Path, Path]:
    env = dict(os.environ, CARGO_TARGET_DIR=str(BUILD_DIR))
    subprocess.run(["cargo", "build", "--release", "-p", "gordian_knot-x11", "--bin", "gordian_knot",
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
