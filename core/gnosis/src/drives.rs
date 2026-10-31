// Local drives for GNOSIS's sidebar, from udisks2 on the system bus: The
// external and removable filesystems (udisks' HintSystem off) and any
// mounted under /run/media, /media or /mnt, named the way udisks hints.
// A watcher thread follows udisks' signals and an action thread mounts and
// unmounts, so a slow device never stalls drawing; both wake GNOSIS through
// one eventfd.

use std::collections::HashMap;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use zbus::blocking::Connection;
use zbus::fdo::ManagedObjects;
use zbus::zvariant::{OwnedValue, Value};

const UDISKS: &str = "org.freedesktop.UDisks2";
const BLOCK: &str = "org.freedesktop.UDisks2.Block";
const FILESYSTEM: &str = "org.freedesktop.UDisks2.Filesystem";
// A system filesystem mounted under one of these is the user's to open.
const USER_MOUNTS: &[&str] = &["/run/media/", "/media/", "/mnt/"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drive {
    // The udisks object, which mount and unmount address.
    pub object: String,
    pub label: String,
    pub size: u64,
    pub mount: Option<PathBuf>,
}

#[derive(Debug)]
pub enum Event {
    Drives(Vec<Drive>),
    Mounted(String, Result<PathBuf, String>),
    Unmounted(String, Result<(), String>),
}

enum Action {
    Mount(String),
    Unmount(String),
}

// The drives in udisks' object tree, sorted by label.
pub fn parse(objects: &ManagedObjects) -> Vec<Drive> {
    let mut drives: Vec<Drive> = objects
        .iter()
        .filter_map(|(path, interfaces)| {
            let block = interfaces.iter().find(|(name, _)| name.as_str() == BLOCK)?.1;
            let filesystem = interfaces.iter().find(|(name, _)| name.as_str() == FILESYSTEM)?.1;
            if text(block, "IdUsage") != "filesystem" || flag(block, "HintIgnore") {
                return None;
            }
            let mount = mount_points(filesystem).into_iter().next();
            let user_mounted = mount
                .as_ref()
                .is_some_and(|m| USER_MOUNTS.iter().any(|p| m.as_os_str().as_bytes().starts_with(p.as_bytes())));
            if flag(block, "HintSystem") && !user_mounted {
                return None;
            }
            let device = bytes_path(block.get("Device"));
            let label = [text(block, "HintName"), text(block, "IdLabel")]
                .into_iter()
                .find(|s| !s.is_empty())
                .or_else(|| device.as_ref().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_else(|| "drive".into());
            let size = block.get("Size").and_then(|v| u64::try_from(v).ok()).unwrap_or(0);
            Some(Drive { object: path.to_string(), label, size, mount })
        })
        .collect();
    drives.sort_by(|a, b| a.label.cmp(&b.label).then(a.object.cmp(&b.object)));
    drives
}

fn text(props: &HashMap<String, OwnedValue>, key: &str) -> String {
    props.get(key).and_then(|v| v.downcast_ref::<String>().ok()).unwrap_or_default()
}

fn flag(props: &HashMap<String, OwnedValue>, key: &str) -> bool {
    props.get(key).and_then(|v| v.downcast_ref::<bool>().ok()).unwrap_or(false)
}

// A udisks byte-string path (`ay`, NUL-terminated).
fn bytes_path(value: Option<&OwnedValue>) -> Option<PathBuf> {
    let bytes: Vec<u8> = value?.try_clone().ok()?.try_into().ok()?;
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    (end > 0).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(&bytes[..end])))
}

fn mount_points(props: &HashMap<String, OwnedValue>) -> Vec<PathBuf> {
    let Some(Value::Array(points)) = props.get("MountPoints").map(|v| &**v) else {
        return Vec::new();
    };
    points
        .iter()
        .filter_map(|p| {
            let bytes: Vec<u8> = p.try_clone().ok()?.try_into().ok()?;
            let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
            (end > 0).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(&bytes[..end])))
        })
        .collect()
}

pub fn list(conn: &Connection) -> zbus::Result<Vec<Drive>> {
    let manager = zbus::blocking::fdo::ObjectManagerProxy::builder(conn)
        .destination(UDISKS)?
        .path("/org/freedesktop/UDisks2")?
        .build()?;
    Ok(parse(&manager.get_managed_objects()?))
}

// udisks takes options as a{sv}; GNOSIS has no polkit agent to answer a
// password prompt, so it asks for none.
fn options() -> HashMap<&'static str, Value<'static>> {
    HashMap::from([("auth.no_user_interaction", Value::from(true))])
}

fn filesystem<'a>(conn: &'a Connection, object: &'a str) -> zbus::Result<zbus::blocking::Proxy<'a>> {
    zbus::blocking::Proxy::new(conn, UDISKS, object, FILESYSTEM)
}

fn mount(conn: &Connection, object: &str) -> Result<PathBuf, String> {
    let path: String = filesystem(conn, object)
        .and_then(|p| p.call("Mount", &(options(),)))
        .map_err(|e| e.to_string())?;
    Ok(PathBuf::from(path))
}

fn unmount(conn: &Connection, object: &str) -> Result<(), String> {
    filesystem(conn, object)
        .and_then(|p| p.call::<_, _, ()>("Unmount", &(options(),)))
        .map_err(|e| e.to_string())
}

pub struct Drives {
    actions: mpsc::Sender<Action>,
    events: mpsc::Receiver<Event>,
    wake: OwnedFd,
}

impl Drives {
    // Follow udisks on the system bus. None without a bus; an absent udisks
    // only means an empty list.
    pub fn start() -> Option<Self> {
        Self::with(Connection::system().ok()?)
    }

    pub fn with(conn: Connection) -> Option<Self> {
        let raw = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if raw < 0 {
            return None;
        }
        let wake = unsafe { OwnedFd::from_raw_fd(raw) };
        let (outbox, events) = mpsc::channel();
        let (actions, inbox) = mpsc::channel();

        let notify = |fd: &OwnedFd| {
            let one: u64 = 1;
            unsafe { libc::write(fd.as_raw_fd(), (&one as *const u64).cast(), 8) };
        };

        let (watch_conn, watch_out, watch_wake) = (conn.clone(), outbox.clone(), wake.try_clone().ok()?);
        std::thread::Builder::new()
            .name("gnosis-drives".into())
            .spawn(move || {
                let rule = zbus::MatchRule::builder()
                    .msg_type(zbus::message::Type::Signal)
                    .sender(UDISKS)
                    .map(|b| b.build());
                let signals = rule.ok().and_then(|r| zbus::blocking::MessageIterator::for_match_rule(r, &watch_conn, Some(64)).ok());
                let send = |drives| {
                    let ok = watch_out.send(Event::Drives(drives)).is_ok();
                    notify(&watch_wake);
                    ok
                };
                if !send(list(&watch_conn).unwrap_or_default()) {
                    return;
                }
                for _ in signals.into_iter().flatten() {
                    if !send(list(&watch_conn).unwrap_or_default()) {
                        return;
                    }
                }
            })
            .ok()?;

        let act_wake = wake.try_clone().ok()?;
        std::thread::Builder::new()
            .name("gnosis-mounts".into())
            .spawn(move || {
                for action in inbox {
                    let event = match action {
                        Action::Mount(object) => {
                            let result = mount(&conn, &object);
                            Event::Mounted(object, result)
                        }
                        Action::Unmount(object) => {
                            let result = unmount(&conn, &object);
                            Event::Unmounted(object, result)
                        }
                    };
                    if outbox.send(event).is_err() {
                        return;
                    }
                    notify(&act_wake);
                }
            })
            .ok()?;
        Some(Self { actions, events, wake })
    }

    pub fn mount(&self, object: &str) {
        let _ = self.actions.send(Action::Mount(object.into()));
    }

    pub fn unmount(&self, object: &str) {
        let _ = self.actions.send(Action::Unmount(object.into()));
    }

    pub fn fd(&self) -> RawFd {
        self.wake.as_raw_fd()
    }

    pub fn take(&self) -> Vec<Event> {
        let mut count = 0u64;
        unsafe { libc::read(self.wake.as_raw_fd(), (&mut count as *mut u64).cast(), 8) };
        self.events.try_iter().collect()
    }
}

// "64 GB", "512 MB": Decimal units, as drive makers count.
pub fn human_size(bytes: u64) -> String {
    let units = ["B", "kB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < units.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if value >= 10.0 || unit == 0 { format!("{value:.0} {}", units[unit]) } else { format!("{value:.1} {}", units[unit]) }
}

// Whether `path` lies on the drive mounted at `mount`.
pub fn on_mount(path: &Path, mount: &Path) -> bool {
    path.starts_with(mount)
}
