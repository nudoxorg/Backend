//! `nudox-producer-java` — Java oracle producer for `nudox-ir`.
//!
//! # Crate layout
//!
//! - [`schema`]   — Typed serde mirror of the oracle's JSON output (format version 1).
//! - [`javadoc`]  — Pure javadoc comment parser (traditional HTML + JEP 467 Markdown).
//! - [`lower`]    — One-pass lowering from oracle schema into `nudox-ir` IR.
//! - [`producer`] — `impl Producer for JavaProducer`: spawns `javadoc` with the
//!   vendored `oracle/Extractor.java` doclet (compiled by `build.rs`) and lowers
//!   its JSON output. See that module's doc comment for the full invocation
//!   contract, where the compiled doclet classes live, and the honest
//!   multi-version-support story (`--release`, JEP 467 Markdown javadoc).
//!
//! There is no crate-local error type: [`producer::JavaProducer`] returns
//! [`nudox_producer::ProducerError`] directly, the same type every other
//! `Producer` implementation in this workspace returns.
//!
//! # Usage sketch
//!
//! ```ignore
//! use nudox_producer_java::lower::{JavaId, LoweringCtx, lower_extraction};
//! use nudox_ir::build::*;
//! use nudox_ir::id::PackageId;
//!
//! let extraction: nudox_producer_java::schema::Extraction = serde_json::from_str(json)?;
//! let mut low: Lowering<JavaId> = Lowering::new(PackageId::path("my.package"), root_sym);
//! let mut ctx = LoweringCtx::new(&mut low, &extraction.types);
//! lower_extraction(&mut ctx, &extraction);
//! let pkg = low.finish()?;
//! ```
//!
//! Or, end to end, via the `Producer` trait:
//!
//! ```ignore
//! use nudox_producer::{PackageSource, produce};
//! use nudox_producer_java::JavaProducer;
//!
//! let src = PackageSource::new("/path/to/java/pkg", "my-lib", "1.0.0");
//! let produced = produce(&JavaProducer::new(), &src, &lineage, &nudox_ir::foreign::Unlinked)?;
//! let table = produced.table;
//! ```

pub mod javadoc;
pub mod lower;
pub mod producer;
pub mod schema;

// Re-export at the crate root, matching `nudox_producer_typescript`'s and
// `nudox_producer_rust`'s convention — callers (e.g. `ProducerRegistry`)
// name `nudox_producer_java::JavaProducer`, not the submodule path.
pub use producer::JavaProducer;
