use demiurge_core::layout::{Layout, Zoom};
use demiurge_core::monitor::view;

#[test]
fn a_tag_shown_elsewhere_swaps_monitors() {
    let mut tags = vec![0, 1];
    assert!(view(&mut tags, 0, 1));
    assert_eq!(tags, vec![1, 0]);
}

#[test]
fn a_hidden_tag_replaces_the_monitors_own() {
    let mut tags = vec![0, 1];
    assert!(view(&mut tags, 1, 4));
    assert_eq!(tags, vec![0, 4]);
}

#[test]
fn a_monitor_already_showing_the_tag_is_unchanged() {
    let mut tags = vec![2, 3];
    assert!(!view(&mut tags, 0, 2));
    assert_eq!(tags, vec![2, 3]);
    assert!(!view(&mut tags, 5, 1), "a monitor that is not there");
}

#[test]
fn only_a_tiled_window_floats_out_to_zoom() {
    let at = (10, 20, 300, 200);
    assert!(Zoom::new(at, false, Layout::Tile).floated);
    assert!(Zoom::new(at, false, Layout::Monocle).floated);
    assert!(!Zoom::new(at, true, Layout::Tile).floated);
    assert!(!Zoom::new(at, false, Layout::Floating).floated);
    assert_eq!(Zoom::new(at, false, Layout::Tile).geometry, at);
}
