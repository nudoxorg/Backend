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
//! Every one of the six tools bottoms out in an [`nudox_engine::EngineHandle`]
//! call. This crate holds no corpus, no index, and no IR; it opens no files and
//! walks no `Entry`. If a tool here ever answers a question the engine could
//! not, that is a bug, because it means the GUI and an agent can disagree about
//! what the same workspace contains.
//!
//! # Quick start
//!
//! ```no_run
//! # async fn run(engine: nudox_engine::EngineHandle) -> Result<(), nudox_mcp::McpError> {
//! use nudox_mcp::{McpEndpoint, NudoxMcpServer};
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
//!   [`dto::SymbolKeyDto`].
//! * **LR-2** — no `serde_json::Value` in any tool signature. Every schema is
//!   `schemars`-derived from a named type in [`tools`] and [`dto`]. (rmcp's own
//!   transport internals use `Value`; that is below this seam.)
//! * **LR-7** — one schema. [`SCHEMA_SDL`] is served to agents verbatim, both
//!   as the `graph_schema` tool and as the `nudox://schema` resource.
//! * **LR-11** — [`session::Unauthenticated`] and [`session::Session`] are
//!   different types, not a `bool`.
//! * **LR-12 / §L7.5** — [`McpError`] is a `#[non_exhaustive]` `thiserror`
//!   enum. No `anyhow` outside tests.
//!
//! # TODO(engine) — assumptions to reconcile
//!
//! This crate was written against `nudox-engine` as it stood, and two gaps in
//! that crate's public surface shaped the result. Both are recorded here rather
//! than worked around silently.
//!
//! 1. **`SymbolKey` has no public constructor path.** `EngineHandle::open_symbol`
//!    takes a `SymbolKey` (= `nudox_ir::change::StableRef`), but
//!    `nudox_engine::wire` re-exports only `SymbolKey`, `IntroId` and
//!    `PackageLineageId` — not the `EcosystemId`/`PackageName` newtypes needed
//!    to *build* one. `get_symbol` is unimplementable without them, so this
//!    crate takes a `nudox-ir` dependency used for exactly those two
//!    constructors, in exactly one function
//!    ([`dto::SymbolKeyDto::to_wire`]). Adding
//!    `pub use nudox_ir::change::{EcosystemId, PackageName};` to
//!    `nudox_engine::wire` — or better, a `SymbolKey` string codec beside
//!    `nudox_graph`'s existing private `parse_stable_ref` — removes it.
//!
//! 2. **The wire types are not serialisable.** `HitRow`, `SymbolHead` and
//!    `RenderSection` derive neither `serde::Serialize` nor
//!    `schemars::JsonSchema`, and every wire enum is `#[non_exhaustive]`, which
//!    also rules out serde's `remote` escape hatch from a foreign crate. The
//!    [`dto`] module therefore projects them into owned, derive-friendly types.
//!    Adding `#[derive(serde::Serialize, schemars::JsonSchema)]` to
//!    `nudox_engine::wire` (plus `schemars` in its `Cargo.toml`) lets [`dto`]
//!    collapse to just [`dto::SymbolKeyDto`]. `Deserialize` is *not* available
//!    even in principle: `SigToken::Kw(&'static str)` cannot be deserialised,
//!    so results are a serialise-only projection by necessity.
//!
//! Neither gap changes any tool's observable behaviour; both are pure
//! plumbing.
//!
//! # Not assumed
//!
//! §L6's table names `PackageIndexes::usages` and `Corpus::packages` as the
//! bottom of `find_usages` and `list_packages`. `EngineHandle` exposes neither
//! (`corpus()` is `pub(crate)`), and taking a direct `nudox-store` handle would
//! be the "second engine" LR-8 forbids. Both tools therefore go through
//! [`nudox_engine::EngineHandle::query`] — the same `CorpusAdapter` the GUI
//! queries, on the engine's own `LocalSet` (LR-9). No new engine method was
//! assumed for either.

#![warn(missing_docs)]

pub mod dto;
pub mod endpoint;
pub mod error;
pub mod server;
pub mod session;
pub mod tools;

pub use endpoint::{LOOPBACK_BIND, MCP_PATH, McpEndpoint};
pub use error::McpError;
pub use server::{NudoxMcpServer, PACKAGE_URI_PREFIX, SCHEMA_URI};
pub use session::{Session, SessionToken, Unauthenticated};
pub use tools::NudoxTools;

/// The Trustfall schema, served to agents verbatim (LR-7).
///
/// # Why an `include_str!` and not a call into `nudox-graph`
///
/// `nudox_graph::schema()` returns a parsed `&'static trustfall::Schema`, and
/// `trustfall::Schema` has no way to render itself back to SDL — there is no
/// `Display`, no `as_str`, and no retained source text. An agent needs the SDL
/// *text* to write a query against, so the only way to serve it is to include
/// the same file `nudox-graph` compiles in.
///
/// This is one schema in two `include_str!`s, not two schemas: both point at
/// `crates/nudox-graph/schema.graphql`, and `tests/schema.rs` asserts this
/// constant is byte-identical to that file and parses to a valid `Schema`. The
/// tidier fix is a `pub const SCHEMA_SDL` in `nudox-graph` for both crates to
/// share; that is a two-line change in a crate outside this task's scope.
pub const SCHEMA_SDL: &str = include_str!("../../nudox-graph/schema.graphql");
