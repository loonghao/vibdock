//! Platform-independent input contracts shared by the host and device plugins.
//!
//! This crate deliberately has no Windows, HID or service dependencies. It is
//! the smallest reusable layer: what a key chord is, how a press sequence turns
//! into a gesture, and how several controls form a combo. Device adapters feed
//! it press/release edges; the host decides what a match executes.

pub mod combo;
pub mod gesture;
pub mod hotkey;
