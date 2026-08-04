//! HTTP handlers, grouped by the client flow they serve. The router maps routes
//! onto these; each delegates to `server::coordination`.

pub mod admin;
pub mod compiled;
pub mod depshards;
pub mod health;
pub mod indexing;
pub mod rerank;
pub mod search;
