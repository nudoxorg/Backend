//! End-to-end: the published Roslyn oracle over a real C# project, into the IR.
//!
//! Every test here runs the actual `dotnet oracle.dll` subprocess against the
//! checked-in C# project under `tests/csharp/fixture/` and asserts on symbols that
//! really exist in it. Nothing is stubbed and no JSON is hand-authored — that
//! is the point, and it is what separates this file from `lowering.rs`, whose
//! literal `FIXTURE` constant can only ever prove that the lowering agrees with
//! a string someone typed.
//!
//! # Prerequisite
//!
//! The oracle is a build artefact, not a committed binary. Publish it once:
//!
//! ```text
//! cd workspace/compiler/languages/csharp/oracle
//! dotnet publish -c Release --no-self-contained -o publish
//! ```
//!
//! If it is missing these tests **fail** rather than skip. A skipped
//! toolchain-dependent test is indistinguishable from a passing one in CI
//! output, which is how a producer rots unnoticed.

use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

use nudox_ir::{
    build::{FnModifier, GenericParam, Type},
    change::{EcosystemId, PackageLineageId, PackageName},
    entry::{EntryInner, Symbol, Visibility},
    kind::Kind,
    kinds::ty::Variance,
    lower::Lowering,
    package::{IrPackage, PackageId},
};
use nudox_languages::csharp::{
    CSharpProducer,
    schema::{Extraction, GeneratorSupport, TypeSig},
};
use nudox_languages::{PackageSource, Producer};

// ── Harness ───────────────────────────────────────────────────────────────────

/// The checked-in C# project the oracle extracts.
fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/csharp/fixture")
}

fn package_source() -> PackageSource {
    PackageSource::new(fixture_root(), "Nudox.Fixture", "1.0.0")
}

/// The oracle output, produced once for the whole test binary.
///
/// The subprocess is the entire cost of this suite; the lowering is
/// microseconds. Caching the extraction keeps each test honest (it still
/// re-lowers from scratch) without paying for Roslyn once per assertion.
fn extraction() -> &'static Extraction {
    static EXTRACTION: OnceLock<Extraction> = OnceLock::new();

    EXTRACTION.get_or_init(|| {
        let producer = CSharpProducer::from_env();

        assert!(
            producer.is_available(),
            "the C# oracle is not published at {}.\n\
             Build it with:\n  cd workspace/compiler/languages/csharp/oracle && \
             dotnet publish -c Release --no-self-contained -o publish\n\
             or point {} at an existing publish directory.",
            producer.oracle_dll().display(),
            nudox_languages::csharp::producer::ORACLE_PATH_ENV,
        );

        let source = package_source();

        // Doctrine §4: the measured region is the oracle subprocess — the only
        // part whose cost is worth tracking.
        let (result, cost) =
            heart::cost::measured("lower/csharp-fixture-oracle", &source.root, || {
                producer.invoke(&source)
            });

        let extraction = result.unwrap_or_else(|err| {
            panic!("oracle invocation failed: {}", error_chain(&err));
        });

        eprintln!(
            "extracted {} types from Nudox.Fixture in {:.1} ms",
            extraction.types.len(),
            cost.wall.as_secs_f64() * 1_000.0,
        );

        extraction
    })
}

/// Walk the whole `#[source]` chain (doctrine §8: the top-level `Display` is
/// deliberately terse, and reading only it turns a five-second diagnosis into
/// an hour).
fn error_chain(err: &dyn std::error::Error) -> String {
    let mut parts = vec![err.to_string()];
    let mut current = err.source();
    while let Some(source) = current {
        parts.push(source.to_string());
        current = source.source();
    }
    parts.join(": ")
}

/// Source generators are not loaded by the source oracle.  That limitation must
/// be explicit in the extraction contract; otherwise an API emitted into this
/// partial type is indistinguishable from an API the package simply does not
/// have.
#[test]
fn source_generator_support_is_visible_for_minimum_repro() {
    let root = tempfile::tempdir().expect("create generator repro root");
    std::fs::write(
        root.path().join("GeneratorRepro.csproj"),
        r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
  </PropertyGroup>
  <ItemGroup>
    <Analyzer Include="configured-generator.dll" />
  </ItemGroup>
</Project>
"#,
    )
    .expect("write generator configuration");
    std::fs::write(
        root.path().join("Api.cs"),
        r#"
namespace GeneratorRepro;

public partial class Api
{
    // A configured source generator would add GeneratedMethod here.
}
"#,
    )
    .expect("write generator repro source");

    let source = PackageSource::new(root.path(), "GeneratorRepro", "1.0.0");
    let extraction = CSharpProducer::from_env()
        .invoke(&source)
        .expect("the minimum generator repro must extract");

    assert_eq!(
        extraction.diagnostics.generator_support,
        GeneratorSupport::Unavailable,
        "generator support must be visible instead of silently omitting generated API"
    );
    assert!(
        extraction
            .types
            .iter()
            .flat_map(|ty| ty.members.methods.iter())
            .all(|method| method.name != "GeneratedMethod"),
        "the repro must not claim an ungenerated member exists"
    );
}

#[test]
fn oracle_and_lowering_preserve_decimal_and_array_rank_availability() {
    let root = tempfile::tempdir().expect("create typed-gap repro root");
    std::fs::write(
        root.path().join("TypedGaps.csproj"),
        r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup>
</Project>
"#,
    )
    .expect("write typed-gap project");
    std::fs::write(
        root.path().join("Api.cs"),
        "namespace TypedGaps; public class Api { public decimal Price; public int[,,] Cube; }",
    )
    .expect("write typed-gap source");

    let source = PackageSource::new(root.path(), "TypedGaps", "1.0.0");
    let extraction = CSharpProducer::from_env()
        .invoke(&source)
        .expect("typed-gap repro must extract");
    let api = extraction
        .types
        .iter()
        .find(|ty| ty.simple_name == "Api")
        .expect("Api must be extracted");
    let price = api
        .members
        .fields
        .iter()
        .find(|field| field.name == "Price")
        .expect("Price must be extracted");
    assert!(matches!(
        &price.ty,
        TypeSig::Named { name, .. } if name == "System.Decimal"
    ));
    let cube = api
        .members
        .fields
        .iter()
        .find(|field| field.name == "Cube")
        .expect("Cube must be extracted");
    assert!(matches!(&cube.ty, TypeSig::Array { rank: 3, .. }));
    let package = nudox_languages::csharp::lower(&extraction).expect("typed-gap repro must lower");
    let docs = package
        .iter()
        .filter(|(_, entry)| entry.sym().name == "TypedGaps")
        .map(|(_, entry)| entry.sym().documentation.as_str())
        .next()
        .expect("assembly root must be present");
    assert!(docs.contains("target framework `net10.0`"));
    assert!(docs.contains("Diagnostics: 0 compilation error(s), 0 error type(s)"));
    assert!(docs.contains("Generator support"));
}

/// Lower the cached extraction into a fresh package.
fn lowered() -> IrPackage<String> {
    let producer = CSharpProducer::from_env();

    let root = Symbol {
        name: "Nudox.Fixture".to_owned(),
        visibility: Visibility::Public,
        documentation: String::new(),
        source: fixture_root(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    };

    let mut sink: Lowering<String> = Lowering::new(PackageId::path(fixture_root()), root);
    producer
        .lower(extraction(), &mut sink)
        .expect("lowering the fixture must succeed");

    sink.finish()
        .expect("the lowered fixture must be structurally sound")
}

/// The single entry whose symbol name matches, panicking with the available
/// names when it is absent — a bare `unwrap` here costs a re-run to diagnose.
fn entry<'a>(pkg: &'a IrPackage<String>, name: &str) -> &'a nudox_ir::entry::Entry {
    if let Some((_, e)) = pkg.iter().find(|(_, e)| e.sym().name == name) {
        e
    } else {
        let mut names: Vec<&str> = pkg.iter().map(|(_, e)| e.sym().name.as_str()).collect();
        names.sort_unstable();
        panic!("no entry named {name:?}; present: {names:?}");
    }
}

fn entries<'a>(pkg: &'a IrPackage<String>, name: &str) -> Vec<&'a nudox_ir::entry::Entry> {
    pkg.iter()
        .filter(|(_, e)| e.sym().name == name)
        .map(|(_, e)| e)
        .collect()
}

// ── The extraction itself ─────────────────────────────────────────────────────

/// The oracle must bind the fixture completely.
///
/// This is the load-bearing assertion of the whole file. Roslyn happily returns
/// a document full of error types when references or global usings are missing,
/// and every richness assertion below would then be measuring a degraded
/// extraction while still passing. A non-zero count here means the numbers are
/// not about the fixture.
#[test]
fn oracle_binds_the_fixture_with_no_error_types() {
    let extraction = extraction();

    assert_eq!(
        extraction.diagnostics.error_count, 0,
        "the fixture must compile clean; the oracle reported errors"
    );
    assert_eq!(
        extraction.diagnostics.error_type_count, 0,
        "every type in the fixture must bind to a real symbol"
    );
    assert_eq!(extraction.format, 1);
    assert_eq!(extraction.mode, "source");
    assert_eq!(extraction.assembly.name, "Nudox.Fixture");
    assert!(
        extraction.roslyn.starts_with('5'),
        "expected a Roslyn 5.x oracle, got {:?}",
        extraction.roslyn
    );
}

/// Every declared type kind must survive the walk with its C# form intact.
#[test]
fn every_declared_type_kind_reaches_the_extraction() {
    let extraction = extraction();

    let kinds: Vec<(&str, &str)> = extraction
        .types
        .iter()
        .map(|t| (t.simple_name.as_str(), t.kind.as_str()))
        .collect();

    for (name, kind) in [
        ("Point", "RECORD"),
        ("Extent", "RECORD_STRUCT"),
        ("IEntry", "INTERFACE"),
        ("IReadOnlyCatalog", "INTERFACE"),
        ("Severity", "ENUM"),
        ("CatalogOptions", "ENUM"),
        ("Projection", "DELEGATE"),
        ("Repository", "CLASS"),
        ("Cursor", "STRUCT"),
    ] {
        assert!(
            kinds.contains(&(name, kind)),
            "expected {name} to be extracted as {kind}; got {kinds:?}"
        );
    }
}

/// Every `enclosing` must name a type the document also declares.
///
/// This is the one parent pointer the lowering dereferences unconditionally:
/// `parent_ref` calls `Lowering::refer` on it without checking, and
/// `Lowering::finish` rejects an id that was referred to but never declared. A
/// dangling `enclosing` therefore does not lose one nested type — it fails the
/// whole package. The invariant belongs to the oracle, so it is asserted on the
/// document rather than on the lowered result.
#[test]
fn every_enclosing_pointer_names_a_declared_type() {
    let extraction = extraction();

    let declared: std::collections::HashSet<&str> =
        extraction.types.iter().map(|t| t.doc_id.as_str()).collect();

    for decl in &extraction.types {
        if let Some(enclosing) = &decl.enclosing {
            assert!(
                declared.contains(enclosing.as_str()),
                "{} is nested in {enclosing}, which the document does not declare; \
                 Lowering::finish would reject the package",
                decl.doc_id
            );
        }
    }
}

// ── Documentation ─────────────────────────────────────────────────────────────

/// `<summary>` prose, `<param>` lifted onto a record's positional property, and
/// an inline `<see langword>` must all reach `Symbol::documentation`.
#[test]
fn xml_doc_summary_and_record_param_reach_the_ir() {
    let pkg = lowered();

    let point = entry(&pkg, "Point");
    assert!(
        point.sym().documentation.contains("A point on the page."),
        "the record's <summary> must be lowered, got {:?}",
        point.sym().documentation
    );
    assert!(
        point.sym().documentation.contains("record class"),
        "the C# declaration form must be stamped into the doc"
    );

    // Roslyn lifts a record's `<param name="X">` onto the generated property.
    let x = entry(&pkg, "X");
    assert!(
        x.sym().documentation.contains("the horizontal offset"),
        "a record positional <param> must land on its property, got {:?}",
        x.sym().documentation
    );

    let label = entry(&pkg, "Label");
    assert!(
        label.sym().documentation.contains("`null`"),
        "<see langword=\"null\"/> must render as inline code, got {:?}",
        label.sym().documentation
    );
}

/// `<returns>` belongs to the return parameter, and `<param>` text to the
/// method's own prose.
#[test]
fn returns_and_param_docs_land_on_their_own_entries() {
    let pkg = lowered();

    let store = entry(&pkg, "Store");
    assert!(
        store.sym().documentation.contains("the entry to store"),
        "a <param> description must appear in the method documentation, got {:?}",
        store.sym().documentation
    );

    let returns_doc = pkg.iter().any(|(_, e)| {
        e.sym()
            .documentation
            .contains("when an existing entry was replaced")
    });
    assert!(
        returns_doc,
        "the <returns> text must be lowered onto the return parameter"
    );
}

/// `<exception cref>` must become structured `Function::throws`, not an output
/// parameter, and must also survive as a labelled doc link.
#[test]
fn exception_cref_becomes_throws_and_a_doc_link() {
    let pkg = lowered();

    let load = entry(&pkg, "LoadAsync");
    let function = match load.kind() {
        EntryInner::Owned(Kind::Function(f)) => f,
        other => panic!("LoadAsync must be a Function, got {other:?}"),
    };

    assert_eq!(
        function.throws.len(),
        1,
        "LoadAsync documents exactly one <exception>"
    );

    // `BrokenEntryException` is declared in the same package, so the throw must
    // resolve to a live nominal reference rather than degrading to `Any`.
    assert!(
        matches!(function.throws[0], Type::Nominal(_)),
        "an in-package exception type must lower to Nominal, got {:?}",
        function.throws[0]
    );

    assert!(
        load.sym()
            .doc_links
            .iter()
            .any(|l| l.label.as_deref() == Some("throws")),
        "the exception cref must also be carried as a doc link"
    );
}

/// `<inheritdoc/>` must be resolved oracle-side against the implemented
/// interface member, not passed through as the literal marker.
#[test]
fn inheritdoc_is_resolved_from_the_implemented_interface() {
    let pkg = lowered();

    let counts = entries(&pkg, "Count");
    let resolved = counts
        .iter()
        .any(|e| e.sym().documentation.contains("The number of entries."));

    assert!(
        resolved,
        "Repository.Count uses <inheritdoc/>; its text must come from \
         IReadOnlyCatalog.Count. Documentation seen: {:?}",
        counts
            .iter()
            .map(|e| e.sym().documentation.as_str())
            .collect::<Vec<_>>()
    );

    assert!(
        !pkg.iter()
            .any(|(_, e)| e.sym().documentation.contains("inheritdoc")),
        "no entry may carry an unresolved <inheritdoc/> marker"
    );
}

// ── Types and signatures ──────────────────────────────────────────────────────

/// Declaration-site variance is real API surface and must round-trip.
#[test]
fn declaration_site_variance_round_trips() {
    let pkg = lowered();

    let catalog = entry(&pkg, "IReadOnlyCatalog");
    let generics = match catalog.kind() {
        EntryInner::Owned(Kind::Trait(t)) => &t.generics,
        other => panic!("IReadOnlyCatalog must be a Trait, got {other:?}"),
    };

    match &generics[0] {
        GenericParam::Type { name, variance, .. } => {
            assert_eq!(name, "T");
            assert_eq!(
                *variance,
                Some(Variance::Covariant),
                "`out T` must lower to covariant"
            );
        }
        other => panic!("expected a type parameter, got {other:?}"),
    }

    let projection = entry(&pkg, "Projection");
    let generics = match projection.kind() {
        EntryInner::Owned(Kind::Alias(a)) => &a.generics,
        other => panic!("Projection must be an Alias, got {other:?}"),
    };

    let variances: Vec<Option<Variance>> = generics
        .iter()
        .map(|g| match g {
            GenericParam::Type { variance, .. } => *variance,
            _ => None,
        })
        .collect();

    assert_eq!(
        variances,
        vec![Some(Variance::Contravariant), Some(Variance::Covariant)],
        "`Projection<in TSource, out TResult>` must keep both variances"
    );
}

/// C#'s keyword constraints have no structural slot, so they ride as synthetic
/// `csharp:`-prefixed predicates; real interface bounds stay real types.
#[test]
fn type_parameter_constraints_reach_the_where_clause() {
    let pkg = lowered();

    let repository = entry(&pkg, "Repository");
    let wheres = match repository.kind() {
        EntryInner::Owned(Kind::Record(r)) => &r.wheres,
        other => panic!("Repository must be a Record, got {other:?}"),
    };

    let rendered = format!("{wheres:?}");
    for keyword in ["csharp:class", "csharp:new()"] {
        assert!(
            rendered.contains(keyword),
            "the `where T : class, IEntry, new()` clause must carry {keyword}, got {rendered}"
        );
    }

    // `where T : IEntry` binds under an enabled nullable context, so the bound
    // arrives wrapped in the `notAnnotated` marker. Peel it before asking
    // whether the constraint resolved: the substantive claim is that an
    // in-package interface bound becomes a live reference rather than `Any`.
    let resolves_to_a_real_type = wheres.iter().any(|w| {
        w.bounds.iter().any(|b| {
            let base = match b {
                Type::Annotated { inner, .. } => inner.as_ref(),
                other => other,
            };
            matches!(base, Type::Nominal(_) | Type::Apply { .. })
        })
    });

    assert!(
        resolves_to_a_real_type,
        "the IEntry bound must resolve to a real type, not Type::Any; got {rendered}"
    );

    // `where T : unmanaged` lives on a method, not the type.
    let read = entry(&pkg, "ReadUnaligned");
    let wheres = match read.kind() {
        EntryInner::Owned(Kind::Function(f)) => &f.wheres,
        other => panic!("ReadUnaligned must be a Function, got {other:?}"),
    };
    assert!(
        format!("{wheres:?}").contains("csharp:unmanaged"),
        "the unmanaged constraint must reach the method's where clause"
    );
}

/// Nullable reference types are three-state and must not collapse: `string?`
/// and a non-null `string` have to lower differently.
#[test]
fn nullable_reference_annotations_stay_three_state() {
    let pkg = lowered();

    let label = entry(&pkg, "Label");
    let ty = match label.kind() {
        EntryInner::Owned(Kind::Field(f)) => f.ty.as_ref().expect("Label must have a type"),
        other => panic!("Label must be a Field, got {other:?}"),
    };
    assert!(
        matches!(ty, Type::Union(parts) if parts.len() == 2 && parts[1] == Type::Never),
        "`string?` must lower to Union([Str, Never]), got {ty:?}"
    );

    let key = entry(&pkg, "Key");
    let ty = match key.kind() {
        EntryInner::Owned(Kind::Field(f)) => f.ty.as_ref().expect("Key must have a type"),
        other => panic!("Key must be a Field, got {other:?}"),
    };
    match ty {
        Type::Annotated { annotation, .. } => assert_eq!(
            annotation.token, "notAnnotated",
            "a non-null `string` must record that it was declared non-null"
        ),
        other => panic!("`string` under NRT must lower to Annotated, got {other:?}"),
    }
}

/// `async`, iterator and `unsafe` are signature modifiers the IR carries
/// structurally rather than as prose.
#[test]
fn signature_modifiers_are_structural() {
    let pkg = lowered();

    let modifiers = |name: &str| -> Vec<FnModifier> {
        match entry(&pkg, name).kind() {
            EntryInner::Owned(Kind::Function(f)) => f.modifiers.to_vec(),
            other => panic!("{name} must be a Function, got {other:?}"),
        }
    };

    assert!(
        modifiers("LoadAsync").contains(&FnModifier::Async),
        "an `async` method must carry FnModifier::Async"
    );
    assert!(
        modifiers("Keys").contains(&FnModifier::Generator),
        "a `yield`-bearing method must carry FnModifier::Generator"
    );
    assert!(
        modifiers("ReadUnaligned").contains(&FnModifier::Unsafe),
        "a pointer-taking method must carry FnModifier::Unsafe"
    );
}

/// A named value tuple's labels are part of the signature.
#[test]
fn named_tuple_element_labels_survive() {
    let pkg = lowered();

    let describe_return = pkg
        .iter()
        .find(|(id, _)| id.is_some_and(|i| i.contains("Describe") && i.ends_with("#ret")))
        .map(|(_, e)| e);

    let ty = match describe_return.map(nudox_ir::entry::Entry::kind) {
        Some(EntryInner::Owned(Kind::Param(p))) => {
            p.ty.as_ref()
                .expect("the return parameter must have a type")
        }
        other => panic!("Describe's return parameter is missing, got {other:?}"),
    };

    let rendered = format!("{ty:?}");
    assert!(
        rendered.contains("count") && rendered.contains("options"),
        "the tuple labels `count` and `options` must survive, got {rendered}"
    );
}

/// A delegate is a callable type, and must lower structurally rather than to a
/// bare opaque alias.
#[test]
fn delegate_lowers_to_a_function_pointer_alias() {
    let pkg = lowered();

    let projection = entry(&pkg, "Projection");
    let target = match projection.kind() {
        EntryInner::Owned(Kind::Alias(a)) => {
            a.target.as_ref().expect("delegate must have a target")
        }
        other => panic!("Projection must be an Alias, got {other:?}"),
    };

    match target {
        Type::FunctionPointer { params, ret, abi } => {
            assert_eq!(params.len(), 1, "Projection takes one parameter");
            assert!(ret.is_some(), "Projection returns a value");
            assert!(abi.is_none(), "a managed delegate has no unmanaged ABI");
        }
        other => panic!("a delegate must lower to FunctionPointer, got {other:?}"),
    }
}

// ── Members ───────────────────────────────────────────────────────────────────

/// Accessor shape, asymmetric accessor visibility, and `init` must be recorded.
#[test]
fn property_accessors_and_asymmetric_visibility_are_recorded() {
    let pkg = lowered();

    let options = entry(&pkg, "Options");
    assert!(
        options.sym().documentation.contains("Accessor: get; init"),
        "an init-only property must record its accessor shape, got {:?}",
        options.sym().documentation
    );

    let last_error = entry(&pkg, "LastError");
    assert!(
        last_error.sym().documentation.contains("private set"),
        "an asymmetric setter's visibility must be recorded, got {:?}",
        last_error.sym().documentation
    );
}

/// Events are a distinct C# member kind and must not be lost among properties.
#[test]
fn events_are_lowered_with_their_accessor_visibility() {
    let pkg = lowered();

    let stored = entry(&pkg, "Stored");
    assert!(
        stored.sym().documentation.contains("Declared: `event`"),
        "an event must be marked as such, got {:?}",
        stored.sym().documentation
    );

    let cleared = entry(&pkg, "Cleared");
    assert_eq!(
        cleared.sym().visibility,
        Visibility::Protected,
        "`protected internal` widens to Protected in the IR"
    );
    assert!(
        cleared.sym().documentation.contains("protectedInternal"),
        "the original C# accessibility pair must stay recoverable, got {:?}",
        cleared.sym().documentation
    );
}

/// An explicit interface implementation is a real member with a qualified
/// display name.
#[test]
fn explicit_interface_implementation_is_named_after_its_interface() {
    let pkg = lowered();

    let dispose = entry(&pkg, "IDisposable.Dispose");
    assert!(
        dispose
            .sym()
            .documentation
            .contains("Explicit implementation of"),
        "an explicit implementation must say so, got {:?}",
        dispose.sym().documentation
    );
}

/// Parameter modifiers change how a call site must be written, so they are
/// structural attributes rather than prose.
#[test]
fn parameter_modifiers_are_structural() {
    let pkg = lowered();

    use nudox_ir::build::ParamAttribute;

    let attributes = |method: &str, param: &str| -> Vec<ParamAttribute> {
        let e = pkg
            .iter()
            .find(|(id, e)| e.sym().name == param && id.is_some_and(|i| i.contains(method)))
            .map_or_else(|| panic!("no parameter {param} on {method}"), |(_, e)| e);

        match e.kind() {
            EntryInner::Owned(Kind::Param(p)) => p.attributes.to_vec(),
            other => panic!("{param} must be a Param, got {other:?}"),
        }
    };

    assert!(
        attributes("TryTake", "entry").contains(&ParamAttribute::Out),
        "an `out` parameter must be marked Out"
    );
    assert!(
        attributes("StoreAll", "entries").contains(&ParamAttribute::Variadic),
        "a `params` array must be marked Variadic"
    );

    let mode = attributes("Merge", "mode");
    assert!(
        mode.contains(&ParamAttribute::Optional),
        "a defaulted parameter must be marked Optional"
    );

    let other = attributes("Merge", "other");
    assert!(
        other.contains(&ParamAttribute::Borrowing),
        "an `in` parameter must be marked Borrowing"
    );
}

/// Enum members carry their discriminants, and `[Flags]` is surfaced.
#[test]
fn enum_variants_carry_discriminants() {
    let pkg = lowered();

    let severity = entry(&pkg, "Severity");
    let variants = match severity.kind() {
        EntryInner::Owned(Kind::Enum(e)) => e.variants.len(),
        other => panic!("Severity must be an Enum, got {other:?}"),
    };
    assert_eq!(variants, 3, "Severity has three members");

    let warning = entry(&pkg, "Warning");
    match warning.kind() {
        EntryInner::Owned(Kind::Variant(v)) => assert_eq!(
            v.discr.as_deref(),
            Some("10"),
            "an explicit enum value must be preserved"
        ),
        other => panic!("Warning must be a Variant, got {other:?}"),
    }

    let options = entry(&pkg, "CatalogOptions");
    assert!(
        options.sym().documentation.contains("[Flags]"),
        "a bit-field enum must be marked, got {:?}",
        options.sym().documentation
    );
    // The note renders the underlying type through `type_display`, which takes
    // the trailing segment — `Byte`, not `System.Byte`.
    assert!(
        options
            .sym()
            .documentation
            .contains("Underlying type: `Byte`"),
        "a non-default underlying type must be recorded, got {:?}",
        options.sym().documentation
    );
}

/// Attributes, `[Obsolete]` and `[EditorBrowsable(Never)]` are three different
/// facts and must not be conflated.
#[test]
fn attributes_deprecation_and_hiding_are_distinct() {
    let pkg = lowered();

    let repository = entry(&pkg, "Repository");
    let attrs = format!("{:?}", repository.sym().attrs);
    assert!(
        attrs.contains("Tracked"),
        "a user-defined attribute must be rendered, got {attrs}"
    );
    assert!(
        attrs.contains("Eager"),
        "a named attribute argument must be rendered, got {attrs}"
    );

    let add = entry(&pkg, "Add");
    let deprecation = add
        .sym()
        .deprecation
        .as_ref()
        .expect("[Obsolete] must lower to a Deprecation");
    assert!(
        deprecation
            .note
            .as_deref()
            .is_some_and(|n| n.contains("Use Store")),
        "the obsolete message must be carried, got {:?}",
        deprecation.note
    );

    let reset = entry(&pkg, "ResetInternalState");
    assert!(
        reset.sym().documentation.contains("EditorBrowsable"),
        "a hidden member must be marked, got {:?}",
        reset.sym().documentation
    );
    assert!(
        reset.sym().deprecation.is_none(),
        "hidden is not deprecated; the two facts must stay separate"
    );
}

/// Operators and user-defined conversions are distinguishable from ordinary
/// methods.
#[test]
fn operators_and_conversions_record_their_kind() {
    let pkg = lowered();

    let addition = entry(&pkg, "op_Addition");
    assert!(
        addition.sym().documentation.contains("static"),
        "an operator is static, got {:?}",
        addition.sym().documentation
    );

    let conversion = entry(&pkg, "op_Implicit");
    assert!(
        conversion.sym().documentation.contains("implicit operator"),
        "a conversion must record its operator kind, got {:?}",
        conversion.sym().documentation
    );
}

/// An extension method is an ordinary static method plus a receiver, and both
/// halves must be visible.
#[test]
fn extension_methods_mark_their_receiver() {
    let pkg = lowered();

    let count = entry(&pkg, "CountAtLeast");
    assert!(
        count.sym().documentation.contains("extension"),
        "an extension method must be marked, got {:?}",
        count.sym().documentation
    );

    let receiver = pkg
        .iter()
        .find(|(id, e)| e.sym().name == "catalog" && id.is_some_and(|i| i.contains("CountAtLeast")))
        .map(|(_, e)| e)
        .expect("CountAtLeast's receiver parameter must exist");

    assert!(
        receiver.sym().documentation.contains("extension receiver"),
        "the first parameter must be marked as the receiver, got {:?}",
        receiver.sym().documentation
    );
}

/// A type nested inside a generic type must keep its parent link.
#[test]
fn nested_type_inside_a_generic_keeps_its_parent() {
    let pkg = lowered();

    let cursor = pkg
        .iter()
        .find(|(id, e)| e.sym().name == "Cursor" && id.is_some_and(|i| i.contains("Repository")))
        .and_then(|(id, _)| id.cloned())
        .expect("Repository<T>.Cursor must be extracted");

    assert_eq!(
        cursor, "T:Nudox.Fixture.Repository`1.Cursor",
        "a nested type's doc-id must be qualified by its container"
    );
}

// ── The full trait pipeline ───────────────────────────────────────────────────

/// `nudox_languages::produce` must drive the C# producer end to end and seal a
/// table — the path the registry actually takes.
#[test]
fn produce_drives_the_csharp_producer_to_a_sealed_table() {
    let producer = CSharpProducer::from_env();
    assert!(
        producer.is_available(),
        "the C# oracle must be published; see this file's module docs"
    );

    let source = package_source();
    let lineage =
        PackageLineageId::new(EcosystemId::new("nuget"), PackageName::new("Nudox.Fixture"));

    let (result, cost) =
        heart::cost::measured("lower/csharp-fixture-produce", &source.root, || {
            nudox_languages::produce(&producer, &source, &lineage, &nudox_ir::foreign::Unlinked)
        });

    let table = result
        .unwrap_or_else(|err| panic!("produce failed: {}", error_chain(&err)))
        .table;

    // Assert on what is in the table, not just how much of it there is: a count
    // alone would pass against a table full of the wrong entries.
    let names: Vec<&str> = table.iter().map(|(_, e)| e.sym().name.as_str()).collect();

    for expected in [
        "Repository",
        "LoadAsync",
        "IDisposable.Dispose",
        "Projection",
        "Severity",
        "CountAtLeast",
        "Cursor",
    ] {
        assert!(
            names.contains(&expected),
            "the sealed table must contain {expected}; got {names:?}"
        );
    }

    // A baseline, not a threshold. If this moves, something changed in the
    // oracle or the lowering and the diff should say which — an inequality
    // here would let a silent loss of members through.
    //
    // Re-baselined 96 -> 98 on 2026-08-23 after review: the current canonical
    // (nix-built Roslyn) oracle extracts two additional real members of the
    // fixture that the 2026-08-08 measurement did not. The +2 was confirmed to
    // be genuine coverage, not duplication: dumping the full sealed table showed
    // 98 distinct, recognisable `Nudox.Fixture` members (operators `op_Addition`
    // /`op_Implicit`, the `this[]` indexer, `TrackedAttribute`, …) with no
    // phantom entries, and the count is unchanged whether the working-tree
    // `csharp/lower.rs`/`types.rs` edits are applied or stashed — so this is an
    // extraction gain, never a lowering-side regression.
    let count = table.len();
    assert_eq!(
        count, 98,
        "Nudox.Fixture lowers to 98 sealed entries; a change here is a change \
         in extraction coverage and must be reviewed, not re-baselined blindly"
    );

    eprintln!(
        "produce(): {count} sealed entries in {:.1} ms",
        cost.wall.as_secs_f64() * 1_000.0
    );
}

#[test]
fn roslyn_resolves_fixture_method_call_edges() {
    let extraction = extraction();
    assert!(
        extraction.references.iter().any(|reference| {
            reference.owner.contains("StoreAll")
                && reference.target.contains("Store")
                && reference.start < reference.end
        }),
        "Roslyn must emit a located StoreAll -> Store call edge; got {:?}",
        extraction.references
    );
}
