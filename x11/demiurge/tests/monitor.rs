use demiurge_x11::monitor::{desktop_mode, Monitor, Monitors};
use x11rb::protocol::randr::{ModeFlag, ModeInfo};

fn mk(name: &str, x: i32, y: i32, w: u32, h: u32) -> Monitor {
    Monitor {
        name: name.into(),
        x,
        y,
        width: w,
        height: h,
    }
}

#[test]
fn try_new_rejects_empty() {
    assert!(Monitors::try_new(Vec::new()).is_none());
}

#[test]
fn try_new_accepts_non_empty() {
    let m = Monitors::try_new(vec![mk("DP-1", 0, 0, 1920, 1080)]).expect("non-empty");
    assert_eq!(m.len(), 1);
    assert_eq!(m.first().name, "DP-1");
}

#[test]
fn single_yields_one_element() {
    let m = Monitors::single(mk("HDMI-A-0", 0, 0, 2560, 1440));
    assert_eq!(m.len(), 1);
    assert_eq!(m.first().width, 2560);
}

#[test]
fn first_returns_head_with_multiple_elements() {
    let m = Monitors::try_new(vec![
        mk("DP-1", 0, 0, 1920, 1080),
        mk("DP-2", 1920, 0, 2560, 1440),
    ])
    .unwrap();
    assert_eq!(m.first().name, "DP-1");
    assert_eq!(m.len(), 2);
}

#[test]
fn deref_to_slice_supports_iter() {
    let m = Monitors::try_new(vec![
        mk("a", 0, 0, 100, 100),
        mk("b", 100, 0, 100, 100),
    ])
    .unwrap();
    let names: Vec<&str> = m.iter().map(|x| x.name.as_str()).collect();
    assert_eq!(names, vec!["a", "b"]);
}

#[test]
fn equality_is_elementwise() {
    let a = Monitors::single(mk("DP-1", 0, 0, 1920, 1080));
    let b = Monitors::single(mk("DP-1", 0, 0, 1920, 1080));
    let c = Monitors::single(mk("DP-1", 0, 0, 2560, 1440));
    assert_eq!(a, b);
    assert_ne!(a, c);
}

// The desktop mode, over the Alienware AW2521HF's own timings as its EDID
// and the NVIDIA driver give them: The EDID prefers 1080p at 60 Hz.

fn mode(id: u32, (w, h): (u16, u16), khz: u32, htotal: u16, vtotal: u16, flags: ModeFlag) -> ModeInfo {
    ModeInfo { id, width: w, height: h, dot_clock: khz * 1000, htotal, vtotal, mode_flags: flags, ..Default::default() }
}

fn aw2521hf() -> Vec<ModeInfo> {
    let p = ModeFlag::default();
    vec![
        mode(0x1c2, (1920, 1080), 148_500, 2200, 1125, p), // 60.00, preferred
        mode(0x1c3, (1920, 1080), 346_500, 2080, 1157, p), // 143.98
        mode(0x1c4, (1920, 1080), 285_500, 2080, 1144, p), // 119.98
        mode(0x1c5, (1920, 1080), 148_350, 2200, 1125, p), // 59.94
        mode(0x1d0, (1920, 1080), 567_360, 2040, 1160, p), // 239.76
        mode(0x1c7, (1600, 900), 108_000, 1800, 1000, p),
    ]
}

#[test]
fn the_desktop_mode_is_the_preferred_resolution_at_its_highest_rate() {
    let all = aw2521hf();
    let ids: Vec<u32> = all.iter().map(|m| m.id).collect();
    assert_eq!(desktop_mode(&all, &ids, 1), Some(0x1d0));
    // Where the link cannot carry 240 Hz the server offers 144 at most.
    let short: Vec<u32> = ids.iter().copied().filter(|&id| id != 0x1d0).collect();
    assert_eq!(desktop_mode(&all, &short, 1), Some(0x1c3));
}

#[test]
fn an_interlaced_mode_is_never_the_desktop_mode() {
    let mut all = aw2521hf();
    all.push(mode(0x1e0, (1920, 1080), 600_000, 2200, 1125, ModeFlag::INTERLACE));
    let ids: Vec<u32> = all.iter().map(|m| m.id).collect();
    assert_eq!(desktop_mode(&all, &ids, 1), Some(0x1d0));
}

#[test]
fn an_output_preferring_no_mode_has_no_desktop_mode() {
    let all = aw2521hf();
    let ids: Vec<u32> = all.iter().map(|m| m.id).collect();
    assert_eq!(desktop_mode(&all, &ids, 0), None);
}
