// Tests for the unmap_ignore counter on Client. The counter prevents
// WM-initiated unmaps (tag switch, move-to-tag) from being misread as real
// client withdraws, which would unmanage the window and make it unreachable.
//
// We can't unit-test the event handler itself without an X connection, so
// we verify the invariants of the counter that the handler relies on.

use demiurge_x11::wm::Client;

fn fresh_client() -> Client {
    Client {
        window: 0,
        tag: 0,
        floating: false,
        fullscreen: false,
        above: false,
        x: 0,
        y: 0,
        w: 800,
        h: 600,
        saved_floating: None,
        saved_fullscreen: None,
        title: String::new(),
        unmap_ignore: 0,
    }
}

#[test]
fn fresh_client_starts_with_zero_ignore() {
    let c = fresh_client();
    assert_eq!(c.unmap_ignore, 0);
}

#[test]
fn bump_then_consume_matches_self_unmap() {
    // The handler's rule: if unmap_ignore > 0, decrement and return early
    // (skip unmanage). One bump matched by one decrement leaves the counter
    // back at 0 and the client still managed.
    let mut c = fresh_client();
    c.unmap_ignore = c.unmap_ignore.saturating_add(1);
    assert_eq!(c.unmap_ignore, 1);

    // Simulate the handler consuming the pending ignore.
    if c.unmap_ignore > 0 {
        c.unmap_ignore -= 1;
    }
    assert_eq!(c.unmap_ignore, 0);
}

#[test]
fn multiple_bumps_are_balanced_by_multiple_consumes() {
    // Rapid tag switching can enqueue several self-unmaps before any notify
    // returns. Each bump must be paired with exactly one consume; the final
    // count should be zero.
    let mut c = fresh_client();
    for _ in 0..5 {
        c.unmap_ignore = c.unmap_ignore.saturating_add(1);
    }
    assert_eq!(c.unmap_ignore, 5);

    for _ in 0..5 {
        if c.unmap_ignore > 0 {
            c.unmap_ignore -= 1;
        }
    }
    assert_eq!(c.unmap_ignore, 0);
}

#[test]
fn real_withdraw_with_zero_counter_would_unmanage() {
    // A client withdraw arrives with unmap_ignore == 0. The handler's rule
    // is: only skip unmanage when unmap_ignore > 0. So a zero-counter unmap
    // falls through to the unmanage path. This test encodes that decision.
    let c = fresh_client();
    let should_unmanage = c.unmap_ignore == 0;
    assert!(should_unmanage, "withdraw with zero counter must unmanage");
}

#[test]
fn saturating_add_does_not_wrap() {
    // If somehow we enqueue more self-unmaps than u8 can count, we must
    // saturate rather than wrap. Wrapping from 255 back to 0 would cause
    // the next WM-initiated unmap to be misread as a client withdraw.
    let mut c = fresh_client();
    c.unmap_ignore = u8::MAX;
    c.unmap_ignore = c.unmap_ignore.saturating_add(1);
    assert_eq!(c.unmap_ignore, u8::MAX);
}
