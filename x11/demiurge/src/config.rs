// Config schema lives in demiurge-core. This module re-exports it so
// `crate::config::Foo` paths in the X11 implementation keep working
// without rippling demiurge_core paths through every consumer.

pub use demiurge_core::config::*;
