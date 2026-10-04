//! Draft-only mail assistant logic.
//!
//! This crate does not send mail and does not open network connections.

pub mod backend;
pub mod briefing;
pub mod clock;
pub mod detector;
pub mod drafts;
pub mod errors;
pub mod guards;
pub mod model;
pub mod scheduler;
pub mod store;
pub mod tracker;
pub mod untrusted;

#[cfg(test)]
pub(crate) mod testing;

#[cfg(any(test, feature = "test-support"))]
pub use store::store_contract_suite;

pub const CRATE_NAME: &str = "tagos-mail-assistant";
