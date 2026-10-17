use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use demiurge_x11::journal;

#[test]
fn only_a_virtual_console_counts_as_a_vt() {
    let null = std::fs::File::open("/dev/null").unwrap();
    assert!(!journal::fd_is_vt(null.as_raw_fd()));

    let (r, _w) = std::os::unix::net::UnixStream::pair().unwrap();
    assert!(!journal::fd_is_vt(r.as_raw_fd()));

    let file = std::fs::File::open("/proc/self/status").unwrap();
    assert!(!journal::fd_is_vt(file.as_raw_fd()));

    assert!(!journal::fd_is_vt(-1));
}

#[test]
fn stream_header_is_seven_lines() {
    let h = journal::stream_header("demiurge");
    assert_eq!(h, "demiurge\n\n6\n0\n0\n0\n0\n");
}

#[test]
fn lines_written_to_the_stream_reach_the_journal() {
    if !Path::new("/run/systemd/journal/stdout").exists() {
        eprintln!("journald stream socket absent; skipping");
        return;
    }
    let id = format!("demiurge-journal-test-{}", std::process::id());
    let marker = format!("marker {}", std::process::id());
    {
        let mut s = journal::connect(&id).unwrap();
        writeln!(s, "{}", marker).unwrap();
    }

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let out = Command::new("journalctl")
            .args(["-t", &id, "-o", "cat", "--no-pager"])
            .output()
            .unwrap();
        if String::from_utf8_lossy(&out.stdout).contains(&marker) {
            break;
        }
        assert!(Instant::now() < deadline, "marker never reached the journal");
        std::thread::sleep(Duration::from_millis(100));
    }
}
