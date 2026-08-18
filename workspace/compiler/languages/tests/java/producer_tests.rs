//! Integration tests for `nudox-languages`: real oracle, real
//! third-party library, real end-to-end lowering.
//!
//! # Fixtures
//!
//! - `tests/java/fixtures/gson/` — Gson 2.11.0 (`google/gson` @
//!   `828a97be0f8d58108b140b77df8dc76b657f4a87`, tag `gson-parent-2.11.0`),
//!   vendored, real, third-party, Apache-2.0. It is unmodified except two
//!   disclosed additions needed to make a standalone (no Maven, no network)
//!   `javadoc` compile succeed:
//!     - `com/google/gson/internal/GsonBuildConfig.java` — this **is** real
//!       Gson source (`src/main/java-templates/…`, Maven-templated at build
//!       time); the `${project.version}` placeholder is resolved to the real
//!       released `2.11.0`, exactly what Maven's `templating-maven-plugin`
//!       does.
//!     - `com/google/errorprone/annotations/{CanIgnoreReturnValue,InlineMe}.java`
//!       — NOT Gson source (see each file's header): minimal stand-ins for
//!       Gson's optional `error_prone_annotations` compile-time dependency,
//!       matching the real annotations' public shape exactly.
//!
//!   `module-info.java` is intentionally **not** vendored: it declares
//!   `requires static com.google.errorprone.annotations`, and `javadoc`'s
//!   module resolution fails closed on any unresolved `requires` — even a
//!   static one — regardless of classpath. JPMS directive lowering is
//!   instead verified against `tests/java/fixtures/modern`, below.
//! - `tests/java/fixtures/modern/` — small, hand-written **and genuinely compiled
//!   and doc-processed through the real oracle** (not the fabricated-JSON
//!   kind of fixture the unit tests in `src/lower.rs` use) supplementary
//!   probe for constructs Gson's own source does not exercise: `record`,
//!   `sealed interface … permits`, and a `module-info.java` with
//!   `requires`/`exports`/`uses`/`provides` directives. It is explicitly a
//!   supplement, not "the" real third-party library — see the module doc on
//!   `nudox_languages::java::producer` for why Gson alone cannot cover these.
//! - `tests/java/fixtures/markdown-doc-probe/` — a single JEP 467 (`///`)
//!   Markdown-doc-commented file, used to verify — adaptively, based on the
//!   running JDK's own reported version — whether this toolchain's `javadoc`
//!   actually recognizes `///` as a doc comment.
//!
//! Every test asserts on real content (a symbol that really exists, a doc
//! string that really was extracted, a real compiler error message) — never
//! bare `is_ok()` or a nonzero count.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use nudox_ir::change::{EcosystemId, PackageLineageId, PackageName};
use nudox_ir::entry::{Symbol, Visibility};
use nudox_ir::index::Ref;
use nudox_ir::kind::Kind;
use nudox_ir::kinds::ty::Type;
use nudox_ir::lower::Lowering;
use nudox_ir::package::{IrPackage, PackageId};
use nudox_languages::java::JavaProducer;
use nudox_languages::java::lower::{JavaId, LoweringCtx, lower_extraction};
use nudox_languages::java::schema::Extraction;
use nudox_languages::{PackageSource, Producer, ProducerError, oracle, produce};

// ── Helpers ──────────────────────────────────────────────────────────────────

fn fixture_root(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/java/fixtures")
        .join(name)
}

/// Print the whole `#[source]` chain. `ProducerError`'s top-level `Display`
/// is deliberately terse; reading only it is how a five-second diagnosis
/// becomes an hour (docs/AGENTS-DOCTRINE.md §8).
fn error_chain(err: &dyn std::error::Error) -> String {
    let mut chain = err.to_string();
    let mut cursor: &dyn std::error::Error = err;
    while let Some(source) = std::error::Error::source(cursor) {
        let _ = write!(chain, "\n  caused by: {source}");
        cursor = source;
    }
    chain
}

/// Invoke the real oracle over `fixture` and lower it, keeping the typed
/// `JavaId` (unlike going through `nudox_languages::produce`, which seals into
/// a type-erased `PristineIntroTable`) so tests can assert on exact
/// identities, not just names.
fn lower_fixture(fixture: &str, package_name: &str) -> IrPackage<JavaId> {
    let root = fixture_root(fixture);
    let src = PackageSource::new(&root, package_name, "test");
    let extraction = JavaProducer::new().invoke(&src).unwrap_or_else(|e| {
        panic!(
            "{fixture} oracle invocation must succeed:\n{}",
            error_chain(&e)
        )
    });

    let pkg_id = PackageId::path(&root);
    let root_sym = Symbol {
        name: package_name.to_owned(),
        visibility: Visibility::Public,
        documentation: String::new(),
        source: root,
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    };
    let mut low: Lowering<JavaId> = Lowering::new(pkg_id, root_sym);
    let mut ctx = LoweringCtx::new(&mut low, &extraction.types);
    lower_extraction(&mut ctx, &extraction);
    low.finish()
        .unwrap_or_else(|e| panic!("{fixture} must lower without structural errors: {e}"))
}

#[test]
fn java_oracle_resolves_default_method_calls() {
    let root = fixture_root("modern");
    let source = PackageSource::new(&root, "modern", "test");
    let extraction = JavaProducer::new()
        .invoke(&source)
        .expect("modern Java fixture must invoke");
    assert!(
        extraction.references.iter().any(|reference| {
            reference.owner.contains("Shape#describe")
                && reference.target.contains("Shape#area")
                && reference.start < reference.end
        }),
        "javac Trees must emit a located describe -> area edge; got {:?}",
        extraction.references
    );
}

// ── 1. Gson 2.11.0 — full `produce()` pipeline ──────────────────────────────

/// The literal acceptance shape: `Producer` wired end to end through
/// `nudox_languages::produce`, over a real third-party library, asserted on
/// real symbols that exist in Gson 2.11.0 — never on `is_ok()` or a bare
/// count.
#[test]
fn gson_2_11_0_lowers_end_to_end_via_produce() {
    let root = fixture_root("gson");
    let src = PackageSource::new(&root, "gson", "2.11.0");
    let lineage = PackageLineageId::new(EcosystemId::new("maven"), PackageName::new("gson"));

    let (table, cost) = heart::cost::measured("lower/gson-2.11.0", &root, || {
        produce(
            &JavaProducer::new(),
            &src,
            &lineage,
            &nudox_ir::foreign::Unlinked,
        )
        .unwrap_or_else(|e| panic!("gson must lower without error:\n{}", error_chain(&e)))
        .table
    });

    assert!(!table.is_empty(), "gson lowered to zero entries");

    let names: Vec<&str> = table.iter().map(|(_, e)| e.sym().name.as_str()).collect();
    for expected in [
        "Gson",
        "GsonBuilder",
        "TypeAdapter",
        "TypeAdapterFactory",
        "FieldNamingPolicy",
        "SerializedName",
        "JsonObject",
        "JsonArray",
        "JsonParser",
    ] {
        assert!(
            names.contains(&expected),
            "expected real Gson symbol {expected:?} in the lowered table ({} entries total)",
            table.len(),
        );
    }

    // Every one of FieldNamingPolicy's 7 real enum constants must survive.
    for variant in [
        "IDENTITY",
        "UPPER_CAMEL_CASE",
        "UPPER_CAMEL_CASE_WITH_SPACES",
        "UPPER_CASE_WITH_UNDERSCORES",
        "LOWER_CASE_WITH_UNDERSCORES",
        "LOWER_CASE_WITH_DASHES",
        "LOWER_CASE_WITH_DOTS",
    ] {
        assert!(
            names.contains(&variant),
            "FieldNamingPolicy enum constant {variant:?} missing from the lowered table"
        );
    }

    // Gson.toJson has 8 real overloads in 2.11.0 — overload identity means
    // each becomes its own declaration (see JavaId's module doc).
    let tojson_count = names.iter().filter(|n| **n == "toJson").count();
    assert!(
        tojson_count >= 8,
        "expected >= 8 distinct toJson overload entries, got {tojson_count}"
    );

    eprintln!(
        "gson-2.11.0 produce(): {} entries in {:.2}s; sample: {:?}",
        table.len(),
        cost.wall.as_secs_f64(),
        &names[..names.len().min(12)],
    );
}

/// A nested/inner class (`ReflectionAccessFilter.FilterResult`, a real
/// member-nested enum in Gson) must retain a parent link once sealed into a
/// `PristineIntroTable` — proving nesting survives the full pipeline, not
/// just the typed pre-seal package.
#[test]
fn gson_nested_class_keeps_its_parent_through_the_full_pipeline() {
    let root = fixture_root("gson");
    let src = PackageSource::new(&root, "gson", "2.11.0");
    let lineage = PackageLineageId::new(EcosystemId::new("maven"), PackageName::new("gson"));
    let table = produce(
        &JavaProducer::new(),
        &src,
        &lineage,
        &nudox_ir::foreign::Unlinked,
    )
    .unwrap_or_else(|e| panic!("gson must lower without error:\n{}", error_chain(&e)))
    .table;

    let (intro, _) = table
        .iter()
        .find(|(_, e)| e.sym().name == "FilterResult")
        .expect("ReflectionAccessFilter.FilterResult must be lowered");

    let parent_intro = table
        .parent_of(intro)
        .expect("FilterResult must have a parent (ReflectionAccessFilter)");
    let parent = table
        .get(parent_intro)
        .expect("FilterResult's parent must be a live entry");
    assert_eq!(parent.sym().name, "ReflectionAccessFilter");
}

// ── 2. Gson 2.11.0 — typed-id lowering (real oracle, precise assertions) ───

/// Generics with a bound (`TypeAdapter<T>`), a checked `throws` clause on a
/// real generic method, and an annotation type's structural lowering are all
/// exercised against the real oracle's output for a real library — not the
/// hand-authored JSON `src/lower.rs`'s own unit tests use.
#[test]
fn gson_type_adapter_and_annotation_richness() {
    let (pkg, cost) =
        heart::cost::measured("typed-lower/gson-2.11.0", &fixture_root("gson"), || {
            lower_fixture("gson", "gson")
        });

    // -- TypeAdapter<T>: a bounded-nothing but real generic type parameter. --
    let type_adapter = pkg
        .iter()
        .find(|(id, _)| id.is_some_and(|i| i.0 == "com.google.gson.TypeAdapter"))
        .expect("com.google.gson.TypeAdapter must be lowered")
        .1;
    match type_adapter.kind().as_owned_kind() {
        Some(Kind::Record(r)) => {
            assert!(
                !r.generics.is_empty(),
                "TypeAdapter must carry its <T> generic parameter"
            );
        }
        other => panic!("TypeAdapter must lower as an owned Record kind, got {other:?}"),
    }

    // -- A real checked exception: TypeAdapter.write(...) throws IOException. --
    let write_with_throws = pkg.iter().find(|(_, e)| {
        e.sym().name == "write"
            && matches!(
                e.kind().as_owned_kind(),
                Some(Kind::Function(f)) if !f.throws.is_empty()
            )
    });
    let (_, write_entry) = write_with_throws.expect(
        "at least one `write` method (TypeAdapter.write) must carry a nonempty Function::throws",
    );
    if let Some(Kind::Function(f)) = write_entry.kind().as_owned_kind() {
        assert_eq!(
            f.throws.len(),
            1,
            "write's throws clause has exactly one declared type"
        );
        // `java.io.IOException` is outside this extraction. It used to degrade
        // to `Type::Any`, which is what this assertion pinned; erasing every
        // foreign type that way is what made distinct overloads encode to
        // identical signature skeletons and lose 28 real Gson methods at seal.
        // It is now a named cross-package reference: the exception's own name
        // survives, so it renders and so the skeleton stays discriminating.
        match &f.throws[0] {
            Type::Nominal(Ref::Foreign { key, target }) => {
                assert_eq!(
                    key.path.as_ref(),
                    "java.io.IOException",
                    "the throws type must keep its fully-qualified source name"
                );
                assert_eq!(key.display.as_ref(), "IOException");
                assert!(
                    target.is_none(),
                    "the JDK is not in the corpus, so this must be named-but-unlinked \
                     rather than linked to a fabricated target"
                );
            }
            other => panic!(
                "`write` throws a type outside the extraction; it must survive as a \
                 named cross-package reference, got {other:?}"
            ),
        }
        assert!(
            write_entry.sym().documentation.contains("Throws:"),
            "write's documentation must carry a prose Throws: section"
        );
    }

    // -- SerializedName: a real annotation type, lowered as a Trait carrying
    //    the documented "annotation_interface" marker section. --
    let serialized_name = pkg
        .iter()
        .find(|(id, _)| id.is_some_and(|i| i.0 == "com.google.gson.annotations.SerializedName"))
        .expect("com.google.gson.annotations.SerializedName must be lowered")
        .1;
    match serialized_name.kind().as_owned_kind() {
        Some(Kind::Trait(_)) => {
            assert!(
                serialized_name
                    .sym()
                    .documentation
                    .contains("annotation_interface"),
                "SerializedName's documentation must carry the annotation_interface marker"
            );
        }
        other => panic!("SerializedName must lower as an owned Trait kind, got {other:?}"),
    }

    // -- Real javadoc: Gson's class doc genuinely mentions its own name and
    //    was extracted (not empty, not a stub). --
    let gson_class = pkg
        .iter()
        .find(|(id, _)| id.is_some_and(|i| i.0 == "com.google.gson.Gson"))
        .expect("com.google.gson.Gson must be lowered")
        .1;
    assert!(
        gson_class.sym().documentation.contains("Gson"),
        "Gson's class documentation must mention Gson; got {:?}",
        gson_class.sym().documentation,
    );

    // -- Declaration-site annotations: `Gson#toString` really is annotated
    //    `@Override` in Gson 2.11.0's source, and it must now survive into
    //    `Symbol.attrs` (previously dropped entirely — every annotation
    //    except `@Deprecated` was, before `annotation_attrs`). --
    let gson_to_string = pkg
        .iter()
        .find(|(id, e)| {
            id.is_some_and(|i| i.0.starts_with("com.google.gson.Gson#toString("))
                && matches!(e.kind().as_owned_kind(), Some(Kind::Function(_)))
        })
        .expect("Gson.toString() must be lowered as a Function")
        .1;
    assert!(
        gson_to_string
            .sym()
            .attrs
            .iter()
            .any(|a| a.token == "java.lang.Override"),
        "Gson.toString()'s real @Override annotation must survive into Symbol.attrs; got {:?}",
        gson_to_string.sym().attrs,
    );

    eprintln!(
        "gson-2.11.0 typed lowering: {} entries in {:.2}s",
        pkg.iter().count(),
        cost.wall.as_secs_f64(),
    );
}

// ── 3. "modern" fixture — records, sealed interfaces, module directives ────

/// Constructs Gson's own source does not use: `record` (with a real
/// component), `sealed interface … permits`, an interface default method,
/// and JPMS module directives (`requires`/`exports`/`uses`/`provides`).
#[test]
fn modern_fixture_captures_records_sealed_and_module_directives() {
    let (pkg, cost) = heart::cost::measured("typed-lower/modern", &fixture_root("modern"), || {
        lower_fixture("modern", "modern")
    });

    // -- record Circle(double radius): the component is a real Field entry. --
    let radius = pkg
        .iter()
        .find(|(id, _)| id.is_some_and(|i| i.0 == "com.example.modern.Circle#radius"))
        .expect("Circle's `radius` record component must be lowered as a Field");
    assert!(matches!(
        radius.1.kind().as_owned_kind(),
        Some(Kind::Field(_))
    ));

    // -- record Pair<A extends Comparable<A>, B>: a bounded generic on a record. --
    let pair = pkg
        .iter()
        .find(|(id, _)| id.is_some_and(|i| i.0 == "com.example.modern.Pair"))
        .expect("Pair must be lowered")
        .1;
    match pair.kind().as_owned_kind() {
        Some(Kind::Record(r)) => assert_eq!(
            r.generics.len(),
            2,
            "Pair<A, B> must carry two generic params"
        ),
        other => panic!("Pair must lower as Record, got {other:?}"),
    }

    // -- sealed interface Shape permits Circle, Square: doc-section carries
    //    the permitted-subtypes list (the IR has no structural `sealed
    //    permits` slot — see lower.rs's module doc for why this rides docs). --
    let shape = pkg
        .iter()
        .find(|(id, _)| id.is_some_and(|i| i.0 == "com.example.modern.Shape"))
        .expect("Shape must be lowered")
        .1;
    assert!(matches!(shape.kind().as_owned_kind(), Some(Kind::Trait(_))));
    assert!(
        shape
            .sym()
            .documentation
            .contains("Sealed; permitted subtypes")
            && shape.sym().documentation.contains("Circle")
            && shape.sym().documentation.contains("Square"),
        "Shape's documentation must record its permitted subtypes; got {:?}",
        shape.sym().documentation,
    );

    // -- default method Shape.describe(): Function::is_defaulted must be true.
    //    Match on id prefix *and* `Kind::Function`, not the id prefix alone —
    //    `describe()`'s own `$return` output-param entry is namespaced as
    //    `.../describe()/$return`, which also satisfies a bare prefix match. --
    let describe = pkg
        .iter()
        .find(|(id, e)| {
            id.is_some_and(|i| i.0.starts_with("com.example.modern.Shape#describe("))
                && matches!(e.kind().as_owned_kind(), Some(Kind::Function(_)))
        })
        .expect("Shape.describe must be lowered as a Function")
        .1;
    match describe.kind().as_owned_kind() {
        Some(Kind::Function(f)) => assert!(f.is_defaulted, "Shape.describe is a `default` method"),
        other => panic!("describe must lower as Function, got {other:?}"),
    }

    // -- enum Direction with a real body method (opposite()) and constants. --
    for constant in ["NORTH", "EAST", "SOUTH", "WEST"] {
        let id = format!("com.example.modern.Direction#{constant}");
        assert!(
            pkg.iter().any(|(pid, _)| pid.is_some_and(|i| i.0 == id)),
            "Direction.{constant} must be lowered"
        );
    }
    assert!(
        pkg.iter().any(|(id, e)| {
            id.is_some_and(|i| i.0.starts_with("com.example.modern.Direction#opposite("))
                && matches!(e.kind().as_owned_kind(), Some(Kind::Function(_)))
        }),
        "Direction.opposite() (a real enum body method) must be lowered as a Function"
    );

    // -- module-info.java: requires/exports/uses/provides directives all
    //    parsed by the oracle and lowered to a Module entry. `kinds::Module`
    //    is a unit struct with no fields, so directives ride documentation
    //    prose (`render_directive`) — assert on that content, not just that
    //    a Module entry with the right name exists. --
    let module = pkg
        .iter()
        .find(|(id, _)| id.is_some_and(|i| i.0 == "mod:com.example.modern"))
        .expect("the com.example.modern module-info.java must lower to a Module entry");
    assert_eq!(module.1.sym().name, "com.example.modern");
    let module_doc = &module.1.sym().documentation;
    for expected in [
        "requires `java.logging`",
        "exports `com.example.modern`",
        "exports `com.example.modern.spi` to com.example.consumer",
        "uses `com.example.modern.Shape`",
        "provides `com.example.modern.Shape` with com.example.modern.Square",
    ] {
        assert!(
            module_doc.contains(expected),
            "module directive {expected:?} must appear in the Module entry's \
             documentation; got {module_doc:?}"
        );
    }

    eprintln!(
        "modern fixture typed lowering: {} entries in {:.2}s",
        pkg.iter().count(),
        cost.wall.as_secs_f64(),
    );
}

// ── 4. Adversarial: version support is honest, not aspirational ────────────

/// `--release 8` must genuinely reject Java 16+ `record` syntax — proving
/// the flag actually gates the source level rather than being a label the
/// oracle ignores. Calls `oracle::run_json` directly (the same helper
/// `JavaProducer::invoke` uses) with an explicit `--release`, rather than
/// mutating `NUDOX_JAVA_RELEASE` — env vars are process-global and `cargo
/// test` runs tests concurrently, so mutating one from a test would race
/// every other test that invokes the real oracle in the same process.
#[test]
fn release_8_genuinely_rejects_java_16_plus_record_syntax() {
    let circle = fixture_root("modern").join("com/example/modern/Circle.java");
    assert!(
        circle.is_file(),
        "fixture file must exist: {}",
        circle.display()
    );

    let classes_dir = std::env::var("NUDOX_JAVA_ORACLE_CLASSES")
        .unwrap_or_else(|_| concat!(env!("OUT_DIR"), "/classes").to_owned());

    let (result, _cost) = heart::cost::measured(
        "release-gate/java8-vs-record",
        &fixture_root("modern"),
        || {
            oracle::run_json::<Extraction, _, _, _>(
                "java-javadoc-test/1",
                "javadoc",
                [
                    "-quiet".to_owned(),
                    "-doclet".to_owned(),
                    "nudox.oracle.Extractor".to_owned(),
                    "-docletpath".to_owned(),
                    classes_dir,
                    "--release".to_owned(),
                    "8".to_owned(),
                    circle.to_string_lossy().into_owned(),
                ],
            )
        },
    );

    let err = result.expect_err("--release 8 must reject a `record` declaration");
    match err {
        ProducerError::OracleExit { stderr, .. } => {
            assert!(
                stderr.contains("records are not supported") || stderr.contains("-source 8"),
                "expected javac's real 'records are not supported in -source 8' \
                 diagnostic; got stderr: {stderr:?}"
            );
        }
        other => panic!("expected ProducerError::OracleExit, got {other:?}"),
    }
}

/// JEP 467 (`///` Markdown javadoc) honesty check, adaptive to whatever JDK
/// is actually running this test: on JDK < 23 `///` is not a doc comment at
/// all (verified: `Elements.getDocComment` returns `null`), so this asserts
/// the documented *current* limitation; on JDK 23+ it asserts the feature
/// actually works, so this test does not silently start lying the day the
/// toolchain is upgraded.
#[test]
fn markdown_doc_comment_support_matches_the_running_jdk() {
    let root = fixture_root("markdown-doc-probe");
    let src = PackageSource::new(&root, "markdown-doc-probe", "test");
    let extraction = JavaProducer::new().invoke(&src).unwrap_or_else(|e| {
        panic!(
            "markdown-doc-probe oracle invocation must succeed:\n{}",
            error_chain(&e)
        )
    });

    let major: u32 = extraction
        .java_version
        .split('.')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    assert!(
        major > 0,
        "oracle must report a parseable java_version, got {:?}",
        extraction.java_version
    );

    let probe = extraction
        .types
        .iter()
        .find(|t| t.qualified_name.as_ref() == "com.example.MarkdownDocProbe")
        .expect("MarkdownDocProbe must be lowered by the oracle");

    if major >= 23 {
        assert_eq!(
            probe.doc_kind.as_deref(),
            Some("END_OF_LINE"),
            "on JDK {major} (>= 23), `///` must be reported as Markdown (END_OF_LINE)"
        );
        assert!(
            probe.doc.as_deref().is_some_and(|d| !d.trim().is_empty()),
            "on JDK {major}, the /// doc comment text must actually be captured"
        );
    } else {
        // The documented, verified-empirically JDK 21 reality: `///` is an
        // ordinary line comment to javac before JDK 23, so it is never
        // retained as a doc comment at all.
        assert_eq!(
            probe.doc, None,
            "on JDK {major} (< 23), `///` is not recognized as a doc comment at \
             all — if this now captures text, JEP 467 support has changed and \
             this test's else-branch (and the producer module doc) need updating"
        );
        assert_eq!(probe.doc_kind, None);
    }
}

/// A package root with zero `.java` files is a typed error, not a silent
/// empty success — an empty extraction and "we never found any sources"
/// must not be observably the same outcome (see `discover_java_sources`'s
/// doc comment).
#[test]
fn empty_source_tree_is_a_typed_error_not_a_silent_empty_success() {
    let dir = tempdir();
    let src = PackageSource::new(dir.path(), "nothing-here", "0.0.0");
    let err = JavaProducer::new()
        .invoke(&src)
        .expect_err("a package root with no .java files must not succeed");
    assert!(
        matches!(err, ProducerError::OracleSpawn { .. }),
        "expected ProducerError::OracleSpawn naming the empty root, got {err:?}"
    );
}

/// Minimal temp-dir helper (no `tempfile` dependency in this crate).
fn tempdir() -> TempDir {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "nudox-languages-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default(),
    ));
    std::fs::create_dir_all(&path).expect("create temp dir");
    TempDir(path)
}

struct TempDir(PathBuf);

impl TempDir {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
