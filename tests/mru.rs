// Two-stack browser-back/forward navigation tests as pure data structures
// (no X11 connection needed). The wm.rs/mru.rs implementation is exercised
// in Xephyr; these tests pin the transition rules in isolation.

type Win = u32;

struct Nav {
    history: Vec<Win>,
    future: Vec<Win>,
    current: Option<Win>,
}

impl Nav {
    fn new() -> Self {
        Nav {
            history: Vec::new(),
            future: Vec::new(),
            current: None,
        }
    }

    // Direct focus change (click, _NET_ACTIVE_WINDOW, view_tag, app launch).
    // Pushes the previous current to history and clears the future stack.
    fn direct_focus(&mut self, target: Win) {
        if let Some(old) = self.current {
            if old != target {
                self.history.push(old);
            }
        }
        self.future.clear();
        self.current = Some(target);
    }

    // Alt+Tab one step back: future.push(current); current = history.pop()
    fn step_back(&mut self) {
        if let Some(target) = self.history.pop() {
            if let Some(cur) = self.current {
                self.future.push(cur);
            }
            self.current = Some(target);
        }
    }

    // Alt+Shift+Tab one step forward: history.push(current); current = future.pop()
    fn step_forward(&mut self) {
        if let Some(target) = self.future.pop() {
            if let Some(cur) = self.current {
                self.history.push(cur);
            }
            self.current = Some(target);
        }
    }

    // Window destroyed: scrub from both stacks.
    fn on_unmanage(&mut self, gone: Win) {
        self.history.retain(|&w| w != gone);
        self.future.retain(|&w| w != gone);
    }
}

#[test]
fn direct_focus_pushes_current_to_history_and_clears_future() {
    let mut nav = Nav::new();
    nav.current = Some(1);
    nav.future = vec![5, 6];
    nav.direct_focus(2);
    assert_eq!(nav.history, vec![1]);
    assert!(nav.future.is_empty());
    assert_eq!(nav.current, Some(2));
}

#[test]
fn direct_focus_same_window_is_noop_for_history() {
    let mut nav = Nav::new();
    nav.current = Some(7);
    nav.history = vec![1, 2];
    nav.direct_focus(7);
    assert_eq!(nav.history, vec![1, 2]);
    assert_eq!(nav.current, Some(7));
}

#[test]
fn step_back_moves_current_to_future_and_pops_history() {
    let mut nav = Nav::new();
    nav.history = vec![1, 2, 3];
    nav.current = Some(4);
    nav.step_back();
    assert_eq!(nav.history, vec![1, 2]);
    assert_eq!(nav.future, vec![4]);
    assert_eq!(nav.current, Some(3));
}

#[test]
fn step_forward_is_inverse_of_step_back() {
    let mut nav = Nav::new();
    nav.history = vec![1, 2, 3];
    nav.current = Some(4);
    nav.step_back();
    nav.step_forward();
    assert_eq!(nav.history, vec![1, 2, 3]);
    assert_eq!(nav.future, vec![]);
    assert_eq!(nav.current, Some(4));
}

#[test]
fn back_then_forward_restores_original_state() {
    let mut nav = Nav::new();
    // Open A, B, C, D in order
    nav.direct_focus(1);
    nav.direct_focus(2);
    nav.direct_focus(3);
    nav.direct_focus(4);
    let original_history = nav.history.clone();
    let original_current = nav.current;

    nav.step_back();
    nav.step_back();
    nav.step_forward();
    nav.step_forward();

    assert_eq!(nav.history, original_history);
    assert_eq!(nav.current, original_current);
    assert!(nav.future.is_empty());
}

#[test]
fn unmanage_scrubs_from_both_stacks() {
    let mut nav = Nav::new();
    nav.history = vec![1, 2, 3];
    nav.future = vec![4, 5];
    nav.on_unmanage(2);
    nav.on_unmanage(5);
    assert_eq!(nav.history, vec![1, 3]);
    assert_eq!(nav.future, vec![4]);
}

#[test]
fn new_window_managed_pushes_old_current() {
    // Open A, then open B
    let mut nav = Nav::new();
    nav.direct_focus(1);
    assert_eq!(nav.history, vec![]);
    assert_eq!(nav.current, Some(1));

    nav.direct_focus(2);
    assert_eq!(nav.history, vec![1]);
    assert_eq!(nav.current, Some(2));
}

#[test]
fn alt_tab_then_open_new_window_clears_future() {
    // Open A, B, C; Alt+Tab back to B; then open D — D's open clears future
    let mut nav = Nav::new();
    nav.direct_focus(1);
    nav.direct_focus(2);
    nav.direct_focus(3);
    nav.step_back();
    assert_eq!(nav.current, Some(2));
    assert_eq!(nav.future, vec![3]);

    nav.direct_focus(4);
    assert_eq!(nav.current, Some(4));
    assert!(nav.future.is_empty());
    // 2 is now in history, 3 is gone from the navigation stack entirely
    assert_eq!(nav.history, vec![1, 2]);
}

#[test]
fn step_back_with_empty_history_is_noop() {
    let mut nav = Nav::new();
    nav.current = Some(1);
    nav.step_back();
    assert!(nav.history.is_empty());
    assert!(nav.future.is_empty());
    assert_eq!(nav.current, Some(1));
}
