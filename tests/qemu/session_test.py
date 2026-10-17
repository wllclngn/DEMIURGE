#!/usr/bin/env python3
"""
GORDIAN KNOT in a real DEMIURGE session, in QEMU.

The disk is the one `[0] RANDOM/ARCH/arch-vm.py test --demiurge` leaves:
Arch installed by arch-install.py, DEMIURGE installed by post-install.py
through its own install.py. This boots a throwaway overlay of it (the
installed image is never written), turns on a test-only autologin on
tty1, and lets the user's own ~/.bash_profile start the session the way
it starts on archd: startx, real Xorg on vt1, demiurge.

Then, against the real thing:
  - demiurge starts gordian_knot --daemon from [input.idle] lock_seconds
  - the daemon locks the idle session; the password unlocks it
  - a `lock` keybind locks; the password unlocks it
  - the VT lock takes a fresh console away from Xorg, accepts the password
    from a real (uinput) keyboard and gives vt1 back to X
  - lock-before-suspend: S3 through systemd, woken over QMP, locked on resume

Usage: tests/qemu/session_test.py      (after arch-vm.py test --demiurge)
"""

import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ARCH_VM = Path.home() / ".cache" / "arch-vm"
BASE_DISK = ARCH_VM / "arch-test.qcow2"
BASE_VARS = ARCH_VM / "OVMF_VARS.4m.fd"
KEY = ARCH_VM / "test" / "id_ed25519"
WORK = ARCH_VM / "session"
OVMF_CODE = next(p for p in (Path("/usr/share/edk2/x64/OVMF_CODE.4m.fd"),
                             Path("/usr/share/edk2-ovmf/x64/OVMF_CODE.4m.fd")) if p.exists())

USER = "mod"
PASSWORD = "arch-vm-test"
REPO = Path(__file__).resolve().parents[2]
BUILD_DIR = Path("/tmp/demiurge-build")
LOCK_SECONDS = 10

PROBE_ENV = f"DISPLAY=:0 XAUTHORITY=/home/{USER}/.Xauthority"


def log(level: str, msg: str) -> None:
    print(f"[{time.strftime('%H:%M:%S')}] {'[' + level + ']':<8} {msg}", flush=True)


def wait_for(pred, timeout: float, step: float = 0.5) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if pred():
            return True
        time.sleep(step)
    return pred()


class Qmp:
    def __init__(self, path: Path):
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.connect(str(path))
        self.buf = b""
        self.read()  # greeting
        self.call("qmp_capabilities")

    def read(self) -> dict:
        while b"\n" not in self.buf:
            self.buf += self.sock.recv(4096)
        line, self.buf = self.buf.split(b"\n", 1)
        return json.loads(line)

    def call(self, cmd: str) -> dict:
        self.sock.sendall(json.dumps({"execute": cmd}).encode() + b"\n")
        while True:
            msg = self.read()
            if "return" in msg or "error" in msg:
                return msg


class Vm:
    def __init__(self):
        shutil.rmtree(WORK, ignore_errors=True)
        WORK.mkdir(parents=True)
        self.disk = WORK / "session.qcow2"
        subprocess.run(["qemu-img", "create", "-q", "-f", "qcow2", "-b", str(BASE_DISK), "-F", "qcow2",
                        str(self.disk)], check=True)
        shutil.copy2(BASE_VARS, WORK / "vars.fd")
        with socket.socket() as s:
            s.bind(("127.0.0.1", 0))
            self.port = s.getsockname()[1]
        self.qmp_path = WORK / "qmp.sock"
        self.proc = subprocess.Popen([
            "qemu-system-x86_64", "-machine", "q35", "-enable-kvm", "-cpu", "host",
            "-m", "4G", "-smp", "4",
            "-drive", f"if=pflash,format=raw,readonly=on,file={OVMF_CODE}",
            "-drive", f"if=pflash,format=raw,file={WORK / 'vars.fd'}",
            "-drive", f"file={self.disk},if=virtio,format=qcow2",
            "-nic", f"user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:{self.port}-:22",
            "-vga", "std", "-display", "none",
            "-serial", f"file:{WORK / 'serial.log'}",
            # S3 for the lock-before-suspend case; QMP wakes it.
            "-global", "ICH9-LPC.disable_s3=0",
            "-qmp", f"unix:{self.qmp_path},server=on,wait=off",
        ], stdin=subprocess.DEVNULL)
        if not wait_for(lambda: self.run("true").returncode == 0, 240, 3):
            raise RuntimeError(f"guest never answered ssh; see {WORK / 'serial.log'}")
        self.qmp = Qmp(self.qmp_path)

    def ssh(self) -> list[str]:
        return ["ssh", "-i", str(KEY), "-p", str(self.port), "-o", "StrictHostKeyChecking=no",
                "-o", "UserKnownHostsFile=/dev/null", "-o", "LogLevel=ERROR",
                "-o", "ConnectTimeout=3", "-o", "BatchMode=yes", f"{USER}@127.0.0.1"]

    def run(self, cmd: str, timeout: float = 60) -> subprocess.CompletedProcess:
        try:
            return subprocess.run(self.ssh() + [cmd], capture_output=True, text=True, timeout=timeout)
        except subprocess.TimeoutExpired:
            return subprocess.CompletedProcess(cmd, 124, "", "timeout")

    def out(self, cmd: str) -> str:
        return self.run(cmd).stdout.strip()

    def push(self, local: Path, remote: str) -> None:
        subprocess.run(["scp", "-q", "-i", str(KEY), "-P", str(self.port), "-o", "StrictHostKeyChecking=no",
                        "-o", "UserKnownHostsFile=/dev/null", "-o", "LogLevel=ERROR", "-o", "BatchMode=yes",
                        str(local), f"{USER}@127.0.0.1:{remote}"], check=True)

    def probe(self, args: str) -> str:
        return self.out(f"{PROBE_ENV} gk-probe {args}")

    def locked(self) -> bool:
        if self.probe("grab-state") != "held":
            return False
        painted = self.probe("painted")
        return painted.isdigit() and int(painted) > 2

    def holder_running(self) -> bool:
        return self.run("pgrep -x -f /usr/local/bin/gordian_knot >/dev/null || "
                        "pgrep -x -f gordian_knot >/dev/null").returncode == 0

    def unlock(self) -> bool:
        self.probe(f"type {PASSWORD}")
        self.probe("key Return")
        return wait_for(lambda: self.probe("grab-state") == "free" and not self.holder_running(), 20)

    def close(self) -> None:
        self.run("sudo -n systemctl poweroff", timeout=10)
        try:
            self.proc.wait(timeout=60)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()


def setup(vm: Vm) -> None:
    env = dict(os.environ, CARGO_TARGET_DIR=str(BUILD_DIR))
    subprocess.run(["cargo", "build", "--release", "-p", "demiurge-x11", "--example", "gk_probe"],
                   cwd=REPO, env=env, check=True)
    vm.push(BUILD_DIR / "release" / "examples" / "gk_probe", "/tmp/gk-probe")
    with tempfile.TemporaryDirectory() as d:
        autologin = Path(d) / "autologin.conf"
        autologin.write_text("[Service]\nExecStart=\n"
                             f"ExecStart=-/sbin/agetty -o '-p -f -- \\\\u' --noclear --autologin {USER} %I $TERM\n")
        vm.push(autologin, "/tmp/autologin.conf")
        edit = Path(d) / "edit.py"
        edit.write_text(f"""
import re
from pathlib import Path
p = Path.home() / ".config/demiurge/config.toml"
t = p.read_text()
t = re.sub(r"(?m)^lock_seconds = .*$", "lock_seconds = {LOCK_SECONDS}", t)
t += '\\n[[keybind]]\\nmods = ["Super"]\\nkey = "Escape"\\naction = "lock"\\n'
p.write_text(t)
""")
        vm.push(edit, "/tmp/edit.py")
    steps = [
        "sudo -n install -m 0755 /tmp/gk-probe /usr/local/bin/gk-probe",
        "sudo -n install -D -m 0644 /tmp/autologin.conf /etc/systemd/system/getty@tty1.service.d/autologin.conf",
        "python3 /tmp/edit.py",
        "grep -q 'lock_seconds = 10' ~/.config/demiurge/config.toml",
        "sudo -n systemctl daemon-reload && sudo -n systemctl restart getty@tty1",
    ]
    for s in steps:
        r = vm.run(s)
        if r.returncode != 0:
            raise RuntimeError(f"setup step failed: {s}\n{r.stdout}{r.stderr}")
    if not wait_for(lambda: vm.run("pgrep -x demiurge && test -S /tmp/.X11-unix/X0").returncode == 0, 60):
        raise RuntimeError("the tty1 autologin never brought up startx + demiurge")
    time.sleep(2)


def test_wm_starts_the_daemon(vm: Vm) -> str | None:
    if not wait_for(lambda: vm.run("pgrep -f 'gordian_knot --daemon'").returncode == 0, 20):
        return "demiurge did not start gordian_knot --daemon with lock_seconds set"
    return None


def test_idle_lock_and_unlock(vm: Vm) -> str | None:
    if not wait_for(vm.locked, LOCK_SECONDS + 30, 1):
        return f"the idle session was not locked within {LOCK_SECONDS + 30}s"
    if not vm.unlock():
        return "the right password did not unlock the idle lock"
    # The rest of the run drives input on its own schedule.
    vm.run("pkill -f 'gordian_knot --daemon'")
    return None


def test_keybind_lock(vm: Vm) -> str | None:
    vm.probe("key super+Escape")
    if not wait_for(vm.locked, 10):
        return "Super+Escape (action = \"lock\") did not lock"
    if not vm.unlock():
        return "the right password did not unlock the keybind lock"
    return None


def test_vt_lock_over_xorg(vm: Vm) -> str | None:
    before = vm.out("cat /sys/class/tty/tty0/active")
    pid = vm.out("nohup /usr/local/bin/gordian_knot --vt > /tmp/vt.log 2>&1 & echo $!")
    if not wait_for(lambda: vm.out("cat /sys/class/tty/tty0/active") != before, 10):
        return f"never left {before}: {vm.out('cat /tmp/vt.log')}"
    time.sleep(1)
    vm.run(f"sudo -n gk-probe uinput-type {PASSWORD} && sudo -n gk-probe uinput-key enter")
    if not wait_for(lambda: vm.run(f"kill -0 {pid}").returncode != 0, 20):
        return f"the VT lock did not accept the password: {vm.out('cat /tmp/vt.log')}"
    if not wait_for(lambda: vm.out("cat /sys/class/tty/tty0/active") == before, 10):
        return f"{before} was not given back ({vm.out('cat /sys/class/tty/tty0/active')})"
    if vm.run("pgrep -x demiurge").returncode != 0:
        return "the X session did not survive the VT lock"
    return None


def test_lock_before_suspend(vm: Vm) -> str | None:
    vm.run("sed -i 's/^lock_seconds = .*/lock_seconds = 3600/' ~/.config/demiurge/config.toml")
    vm.run(f"{PROBE_ENV} nohup gordian_knot --daemon > /tmp/daemon.log 2>&1 &")
    if not wait_for(lambda: "gordian_knot" in vm.out("systemd-inhibit --list --no-pager"), 15):
        return f"no sleep inhibitor: {vm.out('cat /tmp/daemon.log')}"
    vm.run("sudo -n systemctl --no-block suspend")
    if not wait_for(lambda: vm.qmp.call("query-status").get("return", {}).get("status") == "suspended", 40):
        return (f"the guest never reached S3 (status "
                f"{vm.qmp.call('query-status').get('return', {}).get('status')})")
    log("INFO", "guest suspended (S3); waking it over QMP")
    vm.qmp.call("system_wakeup")
    if not wait_for(lambda: vm.run("true").returncode == 0, 60, 2):
        return "the guest did not come back from S3"
    if not wait_for(vm.locked, 10):
        return f"not locked on resume: {vm.out('cat /tmp/daemon.log')}"
    if not vm.unlock():
        return "the right password did not unlock after resume"
    return None


def main() -> int:
    if not BASE_DISK.exists() or not KEY.exists():
        log("ERROR", "no installed disk; run [0] RANDOM/ARCH/arch-vm.py test --demiurge first")
        return 2
    vm = Vm()
    try:
        setup(vm)
        log("INFO", "real session up: autologin -> startx -> Xorg on vt1 -> demiurge")
        tests = [test_wm_starts_the_daemon, test_idle_lock_and_unlock, test_keybind_lock,
                 test_vt_lock_over_xorg, test_lock_before_suspend]
        failed = 0
        for t in tests:
            log("INFO", f"RUN   {t.__name__}")
            err = t(vm)
            if err:
                failed += 1
                log("ERROR", f"FAIL  {t.__name__}: {err}")
            else:
                log("INFO", f"PASS  {t.__name__}")
        log("INFO", f"{len(tests) - failed}/{len(tests)} passed")
        return 1 if failed else 0
    finally:
        vm.close()


if __name__ == "__main__":
    sys.exit(main())
