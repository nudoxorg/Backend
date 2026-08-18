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
//! # fn run(engine: nudox_engine::EngineHandle) -> Result<(), nudox_engine::mcp::McpError> {
//! use nudox_engine::mcp::{AccountGate, McpHost};
//!
//! let mut host = McpHost::start(&engine, AccountGate::unmetered("documentation example"))?;
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
//! # async fn run(engine: nudox_engine::EngineHandle) -> Result<(), nudox_engine::mcp::McpError> {
//! use nudox_engine::mcp::{AccountGate, McpEndpoint, NudoxMcpServer};
//!
//! let endpoint = McpEndpoint::start(NudoxMcpServer::new(
//!     engine,
//!     AccountGate::unmetered("documentation example"),
//! )).await?;
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
//! * **LR-7** — one schema. [`SCHEMA_SDL`] is the only thing ever parsed by
//!   the graph plane or asserted byte-identical to `schema.graphql`; it is
//!   re-exported from `crate::graph::SCHEMA_SDL` so there is exactly one
//!   `include_str!` in the whole workspace, and it is what `nudox://schema`
//!   and `schema(full: true)` serve verbatim. [`schema_card::SCHEMA_CARD`]
//!   is a separate, DERIVED, hand-maintained summary of the same file — the
//!   `schema` default — that carries no such guarantee: it is
//!   token-frugal prose, not the schema, and `schema.graphql` wins on any
//!   disagreement.
//! * **LR-8** — this crate reads no `nudox-ir` type directly.  `SymbolKey` is
//!   constructed through the `EcosystemId`/`PackageName` re-exports on
//!   `crate::wire` (which itself re-exports them from `nudox-ir`).
//! * **LR-11** — [`session::Unauthenticated`] and [`session::Session`] are
//!   different types, not a `bool`.
//! * **LR-12 / §L7.5** — [`McpError`] is a `#[non_exhaustive]` `thiserror`
//!   enum. No `anyhow` outside tests.

#![warn(missing_docs)]

pub mod account;
/// The symbol ADDRESS scheme: a readable, resolvable alternative to a bare
/// `ecosystem:name#introhex` key (docs/MCP-SURFACE-PLAN.md §4). See the
/// module docs for the grammar and the four-stage resolver.
pub mod address;
pub mod endpoint;
pub mod error;
pub mod host;
/// Persisting the endpoint's port and token across restarts. See the module
/// doc for why this is not folded into `account::cache`.
pub mod identity;
/// The job registry behind `index`. Private: it is machinery, not
/// vocabulary — nothing outside this crate needs to name a job.
mod index;
pub mod key;
/// Occurrence-specific Markdown projection.
pub(crate) mod occurrence_format;
/// Typed-result Markdown projections.
pub(crate) mod result_format;
/// The compact `schema` reference card — a derived, token-frugal
/// summary of [`SCHEMA_SDL`]. See the module docs for why it exists and
/// what keeps it honest.
pub mod schema_card;
/// Semantic-search Markdown projection.
pub(crate) mod semantic_format;
pub mod server;
pub mod session;
pub mod tools;

pub use account::{
    AccountGate, AccountHost, AccountSummary, ApiKey, ApiKeyError, KeySource, Posture,
    SignInFailure,
};
pub use address::{
    Address, AddressParseError, AddressSegment, AutoResolveHeuristic, Candidate, PackageCoord,
    Qualifier, ResolveOutcome, render_address, resolve_in_package,
};
pub use endpoint::{LOOPBACK_BIND, MCP_PATH, McpEndpoint, PortPreference, preferred_bind};
pub use error::McpError;
pub use host::{DRAIN_TIMEOUT, McpHost, ShutdownOutcome};
pub use identity::EndpointIdentity;
pub use key::SymbolKeyDto;
pub use result_format::MarkdownResult;
pub use schema_card::SCHEMA_CARD;
pub use server::{NudoxMcpServer, PACKAGE_URI_PREFIX, SCHEMA_URI};
pub use session::{Session, SessionToken, Unauthenticated};
pub use tools::NudoxTools;

/// Render a typed MCP result with the canonical compact Markdown projection.
///
/// The server uses this same projection for every successful tool call. The
/// public wrapper lets integration benches exercise status variants that are
/// not practical to create through a live transport (for example an indexing
/// job observed while it is still downloading) without maintaining a second
/// formatter in the test harness.
pub fn render_markdown<T: MarkdownResult>(result: &T) -> String {
    result.to_markdown()
}

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
