// Cyclical MRU ring semantics, tested as a pure data structure (no X11
// connection needed). The wm.rs/mru.rs implementation is exercised in
// Xephyr; these tests pin the transition rules in isolation.

type Win = u32;

struct Mru {
    ring: Vec<Win>,
    cycle_index: Option<usize>,
}

impl Mru {
    fn new() -> Self {
        Mru {
            ring: Vec::new(),
            cycle_index: None,
        }
    }

    // Focus: promote window to front; existing occurrences removed.
    fn on_focus(&mut self, win: Win) {
        self.ring.retain(|&w| w != win);
        self.ring.insert(0, win);
    }

    // Window destroyed: remove from ring.
    fn on_unmanage(&mut self, gone: Win) {
        self.ring.retain(|&w| w != gone);
    }

    // Alt+Tab press. No-op when ring has fewer than 2 windows.
    fn cycle(&mut self, forward: bool) {
        if self.ring.len() < 2 {
            return;
        }
        let len = self.ring.len();
        let next = match self.cycle_index {
            Some(i) => {
                if forward {
                    (i + 1) % len
                } else {
                    (i + len - 1) % len
                }
            }
            None => {
                if forward {
                    1
                } else {
                    len - 1
                }
            }
        };
        self.cycle_index = Some(next);
    }

    // Alt release: commit the cycle target to the front of the ring.
    fn commit(&mut self) {
        if let Some(i) = self.cycle_index.take() {
            if let Some(&target) = self.ring.get(i) {
                self.on_focus(target);
            }
        }
    }
}

#[test]
fn focus_promotes_to_front() {
    let mut m = Mru::new();
    m.on_focus(1);
    m.on_focus(2);
    m.on_focus(3);
    assert_eq!(m.ring, vec![3, 2, 1]);
}

#[test]
fn focus_dedupes_existing_occurrence() {
    let mut m = Mru::new();
    m.on_focus(1);
    m.on_focus(2);
    m.on_focus(3);
    m.on_focus(1);
    assert_eq!(m.ring, vec![1, 3, 2]);
    assert_eq!(m.ring.len(), 3);
}

#[test]
fn unmanage_removes_from_ring() {
    let mut m = Mru::new();
    m.on_focus(1);
    m.on_focus(2);
    m.on_focus(3);
    m.on_unmanage(2);
    assert_eq!(m.ring, vec![3, 1]);
}

#[test]
fn cycle_forward_starts_at_second_most_recent() {
    let mut m = Mru::new();
    m.on_focus(1);
    m.on_focus(2);
    m.on_focus(3);
    // ring = [3, 2, 1]; Alt+Tab forward goes to index 1 (window 2)
    m.cycle(true);
    assert_eq!(m.cycle_index, Some(1));
}

#[test]
fn cycle_backward_starts_at_tail() {
    let mut m = Mru::new();
    m.on_focus(1);
    m.on_focus(2);
    m.on_focus(3);
    // ring = [3, 2, 1]; Alt+Shift+Tab goes to index 2 (window 1)
    m.cycle(false);
    assert_eq!(m.cycle_index, Some(2));
}

#[test]
fn cycle_wraps_forward() {
    let mut m = Mru::new();
    m.on_focus(1);
    m.on_focus(2);
    m.on_focus(3);
    // ring = [3, 2, 1]; forward three times wraps back to 0
    m.cycle(true); // 1
    m.cycle(true); // 2
    m.cycle(true); // 0
    assert_eq!(m.cycle_index, Some(0));
}

#[test]
fn cycle_wraps_backward() {
    let mut m = Mru::new();
    m.on_focus(1);
    m.on_focus(2);
    m.on_focus(3);
    // ring = [3, 2, 1]; backward from None lands at tail (2), then
    // backward wraps: 2 -> 1 -> 0
    m.cycle(false); // 2
    m.cycle(false); // 1
    m.cycle(false); // 0
    assert_eq!(m.cycle_index, Some(0));
}

#[test]
fn commit_promotes_cycle_target_to_front() {
    let mut m = Mru::new();
    m.on_focus(1);
    m.on_focus(2);
    m.on_focus(3);
    // ring = [3, 2, 1]; Alt+Tab twice selects index 2 (window 1)
    m.cycle(true);
    m.cycle(true);
    m.commit();
    assert_eq!(m.ring, vec![1, 3, 2]);
    assert_eq!(m.cycle_index, None);
}

#[test]
fn commit_without_cycle_is_noop() {
    let mut m = Mru::new();
    m.on_focus(1);
    m.on_focus(2);
    let before = m.ring.clone();
    m.commit();
    assert_eq!(m.ring, before);
}

#[test]
fn cycle_needs_two_windows() {
    let mut m = Mru::new();
    m.on_focus(1);
    m.cycle(true);
    assert_eq!(m.cycle_index, None);
}

#[test]
fn cycle_empty_ring_is_noop() {
    let mut m = Mru::new();
    m.cycle(true);
    m.cycle(false);
    assert_eq!(m.cycle_index, None);
    assert!(m.ring.is_empty());
}

#[test]
fn new_window_focused_lands_at_front() {
    let mut m = Mru::new();
    m.on_focus(1);
    m.on_focus(2);
    // New window 3 gets focused on manage
    m.on_focus(3);
    assert_eq!(m.ring, vec![3, 2, 1]);
}

#[test]
fn repeated_focus_of_same_window_is_idempotent() {
    let mut m = Mru::new();
    m.on_focus(1);
    m.on_focus(2);
    m.on_focus(2);
    m.on_focus(2);
    assert_eq!(m.ring, vec![2, 1]);
}

#[test]
fn unmanage_nonexistent_is_noop() {
    let mut m = Mru::new();
    m.on_focus(1);
    m.on_focus(2);
    m.on_unmanage(99);
    assert_eq!(m.ring, vec![2, 1]);
}

#[test]
fn alt_tab_alt_tab_commit_cycles_to_oldest_and_back() {
    // ring = [C, B, A]; Alt+Tab goes to B (commit → [B, C, A]);
    // Alt+Tab again goes to C (commit → [C, B, A])
    let mut m = Mru::new();
    m.on_focus(0xA);
    m.on_focus(0xB);
    m.on_focus(0xC);

    m.cycle(true);
    m.commit();
    assert_eq!(m.ring, vec![0xB, 0xC, 0xA]);

    m.cycle(true);
    m.commit();
    assert_eq!(m.ring, vec![0xC, 0xB, 0xA]);
}
