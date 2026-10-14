//! Platform integration adapters.
//!
//! Adapters declare capabilities through [`crate::capabilities::Probe`] and must
//! never report a capability as verified without live installed-client evidence.
//! A declaration based only on published documentation reports
//! [`crate::capabilities::Support::Unknown`] for what it cannot see: the absence
//! of a documented API is not evidence that the capability is absent.

pub mod codex;
