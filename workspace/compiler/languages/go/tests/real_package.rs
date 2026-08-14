//! Real, third-party Go package lowering — end to end through `impl Producer
//! for GoProducer`.
//!
//! Every other Go producer test in this crate runs against embedded,
//! hand-authored oracle JSON (`src/tests/lowering.rs`, `tests/snapshot.rs`).
//! Hand fixtures are fast and exercise exactly the shapes their author
//! thought of — but they cannot tell us whether the real `go/types`-backed
//! oracle binary, run against source nobody on this program wrote, actually
//! produces what the lowering layer expects, or whether `Producer::invoke`
//! (a subprocess spawn through `nudox_producer::oracle::run_json`) actually
//! wires up correctly end to end. This test builds the oracle from its
//! committed Go source and runs the full `nudox_producer::produce` pipeline
//! (`invoke` → `lower` → `finish` → `seal`) against a real checkout of
//! `go.uber.org/zap`, then asserts on real, specific content in the result.
//!
//! # Why zap
//!
//! `go.uber.org/zap` (Uber's structured logging library, ~23k GitHub stars)
//! was chosen because one module of it genuinely exercises every "richness"
//! axis the mission named, with no need to stitch together multiple
//! packages:
//!
//! * **doc comments** — every exported symbol below has a real, prose doc
//!   comment, not a placeholder.
//! * **an iota-derived enum** — `zapcore.Level` (`DebugLevel = iota - 1`
//!   through `FatalLevel`), each variant individually documented.
//! * **interfaces with embedded interfaces** — `zap.Sink` embeds
//!   `zapcore.WriteSyncer` and `io.Closer`; its full method set (`Close`,
//!   `Sync`, `Write`) is only visible after embedding expansion.
//! * **an embedded struct field** — `zapcore.CheckedEntry` embeds
//!   `zapcore.Entry` anonymously.
//! * **Go 1.18+ generics** — `internal/pool.Pool[T any]`.
//! * **struct tags** — `zap.SamplingConfig`'s fields carry `json`/`yaml`
//!   tags.
//!
//! It is also small (124 `.go` files, ~1.1 MB) and has exactly one
//! non-test, non-stdlib dependency (`go.uber.org/multierr`), so `go build`
//! and `go/packages.Load` both stay fast.
//!
//! # Fixture provenance
//!
//! `result/zap-1.28.0` is an unmodified copy of the
//! `go.uber.org/zap@v1.28.0` module tree, fetched via `go mod download` and
//! copied out of the module cache (`go list -m -f '{{.Dir}}'
//! go.uber.org/zap`). `result/` is gitignored (see
//! docs/AGENTS-DOCTRINE.md's "hard-won facts" on why: it must be re-derived, not
//! assumed present, and re-derivation must be verified against a known
//! measurement — this file's assertions are that measurement).
//!
//! # Toolchain
//!
//! Requires `go` on `PATH` (present via nix in this program's devshell).
//! The oracle binary is built once, into Cargo's per-test-binary scratch
//! directory (`CARGO_TARGET_TMPDIR`), and the pipeline runs once; every
//! assertion below reads the one resulting table.

use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

use nudox_ir::{
    apply::PristineIntroTable,
    change::{EcosystemId, PackageLineageId, PackageName},
    kinds::{Const, Enum, Field, Function, Record, Trait, Variant, function::Receiver},
};
use nudox_producer::{PackageSource, produce};
use nudox_producer_go::producer::GoProducer;

// ---------------------------------------------------------------------------
// Fixture + oracle plumbing
// ---------------------------------------------------------------------------

/// Where the zap checkout lives, relative to this crate.
fn zap_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../result/zap-1.28.0")
        .canonicalize()
        .expect(
            "no checkout at result/zap-1.28.0 — see this file's module doc for how \
             it was built and re-derive it the same way",
        )
}

/// Build the oracle binary once (into Cargo's per-test-binary scratch dir).
fn oracle_bin() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let oracle_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("oracle");
        let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("nudox-go-oracle");
        let status = std::process::Command::new("go")
            .current_dir(&oracle_dir)
            .arg("build")
            .arg("-o")
            .arg(&out)
            .arg("./...")
            .status()
            .expect(
                "`go` must be on PATH to build the oracle (docs/AGENTS-DOCTRINE.md's toolchain \
                 note: Go 1.26.4 via nix)",
            );
        assert!(status.success(), "go build of the oracle failed");
        out
    })
}

fn lineage() -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("go"), PackageName::new("go.uber.org/zap"))
}

/// Run the full `Producer` pipeline once. `NUDOX_GO_ORACLE_BIN` is the only
/// piece of process-global state this file touches, and every caller sets it
/// to the same path, so parallel `#[test]` execution within this binary
/// cannot observe a torn or divergent value.
fn lower_real_zap() -> PristineIntroTable {
    let root = zap_root();
    assert!(
        root.join("go.mod").is_file(),
        "no go.mod at {} — the zap checkout is missing or incomplete",
        root.display()
    );

    // SAFETY: single-value, single-purpose env var; see the function doc.
    unsafe {
        std::env::set_var("NUDOX_GO_ORACLE_BIN", oracle_bin());
    }

    let src = PackageSource::new(&root, "go.uber.org/zap", "1.28.0");
    let lid = lineage();

    let (table, _cost) = nudox_test_support::measured("go-real-zap", &root, || {
        produce(&GoProducer, &src, &lid, &nudox_ir::foreign::Unlinked).expect(
            "go.uber.org/zap is a well-formed, dependency-light module; \
             lowering must succeed end to end",
        )
    .table});
    table
}

// ---------------------------------------------------------------------------
// The end-to-end test
// ---------------------------------------------------------------------------

#[test]
fn lowers_the_real_zap_logging_package_end_to_end() {
    let table = lower_real_zap();

    // ---- shape: this is not a stub result ----
    // 457 package-level decls were counted directly from the oracle's own
    // JSON for this exact checkout (`jq '.packages[].decls|length'`, summed
    // across all 15 packages). The table also holds every field, method,
    // param, and iota variant nested under them, so it must be substantially
    // larger — a producer that "succeeds" with a near-zero or suspiciously
    // round count is the failure this guards against (docs/AGENTS-DOCTRINE.md §4:
    // never just a non-zero count — but a *floor* tied to an independently
    // counted real number is exactly the "content" doctrine asks for).
    assert!(
        table.len() > 1000,
        "expected well over 1000 live entries for real zap (457 package-level decls \
         across 15 packages, plus their fields/methods/params/variants); got {}",
        table.len()
    );

    // ---- doc comments really resolved ----
    let (_, level_entry) = find_unique_kind::<Enum>(&table, "Level")
        .expect("zapcore.Level (the iota-based Level enum) must be declared as an Enum");
    assert!(
        level_entry.sym().documentation.contains("logging priority"),
        "Level's doc comment (\"A Level is a logging priority...\") must survive lowering \
         verbatim; got {:?}",
        level_entry.sym().documentation
    );

    // ---- iota enum: real variants with real go/types constant.Value discriminants ----
    let (level_id, _) = find_unique_kind::<Enum>(&table, "Level").unwrap();
    let level_children: Vec<&nudox_ir::entry::Entry> = table
        .children_of(level_id)
        .iter()
        .filter_map(|id| table.get(*id))
        .collect();
    let debug_level = level_children
        .iter()
        .find(|e| e.sym().name == "DebugLevel")
        .expect("DebugLevel must be a child of the Level enum");
    assert!(debug_level.downcast::<Variant>().is_some(), "DebugLevel must be a Variant");
    assert_eq!(
        debug_level.downcast::<Variant>().unwrap().body().discr.as_deref(),
        Some("-1"),
        "DebugLevel = iota - 1 must lower with its exact go/types constant.Value, \"-1\""
    );
    let fatal_level = level_children
        .iter()
        .find(|e| e.sym().name == "FatalLevel")
        .expect("FatalLevel must be a child of the Level enum");
    assert_eq!(
        fatal_level.downcast::<Variant>().unwrap().body().discr.as_deref(),
        Some("5"),
        "FatalLevel is the 6th iota value (DebugLevel=-1..FatalLevel=5)"
    );

    // ---- FINDING: the iota-enum heuristic over-includes block-mate sentinel
    // consts. `_minLevel`, `_maxLevel`, and `InvalidLevel` sit in the SAME
    // `const ( ... )` block as the seven real Level values; Go's own
    // `usesIota` sets `groupHasIota` for the whole block (not per spec), and
    // `detect_iota_enums` (lower/mod.rs) only checks `group_has_iota` + the
    // const's own type, not whether that spec's own value expression
    // mentions `iota`. `_minLevel = DebugLevel` and `InvalidLevel =
    // _maxLevel + 1` don't literally use `iota`, but they get pulled in as
    // Level "variants" anyway. This is real, oracle-verified behavior, not a
    // hypothesis — see this test's sibling assertions above for the
    // legitimate variants and the richness report for why this is a
    // heuristic-fidelity gap rather than a crash.
    let over_included: Vec<&str> = level_children
        .iter()
        .filter(|e| matches!(e.sym().name.as_str(), "_minLevel" | "_maxLevel" | "InvalidLevel"))
        .map(|e| e.sym().name.as_str())
        .collect();
    assert_eq!(
        over_included.len(),
        3,
        "documenting current (over-inclusive) behavior: _minLevel/_maxLevel/InvalidLevel \
         are all pulled in as Level variants because they share the block's groupHasIota \
         flag and the Level type, even though their own expressions don't say `iota`; \
         got {over_included:?}"
    );

    // ---- receiver methods: Level.String() has a value receiver, real doc ----
    let level_string = level_children
        .iter()
        .find(|e| e.sym().name == "String")
        .expect("Level.String() must be a child of the Level enum");
    let string_fn = level_string
        .downcast::<Function>()
        .expect("Level.String must be a Function");
    assert_eq!(
        string_fn.body().receiver,
        Some(Receiver::Owned),
        "func (l Level) String() has a value receiver, must lower to Receiver::Owned"
    );
    assert!(
        level_string.sym().documentation.contains("lower-case ASCII"),
        "Level.String's doc comment must survive lowering; got {:?}",
        level_string.sym().documentation
    );

    // ---- generics: internal/pool.Pool[T any], disambiguated from the
    // unrelated non-generic buffer.Pool (a real cross-package name collision
    // this fixture happens to contain) by generic arity. ----
    let (_, pool_entry) = table
        .iter()
        .find(|(_, e)| {
            e.sym().name == "Pool"
                && e.downcast::<Record>()
                    .map(|r| r.body().generics.len() == 1)
                    .unwrap_or(false)
        })
        .expect("internal/pool.Pool[T any] must be declared with exactly one generic param");
    let pool_record = pool_entry.downcast::<Record>().unwrap();
    assert_eq!(pool_record.body().generics.len(), 1);

    // ---- interfaces with embedded interfaces: Sink's full method set
    // (Close from io.Closer, Sync + Write from zapcore.WriteSyncer) must be
    // visible as children even though none is declared directly on Sink. ----
    let (sink_id, sink_entry) = table
        .iter()
        .find(|(_, e)| e.sym().name == "Sink")
        .expect("zap.Sink must be declared");
    assert!(sink_entry.downcast::<Trait>().is_some(), "Sink (interface) must lower to Trait");
    let sink_method_names: Vec<String> = table
        .children_of(sink_id)
        .iter()
        .filter_map(|id| table.get(*id))
        .filter(|e| e.downcast::<Function>().is_some())
        .map(|e| e.sym().name.clone())
        .collect();
    for expected in ["Close", "Sync", "Write"] {
        assert!(
            sink_method_names.iter().any(|n| n == expected),
            "Sink's embedded-interface method set (io.Closer + zapcore.WriteSyncer) \
             must include {expected} as a child; got {sink_method_names:?}"
        );
    }

    // ---- embedded struct field: CheckedEntry embeds Entry anonymously ----
    let (checked_id, checked_entry) = table
        .iter()
        .find(|(_, e)| e.sym().name == "CheckedEntry")
        .expect("zapcore.CheckedEntry must be declared");
    assert!(checked_entry.downcast::<Record>().is_some());
    let embedded_field = table
        .children_of(checked_id)
        .iter()
        .filter_map(|id| table.get(*id))
        .find(|e| e.sym().name == "Entry" && e.downcast::<Field>().is_some())
        .expect("CheckedEntry must have an Entry field child");
    assert!(
        embedded_field.sym().attrs.iter().any(|a| a.token == "embedded"),
        "CheckedEntry's embedded `Entry` field must carry the \"embedded\" AttrTok; got {:?}",
        embedded_field.sym().attrs
    );

    // ---- struct tags: SamplingConfig's fields carry real json/yaml tags ----
    let (sampling_id, _) = table
        .iter()
        .find(|(_, e)| e.sym().name == "SamplingConfig")
        .expect("zap.SamplingConfig must be declared");
    let initial_field = table
        .children_of(sampling_id)
        .iter()
        .filter_map(|id| table.get(*id))
        .find(|e| e.sym().name == "Initial" && e.downcast::<Field>().is_some())
        .expect("SamplingConfig must have an Initial field child");
    let tag = initial_field
        .sym()
        .attrs
        .iter()
        .find(|a| a.token == "tag")
        .and_then(|a| a.arg.as_deref())
        .unwrap_or_default();
    assert!(
        tag.contains(r#"json:"initial""#) && tag.contains(r#"yaml:"initial""#),
        "SamplingConfig.Initial's raw struct tag must survive lowering byte-for-byte; \
         got {tag:?}"
    );

    // ---- package-level const, for completeness (not just types/funcs) ----
    let has_a_const = table
        .iter()
        .any(|(_, e)| e.downcast::<Const>().is_some());
    assert!(has_a_const, "at least one package-level Const must be declared somewhere in zap");

    eprintln!(
        "lowered {} live entries from go.uber.org/zap@1.28.0 (15 packages) via the real oracle",
        table.len()
    );
}

/// Find the single table entry named `name` whose owned kind downcasts as
/// `K`, disambiguating real cross-package/cross-kind name collisions (this
/// fixture has two: `zapcore.Level` the type vs. `zaptest.Level` the func;
/// `buffer.Pool` vs `internal/pool.Pool`, disambiguated separately by arity
/// where `K` alone is not enough — see the generics assertion above).
fn find_unique_kind<'a, K: nudox_ir::kind::EntryKind>(
    table: &'a PristineIntroTable,
    name: &str,
) -> Option<(nudox_ir::change::IntroId, &'a nudox_ir::entry::Entry)> {
    table
        .iter()
        .find(|(_, e)| e.sym().name == name && e.downcast::<K>().is_some())
}
