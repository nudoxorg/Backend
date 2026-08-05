//! TypeScript producer for the nudox-ir pipeline (OXC tier).
//!
//! Implements [`nudox_producer::Producer`] using OXC as the in-process parse
//! and semantic engine. The tsz oracle tier is explicitly out of scope — a seam
//! for it is provided in [`producer`] via [`TsOracle`].
//!
//! # Architecture
//!
//! ```text
//! invoke():
//!   entry::discover_entry_points()  → Vec<PathBuf>
//!   graph::build_and_extract()      → Vec<ModuleFacts>    (arena dropped per module)
//!   returns OwnedOracle { modules }
//!
//! lower():
//!   emit::lower_package(&oracle.modules, out) → one-pass into Lowering<TsId>
//! ```
//!
//! # OXC lifetime problem — how we solved it
//!
//! OXC parses into a per-module `Allocator` arena; the AST is `'src`-bound.
//! `Producer::lower` takes `&Self::Oracle`, so a self-referential Oracle that
//! holds both Allocator and AST cannot be expressed safely.
//!
//! Resolution: **Option C — extract to owned structures in `invoke()`**.
//! `graph::build_and_extract()` follows the pattern from the old `oxc/graph.rs`:
//! it scopes one `Allocator` per module, parses, extracts into owned
//! `ModuleFacts` (no arena references), and drops the allocator. By the time
//! `invoke()` returns, all AST data is owned. `lower()` then works purely on
//! owned `ModuleFacts` — no lifetime puzzle.
//!
//! This costs O(n) allocation for string copies but matches the IR's own
//! ownership model (Symbol fields are owned Strings). No `ouroboros` is needed.
//!
//! The `Producer` trait does NOT need to change.
//!
//! # tsz seam
//!
//! [`TsOracle`] is a sealed trait that the OXC oracle and a future tsz oracle
//! both implement. [`TypescriptProducer`] is generic over `O: TsOracle`. Right
//! now only `OxcOracle` exists; tsz would implement `TsOracle` and be selected
//! by constructing `TypescriptProducer::<TszOracle>::new()`.

pub mod emit;
pub mod entry;
pub mod extract;
pub mod graph;
pub mod id;
pub mod oracle;
pub mod producer;

pub use producer::TypescriptProducer;

// Re-export the oracle type so callers can name it.
pub use producer::OwnedOracle;

// Re-export TszOracle when the tsz feature is active.
#[cfg(feature = "tsz")]
pub use oracle::tsz::TszOracle;
