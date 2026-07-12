//! HTTP handlers, grouped by the client flow they serve. The router maps routes
//! onto these; each delegates to `server::coordination`.

pub mod admin;
pub mod health;
pub mod indexing;
pub mod search;
