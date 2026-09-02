//! In-process TypeScript semantic frontend for the canonical compiler.
//!
//! The crate parses one caller-supplied TypeScript or TSX source text per OXC
//! arena and produces typed fact records: declaration facts carrying a closed
//! nine-kind vocabulary and byte-unit source spans. Facts are standalone owned
//! records shaped for canonical-data admission; they never reference the parse
//! arena, so every arena is dropped before the facts escape the extraction call.
//!
//! Coordinate law: OXC spans are byte units, and every fact span this crate
//! emits is a byte-unit [`facts::SourceSpan`] validated against the exact
//! caller source. UTF-16 code-unit coordinates belong to a different authority
//! and are converted at the driver's typed coordinate boundary, never here.
//!
//! Boundary law: the crate owns no compiler-driver dependency, resolver graph,
//! process, or network access. Import target resolution remains open scope.
#![forbid(unsafe_code)]
pub mod facts;

mod extract;

pub use extract::extract_module;
