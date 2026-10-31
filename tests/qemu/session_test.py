#!/usr/bin/env python3
"""
GORDIAN KNOT in a real DEMIURGE session, in QEMU.

The disk is the one `[0] RANDOM/ARCH/arch-vm.py test --demiurge` leaves:
Arch installed by arch-install.py, DEMIURGE installed by post-install.py
through its own install.py. This boots a throwaway overlay of it (the
installed image is never written), turns on a test-only autologin on
tty1, and lets the user's own ~/.bash_profile start the session the way
it starts on archd: startx, real Xorg on vt1, demiurge.

Then, against the real thing, with this tree's demiurge, gordian_knot and
ABRAXAS installed and ABRAXAS's unit wanted by graphical-session.target:
  - startx's session begins graphical-session.target, and ABRAXAS runs on
    the session's display on its x11 backend, inside its seccomp filter
  - demiurge starts gordian_knot --daemon from [input.idle] lock_seconds
  - the daemon locks the idle session on a VT; the password unlocks it
  - a `lock` keybind locks; the password unlocks it
  - the lock takes a fresh console away from Xorg: An XInput2 spy on Xorg
    counts the keys typed before the lock and none of the password typed
    on a real (uinput) keyboard during it, and vt1 goes back to X
  - a killed lock holder: The warden gives vt1 back and locks X again
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

    # The in-X lock: The grab held, the panels painted.
    def locked(self) -> bool:
        if self.probe("grab-state") != "held":
            return False
        painted = self.probe("painted")
        return painted.isdigit() and int(painted) > 2

    def active_vt(self) -> str:
        return self.out("cat /sys/class/tty/tty0/active")

    # The VT lock: A console other than X's, a lock holder running.
    def vt_locked(self) -> bool:
        return self.active_vt() not in ("", "tty1") and self.holder_running()

    def vt_unlock(self) -> bool:
        self.run(f"sudo -n gk-probe uinput-type {PASSWORD} && sudo -n gk-probe uinput-key enter")
        return wait_for(lambda: self.active_vt() == "tty1" and not self.holder_running(), 20)

    def holder_running(self) -> bool:
        return self.run(f"pgrep -x -f {X11_BIN}/gordian_knot >/dev/null || "
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


# The X11 stack's binaries, where its installer puts them.
X11_BIN = "/usr/local/lib/demiurge/x11/bin"


def setup(vm: Vm) -> None:
    env = dict(os.environ, CARGO_TARGET_DIR=str(BUILD_DIR))
    subprocess.run(["cargo", "build", "--release", "-p", "gordian_knot-x11", "--bin", "gordian_knot",
                    "--example", "gk_probe", "-p", "demiurge-x11", "--bin", "demiurge"], cwd=REPO, env=env, check=True)
    subprocess.run(["cargo", "build", "--release", "-p", "abraxas-x11"], cwd=REPO, env=env, check=True)
    vm.push(BUILD_DIR / "release" / "examples" / "gk_probe", "/tmp/gk-probe")
    vm.push(BUILD_DIR / "release" / "gordian_knot", "/tmp/gordian_knot")
    vm.push(BUILD_DIR / "release" / "demiurge", "/tmp/demiurge")
    vm.push(BUILD_DIR / "release" / "abraxas", "/tmp/abraxas")
    vm.push(REPO / "contrib" / "systemd" / "abraxas@.service", "/tmp/abraxas@.service")
    vm.push(REPO / "contrib" / "systemd" / "demiurge-x11.target", "/tmp/demiurge-x11.target")
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
        f"sudo -n install -D -o root -g root -m 4755 /tmp/gordian_knot {X11_BIN}/gordian_knot",
        f"sudo -n install -D -m 0755 /tmp/demiurge {X11_BIN}/demiurge",
        f"sudo -n install -D -m 0755 /tmp/abraxas {X11_BIN}/abraxas",
        f"sudo -n ln -sfn {X11_BIN}/demiurge /usr/local/bin/demiurge",
        "sudo -n rm -f /usr/local/bin/gordian_knot ~/.local/bin/abraxas ~/.config/systemd/user/abraxas.service"
        " ~/.config/systemd/user/demiurge-session.target",
        "mkdir -p ~/.config/systemd/user ~/.config/abraxas && mv /tmp/abraxas@.service"
        " /tmp/demiurge-x11.target ~/.config/systemd/user/",
        "printf '[location]\\nlatitude = 51.48\\nlongitude = 0.0\\n' > ~/.config/abraxas/config.toml",
        "systemctl --user daemon-reload && systemctl --user enable abraxas@x11.service",
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


# The X11 session begins the user manager's graphical session, so ABRAXAS,
# wanted by graphical-session.target, runs on its display. A window manager
# that never began it left ABRAXAS unstarted under startx.
def test_the_x11_session_starts_abraxas(vm: Vm) -> str | None:
    def journal() -> str:
        return vm.out("journalctl --user -b -u abraxas@x11.service --no-pager -o cat")

    if not wait_for(lambda: vm.run("systemctl --user is-active --quiet graphical-session.target").returncode == 0, 20):
        return ("graphical-session.target never started under startx:\n"
                + vm.out("systemctl --user status demiurge-x11.target --no-pager"))
    if not wait_for(lambda: "daemon started (backend: x11" in journal(), 20):
        return f"ABRAXAS never started on its x11 backend:\n{journal()}"
    if "seccomp: syscall whitelist active" not in journal():
        return f"ABRAXAS ran outside its seccomp filter:\n{journal()}"
    restarts = vm.out("systemctl --user show -p NRestarts --value abraxas@x11.service")
    if restarts != "0":
        return f"ABRAXAS restarted {restarts} times:\n{journal()}"
    return None


def test_wm_starts_the_daemon(vm: Vm) -> str | None:
    if not wait_for(lambda: vm.run("pgrep -f 'gordian_knot --daemon'").returncode == 0, 20):
        return "demiurge did not start gordian_knot --daemon with lock_seconds set"
    return None


def test_idle_lock_and_unlock(vm: Vm) -> str | None:
    if not wait_for(vm.vt_locked, LOCK_SECONDS + 30, 1):
        return f"the idle session was not locked within {LOCK_SECONDS + 30}s"
    if not vm.vt_unlock():
        return "the right password did not unlock the idle lock"
    # The rest of the run drives input on its own schedule.
    vm.run("pkill -f 'gordian_knot --daemon'")
    return None


def test_keybind_lock(vm: Vm) -> str | None:
    vm.probe("key super+Escape")
    if not wait_for(vm.vt_locked, 10):
        return "Super+Escape (action = \"lock\") did not lock"
    if not vm.vt_unlock():
        return "the right password did not unlock the keybind lock"
    return None


# How many raw key presses an XInput2 spy on Xorg sees while uinput types
# `text` (and Enter).
def spied_keys(vm: Vm, text: str) -> int:
    vm.run(f"{PROBE_ENV} nohup gk-probe xi2-raw 6 > /tmp/spy.out 2>&1 &")
    time.sleep(1.5)
    vm.run(f"sudo -n gk-probe uinput-type {text} && sudo -n gk-probe uinput-key enter")
    wait_for(lambda: vm.run("pgrep -f 'gk-probe xi2-raw' >/dev/null").returncode != 0, 15)
    seen = vm.out("cat /tmp/spy.out")
    return int(seen) if seen.isdigit() else -1


def test_vt_lock_over_xorg(vm: Vm) -> str | None:
    # The spy works: With X on the active VT it counts what is typed.
    control = spied_keys(vm, "abcdef")
    if control < 6:
        return f"the XInput2 spy saw {control} of 7 keys with X active; the measurement means nothing"
    pid = vm.out(f"{PROBE_ENV} nohup {X11_BIN}/gordian_knot > /tmp/vt.log 2>&1 & echo $!")
    if not wait_for(lambda: vm.active_vt() != "tty1", 10):
        return f"never left tty1: {vm.out('cat /tmp/vt.log')}"
    time.sleep(1)
    seen = spied_keys(vm, PASSWORD)
    log("INFO", f"MEASURED xi2-raw: {control} keys seen with X active, {seen} of the password during the lock")
    if not wait_for(lambda: vm.run(f"kill -0 {pid}").returncode != 0, 20):
        return f"the lock did not accept the password: {vm.out('cat /tmp/vt.log')}"
    if seen != 0:
        return f"an X client saw {seen} raw key presses of the password typed into the lock"
    if not wait_for(lambda: vm.active_vt() == "tty1", 10):
        return f"tty1 was not given back ({vm.active_vt()})"
    if vm.run("pgrep -x demiurge").returncode != 0:
        return "the X session did not survive the lock"
    return None


def test_a_killed_holder_gives_vt1_back_locked(vm: Vm) -> str | None:
    pid = vm.out(f"{PROBE_ENV} nohup {X11_BIN}/gordian_knot > /tmp/killed.log 2>&1 & echo $!")
    if not wait_for(lambda: vm.active_vt() != "tty1", 10):
        return f"never left tty1: {vm.out('cat /tmp/killed.log')}"
    time.sleep(1)
    vm.run(f"sudo -n kill -KILL {pid}")
    if not wait_for(lambda: vm.active_vt() == "tty1", 10):
        return f"the console stayed on {vm.active_vt()}: {vm.out('cat /tmp/killed.log')}"
    if not wait_for(vm.locked, 10):
        return f"X was not locked again: {vm.out('cat /tmp/killed.log')}"
    if not vm.unlock():
        return "the right password did not unlock the relocked session"
    return None


def test_lock_before_suspend(vm: Vm) -> str | None:
    vm.run("sed -i 's/^lock_seconds = .*/lock_seconds = 3600/' ~/.config/demiurge/config.toml")
    vm.run(f"{PROBE_ENV} nohup {X11_BIN}/gordian_knot --daemon > /tmp/daemon.log 2>&1 &")
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
    if not wait_for(vm.vt_locked, 10):
        return f"not locked on resume: {vm.out('cat /tmp/daemon.log')}"
    if not vm.vt_unlock():
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
        tests = [test_the_x11_session_starts_abraxas, test_wm_starts_the_daemon, test_idle_lock_and_unlock, test_keybind_lock,
                 test_vt_lock_over_xorg, test_a_killed_holder_gives_vt1_back_locked, test_lock_before_suspend]
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
