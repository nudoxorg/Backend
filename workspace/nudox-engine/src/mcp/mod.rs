//! `nudox-mcp` — the MCP server `lindsey` hosts (GUI-LOCAL-PLAN §L6).
//!
//! # What this crate is
//!
//! An MCP (Model Context Protocol) server that exposes the local documentation
//! corpus to coding agents. It is L4 of the §L0 stack and the *only* crate that
//! speaks MCP:
//!
//! ```text
//! agent  →  nudox-mcp  →  nudox-engine  →  {nudox-graph, nudox-store}  →  nudox-ir
//! ```
//!
//! # LR-8: a view of the engine, never a second engine
//!
//! Every tool bottoms out in an [`crate::EngineHandle`]
//! call. This crate holds no corpus, no index, and no IR; it opens no files and
//! walks no `Entry`. If a tool here ever answers a question the engine could
//! not, that is a bug, because it means the GUI and an agent can disagree about
//! what the same workspace contains.
//!
//! # Quick start
//!
//! From a synchronous host — which is what `lindsey` is, and what
//! [`host::McpHost`] exists for (docs/LIMITATIONS.md L35):
//!
//! ```no_run
//! # fn run(engine: crate::EngineHandle) -> Result<(), crate::mcp::McpError> {
//! use crate::mcp::McpHost;
//!
//! let mut host = McpHost::start(&engine)?;
//! println!("listening on {:?}", host.url());                 // status bar
//! println!("{:?}", host.client_config_snippet());            // Settings → Connection
//! host.stop();                                               // window close
//! # Ok(())
//! # }
//! ```
//!
//! From async code, one layer lower:
//!
//! ```no_run
//! # async fn run(engine: crate::EngineHandle) -> Result<(), crate::mcp::McpError> {
//! use crate::mcp::{McpEndpoint, NudoxMcpServer};
//!
//! let endpoint = McpEndpoint::start(NudoxMcpServer::new(engine)).await?;
//! println!("listening on {}", endpoint.url());       // status bar
//! println!("{}", endpoint.client_config_snippet());  // Settings → Connection
//! endpoint.stop().await;                             // window close
//! # Ok(())
//! # }
//! ```
//!
//! # Standing decisions this crate is judged against
//!
//! * **LR-1** — the MCP tool argument *is* `SymbolKey`, spelled
//!   `ecosystem:name#introhex`. No parallel id type is invented; see
//!   [`key::SymbolKeyDto`].
//! * **LR-2** — no `serde_json::Value` in any tool signature. Every schema is
//!   `schemars`-derived from named types in [`tools`] and the wire vocabulary.
//! * **LR-7** — one schema. [`SCHEMA_SDL`] is served to agents verbatim, both
//!   as the `graph_schema` tool and as the `nudox://schema` resource.  It is
//!   re-exported from `crate::graph::SCHEMA_SDL` so there is exactly one
//!   `include_str!` in the whole workspace.
//! * **LR-8** — this crate reads no `nudox-ir` type directly.  `SymbolKey` is
//!   constructed through the `EcosystemId`/`PackageName` re-exports on
//!   `crate::wire` (which itself re-exports them from `nudox-ir`).
//! * **LR-11** — [`session::Unauthenticated`] and [`session::Session`] are
//!   different types, not a `bool`.
//! * **LR-12 / §L7.5** — [`McpError`] is a `#[non_exhaustive]` `thiserror`
//!   enum. No `anyhow` outside tests.

#![warn(missing_docs)]

pub mod account;
pub mod endpoint;
pub mod error;
pub mod host;
/// The job registry behind `index_package`. Private: it is machinery, not
/// vocabulary — nothing outside this crate needs to name a job.
mod index;
pub mod key;
/// Markdown primitives and compact MCP projections.
pub(crate) mod markdown;
/// Occurrence-specific Markdown projection.
pub(crate) mod occurrence_format;
/// Typed-result Markdown projections.
pub(crate) mod result_format;
pub mod server;
pub mod session;
/// Semantic-search Markdown projection.
pub(crate) mod semantic_format;
pub mod tools;

pub use account::{
    AccountGate, AccountHost, AccountSummary, ApiKey, ApiKeyError, KeySource, Posture,
    SignInFailure,
};
pub use endpoint::{LOOPBACK_BIND, MCP_PATH, McpEndpoint};
pub use error::McpError;
pub use host::{DRAIN_TIMEOUT, McpHost, ShutdownOutcome};
pub use key::SymbolKeyDto;
pub use server::{NudoxMcpServer, PACKAGE_URI_PREFIX, SCHEMA_URI};
pub use session::{Session, SessionToken, Unauthenticated};
pub use tools::NudoxTools;

/// The Trustfall schema, served to agents verbatim (LR-7).
///
/// Re-exported from [`crate::graph::SCHEMA_SDL`] so there is exactly one
/// `include_str!` in the whole workspace — in `nudox-graph` — rather than
/// two crates each reaching into the same file by path.
///
/// `tests/schema_source.rs` asserts this constant is byte-identical to
/// `crates/nudox-graph/schema.graphql` and that it parses to a valid schema,
/// so drift still fails loudly (LR-7).
pub const SCHEMA_SDL: &str = crate::graph::SCHEMA_SDL;
