// ABRAXAS's daemon under a compositor's gamma control: A stand-in
// compositor speaking wlr-gamma-control-unstable-v1 on the wire, whose
// monitors come and go, with the real daemon connected to it. Each ramp the
// daemon sets is written after its seccomp filter is armed.

use std::collections::{HashMap, VecDeque};
use std::fs::{self, File};
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use abraxas::config::{self, OverrideState, Paths, WeatherData};

const GAMMA_SIZE: u32 = 256;
// The gamma manager's global name; monitors take others
const MANAGER: u32 = 1;

enum Object {
    Display,
    Registry,
    Manager,
    Output(u32),
    Control(u32),
}

enum Order {
    Plug(u32),
    Unplug(u32),
}

// A ramp the daemon set: The monitor's global name and its R, G then B
// entries.
struct Ramp {
    monitor: u32,
    entries: Vec<u16>,
}

struct Args<'a>(&'a [u8]);

impl Args<'_> {
    fn uint(&mut self) -> u32 {
        let v = u32::from_ne_bytes(self.0[..4].try_into().unwrap());
        self.0 = &self.0[4..];
        v
    }

    fn string(&mut self) -> String {
        let len = self.uint() as usize;
        let s = String::from_utf8_lossy(&self.0[..len.saturating_sub(1)]).into_owned();
        self.0 = &self.0[(len + 3) & !3..];
        s
    }
}

#[derive(Default)]
struct Wire(Vec<u8>);

impl Wire {
    fn uint(mut self, v: u32) -> Self {
        self.0.extend(v.to_ne_bytes());
        self
    }

    fn string(mut self, s: &str) -> Self {
        self = self.uint(s.len() as u32 + 1);
        self.0.extend(s.as_bytes());
        self.0.push(0);
        while self.0.len() % 4 != 0 {
            self.0.push(0);
        }
        self
    }
}

fn send(client: &mut UnixStream, object: u32, opcode: u32, args: Wire) {
    let size = 8 + args.0.len() as u32;
    let mut msg = Vec::with_capacity(size as usize);
    msg.extend(object.to_ne_bytes());
    msg.extend((size << 16 | opcode).to_ne_bytes());
    msg.extend(args.0);
    let _ = client.write_all(&msg);
}

// One recvmsg: Bytes onto `buf`, passed fds onto `fds`. False once the
// client is gone.
fn receive(client: &UnixStream, buf: &mut Vec<u8>, fds: &mut VecDeque<OwnedFd>) -> bool {
    let mut data = [0u8; 4096];
    let mut control = [0u64; 32];
    let mut iov = libc::iovec { iov_base: data.as_mut_ptr().cast(), iov_len: data.len() };
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = std::mem::size_of_val(&control);
    let n = unsafe { libc::recvmsg(client.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC) };
    if n <= 0 {
        return false;
    }
    buf.extend_from_slice(&data[..n as usize]);
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                let first = libc::CMSG_DATA(cmsg) as *const RawFd;
                let count = ((*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize) / std::mem::size_of::<RawFd>();
                for i in 0..count {
                    fds.push_back(OwnedFd::from_raw_fd(first.add(i).read_unaligned()));
                }
            }
            cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
        }
    }
    true
}

fn read_ramp(fd: &OwnedFd) -> Vec<u16> {
    let mut bytes = vec![0u8; GAMMA_SIZE as usize * 6];
    let n = unsafe { libc::pread(fd.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len(), 0) };
    bytes.truncate(n.max(0) as usize);
    bytes.chunks_exact(2).map(|b| u16::from_ne_bytes([b[0], b[1]])).collect()
}

// The stand-in compositor: One client, a gamma manager, and the monitors
// the test plugs in. A monitor unplugged fails the control holding it and
// leaves the registry, as demiurge-wl does.
fn compositor(listener: UnixListener, orders: mpsc::Receiver<Order>, ramps: mpsc::Sender<Ramp>) {
    let Ok((mut client, _)) = listener.accept() else {
        return;
    };
    let mut objects = HashMap::from([(1, Object::Display)]);
    let mut registry = None;
    let mut monitors: Vec<u32> = Vec::new();
    let mut held: HashMap<u32, u32> = HashMap::new();
    let mut buf = Vec::new();
    let mut fds = VecDeque::new();
    loop {
        while let Ok(order) = orders.try_recv() {
            match order {
                Order::Plug(monitor) => {
                    monitors.push(monitor);
                    if let Some(reg) = registry {
                        send(&mut client, reg, 0, Wire::default().uint(monitor).string("wl_output").uint(4));
                    }
                }
                Order::Unplug(monitor) => {
                    monitors.retain(|&m| m != monitor);
                    held.retain(|&control, &mut m| {
                        if m == monitor {
                            send(&mut client, control, 1, Wire::default());
                        }
                        m != monitor
                    });
                    if let Some(reg) = registry {
                        send(&mut client, reg, 1, Wire::default().uint(monitor));
                    }
                }
            }
        }

        let mut pfd = libc::pollfd { fd: client.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        if unsafe { libc::poll(&mut pfd, 1, 20) } <= 0 {
            continue;
        }
        if !receive(&client, &mut buf, &mut fds) {
            return;
        }

        while buf.len() >= 8 {
            let sender = u32::from_ne_bytes(buf[0..4].try_into().unwrap());
            let word = u32::from_ne_bytes(buf[4..8].try_into().unwrap());
            let size = (word >> 16) as usize;
            if buf.len() < size {
                break;
            }
            let mut args = Args(&buf[8..size]);
            match (objects.get(&sender), word & 0xffff) {
                // wl_display.sync: wl_callback.done, then delete_id
                (Some(Object::Display), 0) => {
                    let callback = args.uint();
                    send(&mut client, callback, 0, Wire::default().uint(0));
                    send(&mut client, 1, 1, Wire::default().uint(callback));
                }
                // wl_display.get_registry
                (Some(Object::Display), 1) => {
                    let id = args.uint();
                    objects.insert(id, Object::Registry);
                    registry = Some(id);
                    send(&mut client, id, 0, Wire::default().uint(MANAGER).string("zwlr_gamma_control_manager_v1").uint(1));
                    for &monitor in &monitors {
                        send(&mut client, id, 0, Wire::default().uint(monitor).string("wl_output").uint(4));
                    }
                }
                // wl_registry.bind
                (Some(Object::Registry), 0) => {
                    let name = args.uint();
                    let _interface = args.string();
                    let _version = args.uint();
                    let id = args.uint();
                    objects.insert(id, if name == MANAGER { Object::Manager } else { Object::Output(name) });
                }
                // get_gamma_control: Its size, or failed for a monitor gone
                // or held
                (Some(Object::Manager), 0) => {
                    let id = args.uint();
                    let monitor = match objects.get(&args.uint()) {
                        Some(Object::Output(m)) => *m,
                        _ => 0,
                    };
                    objects.insert(id, Object::Control(monitor));
                    if monitors.contains(&monitor) && !held.values().any(|&m| m == monitor) {
                        held.insert(id, monitor);
                        send(&mut client, id, 0, Wire::default().uint(GAMMA_SIZE));
                    } else {
                        send(&mut client, id, 1, Wire::default());
                    }
                }
                (Some(&Object::Control(monitor)), 0) => {
                    let fd = fds.pop_front().expect("set_gamma carries an fd");
                    if held.contains_key(&sender) {
                        let _ = ramps.send(Ramp { monitor, entries: read_ramp(&fd) });
                    }
                }
                // destroy, on a control or the manager
                (Some(Object::Control(_) | Object::Manager), 1) => {
                    held.remove(&sender);
                    objects.remove(&sender);
                    send(&mut client, 1, 1, Wire::default().uint(sender));
                }
                _ => {}
            }
            buf.drain(..size);
        }
    }
}

struct Session {
    dir: PathBuf,
    orders: mpsc::Sender<Order>,
    ramps: mpsc::Receiver<Ramp>,
    daemon: Child,
}

impl Session {
    // The daemon held at 3000K, with a fresh weather cache so it fetches
    // nothing, against a compositor with `monitors` plugged in.
    fn start(name: &str, monitors: &[u32]) -> Self {
        let dir = std::env::temp_dir().join(format!("abraxas-wayland-{}-{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&dir);
        let config_dir = dir.join(".config").join("abraxas");
        fs::create_dir_all(&config_dir).unwrap();
        let paths = Paths::in_dir(&config_dir);
        let now = abraxas::now_epoch();
        config::save_location(&paths, 41.8781, -87.6298).unwrap();
        config::save_weather_cache(&paths, &WeatherData {
            cloud_cover: 0,
            forecast: "Clear".to_string(),
            temperature: 20.0,
            is_day: true,
            fetched_at: now,
            has_error: false,
        })
        .unwrap();
        config::save_override(&paths, &OverrideState {
            active: true,
            target_temp: 3000,
            duration_minutes: 0,
            issued_at: now,
            start_temp: 3000,
        })
        .unwrap();

        let listener = UnixListener::bind(dir.join("wayland-test")).unwrap();
        let (orders, order_rx) = mpsc::channel();
        let (ramp_tx, ramps) = mpsc::channel();
        for &monitor in monitors {
            orders.send(Order::Plug(monitor)).unwrap();
        }
        std::thread::spawn(move || compositor(listener, order_rx, ramp_tx));

        let daemon = Command::new(env!("CARGO_BIN_EXE_abraxas-wl"))
            .arg("--daemon")
            .env("HOME", &dir)
            .env("XDG_RUNTIME_DIR", &dir)
            .env("WAYLAND_DISPLAY", "wayland-test")
            .env_remove("WAYLAND_SOCKET")
            .env_remove("DISPLAY")
            .stdout(Stdio::null())
            .stderr(File::create(dir.join("daemon.log")).unwrap())
            .spawn()
            .unwrap();
        Session { dir, orders, ramps, daemon }
    }

    fn log(&self) -> String {
        fs::read_to_string(self.dir.join("daemon.log")).unwrap_or_default()
    }

    fn running(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.log().contains("daemon started") {
            if let Some(status) = self.daemon.try_wait().unwrap() {
                panic!("daemon ended ({status:?}) before its event loop:\n{}", self.log());
            }
            assert!(Instant::now() < deadline, "no event loop in 10s:\n{}", self.log());
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn ramp(&mut self) -> Ramp {
        match self.ramps.recv_timeout(Duration::from_secs(5)) {
            Ok(ramp) => ramp,
            Err(_) => panic!("no ramp in 5s (daemon {:?}):\n{}", self.daemon.try_wait().unwrap(), self.log()),
        }
    }

    // SIGTERM, and the daemon's own clean exit
    fn stop(&mut self) {
        unsafe { libc::kill(self.daemon.id() as i32, libc::SIGTERM) };
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = self.daemon.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "no exit 10s after SIGTERM:\n{}", self.log());
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(status.success(), "exit {status:?} (signal {:?}):\n{}", status.signal(), self.log());
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

// 3000K: The red ramp tops out well above the blue.
fn warm(ramp: &Ramp) -> bool {
    let n = GAMMA_SIZE as usize;
    ramp.entries.len() == 3 * n && ramp.entries[n - 1] > ramp.entries[3 * n - 1]
}

#[test]
fn a_lit_monitor_is_warmed_under_the_daemons_filter() {
    let mut s = Session::start("lit", &[10]);
    let ramp = s.ramp();
    assert_eq!(ramp.monitor, 10);
    assert!(warm(&ramp), "not a 3000K ramp: {:?}", &ramp.entries[..8]);
    s.running();
    s.stop();
}

#[test]
fn the_daemon_waits_for_a_monitor_and_warms_each_one_that_comes() {
    let mut s = Session::start("hotplug", &[]);
    s.running();
    // Waiting, it logs its temperature once, not on every wake
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(s.log().matches("Manual:").count(), 1, "{}", s.log());

    s.orders.send(Order::Plug(10)).unwrap();
    let ramp = s.ramp();
    assert_eq!(ramp.monitor, 10);
    assert!(warm(&ramp));

    s.orders.send(Order::Unplug(10)).unwrap();
    s.orders.send(Order::Plug(11)).unwrap();
    let ramp = s.ramp();
    assert_eq!(ramp.monitor, 11);
    assert!(warm(&ramp));

    s.stop();
}
