//! Canonical seven-plane sparse language-extension preparation.
//!
//! Each plane stays named and typed. Fact keys are framed through the shared
//! typed/terminal remaps, while sparse bindings are the authority that keeps
//! equal interned facts on multiple declarations distinct without inventing a
//! raw fact-ordinal tie breaker.

mod plane;
