// GNOSIS's drives against a fake udisks2 on a private bus: Which filesystems
// the sidebar lists and how it names them, and mount and unmount through the
// worker threads, the list following udisks' property signals. No real disk
// is touched.

use std::time::{Duration, Instant};

use gnosis::drives::{self, Drive, Drives, Event};

mod common;
use common::udisks::{fake_udisks, private_bus, Bus, ROOT};

fn client(bus: &Bus) -> zbus::blocking::Connection {
    zbus::blocking::connection::Builder::address(bus.address.as_str()).unwrap().build().unwrap()
}

#[test]
fn the_sidebar_lists_external_and_user_mounted_filesystems() {
    let bus = private_bus();
    let (_server, _) = fake_udisks(&bus, "/run/media/u/USB STICK");
    let listed = drives::list(&client(&bus)).unwrap();
    // The internal root, the ignored and the non-filesystem partitions stay
    // out; the internal partition mounted under /mnt is in.
    assert_eq!(
        listed,
        vec![
            Drive {
                object: format!("{ROOT}/block_devices/sdb1"),
                label: "USB STICK".into(),
                size: 64_000_000_000,
                mount: None,
            },
            Drive {
                object: format!("{ROOT}/block_devices/nvme0n1p3"),
                label: "data".into(),
                size: 500_000_000_000,
                mount: Some("/mnt/data".into()),
            },
        ]
    );
}

fn wait_for(drives: &Drives, mut want: impl FnMut(&Event) -> bool) -> Vec<Event> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        let mut pfd = libc::pollfd { fd: drives.fd(), events: libc::POLLIN, revents: 0 };
        unsafe { libc::poll(&mut pfd, 1, 200) };
        for event in drives.take() {
            let done = want(&event);
            seen.push(event);
            if done {
                return seen;
            }
        }
    }
    panic!("no matching event; saw {seen:?}");
}

#[test]
fn mount_and_unmount_go_through_the_workers_and_the_list_follows() {
    let bus = private_bus();
    let (_server, calls) = fake_udisks(&bus, "/run/media/u/USB STICK");
    let drives = Drives::with(client(&bus)).unwrap();
    let stick = format!("{ROOT}/block_devices/sdb1");

    wait_for(&drives, |e| matches!(e, Event::Drives(d) if d.len() == 2));
    drives.mount(&stick);
    wait_for(&drives, |e| matches!(e, Event::Mounted(o, Ok(p)) if *o == stick && p.as_os_str() == "/run/media/u/USB STICK"));
    wait_for(&drives, |e| {
        matches!(e, Event::Drives(d) if d.iter().any(|d| d.object == stick && d.mount.as_deref() == Some("/run/media/u/USB STICK".as_ref())))
    });

    drives.unmount(&stick);
    wait_for(&drives, |e| matches!(e, Event::Unmounted(o, Ok(())) if *o == stick));
    wait_for(&drives, |e| matches!(e, Event::Drives(d) if d.iter().any(|d| d.object == stick && d.mount.is_none())));

    let calls = calls.lock().unwrap().clone();
    assert_eq!(calls, vec![("Mount".into(), stick.clone(), true), ("Unmount".into(), stick, true)]);
}

#[test]
fn sizes_read_the_way_drives_are_sold() {
    assert_eq!(drives::human_size(64_000_000_000), "64 GB");
    assert_eq!(drives::human_size(1_500_000), "1.5 MB");
    assert_eq!(drives::human_size(512), "512 B");
}
