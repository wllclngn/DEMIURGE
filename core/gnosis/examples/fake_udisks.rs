// The fake udisks2 GNOSIS's tests run, as a program of its own for the
// Wayland binary in the QEMU guest: A private D-Bus and the fake on it,
// mounting at the directory named. Prints the bus's address, then each call
// the fake takes, a line each, until killed.

use std::io::Write;
use std::time::Duration;

#[allow(dead_code)]
#[path = "../tests/common/udisks.rs"]
mod udisks;

fn main() {
    let target = std::env::args().nth(1).expect("usage: fake_udisks MOUNT-TARGET");
    let target: &'static str = Box::leak(target.into_boxed_str());
    let bus = udisks::private_bus();
    let (_conn, calls) = udisks::fake_udisks(&bus, target);
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{}", bus.address);
    let mut seen = 0;
    loop {
        std::thread::sleep(Duration::from_millis(100));
        let calls = calls.lock().unwrap();
        for (method, object, quiet) in &calls[seen..] {
            let _ = writeln!(out, "{} {} {}", method, object, quiet);
        }
        seen = calls.len();
        let _ = out.flush();
    }
}
