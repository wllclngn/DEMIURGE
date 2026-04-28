// Shared test infrastructure. Cargo treats `tests/common/mod.rs` as a
// non-test helper (vs `tests/*.rs` which become test binaries), so each
// integration test pulls this in via `mod common;`.

#![allow(dead_code)]

pub mod wm;
