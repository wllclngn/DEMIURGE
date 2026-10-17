use demiurge_x11::layout::{center_in_work_area, Layout};

#[test]
fn centers_in_the_work_area_below_the_bar() {
    // 1280x720 monitor, 24 px bar: Work area 0,24 1280x696.
    assert_eq!(center_in_work_area(0, 24, 1280, 696, 400, 300), (440, 222, 400, 300));
    // Offset monitor (second head at x=2560).
    assert_eq!(center_in_work_area(2560, 40, 1920, 1040, 800, 600), (3120, 260, 800, 600));
}

#[test]
fn oversized_window_is_clamped_to_the_work_area() {
    assert_eq!(center_in_work_area(0, 24, 1280, 696, 4000, 3000), (0, 24, 1280, 696));
    assert_eq!(center_in_work_area(0, 24, 1280, 696, 2000, 100), (0, 322, 1280, 100));
}

#[test]
fn degenerate_sizes_stay_positive() {
    assert_eq!(center_in_work_area(0, 0, 1280, 720, 0, 0), (639, 359, 1, 1));
    assert_eq!(center_in_work_area(0, 0, 0, 0, 10, 10), (0, 0, 1, 1));
}

#[test]
fn layout_next_cycles() {
    assert_eq!(Layout::Floating.next(), Layout::Tile);
    assert_eq!(Layout::Tile.next(), Layout::Monocle);
    assert_eq!(Layout::Monocle.next(), Layout::Floating);
}

#[test]
fn layout_full_cycle() {
    let mut l = Layout::Floating;
    l = l.next(); // Tile
    l = l.next(); // Monocle
    l = l.next(); // Floating
    assert_eq!(l, Layout::Floating);
}

#[test]
fn layout_from_str() {
    assert_eq!(Layout::from_str("floating"), Layout::Floating);
    assert_eq!(Layout::from_str("tile"), Layout::Tile);
    assert_eq!(Layout::from_str("monocle"), Layout::Monocle);
    assert_eq!(Layout::from_str("unknown"), Layout::Floating);
    assert_eq!(Layout::from_str(""), Layout::Floating);
}
