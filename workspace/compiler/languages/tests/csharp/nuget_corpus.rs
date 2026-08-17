//! Every `ecosystem = "nuget"` package in `nix/corpus.nix` lowers end to
//! end (oracle extraction *and* `Lowering::finish`) against its real GitHub
//! source checkout — not just the hand-written `oracle_end_to_end.rs` fixture.
//!
//! # Why this file is `#[ignore]`d
//!
//! Same reason as `real_library.rs`: it needs third-party checkouts under
//! `result/`, which is gitignored exactly like `result/` is for
//! Rust (see docs/AGENTS-DOCTRINE.md §8), and a published oracle. Provision both,
//! then:
//!
//! ```text
//! cargo test -p nudox-languages --test csharp_nuget_corpus -- --ignored --nocapture
//! ```
//!
//! `nix/corpus.nix`'s NuGet packages ship compiled `.nupkg` assemblies,
//! not source (see `docs/CORPUS.md`'s per-ecosystem table) — this crate's
//! source-tier oracle cannot lower a `.nupkg`. `result/<Name>-<Version>/`
//! holds the GitHub tag checkout for each one instead, obtained the way
//! `real_library.rs` obtains Polly's.
//!
//! # Why every entry carries `expect_symbol` and `min_entries`
//!
//! Per docs/AGENTS-DOCTRINE.md §4 ("a test that would pass against a stub is not a
//! test"), this file used to assert only `type_count > 0` and
//! `entry_count > type_count` for every package — a magnitude/ratio check that
//! a stub inflating both counts proportionally would pass for 21 of 22
//! packages. Every [`Entry`] below now follows the pattern
//! `nudox-languages/tests/clang/corpus_sweep.rs` already uses: a real,
//! hand-verified `expect_symbol` (see each entry's comment for the `grep`
//! evidence against that package's own `result/` checkout) paired with a
//! `min_entries` floor set below an actually-measured run (recorded per entry;
//! re-measure with `--nocapture` and look for the `OK` line if a package's
//! oracle or lowering changes and this floor needs updating).

use std::path::{Path, PathBuf};

use nudox_ir::{
    entry::Symbol,
    lower::Lowering,
    package::PackageId,
};
use nudox_languages::{PackageSource, Producer, ProducerError};
use nudox_languages::csharp::CSharpProducer;

fn root(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(rel)
}

/// One nuget-corpus package: where its real source checkout lives, a real
/// symbol that must survive lowering (the doctrine §4 content assertion, not
/// a bare non-zero count), and an entry-count floor measured on a real run.
struct Entry {
    name: &'static str,
    version: &'static str,
    /// Source root relative to the repo root — the sub-project inside the
    /// checkout that actually builds the named NuGet package (several of
    /// these repos are multi-project monorepos, e.g. Polly ships `Polly`,
    /// `Polly.Core`, `Polly.Extensions`, … as separate packages from one
    /// checkout).
    root: &'static str,
    expect_symbol: &'static str,
    min_entries: usize,
}

/// `xunit` is a NuGet meta-package with no assembly of its own — its nuspec
/// (`result/xunit-2.7.0/xunit.nuspec`) declares only dependencies on
/// `xunit.core`/`xunit.assert`/`xunit.analyzers`, confirmed by the `.nupkg`
/// containing no `lib/`. `xunit.core` is used here as the representative real
/// source from the same checkout/tag; there is no source tree that is "the
/// xunit package" to point at instead.
const ENTRIES: &[Entry] = &[
    Entry {
        name: "Newtonsoft.Json",
        version: "13.0.3",
        root: "result/Newtonsoft.Json-13.0.3/Src/Newtonsoft.Json",
        // `public static class JsonConvert` — JsonConvert.cs:53.
        expect_symbol: "JsonConvert",
        // Measured 2026-08-08: 267 types -> 7763 entries.
        min_entries: 6000,
    },
    Entry {
        name: "Newtonsoft.Json",
        version: "12.0.3",
        root: "result/Newtonsoft.Json-12.0.3/Src/Newtonsoft.Json",
        expect_symbol: "JsonConvert",
        // Measured 2026-08-08: 264 types -> 7632 entries.
        min_entries: 6000,
    },
    Entry {
        name: "Serilog",
        version: "3.1.1",
        root: "result/Serilog-3.1.1/src/Serilog",
        // `public class LoggerConfiguration` — LoggerConfiguration.cs:20.
        expect_symbol: "LoggerConfiguration",
        // Measured 2026-08-08: 102 types -> 2730 entries.
        min_entries: 2000,
    },
    Entry {
        name: "Serilog",
        version: "2.12.0",
        root: "result/Serilog-2.12.0/src/Serilog",
        expect_symbol: "LoggerConfiguration",
        // Measured 2026-08-08: 103 types -> 2778 entries.
        min_entries: 2000,
    },
    Entry {
        name: "AutoMapper",
        version: "13.0.1",
        root: "result/AutoMapper-13.0.1/src/AutoMapper",
        // `public sealed class MapperConfiguration` —
        // Configuration/MapperConfiguration.cs:37.
        expect_symbol: "MapperConfiguration",
        // Measured 2026-08-08: 183 types -> 4057 entries.
        min_entries: 3000,
    },
    Entry {
        name: "FluentValidation",
        version: "11.9.0",
        root: "result/FluentValidation-11.9.0/src/FluentValidation",
        // `public abstract class AbstractValidator<T>` — AbstractValidator.cs:36.
        expect_symbol: "AbstractValidator",
        // Measured 2026-08-08: 190 types -> 2694 entries.
        min_entries: 2000,
    },
    Entry {
        name: "Polly",
        version: "8.3.0",
        root: "result/Polly-8.3.0/src/Polly",
        // `public abstract partial class Policy : PolicyBase` — Policy.cs:7.
        expect_symbol: "Policy",
        // Measured 2026-08-08: 175 types -> 5290 entries.
        min_entries: 4000,
    },
    Entry {
        name: "Dapper",
        version: "2.1.35",
        root: "result/Dapper-2.1.35/Dapper",
        // `public static partial class SqlMapper` — SqlMapper.IDynamicParameters.cs:5.
        expect_symbol: "SqlMapper",
        // Measured 2026-08-08: 63 types -> 2776 entries.
        min_entries: 2000,
    },
    Entry {
        name: "MediatR",
        version: "12.2.0",
        root: "result/MediatR-12.2.0/src/MediatR",
        // `public class Mediator : IMediator` — Mediator.cs:16.
        expect_symbol: "Mediator",
        // Measured 2026-08-08: 42 types -> 506 entries.
        min_entries: 400,
    },
    Entry {
        name: "Moq",
        version: "4.20.70",
        root: "result/Moq-4.20.70/src/Moq",
        // `public abstract partial class Mock : IFluentInterface` — Mock.cs:22.
        expect_symbol: "Mock",
        // Measured 2026-08-08: 177 types -> 4290 entries.
        min_entries: 3000,
    },
    Entry {
        name: "xunit",
        version: "2.7.0",
        root: "result/xunit-2.7.0/src/xunit.core",
        // `public class FactAttribute : Attribute` — FactAttribute.cs:13.
        expect_symbol: "FactAttribute",
        // Measured 2026-08-08: 57 types -> 364 entries.
        min_entries: 250,
    },
    Entry {
        name: "NUnit",
        version: "4.1.0",
        root: "result/NUnit-4.1.0/src/NUnitFramework/framework",
        // `public class TestAttribute : ...` — Attributes/TestAttribute.cs:29.
        expect_symbol: "TestAttribute",
        // Measured 2026-08-08: 574 types -> 8505 entries.
        min_entries: 6000,
    },
    Entry {
        name: "StackExchange.Redis",
        version: "2.7.33",
        root: "result/StackExchange.Redis-2.7.33/src/StackExchange.Redis",
        // `public partial class ConnectionMultiplexer` — ConnectionMultiplexer.Debug.cs:5.
        expect_symbol: "ConnectionMultiplexer",
        // Measured 2026-08-08: 373 types -> 18326 entries.
        min_entries: 14000,
    },
    Entry {
        name: "Microsoft.Extensions.DependencyInjection.Abstractions",
        version: "8.0.1",
        root: "result/Microsoft.Extensions.DependencyInjection.Abstractions-8.0.1/src/libraries/Microsoft.Extensions.DependencyInjection.Abstractions/src",
        // `public class ServiceDescriptor` — ServiceDescriptor.cs:14.
        expect_symbol: "ServiceDescriptor",
        // Measured 2026-08-08: 29 types -> 935 entries.
        min_entries: 700,
    },
    Entry {
        name: "Humanizer.Core",
        version: "2.14.1",
        root: "result/Humanizer.Core-2.14.1/src/Humanizer",
        // `public static class InflectorExtensions` — InflectorExtensions.cs:32.
        expect_symbol: "InflectorExtensions",
        // Measured 2026-08-08: 250 types -> 5240 entries.
        min_entries: 4000,
    },
    Entry {
        name: "CsvHelper",
        version: "30.0.1",
        root: "result/CsvHelper-30.0.1/src/CsvHelper",
        // `public class CsvReader : IReader` — CsvReader.cs:23.
        expect_symbol: "CsvReader",
        // Measured 2026-08-08: 227 types -> 3137 entries.
        min_entries: 2000,
    },
    Entry {
        name: "RestSharp",
        version: "111.2.0",
        root: "result/RestSharp-111.2.0/src/RestSharp",
        // `public partial class RestClient : IRestClient` — RestClient.cs:37.
        expect_symbol: "RestClient",
        // Measured 2026-08-08: 110 types -> 2195 entries.
        min_entries: 1500,
    },
    Entry {
        name: "NLog",
        version: "5.2.8",
        root: "result/NLog-5.2.8/src/NLog",
        // `public static class LogManager` — LogManager.cs:54.
        expect_symbol: "LogManager",
        // Measured 2026-08-08: 625 types -> 15514 entries.
        min_entries: 12000,
    },
    Entry {
        name: "Refit",
        version: "7.0.0",
        root: "result/Refit-7.0.0/Refit",
        // `public static class RestService` — RestService.cs:7.
        expect_symbol: "RestService",
        // Measured 2026-08-08: 71 types -> 909 entries.
        min_entries: 600,
    },
    Entry {
        name: "Microsoft.Bcl.AsyncInterfaces",
        version: "8.0.0",
        root: "result/Microsoft.Bcl.AsyncInterfaces-8.0.0/src/libraries/Microsoft.Bcl.AsyncInterfaces/src",
        // `public struct AsyncIteratorMethodBuilder` —
        // System/Runtime/CompilerServices/AsyncIteratorMethodBuilder.cs:18.
        expect_symbol: "AsyncIteratorMethodBuilder",
        // Measured 2026-08-08: 3 types -> 51 entries. Small package; a real
        // floor is still worth more here than a raw non-zero check because 51
        // is small enough that a badly-broken producer could plausibly hit it
        // by accident.
        min_entries: 30,
    },
    Entry {
        name: "protobuf-net",
        version: "3.2.30",
        root: "result/protobuf-net-3.2.30/src/protobuf-net.Core",
        // `public sealed class ProtoContractAttribute : Attribute` —
        // ProtoContractAttribute.cs:12.
        expect_symbol: "ProtoContractAttribute",
        // Measured 2026-08-08: 174 types -> 6126 entries.
        min_entries: 4500,
    },
    // `System.Text.Json` 8.0.2 used to be pinned separately as a known
    // failure: dotnet/runtime ships per-TFM source-file *variants* selected by
    // MSBuild `<Compile Include>` conditions — e.g.
    // `JsonReaderHelper.netstandard.cs` and `JsonReaderHelper.net8.cs` both
    // declared `JsonReaderHelper.IndexOfQuoteOrAnyControlOrBackSlash` with the
    // same signature on the same `partial class`, and this crate's
    // MSBuild-free oracle walked every `.cs` file unconditionally, feeding
    // both into one compilation as a hard duplicate-definition. The oracle's
    // `SourceLoader` (`oracle/SourceLoader.cs`) is now TFM-aware: it groups
    // same-directory, same-base per-TFM variants and keeps only the
    // highest-ranked TFM tag (see `SourceLoader.ApplyTfmPreference`'s doc
    // comment), exactly the condition the old pinned test's own comment named
    // as the signal to move this package here instead of leaving that test to
    // rot. Re-verified 2026-08-08: the duplicate is gone (no
    // `IndexOfQuoteOrAnyControlOrBackSlash` diagnostic) and lowering succeeds
    // cleanly.
    Entry {
        name: "System.Text.Json",
        version: "8.0.2",
        root: "result/System.Text.Json-8.0.2/src/libraries/System.Text.Json/src",
        // `public sealed class JsonDocument` — Document/JsonDocument.cs.
        expect_symbol: "JsonDocument",
        // Measured 2026-08-08: 253 types -> 9045 entries.
        min_entries: 7000,
    },
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
///
/// Returns the type count alongside every lowered entry's symbol name, so a
/// caller can assert both the entry-count floor and the real-symbol content
/// check without lowering twice.
fn lower(
    producer: &CSharpProducer,
    source: &PackageSource,
) -> Result<(usize, Vec<String>), ProducerError> {
    let case = format!("nuget-corpus/{}-{}", source.name.as_str(), source.version);
    let (result, _cost) = heart::cost::measured(&case, source.root(), || {
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

        let names: Vec<String> = package
            .iter()
            .map(|(_, e)| e.sym().name.clone())
            .collect();

        Ok::<_, ProducerError>((extraction.types.len(), names))
    });
    result
}

/// Every nuget-corpus package must lower with a real, structurally sound IR
/// package: a named symbol that really exists in that package's own source
/// (not a bare non-zero count), above an entry-count floor measured on a real
/// run. Per docs/AGENTS-DOCTRINE.md §4, a count-only assertion is not a test — a
/// stub that inflated both `type_count` and `entry_count` proportionally
/// would have passed the old version of this test for 21 of 22 packages.
#[test]
#[ignore = "needs third-party C# checkouts under result/ and a published oracle; see module docs"]
fn all_nuget_corpus_packages_lower_end_to_end() {
    let producer = CSharpProducer::from_env();
    assert!(
        producer.is_available(),
        "publish the oracle first: cd workspace/compiler/languages/csharp/oracle && \
         dotnet publish -c Release --no-self-contained -o publish"
    );

    let mut failures = Vec::new();
    let mut successes = Vec::new();

    for entry in ENTRIES {
        let src_root = root(entry.root);
        if !src_root.is_dir() {
            failures.push(format!(
                "{} {}: no checkout at {}",
                entry.name,
                entry.version,
                src_root.display()
            ));
            continue;
        }

        let source = PackageSource::new(src_root, entry.name, entry.version);
        match lower(&producer, &source) {
            Ok((type_count, names)) => {
                let entry_count = names.len();
                eprintln!(
                    "OK  {} {}: {type_count} types -> {entry_count} IR entries",
                    entry.name, entry.version
                );

                if entry_count < entry.min_entries {
                    failures.push(format!(
                        "{} {}: {entry_count} IR entries from {type_count} types is below the \
                         measured floor of {} — this is not real member-level lowering",
                        entry.name, entry.version, entry.min_entries
                    ));
                    continue;
                }

                if !names.iter().any(|n| n == entry.expect_symbol) {
                    failures.push(format!(
                        "{} {}: expected symbol {:?} not found among {entry_count} lowered \
                         entries — a count alone is not a test (docs/AGENTS-DOCTRINE.md §4)",
                        entry.name, entry.version, entry.expect_symbol
                    ));
                    continue;
                }

                successes.push((entry.name, entry.version, entry_count));
            }
            Err(err) => failures.push(format!(
                "{} {}: {}",
                entry.name,
                entry.version,
                error_chain(&err)
            )),
        }
    }

    eprintln!(
        "\n=== nuget corpus sweep summary: {}/{} succeeded ===",
        successes.len(),
        ENTRIES.len()
    );
    for (name, version, entry_count) in &successes {
        eprintln!("  OK   {name} {version}: {entry_count} entries");
    }

    assert!(
        failures.is_empty(),
        "{} nuget-corpus package(s) failed to lower:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
