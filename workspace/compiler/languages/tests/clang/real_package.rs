//! Lower a real, on-disk, multi-file C "package" end to end through
//! [`ClangProducer`] — the full [`nudox_languages::produce`] pipeline
//! (`invoke` → `lower` → `Lowering::finish`), not the in-memory single-string
//! snippets `src/tests/mod.rs`'s unit tests parse via `extract_unsaved`.
//!
//! # Why this fixture, not a vendored upstream project
//!
//! Unlike `result/` for the Rust producer (docs/LIMITATIONS.md L9), this
//! repository has no established convention for vendoring a real C/C++
//! package as a fixture, and adding one (a new gitignored directory, a
//! provenance note, a known-good measurement to guard against silent
//! corruption — see docs/AGENTS-DOCTRINE.md §8's `log-0.4.33` incident) is a
//! bigger commitment than this module's scope. This fixture is instead
//! authored here, generated fresh into a `tempfile::TempDir` per test run —
//! hermetic, no network, nothing to accidentally `rm -rf` — but it is a
//! **structurally real** multi-file C project: a `src/`/`include/` split
//! (the single most common real-world C layout), a header shared by two
//! translation units, and a genuine `compile_commands.json` shaped exactly
//! like what `cmake -DCMAKE_EXPORT_COMPILE_COMMANDS=ON` or `bear` would
//! write. It exercises the real on-disk discovery path
//! (`producer::find_sources`'s directory walk, one `extract_file` call per
//! real file, cross-TU USR dedup in `producer::merge_oracle`) that no
//! in-memory unsaved-file unit test touches.
//!
//! # The gap this fixture demonstrates
//!
//! `point_distance` and `default_shape` are defined in `src/*.c`, but their
//! signatures name `Point`/`ShapeKind`, which are declared only in
//! `include/mathutils.h` — reached via `#include "mathutils.h"`, a quote
//! include that only searches the *including file's own directory* by
//! default, i.e. `src/`, not `include/`. Resolving it requires the real
//! build's `-Iinclude` flag. Without `compile_commands.json` ingestion, that
//! flag does not exist anywhere `find_sources` can see, libclang cannot
//! resolve the header, both functions become invalid declarations (an
//! unknown type in the signature), and the oracle has zero functions instead
//! of two. `compile_commands.json`-driven `-I` resolution is what turns that
//! into two correctly-typed functions.
//!
//! # What the "without" side asserts, and why it changed
//!
//! It used to assert that the zero-function run came back as a *successful*
//! empty table — "no error, just an empty result indistinguishable from
//! 'this package truly has no public API'", docs/LIMITATIONS.md L2's shape. That
//! indistinguishability was the defect, not the contract, and
//! [`nudox_languages::YieldContract`] closed it: [`produce`] now holds every
//! producer to what it said it would contribute, so a run that ends with
//! only the synthesized root is [`ProducerError::NoDeclarationsContributed`].
//!
//! The `RootOnly` escape hatch that error names is **not** available to this
//! producer, and the pairing in this file is the proof:
//! [`nudox_languages::Producer::yield_contract`] takes only `&self` — no
//! [`PackageSource`] — so it is one standing claim about the producer, not a
//! per-package one. `ClangProducer` declaring `RootOnly` would make the
//! *with*-`compile_commands.json` test in this same file fail as
//! [`ProducerError::YieldContractOutgrown`], because the same producer
//! contributes two functions there. Clang is not degraded; this one input is
//! unanalysable. That distinction is exactly what the typed error carries and
//! an empty `Ok` table did not.

use std::path::Path;

use nudox_ir::change::{EcosystemId, PackageLineageId, PackageName};
use nudox_languages::{PackageSource, Produced, ProducerError, ProducerId, produce};
use nudox_languages::clang::ClangProducer;

/// `clang::Clang` (which `ClangProducer::invoke` constructs internally)
/// allows only one instance in the whole process at a time — see
/// `src/tests/mod.rs`'s `require_clang` doc comment for the full account.
/// `cargo test`'s default harness runs both `#[test]` fns in this file
/// concurrently on separate threads, in this file's own separate test
/// binary (Rust compiles `tests/*.rs` as independent binaries, so this is
/// not shared with `src/tests/mod.rs`'s own guard); without serializing
/// here too, one of the two loses the race for the singleton and fails with
/// `"an instance of Clang already exists"` instead of exercising the
/// fixture. Held for each test's whole body via the returned guard.
static CLANG_SINGLETON: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Write the fixture project into `root`. `with_compile_commands` controls
/// whether the real build's `-Iinclude` flag is discoverable at all — see
/// module docs for what that flag resolves.
fn write_fixture(root: &Path, with_compile_commands: bool) {
    std::fs::create_dir_all(root.join("include")).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();

    std::fs::write(
        root.join("include/mathutils.h"),
        r#"
#ifndef MATHUTILS_H
#define MATHUTILS_H

typedef struct Point {
    double x;
    double y;
} Point;

typedef enum ShapeKind {
    SHAPE_CIRCLE,
    SHAPE_SQUARE
} ShapeKind;

double point_distance(const Point* a, const Point* b);

#endif
"#,
    )
    .unwrap();

    std::fs::write(
        root.join("src/point.c"),
        r#"
#include "mathutils.h"

double point_distance(const Point* a, const Point* b) {
    double dx = a->x - b->x;
    double dy = a->y - b->y;
    return dx * dx + dy * dy;
}
"#,
    )
    .unwrap();

    std::fs::write(
        root.join("src/shapes.c"),
        r#"
#include "mathutils.h"

ShapeKind default_shape(void) {
    return SHAPE_CIRCLE;
}
"#,
    )
    .unwrap();

    if with_compile_commands {
        let root_str = root.to_str().unwrap();
        let db = serde_json::json!([
            {
                "directory": root_str,
                "file": "src/point.c",
                "arguments": ["cc", "-Iinclude", "-std=c11", "-c", "-o", "point.o", "src/point.c"],
            },
            {
                "directory": root_str,
                "file": "src/shapes.c",
                "arguments": ["cc", "-Iinclude", "-std=c11", "-c", "-o", "shapes.o", "src/shapes.c"],
            },
        ]);
        std::fs::write(
            root.join("compile_commands.json"),
            serde_json::to_string_pretty(&db).unwrap(),
        )
        .unwrap();
    }
}

/// Run the whole pipeline over the fixture at `root` and hand back the raw
/// `Result`.
///
/// Deliberately not unwrapped here: both tests in this file are *about* the
/// outcome — one asserts on the content of the success, the other on the typed
/// variant of the failure — so swallowing either into a panic inside this
/// helper would put the thing under test out of the test's reach.
fn produce_fixture(root: &Path, name: &str) -> Result<Produced, ProducerError> {
    let source = PackageSource::new(root, name, "0.0.0");
    let lineage = PackageLineageId::new(EcosystemId::new("cpp"), PackageName::new(name));
    produce(
        &ClangProducer::new(),
        &source,
        &lineage,
        &nudox_ir::foreign::Unlinked,
    )
}

/// The whole `#[source]` chain, joined — docs/AGENTS-DOCTRINE.md §8: a
/// `ProducerError`'s top-level `Display` is deliberately terse, and reading
/// only it is how a five-second diagnosis becomes an hour.
fn chain(err: &ProducerError) -> String {
    let mut links: Vec<String> = vec![err.to_string()];
    let mut cause = std::error::Error::source(err);
    while let Some(c) = cause {
        links.push(c.to_string());
        cause = c.source();
    }
    links.join(" <- ")
}

fn lower_fixture(root: &Path, name: &str) -> Produced {
    produce_fixture(root, name)
        .unwrap_or_else(|err| panic!("clang producer failed: {}", chain(&err)))
}

#[test]
fn real_multi_file_c_package_lowers_end_to_end_with_compile_commands_json() {
    let _guard = CLANG_SINGLETON
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = tempfile::tempdir().expect("tempdir");
    write_fixture(dir.path(), true);

    // `measured()` itself prints the `cost case=… ` line the corpus report
    // parses (docs/AGENTS-DOCTRINE.md §4) — nothing further to emit here.
    let (produced, _cost) =
        heart::cost::measured("clang/real_multi_file_c_package", dir.path(), || {
            lower_fixture(dir.path(), "fixture-mathutils")
        });

    // The other half of the pairing the module docs describe: this run is what
    // makes `YieldContract::RootOnly` unavailable to `ClangProducer`, so pin
    // it. If someone "fixes" the sibling test below by declaring the producer
    // degraded, this assertion — not a message match — is what rejects it.
    assert!(
        produced.contract.degraded().is_none(),
        "clang analysed this package and declared two functions from it, so its \
         yield contract must be `Declarations`, not a degradation: {:?}",
        produced.contract
    );

    let table = produced.table;
    let names: Vec<String> = table.iter().map(|(_, e)| e.sym().name.clone()).collect();
    assert!(
        names.iter().any(|n| n == "point_distance"),
        "point_distance must be lowered once headers resolve; got {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "default_shape"),
        "default_shape must be lowered once headers resolve; got {names:?}"
    );
}

#[test]
fn same_fixture_without_compile_commands_json_fails_as_no_declarations_contributed() {
    // The paired "before" case: same project, same real `-Iinclude`
    // requirement, but with nothing to tell the producer about it. Both
    // functions' signatures name a type (`Point`/`ShapeKind`) libclang cannot
    // resolve without the header, so libclang marks the declarations invalid
    // and `extract::visit_entity` skips them. The oracle is therefore empty,
    // and the lowering holds nothing but the root `produce` synthesized —
    // which is byte-for-byte what a producer that never opened the directory
    // yields. `produce` refuses to hand that back as a success.
    //
    // Asserting the *variant* rather than "the names are absent" is the
    // stronger form (docs/AGENTS-DOCTRINE.md §4: never assert on a message string
    // where you can assert on a typed variant, and a test that would pass
    // against a stub is not a test). The old assertion — "neither name is
    // present" — was satisfied by a table containing only the root, and would
    // equally have been satisfied by a producer that was deleted.
    let _guard = CLANG_SINGLETON
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = tempfile::tempdir().expect("tempdir");
    write_fixture(dir.path(), false);

    let (result, _cost) = heart::cost::measured(
        "clang/real_multi_file_c_package_no_compile_commands",
        dir.path(),
        || produce_fixture(dir.path(), "fixture-mathutils-no-db"),
    );

    match result {
        Err(ProducerError::NoDeclarationsContributed {
            package, producer, ..
        }) => {
            assert_eq!(
                package, "fixture-mathutils-no-db",
                "the failure must name the package it was analysing"
            );
            assert_eq!(
                producer,
                ProducerId("clang-libclang/1"),
                "the failure must name the producer that promised declarations"
            );
        }
        Err(other) => panic!(
            "without compile_commands.json the run must fail as \
             NoDeclarationsContributed — the one variant that says 'we did not \
             actually look at this package'. Got: {}",
            chain(&other)
        ),
        Ok(produced) => {
            let names: Vec<String> = produced
                .table
                .iter()
                .map(|(_, e)| e.sym().name.clone())
                .collect();
            panic!(
                "without compile_commands.json neither function's signature can \
                 resolve Point/ShapeKind, so the producer contributes nothing and \
                 `produce` must reject the run rather than return a table that \
                 looks like a successfully documented empty library. Got Ok with \
                 contract {:?} and names {names:?}",
                produced.contract
            );
        }
    }
}
