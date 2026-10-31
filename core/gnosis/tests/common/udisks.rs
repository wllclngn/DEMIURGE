// A private D-Bus and a fake udisks2 on it, for GNOSIS's drives: The tests
// talk to it directly, and the real GNOSIS binary through
// DBUS_SYSTEM_BUS_ADDRESS. No real disk is touched.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use zbus::object_server::SignalContext;
use zbus::zvariant::OwnedValue;

// A dbus-daemon of its own, gone with the test.
pub struct Bus {
    child: Child,
    pub address: String,
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn private_bus() -> Bus {
    let mut child = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--nopidfile", "--print-address=1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("dbus-daemon");
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
    Bus { child, address: line.trim().to_string() }
}

struct Block {
    device: &'static str,
    label: &'static str,
    hint_name: &'static str,
    usage: &'static str,
    size: u64,
    system: bool,
    ignore: bool,
}

#[zbus::interface(name = "org.freedesktop.UDisks2.Block")]
impl Block {
    #[zbus(property)]
    fn device(&self) -> Vec<u8> {
        let mut bytes = self.device.as_bytes().to_vec();
        bytes.push(0);
        bytes
    }
    #[zbus(property)]
    fn id_label(&self) -> String {
        self.label.into()
    }
    #[zbus(property)]
    fn hint_name(&self) -> String {
        self.hint_name.into()
    }
    #[zbus(property)]
    fn id_usage(&self) -> String {
        self.usage.into()
    }
    #[zbus(property)]
    fn size(&self) -> u64 {
        self.size
    }
    #[zbus(property)]
    fn hint_system(&self) -> bool {
        self.system
    }
    #[zbus(property)]
    fn hint_ignore(&self) -> bool {
        self.ignore
    }
}

// The calls the fake saw: (method, object, no-user-interaction asked).
pub type Calls = Arc<Mutex<Vec<(String, String, bool)>>>;

struct Filesystem {
    object: &'static str,
    target: &'static str,
    mounted: Option<&'static str>,
    calls: Calls,
}

fn quiet(options: &HashMap<String, OwnedValue>) -> bool {
    options.get("auth.no_user_interaction").and_then(|v| v.downcast_ref::<bool>().ok()) == Some(true)
}

#[zbus::interface(name = "org.freedesktop.UDisks2.Filesystem")]
impl Filesystem {
    #[zbus(property)]
    fn mount_points(&self) -> Vec<Vec<u8>> {
        self.mounted.map(|m| [m.as_bytes(), b"\0"].concat()).into_iter().collect()
    }

    async fn mount(&mut self, options: HashMap<String, OwnedValue>, #[zbus(signal_context)] ctxt: SignalContext<'_>) -> String {
        self.calls.lock().unwrap().push(("Mount".into(), self.object.into(), quiet(&options)));
        self.mounted = Some(self.target);
        let _ = self.mount_points_changed(&ctxt).await;
        self.target.into()
    }

    async fn unmount(&mut self, options: HashMap<String, OwnedValue>, #[zbus(signal_context)] ctxt: SignalContext<'_>) {
        self.calls.lock().unwrap().push(("Unmount".into(), self.object.into(), quiet(&options)));
        self.mounted = None;
        let _ = self.mount_points_changed(&ctxt).await;
    }
}

pub const ROOT: &str = "/org/freedesktop/UDisks2";

// udisks as the test sees a machine: An internal root, an internal data
// partition mounted under /mnt, a USB stick, an ignored and a swap partition.
pub fn fake_udisks(bus: &Bus, target: &'static str) -> (zbus::blocking::Connection, Calls) {
    let calls = Calls::default();
    let disk = |name: &'static str| format!("{ROOT}/block_devices/{name}");
    let parts: [(&'static str, Block, Option<&'static str>); 5] = [
        ("nvme0n1p2", Block { device: "/dev/nvme0n1p2", label: "", hint_name: "", usage: "filesystem", size: 249_521_242_112, system: true, ignore: false }, Some("/")),
        ("nvme0n1p3", Block { device: "/dev/nvme0n1p3", label: "data", hint_name: "", usage: "filesystem", size: 500_000_000_000, system: true, ignore: false }, Some("/mnt/data")),
        ("sdb1", Block { device: "/dev/sdb1", label: "USB STICK", hint_name: "", usage: "filesystem", size: 64_000_000_000, system: false, ignore: false }, None),
        ("sdc1", Block { device: "/dev/sdc1", label: "", hint_name: "", usage: "filesystem", size: 1_500_000, system: false, ignore: true }, None),
        ("sdd1", Block { device: "/dev/sdd1", label: "", hint_name: "", usage: "other", size: 8_000_000_000, system: false, ignore: false }, None),
    ];
    let mut builder = zbus::blocking::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .name("org.freedesktop.UDisks2")
        .unwrap();
    for (name, block, mounted) in parts {
        let object: &'static str = Box::leak(disk(name).into_boxed_str());
        builder = builder
            .serve_at(object, block)
            .unwrap()
            .serve_at(object, Filesystem { object, target, mounted, calls: calls.clone() })
            .unwrap();
    }
    let conn = builder.serve_at(ROOT, zbus::fdo::ObjectManager).unwrap().build().unwrap();
    (conn, calls)
}
