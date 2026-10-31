// The mode a monitor's desktop runs in, over the Alienware AW2521HF's own
// timings as its EDID gives them: The EDID prefers 1080p at 60 Hz, and the
// panel runs at 240.

use demiurge_core::monitor::{Timing, desktop_mode, sized_mode};

fn mode((w, h): (u16, u16), clock: u32, htotal: u16, vtotal: u16) -> Timing {
    Timing { size: (w, h), clock, htotal, vtotal, progressive: true, preferred: false }
}

fn aw2521hf() -> Vec<Timing> {
    let mut modes = vec![
        mode((1920, 1080), 148_500, 2200, 1125), // 60.00
        mode((1920, 1080), 346_500, 2080, 1157), // 143.98
        mode((1920, 1080), 285_500, 2080, 1144), // 119.98
        mode((1920, 1080), 148_350, 2200, 1125), // 59.94
        mode((1920, 1080), 567_360, 2040, 1160), // 239.76
        mode((1600, 900), 108_000, 1800, 1000),
    ];
    modes[0].preferred = true;
    modes
}

#[test]
fn a_monitor_is_lit_at_its_preferred_resolution_at_its_highest_rate() {
    let modes = aw2521hf();
    assert_eq!(desktop_mode(&modes), Some(4));
    // Where the link cannot carry 240 Hz the kernel offers 144 at most.
    let short: Vec<Timing> = aw2521hf().into_iter().enumerate().filter(|&(i, _)| i != 4).map(|(_, m)| m).collect();
    assert_eq!(desktop_mode(&short), Some(1));
}

#[test]
fn an_interlaced_or_double_scanned_mode_is_never_lit() {
    let mut modes = aw2521hf();
    let mut fast = mode((1920, 1080), 600_000, 2200, 1125);
    fast.progressive = false;
    modes.push(fast);
    assert_eq!(desktop_mode(&modes), Some(4));
}

#[test]
fn a_monitor_preferring_no_mode_has_no_desktop_mode() {
    let mut modes = aw2521hf();
    modes[0].preferred = false;
    assert_eq!(desktop_mode(&modes), None);
    assert_eq!(desktop_mode(&[]), None);
}

// A size asked by [[display]]: Lit at its highest progressive rate; a size
// the monitor does not offer is no mode.
#[test]
fn a_requested_size_is_lit_at_its_highest_rate() {
    let modes = aw2521hf();
    assert_eq!(sized_mode(&modes, (1920, 1080)), Some(4));
    assert_eq!(sized_mode(&modes, (1600, 900)), Some(5));
    assert_eq!(sized_mode(&modes, (2560, 1440)), None);
}
