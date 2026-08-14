//! `nudox-languages` — C# / Roslyn producer for the nudox-ir.
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
//! - [`error`]   — [`Error`], the error type for this crate's own entry points.
//! - [`producer`] — [`CSharpProducer`], the [`crate::Producer`]
//!   implementation: locate the oracle, run it, lower its output.
//!
//! Two entry points exist and they are not redundant.
//! [`CSharpProducer`] is the one the registry drives, and it runs the oracle
//! itself. [`parse_extraction`] and [`lower`] take oracle JSON that already
//! exists, so a test can exercise the lowering without a .NET toolchain; they
//! return this crate's own [`Error`] rather than
//! [`crate::ProducerError`].
//!
//! # The oracle
//!
//! The oracle is a C# program under `oracle/` in this crate, built with the
//! .NET SDK and Roslyn. It is not committed as a binary; publish it with:
//!
//! ```text
//! cd workspace/compiler/languages/oracle/csharp
//! dotnet publish -c Release --no-self-contained -o publish
//! ```
//!
//! ## Command contract
//!
//! ```text
//! dotnet <publish>/oracle.dll --mode source --root <dir> [--root <dir>...]
//! ```
//!
//! The document is written to **stdout**; every diagnostic goes to stderr; a
//! non-zero exit means no document was produced. `--out FILE` selects a file
//! sink instead, for debugging.
//!
//! ### Why stdout, and not the `--out` file this once documented
//!
//! [`crate::oracle::run_json`] is the shared subprocess helper the
//! [`Producer`] docs point Go, Java and C# at, and it reads the child's stdout.
//! Honouring an `--out`-only contract would have meant hand-rolling
//! `std::process::Command` here and diverging from the other two subprocess
//! producers for no gain. `--out` is kept as an option, not as the contract.
//!
//! This module previously also documented `DOTNET_CLI_HOME`, `DOTNET_NOLOGO`,
//! `DOTNET_CLI_TELEMETRY_OPTOUT` and `DOTNET_SKIP_FIRST_TIME_EXPERIENCE` as
//! required. They are not: `dotnet <app>.dll` is the app host, not the SDK CLI,
//! and it prints no first-run banner and writes nothing to `HOME`. Verified by
//! running the published oracle with an empty `HOME` and none of the four set —
//! stdout began with `{"` and parsed clean. Setting them is still reasonable
//! hardening in a sandbox; it is not a precondition.
//!
//! [`Producer`]: crate::Producer
//!
//! # Id choice
//!
//! `Self::Id = String` where the string is the Roslyn
//! **DocumentationCommentId** (e.g. `T:System.Collections.Generic.List\`1`,
//! `M:Foo.Bar.Method(System.Int32)`). This is already unique, stable, and
//! emitted by the oracle on every symbol — it is the natural join key for `<see
//! cref=…>` resolution via `Lowering::refer`.

pub mod schema;
pub mod xmldoc;
pub mod types;
pub mod lower;
pub mod producer;
pub mod error;

pub use error::Error;
pub use producer::CSharpProducer;

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
pub fn parse_extraction(json: &[u8]) -> Result<Extraction, Error> {
    let extraction: Extraction = serde_json::from_slice(json)?;
    if extraction.format != 1 {
        return Err(Error::UnsupportedFormat {
            format: extraction.format,
        });
    }
    if extraction.types.is_empty() {
        return Err(Error::NoTypes);
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
) -> Result<nudox_ir::package::IrPackage<String>, Error> {
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
        .map_err(|e| Error::oracle(format!("{e}")))
}
