//! Lower a real, third-party C# library end to end.
//!
//! The committed fixture in `oracle_end_to_end.rs` is designed to exercise every
//! construct; this file answers the different question of whether the producer
//! survives a library nobody wrote for it — one with 174 source files, a real
//! MSBuild layout, `ImplicitUsings`, and doc comments written for humans.
//!
//! # Why this test is `#[ignore]`d
//!
//! It needs a third-party checkout that cannot be committed. `.real-csharp/` is
//! covered by the repository's ignore-everything policy, exactly like
//! `.real-crates/` is for Rust. Fetch it with:
//!
//! ```text
//! mkdir -p .real-csharp && cd .real-csharp
//! curl -sSL -o polly.tar.gz \
//!   https://codeload.github.com/App-vNext/Polly/tar.gz/refs/tags/8.5.2
//! tar xzf polly.tar.gz && rm polly.tar.gz
//! ```
//!
//! Then, with the oracle published (see `oracle_end_to_end.rs`):
//!
//! ```text
//! cargo test -p nudox-producer-csharp --test real_library -- --ignored --nocapture
//! ```
//!
//! `NUDOX_CSHARP_PKG_ROOT`, `NUDOX_CSHARP_PKG_NAME` and
//! `NUDOX_CSHARP_PKG_VERSION` override the checkout, following the
//! `NUDOX_PKG_ROOT` convention the Rust real-package tests already use.

use std::path::{Path, PathBuf};

use nudox_ir::{
    entry::{EntryInner, Symbol, Visibility},
    kind::Kind,
    lower::Lowering,
    package::PackageId,
};
use nudox_producer::{PackageSource, Producer};
use nudox_producer_csharp::CSharpProducer;

/// The library this test asserts against when nothing overrides it.
const DEFAULT_NAME: &str = "Polly.Core";
const DEFAULT_VERSION: &str = "8.5.2";

fn package_root() -> PathBuf {
    if let Some(root) = std::env::var_os("NUDOX_CSHARP_PKG_ROOT") {
        return PathBuf::from(root);
    }

    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .join(".real-csharp")
        .join(format!("Polly-{DEFAULT_VERSION}"))
        .join("src")
        .join("Polly.Core")
}

fn package_source() -> PackageSource {
    let name = std::env::var("NUDOX_CSHARP_PKG_NAME").unwrap_or_else(|_| DEFAULT_NAME.to_owned());
    let version =
        std::env::var("NUDOX_CSHARP_PKG_VERSION").unwrap_or_else(|_| DEFAULT_VERSION.to_owned());

    PackageSource::new(package_root(), name, version)
}

/// The second library, which covers what Polly happens not to use.
///
/// Polly.Core declares no events, no indexers and no explicit interface
/// implementations, so on its own it cannot show those paths work on real code.
/// The MVVM toolkit is built out of exactly those constructs.
fn toolkit_source() -> PackageSource {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .join(".real-csharp")
        .join("dotnet-8.4.0")
        .join("src")
        .join("CommunityToolkit.Mvvm");

    PackageSource::new(root, "CommunityToolkit.Mvvm", "8.4.0")
}

fn error_chain(err: &dyn std::error::Error) -> String {
    let mut parts = vec![err.to_string()];
    let mut current = err.source();
    while let Some(source) = current {
        parts.push(source.to_string());
        current = source.source();
    }
    parts.join(": ")
}

/// Run the oracle over `source` and lower it, with the whole thing measured.
///
/// Returns the extraction alongside the package so a caller can assert on the
/// oracle's own diagnostics — without which none of the lowered numbers can be
/// trusted to be about the library rather than about what failed to bind.
fn lower_real_package(
    source: &PackageSource,
) -> (
    nudox_producer_csharp::schema::Extraction,
    nudox_ir::package::IrPackage<String>,
    nudox_test_support::RunCost,
) {
    let producer = CSharpProducer::from_env();
    assert!(
        producer.is_available(),
        "publish the oracle first: cd workspace/compiler/languages/csharp/oracle && \
         dotnet publish -c Release --no-self-contained -o publish"
    );
    assert!(
        source.root().is_dir(),
        "no C# checkout at {}; see this file's module docs",
        source.root().display()
    );

    let case = format!("lower/csharp-{}-{}", source.name.as_str(), source.version);

    let (result, cost) = nudox_test_support::measured(&case, source.root(), || {
        let extraction = producer.invoke(source)?;

        let root = Symbol {
            name: source.name.as_str().to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: source.root.clone(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };

        let mut sink: Lowering<String> = Lowering::new(PackageId::path(source.root()), root);
        producer.lower(&extraction, &mut sink)?;

        let package = sink
            .finish()
            .expect("a real library must lower to a structurally sound package");

        Ok::<_, nudox_producer::ProducerError>((extraction, package))
    });

    let (extraction, package) = result.unwrap_or_else(|err| {
        panic!(
            "lowering {} failed: {}",
            source.name.as_str(),
            error_chain(&err)
        )
    });

    assert_eq!(
        extraction.diagnostics.error_type_count, 0,
        "every type in {} must bind; an error type means the extraction is degraded \
         and every count below would be measuring the damage instead of the library",
        source.name.as_str()
    );

    // Both of these libraries nest a public type inside an internal one, which
    // is precisely the shape that produces a dangling `enclosing` if the oracle
    // filters a container without filtering its children. `Lowering::finish`
    // would then reject the whole package, so the invariant is checked on real
    // code and not only on the fixture.
    let declared: std::collections::HashSet<&str> = extraction
        .types
        .iter()
        .map(|t| t.doc_id.as_str())
        .collect();

    for decl in &extraction.types {
        if let Some(enclosing) = &decl.enclosing {
            assert!(
                declared.contains(enclosing.as_str()),
                "{} is nested in {enclosing}, which {} does not declare",
                decl.doc_id,
                source.name.as_str()
            );
        }
    }

    (extraction, package, cost)
}

/// A real library must lower with every type resolved and its documentation
/// intact.
///
/// The assertions are deliberately about *named symbols that exist in Polly*
/// rather than about counts: a count alone would still pass if the producer
/// emitted the right number of wrong things.
#[test]
#[ignore = "needs a third-party C# checkout under .real-csharp/ and a published oracle; see module docs"]
fn real_library_lowers_with_resolved_types_and_documentation() {
    let source = package_source();
    let (extraction, package, cost) = lower_real_package(&source);

    let entries: Vec<&nudox_ir::entry::Entry> = package.iter().map(|(_, e)| e).collect();
    let names: Vec<&str> = entries.iter().map(|e| e.sym().name.as_str()).collect();

    // Types that are unmistakably Polly's public surface.
    for expected in [
        "ResiliencePipeline",
        "ResiliencePipelineBuilder",
        "ResilienceContext",
        "Outcome",
        "RetryStrategyOptions",
        "CircuitBreakerStrategyOptions",
        "CircuitState",
        "PredicateBuilder",
        "DelayBackoffType",
    ] {
        assert!(
            names.contains(&expected),
            "{expected} must be lowered from {}",
            source.name.as_str()
        );
    }

    // Real prose from Polly's own doc comments, not a placeholder.
    let pipeline = entries
        .iter()
        .find(|e| {
            e.sym().name == "ResiliencePipeline"
                && matches!(e.kind(), EntryInner::Owned(Kind::Record(_)))
        })
        .expect("ResiliencePipeline must lower to a Record");

    assert!(
        pipeline
            .sym()
            .documentation
            .contains("execute the user-provided callbacks"),
        "the library's own <summary> must reach the IR, got {:?}",
        pipeline.sym().documentation
    );

    // Polly is async-first; if not one method carried FnModifier::Async the
    // signature lowering would be silently dropping it.
    let async_methods = entries
        .iter()
        .filter(|e| match e.kind() {
            EntryInner::Owned(Kind::Function(f)) => {
                f.modifiers.contains(&nudox_ir::build::FnModifier::Async)
            }
            _ => false,
        })
        .count();

    assert!(
        async_methods >= 20,
        "Polly.Core declares dozens of async methods; only {async_methods} carried Async"
    );

    // Both record forms occur in Polly, and they must stay distinguishable:
    // `HealthInfo` is a `record struct`, `FallbackHandler<T>` a `record class`.
    // Note both are `internal` — they are only visible here because the
    // extractor keeps non-public declarations and records their visibility,
    // rather than dropping them the way a public-only pass would.
    let declared_as = |name: &str| -> String {
        entries
            .iter()
            .find(|e| e.sym().name == name)
            .unwrap_or_else(|| panic!("{name} must be lowered"))
            .sym()
            .documentation
            .clone()
    };

    assert!(
        declared_as("HealthInfo").contains("record struct"),
        "HealthInfo is a record struct and must be recorded as one"
    );
    assert!(
        declared_as("FallbackHandler").contains("record class"),
        "FallbackHandler<T> is a record class and must be recorded as one"
    );

    let internal_entries = entries
        .iter()
        .filter(|e| e.sym().visibility == Visibility::Internal)
        .count();
    assert!(
        internal_entries > 0,
        "non-public declarations must survive with their visibility recorded"
    );

    let documented = entries
        .iter()
        .filter(|e| !e.sym().documentation.is_empty())
        .count();

    eprintln!(
        "{} {}: {} entries ({} documented, {} async) from {} types in {:.1}s",
        source.name.as_str(),
        source.version,
        entries.len(),
        documented,
        async_methods,
        extraction.types.len(),
        cost.wall.as_secs_f64(),
    );
}

/// Events, indexers and explicit interface implementations must survive a real
/// library that is built out of them.
///
/// Polly exercises none of these three, so passing the Polly test above proves
/// nothing about them. The MVVM toolkit is the counter-example: `ObservableObject`
/// exists to raise `PropertyChanged`, and `ObservableValidator` reaches
/// `INotifyDataErrorInfo` only through an explicit implementation.
#[test]
#[ignore = "needs a third-party C# checkout under .real-csharp/ and a published oracle; see module docs"]
fn real_library_events_indexers_and_explicit_implementations_survive() {
    let source = toolkit_source();
    let (extraction, package, cost) = lower_real_package(&source);

    let entries: Vec<&nudox_ir::entry::Entry> = package.iter().map(|(_, e)| e).collect();
    let named = |name: &str| entries.iter().find(|e| e.sym().name == name);

    // Events. `PropertyChanged` is the whole point of ObservableObject.
    let property_changed = named("PropertyChanged")
        .expect("ObservableObject.PropertyChanged must be lowered");
    assert!(
        property_changed
            .sym()
            .documentation
            .contains("Declared: `event`"),
        "an event must be marked as an event, got {:?}",
        property_changed.sym().documentation
    );

    // Explicit interface implementations, named after the interface they satisfy.
    let explicit = named("INotifyDataErrorInfo.GetErrors").expect(
        "ObservableValidator implements INotifyDataErrorInfo.GetErrors explicitly; \
         it must be lowered under its qualified display name",
    );
    assert!(
        explicit
            .sym()
            .documentation
            .contains("Explicit implementation of"),
        "an explicit implementation must say so, got {:?}",
        explicit.sym().documentation
    );

    // Indexers, which the schema keeps separate from ordinary properties.
    let indexers: usize = extraction
        .types
        .iter()
        .map(|t| t.members.indexers.len())
        .sum();
    assert!(
        indexers >= 5,
        "the toolkit's grouped collections declare several indexers; found {indexers}"
    );

    // Declaration-site variance on a real generic interface.
    let variant: usize = extraction
        .types
        .iter()
        .flat_map(|t| t.type_params.iter())
        .filter(|p| p.variance != "none")
        .count();
    assert!(
        variant > 0,
        "the toolkit declares variant type parameters; none survived"
    );

    let events: usize = extraction
        .types
        .iter()
        .map(|t| t.members.events.len())
        .sum();
    let explicit_impls: usize = extraction
        .types
        .iter()
        .flat_map(|t| t.members.methods.iter())
        .filter(|m| m.explicit_interface.is_some())
        .count();

    eprintln!(
        "{} {}: {} entries from {} types — {} events, {} indexers, \
         {} explicit impls, {} variant type params in {:.1}s",
        source.name.as_str(),
        source.version,
        entries.len(),
        extraction.types.len(),
        events,
        indexers,
        explicit_impls,
        variant,
        cost.wall.as_secs_f64(),
    );
}
