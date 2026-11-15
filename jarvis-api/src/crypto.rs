//! Read-only crypto wallets: public addresses the owner adds, stored in the
//! tables of schema 0023. Addresses are validated by
//! `jarvis_market_data::wallet`, which refuses anything that looks like key
//! material before any other check. There is no key, signing or transaction
//! path. Logs and audit rows name a wallet by id and kind, never by address.

pub mod store;
