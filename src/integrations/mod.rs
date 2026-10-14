//! Platform integration adapters.
//!
//! Adapters declare capabilities through [`crate::capabilities::Probe`] and must
//! never report a capability as verified without live installed-client evidence.

pub mod codex;
