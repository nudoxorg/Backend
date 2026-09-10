//! The `interface-mcp` crate exists to serve the one shared local library to agents over MCP.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
//!
//! The `nudox-mcp` binary is a thin process shell over this library: frames in, [`server::Server`]
//! dispatches, Markdown out. The crate is a library as well as a binary so the integration tests
//! can push the schema card's own worked examples through [`arguments::decode`] — the same decoder
//! the server uses — instead of re-implementing a parser that could agree with a mistake.
//!
//! Everything an agent must know once lives in [`card`]; everything it learns per call is a
//! [`interface_library::Reply`] rendered by [`interface_library::render::markdown`]. No text is
//! authored twice.

pub mod arguments;
pub mod card;
pub mod progress;
pub mod resources;
pub mod server;
pub mod tools;
