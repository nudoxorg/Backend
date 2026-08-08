//! Every `ecosystem = "nuget"` package in `corpus/manifest.toml` lowers end to
//! end (oracle extraction *and* `Lowering::finish`) against its real GitHub
//! source checkout — not just the hand-written `oracle_end_to_end.rs` fixture.
//!
//! # Why this file is `#[ignore]`d
//!
//! Same reason as `real_library.rs`: it needs third-party checkouts under
//! `.real-csharp/`, which is gitignored exactly like `.real-crates/` is for
//! Rust (see AGENTS-DOCTRINE.md §8), and a published oracle. Provision both,
//! then:
//!
//! ```text
//! cargo test -p nudox-producer-csharp --test nuget_corpus -- --ignored --nocapture
//! ```
//!
//! `corpus/manifest.toml`'s NuGet packages ship compiled `.nupkg` assemblies,
//! not source (see `corpus/README.md`'s per-ecosystem table) — this crate's
//! source-tier oracle cannot lower a `.nupkg`. `.real-csharp/<Name>-<Version>/`
//! holds the GitHub tag checkout for each one instead, obtained the way
//! `real_library.rs` obtains Polly's.

use std::path::{Path, PathBuf};

use nudox_ir::{
    entry::Symbol,
    lower::Lowering,
    package::PackageId,
};
use nudox_producer::{PackageSource, Producer, ProducerError};
use nudox_producer_csharp::CSharpProducer;

fn root(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .join(rel)
}

/// `(manifest name, version, source root relative to the repo root)` for
/// every package/version pair under `ecosystem = "nuget"` in
/// `corpus/manifest.toml`, mapped to the sub-project inside its checkout that
/// actually builds the named NuGet package (several of these repos are
/// multi-project monorepos, e.g. Polly ships `Polly`, `Polly.Core`,
/// `Polly.Extensions`, … as separate packages from one checkout).
///
/// `xunit` is a NuGet meta-package with no assembly of its own — its nuspec
/// (`.real-crates/xunit-2.7.0/xunit.nuspec`) declares only dependencies on
/// `xunit.core`/`xunit.assert`/`xunit.analyzers`, confirmed by the `.nupkg`
/// containing no `lib/`. `xunit.core` is used here as the representative real
/// source from the same checkout/tag; there is no source tree that is "the
/// xunit package" to point at instead.
const CASES: &[(&str, &str, &str)] = &[
    ("Newtonsoft.Json", "13.0.3", ".real-csharp/Newtonsoft.Json-13.0.3/Src/Newtonsoft.Json"),
    ("Newtonsoft.Json", "12.0.3", ".real-csharp/Newtonsoft.Json-12.0.3/Src/Newtonsoft.Json"),
    ("Serilog", "3.1.1", ".real-csharp/Serilog-3.1.1/src/Serilog"),
    ("Serilog", "2.12.0", ".real-csharp/Serilog-2.12.0/src/Serilog"),
    ("AutoMapper", "13.0.1", ".real-csharp/AutoMapper-13.0.1/src/AutoMapper"),
    ("FluentValidation", "11.9.0", ".real-csharp/FluentValidation-11.9.0/src/FluentValidation"),
    ("Polly", "8.3.0", ".real-csharp/Polly-8.3.0/src/Polly"),
    ("Dapper", "2.1.35", ".real-csharp/Dapper-2.1.35/Dapper"),
    ("MediatR", "12.2.0", ".real-csharp/MediatR-12.2.0/src/MediatR"),
    ("Moq", "4.20.70", ".real-csharp/Moq-4.20.70/src/Moq"),
    ("xunit", "2.7.0", ".real-csharp/xunit-2.7.0/src/xunit.core"),
    ("NUnit", "4.1.0", ".real-csharp/NUnit-4.1.0/src/NUnitFramework/framework"),
    ("StackExchange.Redis", "2.7.33", ".real-csharp/StackExchange.Redis-2.7.33/src/StackExchange.Redis"),
    (
        "Microsoft.Extensions.DependencyInjection.Abstractions",
        "8.0.1",
        ".real-csharp/Microsoft.Extensions.DependencyInjection.Abstractions-8.0.1/src/libraries/Microsoft.Extensions.DependencyInjection.Abstractions/src",
    ),
    ("Humanizer.Core", "2.14.1", ".real-csharp/Humanizer.Core-2.14.1/src/Humanizer"),
    ("CsvHelper", "30.0.1", ".real-csharp/CsvHelper-30.0.1/src/CsvHelper"),
    ("RestSharp", "111.2.0", ".real-csharp/RestSharp-111.2.0/src/RestSharp"),
    ("NLog", "5.2.8", ".real-csharp/NLog-5.2.8/src/NLog"),
    ("Refit", "7.0.0", ".real-csharp/Refit-7.0.0/Refit"),
    (
        "Microsoft.Bcl.AsyncInterfaces",
        "8.0.0",
        ".real-csharp/Microsoft.Bcl.AsyncInterfaces-8.0.0/src/libraries/Microsoft.Bcl.AsyncInterfaces/src",
    ),
    ("protobuf-net", "3.2.30", ".real-csharp/protobuf-net-3.2.30/src/protobuf-net.Core"),
];

fn error_chain(err: &dyn std::error::Error) -> String {
    let mut parts = vec![err.to_string()];
    let mut current = err.source();
    while let Some(source) = current {
        parts.push(source.to_string());
        current = source.source();
    }
    parts.join(" <- ")
}

/// Run the oracle and the full `Lowering` pipeline over `source`, measured.
fn lower(
    producer: &CSharpProducer,
    source: &PackageSource,
) -> Result<(usize, usize), ProducerError> {
    let case = format!("nuget-corpus/{}-{}", source.name.as_str(), source.version);
    let (result, _cost) = nudox_test_support::measured(&case, source.root(), || {
        let extraction = producer.invoke(source)?;

        let root_sym = Symbol {
            name: source.name.as_str().to_owned(),
            visibility: nudox_ir::entry::Visibility::Public,
            documentation: String::new(),
            source: source.root.clone(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        let mut sink: Lowering<String> = Lowering::new(PackageId::path(source.root()), root_sym);
        producer.lower(&extraction, &mut sink)?;
        let package = sink.finish().map_err(|e| ProducerError::LoweringFailed {
            package: source.name.as_str().to_owned(),
            source: Box::new(e) as Box<dyn std::error::Error + Send + Sync>,
        })?;

        Ok::<_, ProducerError>((extraction.types.len(), package.iter().count()))
    });
    result
}

/// Every nuget-corpus package must lower with a real, structurally sound IR
/// package: not `is_ok()`, but a non-trivial entry count that could not come
/// from an oracle that silently bound nothing.
#[test]
#[ignore = "needs third-party C# checkouts under .real-csharp/ and a published oracle; see module docs"]
fn all_nuget_corpus_packages_lower_end_to_end() {
    let producer = CSharpProducer::from_env();
    assert!(
        producer.is_available(),
        "publish the oracle first: cd workspace/compiler/languages/csharp/oracle && \
         dotnet publish -c Release --no-self-contained -o publish"
    );

    let mut failures = Vec::new();

    for (name, version, rel_root) in CASES {
        let src_root = root(rel_root);
        if !src_root.is_dir() {
            failures.push(format!("{name} {version}: no checkout at {}", src_root.display()));
            continue;
        }

        let source = PackageSource::new(src_root, *name, *version);
        match lower(&producer, &source) {
            Ok((type_count, entry_count)) => {
                assert!(
                    type_count > 0,
                    "{name} {version}: oracle bound zero types from a real checkout"
                );
                assert!(
                    entry_count > type_count,
                    "{name} {version}: {entry_count} IR entries from {type_count} types is too \
                     few to be real member-level lowering, not just type shells"
                );
                eprintln!("OK  {name} {version}: {type_count} types -> {entry_count} IR entries");
            }
            Err(err) => failures.push(format!("{name} {version}: {}", error_chain(&err))),
        }
    }

    assert!(
        failures.is_empty(),
        "{} nuget-corpus package(s) failed to lower:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// `System.Text.Json` 8.0.2 is a known, root-caused exception to the test
/// above, pinned here rather than silently dropped from the corpus.
///
/// dotnet/runtime ships per-TFM source-file *variants* selected by MSBuild
/// `<Compile Include>` conditions in the `.csproj` — e.g.
/// `JsonReaderHelper.netstandard.cs` and `JsonReaderHelper.net8.cs` both
/// declare `JsonReaderHelper.IndexOfQuoteOrAnyControlOrBackSlash` with the
/// same signature on the same `partial class`, and only one is ever compiled
/// into a given TFM's assembly. This crate's oracle is deliberately
/// MSBuild-free (`SourceLoader`'s doc comment: "no MSBuild, no NuGet restore,
/// no project system") and walks every `.cs` file under `--root`
/// unconditionally, so it feeds both variants into one Roslyn compilation and
/// gets two colliding method symbols — a real duplicate-definition, not a
/// producer bug in the sense `Lowering::finish` normally means it (nothing
/// about *this* package's declarations is wrong; the file *selection* is
/// TFM-blind). No other package in the nuget corpus uses this per-TFM
/// filename convention.
///
/// If a future `SourceLoader` becomes TFM-aware, this test starts failing
/// (the panic message changes shape or lowering starts succeeding) — that is
/// the signal to move `System.Text.Json` into `CASES` above.
#[test]
#[ignore = "needs the System.Text.Json checkout under .real-csharp/ and a published oracle; see module docs"]
fn system_text_json_hits_the_known_per_tfm_file_duplication_limitation() {
    let producer = CSharpProducer::from_env();
    assert!(producer.is_available(), "publish the oracle first; see module docs");

    let src_root = root(".real-csharp/System.Text.Json-8.0.2/src/libraries/System.Text.Json/src");
    assert!(src_root.is_dir(), "no checkout at {}", src_root.display());

    let source = PackageSource::new(src_root, "System.Text.Json", "8.0.2");
    let err = lower(&producer, &source).expect_err(
        "System.Text.Json 8.0.2 was expected to still hit the per-TFM duplicate-file \
         limitation; if it now lowers cleanly, move it into CASES in this file instead \
         of leaving this test to rot",
    );

    let chain = error_chain(&err);
    assert!(
        chain.contains("IndexOfQuoteOrAnyControlOrBackSlash"),
        "expected the known JsonReaderHelper.netstandard.cs / .net8.cs duplicate, got: {chain}"
    );
}
