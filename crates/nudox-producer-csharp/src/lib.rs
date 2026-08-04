//! `nudox-producer-csharp` — C# / Roslyn producer for the nudox-ir.
//!
//! # Crate structure
//!
//! - [`schema`]  — Serde mirror of the Roslyn oracle's JSON output. Zero IR
//!   calls; pure salvage from `workspace/compiler/compile/csharp/schema.rs`.
//! - [`xmldoc`]  — C# XML documentation-comment parsing. Pure salvage from
//!   `workspace/compiler/compile/csharp/xmldoc.rs`, adapted for the
//!   self-contained `simple_name` helper.
//! - [`types`]   — TypeSig → nudox-ir `Type` mapping; accessibility →
//!   Visibility; attribute rendering; generic params/where-preds.
//! - [`lower`]   — One-pass lowering of a flat `Extraction` into
//!   `Lowering<String>`, where `String` is the Roslyn DocumentationCommentId
//!   (`T:Ns.Type` / `M:…`).
//! - [`error`]   — `ProducerError` type.
//! - [`producer`] — `CSharpProducer` implementing the (not-yet-published)
//!   `nudox-producer` trait contract. Coded against the published contract
//!   signature; compiles standalone without the `nudox-producer` crate.
//!
//! # Oracle command contract
//!
//! ```text
//! dotnet <publish>/oracle.dll \
//!     --mode source \
//!     --root <dir>... \
//!     --out <outfile.json>
//! ```
//!
//! `DOTNET_CLI_HOME`, `DOTNET_NOLOGO`, `DOTNET_CLI_TELEMETRY_OPTOUT`, and
//! `DOTNET_SKIP_FIRST_TIME_EXPERIENCE` must be set in the environment.  The
//! oracle writes its output to `--out` (not stdout); on success exits 0; on
//! failure exits non-zero with diagnostics on stderr.
//!
//! # Id choice
//!
//! `Self::Id = String` where the string is the Roslyn
//! **DocumentationCommentId** (e.g. `T:System.Collections.Generic.List\`1`,
//! `M:Foo.Bar.Method(System.Int32)`). This is already unique, stable, and
//! emitted by the oracle on every symbol — it is the natural join key for `<see
//! cref=…>` resolution via `Lowering::refer`.

pub mod error;
pub mod lower;
pub mod schema;
pub mod types;
pub mod xmldoc;

pub use error::ProducerError;

use nudox_ir::{
    build::{Symbol, Visibility},
    lower::Lowering,
    package::PackageId,
};

use schema::Extraction;

use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Public API: run and lower
// ---------------------------------------------------------------------------

/// Deserialize an oracle JSON extraction from raw bytes.
///
/// Validates the format version (must be 1) and that at least one type was
/// extracted.
pub fn parse_extraction(json: &[u8]) -> Result<Extraction, ProducerError> {
    let extraction: Extraction = serde_json::from_slice(json)?;
    if extraction.format != 1 {
        return Err(ProducerError::UnsupportedFormat {
            format: extraction.format,
        });
    }
    if extraction.types.is_empty() {
        return Err(ProducerError::NoTypes);
    }
    Ok(extraction)
}

/// Lower a parsed [`Extraction`] into an [`nudox_ir::package::IrPackage`] by
/// constructing a [`Lowering`] and delegating to [`lower::lower_extraction`].
///
/// Returns an error if the lowering finish step detects undeclared refs,
/// duplicates, or cycles.  In practice the oracle ensures doc-ids are unique
/// and non-cyclic, but we validate for safety.
pub fn lower(
    extraction: &Extraction,
) -> Result<nudox_ir::package::IrPackage<String>, ProducerError> {
    let assembly_name = if extraction.assembly.name.is_empty() {
        "assembly"
    } else {
        &extraction.assembly.name
    };

    let root_sym = Symbol {
        name: assembly_name.to_string(),
        visibility: Visibility::Public,
        documentation: String::new(),
        source: PathBuf::new(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    };

    let pkg_id = PackageId::path(assembly_name);
    let mut lowering: Lowering<String> = Lowering::new(pkg_id, root_sym);

    lower::lower_extraction(extraction, &mut lowering);

    lowering
        .finish()
        .map_err(|e| ProducerError::oracle(format!("{e}")))
}
