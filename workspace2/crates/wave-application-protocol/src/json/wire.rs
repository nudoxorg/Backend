//! Declarative wire vocabulary for application JSON.

mod adaptive;
mod application;
mod compiler;
mod envelope;
mod scalar;

pub use envelope::{AdapterErrorEnvelope, ApplicationReplyWire, McpError, McpReply};
