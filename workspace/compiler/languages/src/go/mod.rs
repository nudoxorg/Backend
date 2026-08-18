//! Go producer for the nudox IR.
//!
//! This crate lowers a Go module's type information (extracted by the vendored
//! Go oracle at `workspace/compiler/languages/oracle/go/`) into the new
//! `nudox-ir` package format via a flat, one-pass [`Lowering`] sink.
//!
//! `GoProducer` implements [`crate::Producer`]: `invoke` runs the
//! compiled `nudox-go-oracle` binary as a subprocess (via
//! `crate::oracle::run_json`) and `lower` feeds its JSON output
//! through [`lower::lower_into`]. See `producer` module docs for the oracle
//! binary location contract.
//!
//! ## Crate layout
//!
//! | Module      | Responsibility |
//! |-------------|----------------|
//! | [`oracle`]  | Serde mirrors of the oracle JSON schema (pure deserialization, no IR). |
//! | [`types`]   | Go type → `nudox_ir::kinds::Type` translation. |
//! | [`lower`]   | The flat one-pass lowering: `GoId`, `lower_into`, `lower_output`. |
//! | [`producer`]| `GoProducer` struct: oracle invocation + lowering pipeline, and the `Producer` impl. |
//! | [`error`]   | `Error` and `Result<T>`. |

pub mod error;
pub mod lower;
pub mod oracle;
pub mod producer;
pub mod types;

pub use producer::GoProducer;

#[cfg(test)]
mod tests {
    mod lowering;
}
