//! `nudox-producer-java` — Java oracle producer for `nudox-ir`.
//!
//! # Crate layout
//!
//! - [`schema`]   — Typed serde mirror of the oracle's JSON output.
//! - [`javadoc`]  — Pure javadoc comment parser (traditional HTML + JEP 467 Markdown).
//! - [`lower`]    — One-pass lowering from oracle schema into `nudox-ir` IR.
//! - [`error`]    — Producer error type.
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

pub mod error;
pub mod javadoc;
pub mod lower;
pub mod schema;
