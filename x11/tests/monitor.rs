use demiurge_x11::monitor::{Monitor, Monitors};

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
