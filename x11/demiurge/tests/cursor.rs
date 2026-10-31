// Cursor auto-hide's poll costs nothing while it is off: An idle demiurge
// with `[cursor] auto_hide = false` wakes for its bar clock and nothing
// else, where the 100 ms poll alone woke it ten times a second.

use std::time::Duration;

mod common;
use common::wm::{WmTest, minimal_config};

// Every voluntary context switch of every thread of `pid` so far.
fn wakeups(pid: u32) -> u64 {
    std::fs::read_dir(format!("/proc/{pid}/task"))
        .expect("the window manager's threads")
        .filter_map(|task| std::fs::read_to_string(task.ok()?.path().join("status")).ok())
        .filter_map(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("voluntary_ctxt_switches:"))
                .and_then(|n| n.trim().parse::<u64>().ok())
        })
        .sum()
}

#[test]
fn an_idle_window_manager_without_auto_hide_wakes_only_for_its_clock() {
    let mut wm = WmTest::new();
    wm.start_demiurge(&format!("{}\n[cursor]\nauto_hide = false\n", minimal_config()));
    let pid = wm.demiurge_pid();
    std::thread::sleep(Duration::from_secs(1));
    let before = wakeups(pid);
    std::thread::sleep(Duration::from_secs(3));
    let woke = wakeups(pid) - before;
    // Three clock ticks and their repaints; the poll would add thirty.
    assert!(woke < 15, "an idle window manager woke {woke} times in 3 s");
}
