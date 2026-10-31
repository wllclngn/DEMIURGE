// Per-monitor tag and visibility tests.
//
// On a single Xephyr screen these still drive the new code paths -- the
// active_tags vec is length 1, but view_tag/move_to_tag/tag_visible all
// flow through the new generalized logic. Multi-monitor swap-on-collision
// requires either a multi-output Xephyr setup or a Xinerama harness, both
// of which are larger lifts; the swap branch is covered by visual /
// manual verification for now and the single-monitor code is exercised
// here.

use std::time::Duration;

use x11rb::protocol::xproto::MapState;

mod common;
use common::wm::{minimal_config, WmTest};

const STEP: Duration = Duration::from_secs(2);

#[test]
fn view_tag_via_ewmh_changes_current_desktop() {
    let mut wm = WmTest::new();
    wm.start_demiurge(minimal_config());

    assert_eq!(wm.current_desktop(), Some(0));

    wm.request_view_tag(1);
    let switched = wm.wait_for(|w| w.current_desktop() == Some(1), STEP);
    assert!(switched, "_NET_CURRENT_DESKTOP did not advance to 1");
}

#[test]
fn switching_tags_unmaps_windows_on_old_tag() {
    let mut wm = WmTest::new();
    wm.start_demiurge(minimal_config());

    let win = wm.spawn_window(400, 300);
    let mapped = wm.wait_for(|w| w.client_list().contains(&win), STEP);
    assert!(mapped, "window never managed");

    // Verify it starts mapped on tag 0.
    let viewable = wm.wait_for(
        |w| w.map_state(win) == Some(MapState::VIEWABLE),
        STEP,
    );
    assert!(viewable, "spawn_window should be VIEWABLE on tag 0");

    // Switch to tag 1 -> tag 0's lone window should unmap.
    wm.request_view_tag(1);
    let hidden = wm.wait_for(
        |w| w.map_state(win) == Some(MapState::UNMAPPED),
        STEP,
    );
    assert!(hidden, "window did not unmap when its tag was hidden");

    // The window stays in _NET_CLIENT_LIST (it's still managed, just hidden).
    assert!(wm.client_list().contains(&win));
}

#[test]
fn switching_back_remaps_window() {
    let mut wm = WmTest::new();
    wm.start_demiurge(minimal_config());

    let win = wm.spawn_window(400, 300);
    wm.wait_for(|w| w.client_list().contains(&win), STEP);

    wm.request_view_tag(1);
    wm.wait_for(
        |w| w.map_state(win) == Some(MapState::UNMAPPED),
        STEP,
    );

    wm.request_view_tag(0);
    let remapped = wm.wait_for(
        |w| w.map_state(win) == Some(MapState::VIEWABLE),
        STEP,
    );
    assert!(remapped, "window did not remap when its tag became visible");
}

#[test]
fn move_to_tag_via_ewmh_updates_wm_desktop() {
    let mut wm = WmTest::new();
    wm.start_demiurge(minimal_config());

    let win = wm.spawn_window(400, 300);
    wm.wait_for(|w| w.client_list().contains(&win), STEP);
    wm.wait_for(
        |w| w.active_window() == Some(win),
        STEP,
    );

    // Window starts on tag 0.
    let initial = wm.wait_for(|w| w.wm_desktop(win) == Some(0), STEP);
    assert!(initial, "initial _NET_WM_DESKTOP should be 0");

    // Move the focused window to tag 2.
    wm.request_move_to_tag(win, 2);
    let moved = wm.wait_for(|w| w.wm_desktop(win) == Some(2), STEP);
    assert!(moved, "_NET_WM_DESKTOP did not become 2");

    // Tag 2 isn't visible (we're still on tag 0), so the window
    // should have unmapped.
    let hidden = wm.wait_for(
        |w| w.map_state(win) == Some(MapState::UNMAPPED),
        STEP,
    );
    assert!(hidden, "window did not unmap after move to a hidden tag");
}

#[test]
fn move_to_visible_tag_keeps_window_mapped() {
    let mut wm = WmTest::new();
    wm.start_demiurge(minimal_config());

    let win = wm.spawn_window(400, 300);
    wm.wait_for(|w| w.client_list().contains(&win), STEP);
    wm.wait_for(|w| w.active_window() == Some(win), STEP);

    // Move window to tag 0, the same tag it's on -- expect a no-op
    // semantically; window stays mapped.
    wm.request_move_to_tag(win, 0);
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(wm.map_state(win), Some(MapState::VIEWABLE));
}

#[test]
fn out_of_range_tag_via_ewmh_is_ignored() {
    let mut wm = WmTest::new();
    wm.start_demiurge(minimal_config());

    // minimal_config has 3 tags; tag 99 is out of range. demiurge's
    // view_tag bails on tag >= num_tags, so _NET_CURRENT_DESKTOP must
    // stay at 0.
    wm.request_view_tag(99);
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(wm.current_desktop(), Some(0));
}
