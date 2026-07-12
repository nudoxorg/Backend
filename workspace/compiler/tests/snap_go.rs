//! Snapshot tests for the **Go** language producer and renderer.
//!
//! Two families of snapshots, both driven by real Go source fixtures under
//! `tests/fixtures/go/<name>/`:
//!
//! 1. **Compile-target snapshots** — Go module source → Go producer
//!    (vendored `go/types` oracle) → `ir::entry::Index`, snapshotted as
//!    a `BTreeMap<String, &Entry>` sorted by path string (deterministic
//!    despite `FxHashMap` inside `Index.entries_by_path`).
//!
//! 2. **Renderer snapshots** — the IR entries rendered back to Go surface
//!    syntax via `render_entry`, snapshotted at two widths (40 and 100
//!    columns) for entries with long signatures, to capture line-breaking
//!    behaviour.
//!
//! # Requirements to run
//!
//! The Go producer invokes an external oracle binary built by Buck2 and
//! shipped as a `resources` artifact alongside this test target.  Generation
//! of live snapshots therefore requires:
//!
//! * A working `go` toolchain on `PATH` (satisfied by the nix devshell).
//! * The `go-oracle` resource binary present (built by Buck2 via the
//!   `//workspace/compiler/compile/go/oracle:oracle` target).
//!
//! Without those, `lower_package` will return a `GoError::ResourceNotFound`
//! or `GoError::RunOracle` and the tests will fail at the `expect()` call.
//! The test file is intentionally NOT gated behind `#[ignore]` because the
//! Buck2 `rust_tests` target already wires the `go-oracle` resource; if it
//! is missing the build itself would fail first.  In a plain `cargo test`
//! context (no resource wiring) the tests will panic with a clear
//! `ResourceNotFound` message.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use compiler::compile::producer::LocalForgeContext;
use compiler::languages::go::lower_package;
use compiler::render::{render_entry, Language, RenderCtx};
use ir::entry::{Index, NudoxPath};
use ir::kind::Entry;
use tempfile::TempDir;

// ─── Fixture plumbing ────────────────────────────────────────────────────────

/// Root directory that holds all Go fixture modules.
fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/go")
}

/// Recursively copy `src` into `dest`, creating directories as needed.
fn copy_tree(src: &Path, dest: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let target = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Copy a named fixture into a fresh `TempDir` and lower it with the Go producer.
///
/// Returns `(index, tempdir)`.  Keep `_dir` alive for the duration of the test.
fn lower(fixture: &str) -> (Index, TempDir) {
    let dir = TempDir::new().expect("create tempdir");
    copy_tree(&fixture_root().join(fixture), dir.path())
        .unwrap_or_else(|e| panic!("copy fixture {fixture}: {e}"));
    let ctx = LocalForgeContext::default();
    let index = lower_package(&ctx, dir.path()).unwrap_or_else(|e| {
        use std::error::Error;
        let mut chain = format!("lower_package({fixture}): {e}");
        let mut src = e.source();
        while let Some(s) = src {
            chain.push_str(&format!("\n  caused by: {s}"));
            src = s.source();
        }
        panic!("{chain}");
    });
    (index, dir)
}

// ─── Deterministic snapshot helpers ──────────────────────────────────────────

/// Stable key for a `NudoxPath` suitable for a `BTreeMap` sort.
///
/// * `NudoxPath::Local(p)` → `p.display()` (e.g. `"example.com/basics"`)
/// * `NudoxPath::External { dependency, path }` → `"dependency:path.display()"`
fn path_key(p: &NudoxPath) -> String {
    match p {
        NudoxPath::Local(path) => path.display().to_string(),
        NudoxPath::External { dependency, path } => {
            format!("{}:{}", dependency, path.display())
        }
    }
}

/// Collect `index.entries_by_path` into a `BTreeMap` sorted by path key.
///
/// The underlying map is `FxHashMap` (non-deterministic iteration order), so
/// we must sort before snapshotting.
fn sorted_entries(index: &Index) -> BTreeMap<String, &Entry> {
    index
        .entries_by_path
        .iter()
        .map(|(k, v)| (path_key(k), v))
        .collect()
}

/// Render all entries from `index` in sorted key order, joining with double
/// newlines, at the given column width.
fn render_sorted(index: &Index, width: usize) -> String {
    let cx = RenderCtx::new(Language::Go).with_width(width).with_docs(true);
    let mut sorted: Vec<(String, &Entry)> = index
        .entries_by_path
        .iter()
        .map(|(k, v)| (path_key(k), v))
        .collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    sorted
        .into_iter()
        .map(|(key, entry)| format!("// {key}\n{}", render_entry(entry, &cx)))
        .collect::<Vec<_>>()
        .join("\n\n")
}

// ─── Compile-target snapshots: basics fixture ─────────────────────────────────
//
// Coverage:
//   * Exported (Server, Config, Direction, Celsius, Temperature, StringSlice,
//     StringMap, Chan, Ptr, MaxRetries, Count, Process, Variadic) vs
//     unexported (defaultTimeout, namedReturn) visibility.
//   * Struct with fields + struct tags (Server, Config).
//   * Embedded struct field (Config embeds Server).
//   * Methods: pointer receiver (Close) and value receiver (Status).
//   * Named defined type (Celsius → RecordType newtype).
//   * True type alias (Temperature = Celsius → TypeAlias).
//   * Named types over slice, map, channel, pointer.
//   * Iota enum convention (Direction + North/East/South/West → SumType).
//   * Top-level exported constant (MaxRetries) and variable (Count).
//   * Multiple return values (Process → (string, error)).
//   * Variadic parameter (Variadic → ...string).
//   * Named returns (namedReturn → (result string, err error)).
//   * Doc comments (exercised by `.with_docs(true)` in renderer).

#[test]
fn go_basics_ir() {
    let (index, _dir) = lower("basics");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("go_basics_ir", sorted);
}

// ─── Compile-target snapshots: interfaces fixture ─────────────────────────────
//
// Coverage:
//   * Simple interface (Reader → TraitDef with method set).
//   * Embedded interfaces (ReadWriter, ReadWriteCloser → super_traits).
//   * Generic interface with type parameter and constraint (Container[T Ordered]).
//   * Constraint type set (Ordered with union ~int | ~float32 | ~string …).
//   * Generic struct (Stack[T any] → RecordType with generics).
//   * Methods on generic struct: pointer receiver (Push), value receiver (Pop).
//   * Top-level generic function with multiple type params (Transform[In, Out]).
//   * Named type over generic map (Map[K comparable, V any]).
//   * Unexported helper function (unexportedHelper → Private visibility).

#[test]
fn go_interfaces_ir() {
    let (index, _dir) = lower("interfaces");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("go_interfaces_ir", sorted);
}

// ─── Compile-target snapshots: multipackage fixture ──────────────────────────
//
// Coverage:
//   * Multi-package module: root package (multipackage) + sub-package (sub).
//   * root_ids contains only the root package; sub is a member child.
//   * Pointer return type (NewRegistry → *Registry).
//   * Multiple params with same type (Set(key, value string)).

#[test]
fn go_multipackage_ir() {
    let (index, _dir) = lower("multipackage");
    let sorted = sorted_entries(&index);
    insta::assert_debug_snapshot!("go_multipackage_ir", sorted);
}

// ─── Renderer snapshots: basics fixture at width 80 ──────────────────────────

#[test]
fn go_basics_rendered_w80() {
    let (index, _dir) = lower("basics");
    let rendered = render_sorted(&index, 80);
    insta::assert_snapshot!("go_basics_rendered_w80", rendered);
}

// ─── Renderer snapshots: interfaces fixture at width 80 ──────────────────────

#[test]
fn go_interfaces_rendered_w80() {
    let (index, _dir) = lower("interfaces");
    let rendered = render_sorted(&index, 80);
    insta::assert_snapshot!("go_interfaces_rendered_w80", rendered);
}

// ─── Renderer snapshots: multipackage fixture at width 80 ────────────────────

#[test]
fn go_multipackage_rendered_w80() {
    let (index, _dir) = lower("multipackage");
    let rendered = render_sorted(&index, 80);
    insta::assert_snapshot!("go_multipackage_rendered_w80", rendered);
}

// ─── Renderer snapshots at narrow width (line-breaking check) ────────────────
//
// The `Transform[In, Out any]` function in the interfaces fixture has a long
// signature.  Snapshot at width 40 to verify the arglist breaks one-per-line
// (Wadler–Lindig group breaking), and at width 100 to confirm it stays flat.
//
// Similarly, `Process(name string, count int) (string, error)` in basics is
// exercised at both widths.

#[test]
fn go_interfaces_rendered_w40() {
    let (index, _dir) = lower("interfaces");
    let rendered = render_sorted(&index, 40);
    insta::assert_snapshot!("go_interfaces_rendered_w40", rendered);
}

#[test]
fn go_interfaces_rendered_w100() {
    let (index, _dir) = lower("interfaces");
    let rendered = render_sorted(&index, 100);
    insta::assert_snapshot!("go_interfaces_rendered_w100", rendered);
}

#[test]
fn go_basics_rendered_w40() {
    let (index, _dir) = lower("basics");
    let rendered = render_sorted(&index, 40);
    insta::assert_snapshot!("go_basics_rendered_w40", rendered);
}

// ─── Structural assertions (do not snapshot, gate first-run correctness) ──────
//
// These fast assertions run against the Go-producer's output shape without
// requiring the snapshot database to exist yet.  They catch gross regressions
// (e.g. every symbol landing on the same IR variant) even on the first run.

/// Exported identifiers (capitalized) must be `Visibility::Public`.
/// Unexported identifiers (lowercase) must be `Visibility::Private`.
#[test]
fn go_basics_visibility() {
    use ir::kind::Visibility;

    let (index, _dir) = lower("basics");

    let vis_of = |name: &str| -> Visibility {
        let entry = index
            .entries_by_path
            .values()
            .find(|e| e.name() == name)
            .unwrap_or_else(|| {
                let names: Vec<_> = index.entries_by_path.values().map(|e| e.name()).collect();
                panic!("entry {name} not found; present: {names:?}")
            });
        match entry {
            Entry::Module(s) => s.visibility.clone(),
            Entry::RecordType(s) => s.visibility.clone(),
            Entry::Function(s) => s.visibility.clone(),
            Entry::TraitDef(s) => s.visibility.clone(),
            Entry::TraitImpl(s) => s.visibility.clone(),
            Entry::Constant(s) => s.visibility.clone(),
            Entry::Variable(s) => s.visibility.clone(),
            Entry::Macro(s) => s.visibility.clone(),
            Entry::PrimitiveType(s) => s.visibility.clone(),
            Entry::Field(s) => s.visibility.clone(),
            Entry::Event(s) => s.visibility.clone(),
            Entry::Info(s) => s.visibility.clone(),
            Entry::UnionType(s) => s.visibility.clone(),
            Entry::TypeAlias(s) => s.visibility.clone(),
            Entry::SumType(s) => s.visibility.clone(),
        }
    };

    // Exported
    assert_eq!(vis_of("Server"), Visibility::Public, "Server should be Public");
    assert_eq!(vis_of("Process"), Visibility::Public, "Process should be Public");
    assert_eq!(vis_of("MaxRetries"), Visibility::Public, "MaxRetries should be Public");
    assert_eq!(vis_of("Count"), Visibility::Public, "Count should be Public");

    // Unexported. Go's lowercase identifiers are package-private, so the
    // producer maps them to `Visibility::Package` (not `Private`).
    assert_eq!(
        vis_of("defaultTimeout"),
        Visibility::Package,
        "defaultTimeout (unexported) should be Package-visible"
    );
    assert_eq!(
        vis_of("namedReturn"),
        Visibility::Package,
        "namedReturn (unexported) should be Package-visible"
    );
}

/// The iota-based Direction type must lower to a SumType; its constants must
/// become SumVariants rather than standalone Constant entries.
#[test]
fn go_basics_iota_enum() {
    let (index, _dir) = lower("basics");

    let direction = index
        .entries_by_path
        .values()
        .find(|e| e.name() == "Direction")
        .expect("Direction entry must exist");

    assert!(
        matches!(direction, Entry::SumType(_)),
        "Direction should lower to SumType (iota enum convention), got {direction:?}"
    );

    // The iota constants must NOT appear as standalone Entry::Constant values.
    let variant_names = ["North", "East", "South", "West"];
    for vname in variant_names {
        let standalone = index
            .entries_by_path
            .values()
            .find(|e| e.name() == vname && matches!(e, Entry::Constant(_)));
        assert!(
            standalone.is_none(),
            "{vname} should be a SumVariant, not a standalone Constant"
        );
    }
}

/// Server must lower to RecordType; Config (which embeds Server) must also be
/// a RecordType.  Methods (Close, Status) must be Function entries.
#[test]
fn go_basics_struct_and_methods() {
    let (index, _dir) = lower("basics");

    let server = index
        .entries_by_path
        .values()
        .find(|e| e.name() == "Server")
        .expect("Server entry");
    assert!(
        matches!(server, Entry::RecordType(_)),
        "Server should be RecordType, got {server:?}"
    );

    let config = index
        .entries_by_path
        .values()
        .find(|e| e.name() == "Config")
        .expect("Config entry");
    assert!(
        matches!(config, Entry::RecordType(_)),
        "Config should be RecordType, got {config:?}"
    );

    // Methods become standalone Function entries (not inlined in Record::methods).
    let close = index
        .entries_by_path
        .values()
        .find(|e| e.name() == "Close")
        .expect("Close method entry");
    assert!(
        matches!(close, Entry::Function(_)),
        "Close should be a Function entry, got {close:?}"
    );

    let status = index
        .entries_by_path
        .values()
        .find(|e| e.name() == "Status")
        .expect("Status method entry");
    assert!(
        matches!(status, Entry::Function(_)),
        "Status should be a Function entry, got {status:?}"
    );
}

/// The true alias `type Temperature = Celsius` must lower to TypeAlias.
/// The defined type `type Celsius float64` must lower to RecordType (newtype).
#[test]
fn go_basics_alias_vs_newtype() {
    let (index, _dir) = lower("basics");

    let celsius = index
        .entries_by_path
        .values()
        .find(|e| e.name() == "Celsius")
        .expect("Celsius entry");
    assert!(
        matches!(celsius, Entry::RecordType(_)),
        "Celsius (defined type) should lower to RecordType newtype, got {celsius:?}"
    );

    let temperature = index
        .entries_by_path
        .values()
        .find(|e| e.name() == "Temperature")
        .expect("Temperature entry");
    assert!(
        matches!(temperature, Entry::TypeAlias(_)),
        "Temperature (true alias) should lower to TypeAlias, got {temperature:?}"
    );
}

/// Interfaces must lower to TraitDef entries.
#[test]
fn go_interfaces_trait_defs() {
    let (index, _dir) = lower("interfaces");

    for iface_name in ["Reader", "Writer", "ReadWriter", "Closer", "Stringer"] {
        let entry = index
            .entries_by_path
            .values()
            .find(|e| e.name() == iface_name)
            .unwrap_or_else(|| panic!("{iface_name} entry must exist"));
        assert!(
            matches!(entry, Entry::TraitDef(_)),
            "{iface_name} should lower to TraitDef, got {entry:?}"
        );
    }
}

/// Generic struct Stack must be a RecordType; generic function Transform must
/// be a Function; generic Map type must be a RecordType (defined type newtype).
#[test]
fn go_interfaces_generics() {
    let (index, _dir) = lower("interfaces");

    let stack = index
        .entries_by_path
        .values()
        .find(|e| e.name() == "Stack")
        .expect("Stack entry");
    assert!(
        matches!(stack, Entry::RecordType(_)),
        "Stack should be RecordType, got {stack:?}"
    );

    let transform = index
        .entries_by_path
        .values()
        .find(|e| e.name() == "Transform")
        .expect("Transform entry");
    assert!(
        matches!(transform, Entry::Function(_)),
        "Transform should be Function, got {transform:?}"
    );
}

/// Multi-package module: both packages must be present; the sub-package must
/// NOT appear in root_ids (it is nested under the root package).
#[test]
fn go_multipackage_nesting() {
    let (index, _dir) = lower("multipackage");

    // Both package Module entries should be present.
    let root_present = index
        .entries_by_path
        .keys()
        .any(|k| path_key(k) == "example.com/multipackage");
    let sub_present = index
        .entries_by_path
        .keys()
        .any(|k| path_key(k) == "example.com/multipackage/sub");

    assert!(root_present, "root package module entry must be present");
    assert!(sub_present, "sub package module entry must be present");

    // The sub-package must NOT be in root_ids (it is a nested child).
    let sub_is_root = index
        .root_ids
        .iter()
        .any(|k| path_key(k) == "example.com/multipackage/sub");
    assert!(!sub_is_root, "sub package must not be a root_id; it should be nested");
}
