// The run prompt both stacks share: PATH's executables, the completion
// for what is typed, and the keys that edit, complete, run or close it.

use demiurge_core::prompt::{Key, Prompt, find_completion, scan_path};

const RETURN: u32 = 0xff0d;
const ESCAPE: u32 = 0xff1b;
const TAB: u32 = 0xff09;
const BACKSPACE: u32 = 0xff08;

fn exes() -> Vec<String> {
    vec!["cat".into(), "chmod".into(), "chown".into(), "cp".into()]
}

fn typed(prompt: &mut Prompt, text: &str) {
    for c in text.chars() {
        assert!(matches!(prompt.key(c as u32, &exes()), Key::Open));
    }
}

// scan_path tests

#[test]
fn scan_path_sorted() {
    let exes = scan_path();
    for pair in exes.windows(2) {
        assert!(pair[0] <= pair[1], "scan_path not sorted: '{}' > '{}'", pair[0], pair[1]);
    }
}

#[test]
fn scan_path_no_duplicates() {
    let exes = scan_path();
    for pair in exes.windows(2) {
        assert_ne!(pair[0], pair[1], "scan_path has duplicate: '{}'", pair[0]);
    }
}

// find_completion tests

#[test]
fn find_completion_match() {
    let exes = vec!["cat".into(), "chmod".into(), "chown".into(), "cp".into()];
    assert_eq!(find_completion(&exes, "ch"), Some("chmod".into()));
}

#[test]
fn find_completion_exact() {
    let exes = vec!["cat".into(), "chmod".into(), "cp".into()];
    assert_eq!(find_completion(&exes, "cat"), Some("cat".into()));
}

#[test]
fn find_completion_no_match() {
    let exes = vec!["cat".into(), "cp".into()];
    assert_eq!(find_completion(&exes, "zz"), None);
}

#[test]
fn find_completion_empty_input() {
    let exes = vec!["cat".into()];
    assert_eq!(find_completion(&exes, ""), None);
}

#[test]
fn find_completion_empty_list() {
    let exes: Vec<String> = vec![];
    assert_eq!(find_completion(&exes, "cat"), None);
}

// Keys

#[test]
fn typing_completes_and_tab_takes_the_completion() {
    let mut prompt = Prompt::default();
    typed(&mut prompt, "ch");
    assert_eq!(prompt.input, "ch");
    assert_eq!(prompt.cursor, 2);
    assert_eq!(prompt.completion.as_deref(), Some("chmod"));

    assert!(matches!(prompt.key(TAB, &exes()), Key::Open));
    assert_eq!(prompt.input, "chmod");
    assert_eq!(prompt.cursor, 5);
}

#[test]
fn backspace_takes_back_a_character_and_its_completion() {
    let mut prompt = Prompt::default();
    typed(&mut prompt, "cp");
    assert!(matches!(prompt.key(BACKSPACE, &exes()), Key::Open));
    assert_eq!(prompt.input, "c");
    assert_eq!(prompt.completion.as_deref(), Some("cat"));
    assert!(matches!(prompt.key(BACKSPACE, &exes()), Key::Open));
    assert_eq!(prompt.input, "");
    assert_eq!(prompt.completion, None);
}

#[test]
fn return_runs_what_was_typed() {
    let mut prompt = Prompt::default();
    typed(&mut prompt, "cat -n x");
    match prompt.key(RETURN, &exes()) {
        Key::Run(cmd) => assert_eq!(cmd, "cat -n x"),
        _ => panic!("Return on a command did not run it"),
    }
}

#[test]
fn return_on_nothing_and_escape_close_it() {
    let mut prompt = Prompt::default();
    assert!(matches!(prompt.key(RETURN, &exes()), Key::Closed));
    typed(&mut prompt, "cat");
    assert!(matches!(prompt.key(ESCAPE, &exes()), Key::Closed));
}
