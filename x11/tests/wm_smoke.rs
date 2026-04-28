// End-to-end smoke tests for the WmTest harness. If these pass, the
// harness can launch Xephyr + demiurge, observe EWMH bootstrap, and
// inspect WM state cleanly. Subsequent test files will assume this and
// build feature-specific assertions on top.

use std::time::Duration;

mod common;
use common::wm::{minimal_config, WmTest};

#[test]
fn xephyr_starts_and_demiurge_advertises_itself() {
    let mut wm = WmTest::new();
    wm.start_demiurge(minimal_config());

    let check = wm
        .supporting_wm_check()
        .expect("_NET_SUPPORTING_WM_CHECK on root");
    let name = wm.wm_name(check).expect("_NET_WM_NAME on check window");
    assert!(
        name.starts_with("demiurge"),
        "expected wm name to start with 'demiurge', got {:?}",
        name,
    );
}

#[test]
fn ewmh_desktop_count_matches_config_tags() {
    let mut wm = WmTest::new();
    wm.start_demiurge(minimal_config());

    let count = wm.number_of_desktops().expect("_NET_NUMBER_OF_DESKTOPS");
    assert_eq!(count, 3, "minimal_config has 3 tags");

    let names = wm.desktop_names();
    assert_eq!(names, vec!["A", "B", "C"]);
}

#[test]
fn current_desktop_starts_at_zero() {
    let mut wm = WmTest::new();
    wm.start_demiurge(minimal_config());

    assert_eq!(wm.current_desktop(), Some(0));
}

#[test]
fn workarea_is_populated_per_desktop() {
    let mut wm = WmTest::new();
    wm.start_demiurge(minimal_config());

    let workarea = wm.workarea();
    // Four CARDINALs per desktop: x, y, w, h. minimal_config has 3.
    assert_eq!(workarea.len(), 12, "workarea must be 4 cardinals * 3 tags");

    let geom = wm.desktop_geometry();
    // Two CARDINALs total: width, height.
    assert_eq!(geom.len(), 2, "desktop geometry is (w, h)");
    assert!(geom[0] > 0 && geom[1] > 0, "non-zero dimensions");
}

#[test]
fn spawned_window_is_managed_and_listed() {
    let mut wm = WmTest::new();
    wm.start_demiurge(minimal_config());

    let win = wm.spawn_window(400, 300);
    let listed = wm.wait_for(
        |w| w.client_list().contains(&win),
        Duration::from_secs(2),
    );
    assert!(listed, "spawned window did not enter _NET_CLIENT_LIST");

    let active = wm.wait_for(
        |w| w.active_window() == Some(win),
        Duration::from_secs(2),
    );
    assert!(active, "spawned window did not become _NET_ACTIVE_WINDOW");
}
