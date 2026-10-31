use std::time::{SystemTime, UNIX_EPOCH};

use demiurge_core::clock;

#[test]
fn the_time_comes_out_in_the_format_asked() {
    let year = clock::now("%Y");
    assert_eq!(year.len(), 4, "{year:?}");
    assert!(year.chars().all(|c| c.is_ascii_digit()), "{year:?}");
}

// demiurge-wl's own formatter had a 64-byte buffer, and strftime fills
// nothing when the result does not fit: A long clock_format blanked the
// bar's clock under Wayland only.
#[test]
fn a_long_format_is_not_blanked() {
    let format = "%A, %B %d %Y   %I:%M:%S %p %Z   week %V, day %j of the year";
    assert!(clock::now(format).len() > 64, "{:?}", clock::now(format));
}

#[test]
fn the_wait_ends_on_the_next_whole_second() {
    let wait = clock::until_next_second();
    assert!(wait.as_nanos() > 0 && wait.as_nanos() <= 1_000_000_000, "{wait:?}");
    std::thread::sleep(wait);
    let into = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().subsec_millis();
    assert!(into < 50, "woke {into} ms into the second");
}

// A seconds clock ticks just past the second, and the time it reads there
// must already be the new second: glibc's time() reads the kernel's coarse
// clock, which moves only on its tick, and a tick that read the second
// before showed nothing new and waited a whole second for the next one. In
// the QEMU guest time() named the old second at 20 of 20 such wakes; on a
// host whose wake from idle brings the coarse clock up, this passes either
// way, and the guest's clock test is the one that catches it.
#[test]
fn the_second_the_wait_ends_on_is_the_second_read() {
    for _ in 0..5 {
        std::thread::sleep(clock::until_next_second());
        let second = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        assert_eq!(clock::now("%s"), second.to_string());
    }
}
