//! Go producer for the nudox IR.
//!
//! This crate lowers a Go module's type information (extracted by the vendored
//! Go oracle at `workspace/compiler/compile/go/oracle/`) into the new
//! `nudox-ir` package format via a flat, one-pass [`Lowering`] sink.
//!
//! ## Crate layout
//!
//! | Module      | Responsibility |
//! |-------------|----------------|
//! | [`oracle`]  | Serde mirrors of the oracle JSON schema (pure deserialization, no IR). |
//! | [`types`]   | Go type → `nudox_ir::kinds::Type` translation. |
//! | [`lower`]   | The flat one-pass lowering: `GoId`, `lower_output`. |
//! | [`error`]   | `GoError` and `Result<T>`. |
//! | [`producer`]| `GoProducer` struct (oracle invocation + lowering pipeline). |

pub mod error;
pub mod lower;
pub mod oracle;
pub mod producer;
pub mod types;

#[cfg(test)]
mod tests {
    mod lowering;
}
