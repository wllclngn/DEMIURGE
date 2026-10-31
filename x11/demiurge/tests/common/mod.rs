// Shared test infrastructure. Cargo treats `tests/common/mod.rs` as a
// non-test helper (vs `tests/*.rs` which become test binaries), so each
// integration test pulls this in via `mod common;`.

#![allow(dead_code)]

// GNOSIS's own tests' fake udisks2, for its sidebar test under the window
// manager.
#[path = "../../../../core/gnosis/tests/common/udisks.rs"]
pub mod udisks;
pub mod wm;
