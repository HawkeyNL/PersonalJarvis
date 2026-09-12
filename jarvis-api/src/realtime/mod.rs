//! Single-process, owner-partitioned presentation fanout. The database is the
//! source of truth: disconnect/overflow requires REST reconciliation.
mod hub;
pub(crate) mod voice;
pub(crate) mod websocket;

pub use hub::{Hub, Subscription};
