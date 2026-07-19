//! Synchronization domain.
//!
//! The transport-independent engine lives in [`core`]. Concrete transports are
//! thin adapters layered on top of it, so adding another relay does not duplicate
//! merge, validation, conflict, or atomic-write behavior.

pub mod core;
pub mod directory;
pub mod http;
pub mod ssh;

mod codex;
