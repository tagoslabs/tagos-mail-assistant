//! Draft-only mail assistant logic.
//!
//! This crate does not send mail and does not open network connections.

pub mod backend;
pub mod clock;
pub mod errors;
pub mod guards;
pub mod model;
pub mod store;
pub mod untrusted;

#[cfg(any(test, feature = "test-support"))]
pub use store::store_contract_suite;

pub const CRATE_NAME: &str = "tagos-mail-assistant";
