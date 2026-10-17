#!/usr/bin/env python3
"""
GORDIAN KNOT suite. Runs INSIDE the QEMU guest only, as root; guest.py
pushes it and starts it. It refuses to run anywhere else: these tests take
input grabs, VTs, PAM attempts and faillock tallies, which on a host can
strand the session.

Each test starts its own Xvfb (cookie in the test user's home, as on a
real login) and drives the installed setuid /usr/local/bin/gordian_knot
through gk-probe: XTest for X11 input, /dev/uinput for VT input.
"""

import argparse
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

LOCKER = "/usr/local/bin/gordian_knot"
PROBE = "/usr/local/bin/gk-probe"
LOG_DIR = Path("/tmp/gk-suite")

USER = ""
PASSWORD = ""
HOME = Path()


def log(level: str, msg: str) -> None:
    print(f"[{time.strftime('%H:%M:%S')}] {'[' + level + ']':<8} {msg}", flush=True)


class Fail(Exception):
    pass


def check(cond: bool, msg: str) -> None:
    if not cond:
        raise Fail(msg)


def wait_for(pred, timeout: float, step: float = 0.2) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if pred():
            return True
        time.sleep(step)
    return pred()


def as_user(argv: list[str], env: dict[str, str], **kw) -> subprocess.Popen:
    # setpriv execs in place (runuser forks and lingers), so the Popen pid
    # is the process itself and its /proc children are its own.
    return subprocess.Popen(["setpriv", f"--reuid={USER}", f"--regid={USER}", "--init-groups",
                             "--", *argv], env=env, **kw)


def user_env(display: str | None = None) -> dict[str, str]:
    env = {"HOME": str(HOME), "USER": USER, "LOGNAME": USER, "PATH": "/usr/local/bin:/usr/bin",
           "XDG_RUNTIME_DIR": f"/run/user/{uid()}"}
    if display:
        env |= {"DISPLAY": display, "XAUTHORITY": str(HOME / ".Xauthority")}
    return env


def uid() -> int:
    return int(subprocess.run(["id", "-u", USER], capture_output=True, text=True).stdout)


class Xvfb:
    """A display with a real cookie in the user's ~/.Xauthority."""

    def __init__(self, num: int, size: str = "1280x720"):
        self.display = f":{num}"
        cookie = os.urandom(16).hex()
        auth = HOME / ".Xauthority"
        subprocess.run(["runuser", "-u", USER, "--", "xauth", "-f", str(auth), "add",
                        self.display, ".", cookie], check=True, capture_output=True)
        self.proc = subprocess.Popen(["Xvfb", self.display, "-screen", "0", f"{size}x24",
                                      "-auth", str(auth), "-nolisten", "tcp"],
                                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        sock = Path(f"/tmp/.X11-unix/X{num}")
        if not wait_for(sock.exists, 10):
            raise Fail(f"Xvfb {self.display} never came up")
        time.sleep(0.3)
        self.env = user_env(self.display)

    def probe(self, *args: str) -> str:
        r = as_user([PROBE, *args], self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        out, err = r.communicate(timeout=30)
        if r.returncode != 0:
            raise Fail(f"gk-probe {' '.join(args)}: {err.strip()}")
        return out.strip()

    def close(self) -> None:
        self.proc.terminate()
        self.proc.wait(timeout=10)


class Locker:
    def __init__(self, name: str, env: dict[str, str], *args: str):
        LOG_DIR.mkdir(exist_ok=True)
        self.log_path = LOG_DIR / f"{name}.log"
        self.log = self.log_path.open("w")
        self.proc = as_user([LOCKER, *args], env, stdout=self.log, stderr=subprocess.STDOUT)

    def alive(self) -> bool:
        return self.proc.poll() is None

    def output(self) -> str:
        self.log.flush()
        return self.log_path.read_text(errors="replace")

    def children(self) -> list[int]:
        try:
            kids = Path(f"/proc/{self.proc.pid}/task/{self.proc.pid}/children").read_text().split()
        except FileNotFoundError:
            return []
        return [int(k) for k in kids]

    def close(self) -> None:
        if self.alive():
            for pid in self.children():
                kill_tree(pid)
            os.kill(self.proc.pid, signal.SIGKILL)
            self.proc.wait(timeout=10)
        self.log.close()


def kill_tree(pid: int) -> None:
    try:
        kids = Path(f"/proc/{pid}/task/{pid}/children").read_text().split()
    except FileNotFoundError:
        return
    for k in kids:
        kill_tree(int(k))
    try:
        os.kill(pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def locked(x: Xvfb) -> bool:
    return x.probe("grab-state") == "held" and int(x.probe("painted")) > 2


def unlock(x: Xvfb, locker: Locker, password: str | None = None) -> None:
    x.probe("type", password or PASSWORD)
    x.probe("key", "Return")
    check(wait_for(lambda: not locker.alive(), 15), "locker still running after the right password")
    check(locker.proc.returncode == 0, f"locker exited {locker.proc.returncode}")
    check(x.probe("grab-state") == "free", "grab still held after unlock")


def faillock_reset() -> None:
    subprocess.run(["faillock", "--user", USER, "--reset"], check=False)


# TESTS


def proc_status(pid: int) -> dict[str, str]:
    out = {}
    for line in Path(f"/proc/{pid}/status").read_text().splitlines():
        k, _, v = line.partition(":")
        out[k] = v.strip()
    return out


def test_x11_lock_holds_grab_and_paints():
    x = Xvfb(11)
    lk = Locker("x11_basic", x.env)
    try:
        check(wait_for(lambda: locked(x), 8), "lock never held the grab with panels painted:\n" + lk.output())
        holder = proc_status(lk.proc.pid)
        check(holder["Uid"].split()[0] == "0", f"holder real uid is not 0: {holder['Uid']}")
        kids = lk.children()
        check(len(kids) == 1, f"want exactly one UI child, found {kids}")
        child = proc_status(kids[0])
        check(child["Uid"].split()[1] == str(uid()), f"UI child is not the user: {child['Uid']}")
        check(child["NoNewPrivs"] == "1", "UI child lacks NO_NEW_PRIVS")
        check(child["Seccomp"] == "2", f"UI child is not under a seccomp filter: {child['Seccomp']}")
        x.probe("type", "abc")
        x.probe("key", "BackSpace")
        x.probe("key", "Escape")
        time.sleep(2)  # a clock tick or two through the sandboxed renderer
        unlock(x, lk)
        check("seccomp: denied" not in lk.output(), "the UI child hit its seccomp filter:\n" + lk.output())
    finally:
        lk.close()
        x.close()


def test_wrong_then_right_password():
    x = Xvfb(12)
    lk = Locker("x11_wrong_right", x.env)
    try:
        check(wait_for(lambda: locked(x), 8), "never locked:\n" + lk.output())
        x.probe("type", "not-the-password")
        x.probe("key", "Return")
        time.sleep(4)
        check(lk.alive() and x.probe("grab-state") == "held", "a wrong password released the lock")
        unlock(x, lk)
    finally:
        lk.close()
        x.close()
        faillock_reset()


def test_ctrl_u_clears_the_field():
    x = Xvfb(13)
    lk = Locker("x11_ctrl_u", x.env)
    try:
        check(wait_for(lambda: locked(x), 8), "never locked:\n" + lk.output())
        x.probe("type", "garbage")
        x.probe("key", "ctrl+u")
        unlock(x, lk)
    finally:
        lk.close()
        x.close()
        faillock_reset()


def test_numpad_digits_count():
    pw = "knot42pw"
    subprocess.run(["chpasswd"], input=f"{USER}:{pw}", text=True, check=True)
    x = Xvfb(14)
    lk = Locker("x11_numpad", x.env)
    try:
        check(wait_for(lambda: locked(x), 8), "never locked:\n" + lk.output())
        x.probe("key", "Num_Lock")
        x.probe("type", "knot")
        x.probe("key", "KP_4")
        x.probe("key", "KP_2")
        x.probe("type", "pw")
        x.probe("key", "Return")
        check(wait_for(lambda: not lk.alive(), 15), "keypad digits did not reach the password")
    finally:
        lk.close()
        x.close()
        subprocess.run(["chpasswd"], input=f"{USER}:{PASSWORD}", text=True, check=True)
        faillock_reset()


def test_oversized_root_paints():
    x = Xvfb(15, "5120x2880")
    lk = Locker("x11_oversized", x.env)
    try:
        check(wait_for(lambda: locked(x), 10), "a 5120x2880 root never painted the lock:\n" + lk.output())
        unlock(x, lk)
    finally:
        lk.close()
        x.close()


def test_killed_ui_keeps_the_lock():
    x = Xvfb(16)
    lk = Locker("x11_killed_ui", x.env)
    try:
        check(wait_for(lambda: locked(x), 8), "never locked:\n" + lk.output())
        kids = lk.children()
        check(bool(kids), "no UI child to kill: one process holds the grab, so a crash unlocks")
        for k in kids:
            kill_tree(k)
        time.sleep(1)
        check(lk.alive() and x.probe("grab-state") == "held", "killing the UI released the lock")
        check(wait_for(lambda: locked(x), 8), "the UI never came back after being killed")
        unlock(x, lk)
    finally:
        lk.close()
        x.close()


def test_faillock_notice_reaches_the_user():
    x = Xvfb(17)
    lk = Locker("x11_faillock", x.env)
    try:
        check(wait_for(lambda: locked(x), 8), "never locked:\n" + lk.output())
        for _ in range(3):
            x.probe("type", "wrong-wrong")
            x.probe("key", "Return")
            time.sleep(4)
        x.probe("type", PASSWORD)
        x.probe("key", "Return")
        time.sleep(4)
        check(lk.alive(), "faillock did not engage after 3 failures (test premise)")
        check("locked" in lk.output().lower(),
              "pam_faillock's notice never surfaced; the user sees only 'Authentication failed':\n"
              + lk.output())
        faillock_reset()
        unlock(x, lk)
    finally:
        lk.close()
        x.close()
        faillock_reset()


def active_vt() -> str:
    return Path("/sys/class/tty/tty0/active").read_text().strip()


def test_vt_lock_accepts_the_password():
    before = active_vt()
    lk = Locker("vt", user_env(), "--vt")
    try:
        check(wait_for(lambda: active_vt() != before, 8), f"never switched VT (still {before}):\n" + lk.output())
        time.sleep(1)
        subprocess.run([PROBE, "uinput-type", PASSWORD], check=True)
        subprocess.run([PROBE, "uinput-key", "enter"], check=True)
        check(wait_for(lambda: not lk.alive(), 15), "the VT lock rejected the right password:\n" + lk.output())
        check(lk.proc.returncode == 0, f"VT locker exited {lk.proc.returncode}:\n" + lk.output())
        check(wait_for(lambda: active_vt() == before, 5), f"VT not restored ({active_vt()} != {before})")
    finally:
        lk.close()
        faillock_reset()


def test_daemon_spawns_one_locker():
    cfg = HOME / ".config" / "demiurge"
    subprocess.run(["runuser", "-u", USER, "--", "mkdir", "-p", str(cfg)], check=True)
    (cfg / "config.toml").write_text("[input.idle]\nlock_seconds = 2\n")
    os.chown(cfg / "config.toml", uid(), uid())
    x = Xvfb(18)
    daemon = Locker("daemon", x.env, "--daemon")
    try:
        inhibitors = lambda: subprocess.run(["systemd-inhibit", "--list", "--no-pager"],
                                            capture_output=True, text=True).stdout
        check(wait_for(lambda: "gordian_knot" in inhibitors(), 10),
              "no lock-before-suspend inhibitor registered:\n" + inhibitors() + daemon.output())
        check(wait_for(lambda: locked(x), 20), "the daemon never locked an idle session:\n" + daemon.output())
        time.sleep(15)
        lockers = [p for p in daemon.children()]
        check(len(lockers) == 1, f"{len(lockers)} lockers running after 15 s idle and locked; want 1")
        pid = lockers[0]
        x.probe("type", PASSWORD)
        x.probe("key", "Return")
        gone = lambda: not Path(f"/proc/{pid}").exists() or proc_status(pid).get("State", "").startswith("Z")
        check(wait_for(gone, 15), "the daemon's locker did not exit on the right password")
        check(x.probe("grab-state") == "free", "grab still held after the daemon's locker exited")
        check(daemon.alive(), "the daemon itself exited")
    finally:
        daemon.close()
        x.close()
        (cfg / "config.toml").unlink(missing_ok=True)


def test_report_xi2_raw_events_during_lock():
    """REPORT: Whether another X client sees the password as XInput2 raw
    events while the lock holds the grab. Not a pass/fail gate."""
    x = Xvfb(19)
    lk = Locker("x11_xi2", x.env)
    try:
        check(wait_for(lambda: locked(x), 8), "never locked:\n" + lk.output())
        spy = as_user([PROBE, "xi2-raw", "5"], x.env, stdout=subprocess.PIPE, text=True)
        time.sleep(1)
        x.probe("type", "abcdef")
        seen = spy.communicate(timeout=15)[0].strip()
        log("INFO", f"REPORT xi2-raw: another client saw {seen} raw key presses (6 typed) during the lock")
    finally:
        lk.close()
        x.close()


def in_guest() -> bool:
    return Path("/etc/hostname").read_text().strip() == "knot" and Path("/etc/cloud/cloud-init.disabled").exists()


def main() -> None:
    global USER, PASSWORD, HOME
    p = argparse.ArgumentParser()
    p.add_argument("--user", required=True)
    p.add_argument("--password", required=True)
    p.add_argument("-k", help="run only tests whose name contains this")
    a = p.parse_args()
    if os.geteuid() != 0 or not in_guest():
        log("ERROR", "refusing to run outside the QEMU test guest (and as root)")
        sys.exit(2)
    USER, PASSWORD, HOME = a.user, a.password, Path(f"/home/{a.user}")
    subprocess.run(["loginctl", "enable-linger", USER], check=False)

    tests = [(n, f) for n, f in globals().items() if n.startswith("test_") and callable(f)]
    if a.k:
        tests = [(n, f) for n, f in tests if a.k in n]
    failed = []
    for name, fn in tests:
        log("INFO", f"RUN   {name}")
        try:
            fn()
            log("INFO", f"PASS  {name}")
        except Fail as e:
            failed.append(name)
            log("ERROR", f"FAIL  {name}: {e}")
        except Exception as e:
            failed.append(name)
            log("ERROR", f"FAIL  {name}: {type(e).__name__}: {e}")
    log("INFO", f"{len(tests) - len(failed)}/{len(tests)} passed")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
