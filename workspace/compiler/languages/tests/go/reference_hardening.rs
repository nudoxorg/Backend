//! RED tests for the Go reference-resolution hardening pass (G2, G3, G4, G6, G7
//! from the adversarial audit of `nudox-languages`'s Go producer).
//!
//! Builds real, hermetic Go modules and runs the real `nudox-go-oracle`
//! binary over them — the same pattern as `tests/go/cross_package_implements.rs`
//! and `tests/go/real_package.rs` — because every gap here (interface
//! satisfaction on a newtype, `go/types.Uses`-table call resolution across
//! packages and from method bodies) lives in `go/types`'s own analysis, not
//! in anything a hand-authored JSON fixture could exercise honestly.
//!
//! - G2: `type Celsius float64` satisfying an interface via a method must
//!   populate `Record::super_types` (previously only `lower_struct` read
//!   `decl.implements`; `lower_newtype` silently dropped it).
//! - G3: a call whose target lives in a package this oracle invocation never
//!   loaded (a genuinely different Go module) must survive as a
//!   `record_foreign_occurrence` edge.
//! - G4: a call from inside a METHOD body must be recorded at all (the
//!   oracle used to skip method bodies outright) and resolve to the right
//!   `GoId::Member` owner.
//! - G6 (guard, no code): a field naming a type from ANOTHER PACKAGE IN THE
//!   SAME MODULE must resolve to a same-Lowering-pass `Ref::Intro` — locks
//!   the existing-correct `local`-set routing in `types::lower_type_with_lowering`.
//! - G7 (guard, no code, but also an end-to-end check of the G1 fix): an
//!   EMBEDDED stdlib field (`sync.Mutex`) must resolve to a linkable
//!   `Ref::Foreign` with a `Package` origin, through the real oracle and the
//!   real struct-embedding path — not just the synthetic unit test in
//!   `src/go/types.rs`.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};

use nudox_ir::{
    apply::PristineIntroTable,
    change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef},
    foreign::{ForeignKey, ForeignOrigin, ForeignResolver, Resolution, Unlinked},
    index::Ref,
    kinds::{Field, Record, ty::Type},
    vocab::{Confidence, ReferenceKind},
};
use nudox_languages::go::producer::GoProducer;
use nudox_languages::{PackageSource, Produced, produce};

/// Build the oracle binary once, into Cargo's per-test-binary scratch dir —
/// same pattern as `tests/go/cross_package_implements.rs`.
fn oracle_bin() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let oracle_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("oracle/go");
        let out =
            Path::new(env!("CARGO_TARGET_TMPDIR")).join("nudox-go-oracle-reference-hardening");
        let status = Command::new("go")
            .current_dir(&oracle_dir)
            .args(["build", "-o"])
            .arg(&out)
            .arg("./...")
            .status()
            .expect("`go` must be available to build the committed Go oracle");
        assert!(status.success(), "go build of the committed oracle failed");
        out
    })
}

// ── Table lookup helpers ─────────────────────────────────────────────────────

fn find_id(table: &PristineIntroTable, name: &str, is_kind: impl Fn(&nudox_ir::entry::Entry) -> bool) -> IntroId {
    table
        .iter()
        .find(|(_, e)| e.sym().name == name && is_kind(e))
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("entry `{name}` must be present"))
}

fn record_id(table: &PristineIntroTable, name: &str) -> IntroId {
    find_id(table, name, |e| e.downcast::<Record>().is_some())
}

fn field_ty(table: &PristineIntroTable, struct_name: &str, field_name: &str) -> Type {
    let sid = record_id(table, struct_name);
    let (_, entry) = table
        .iter()
        .find(|(id, e)| {
            e.sym().name == field_name
                && e.downcast::<Field>().is_some()
                && table.parent_of(*id) == Some(sid)
        })
        .unwrap_or_else(|| panic!("field `{struct_name}.{field_name}` must be present"));
    entry
        .downcast::<Field>()
        .expect("checked above")
        .body()
        .ty
        .clone()
        .unwrap_or_else(|| panic!("field `{struct_name}.{field_name}` must carry a type"))
}

// ── Fixture 1: single Go module, two packages (core + remote) ───────────────
//
// Covers G2 (Celsius/Stringer), G4 (Worker.Handle -> validate), G6 (Foo.B ->
// remote.Bar, same module), and G7 (Server's embedded sync.Mutex).

const MODULE1: &str = "example.test/nudox-go-hardening";

const CORE_GO: &str = r#"package core

import (
	"sync"

	"example.test/nudox-go-hardening/remote"
)

// G7: an embedded stdlib type must lower to a linkable Foreign field type
// (Package origin, per the G1 fix), not a bare unlinkable Namespace, and
// this must hold through the real struct-embedding path, not just the
// synthetic unit test.
type Server struct {
	sync.Mutex
}

// G6: a field naming a type from ANOTHER PACKAGE IN THE SAME MODULE must
// resolve to a same-Lowering-pass Ref::Intro, never Foreign.
type Foo struct {
	B remote.Bar
}

// G2: a newtype (non-struct, non-interface underlying type) satisfying an
// interface via a directly-declared method — `lower_newtype` used to drop
// `decl.implements` entirely.
type Celsius float64

func (c Celsius) String() string {
	return "c"
}

type Stringer interface {
	String() string
}

// G4: a call from inside a METHOD body to a same-package function. The
// oracle used to skip method bodies outright (`fn.Recv != nil` short-
// circuited before ever walking the body), so this edge was unrecorded.
type Worker struct{}

func (w *Worker) Handle() {
	validate()
}

func validate() {}
"#;

const REMOTE_GO: &str = r#"package remote

type Bar struct{}
"#;

fn single_module_lineage() -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("go"), PackageName::new(MODULE1))
}

fn produce_single_module() -> Produced {
    let dir = tempfile::tempdir().expect("create temporary Go module");
    fs::write(
        dir.path().join("go.mod"),
        format!("module {MODULE1}\n\ngo 1.22\n"),
    )
    .expect("write go.mod");
    fs::create_dir_all(dir.path().join("core")).expect("create core/ package dir");
    fs::write(dir.path().join("core/core.go"), CORE_GO).expect("write core/core.go");
    fs::create_dir_all(dir.path().join("remote")).expect("create remote/ package dir");
    fs::write(dir.path().join("remote/remote.go"), REMOTE_GO).expect("write remote/remote.go");

    // SAFETY: this test process owns the oracle invocation and writes one
    // immutable binary path before the producer starts; no other thread in
    // this test binary touches this variable concurrently (matches the
    // established pattern in `cross_package_implements.rs`).
    unsafe { std::env::set_var("NUDOX_GO_ORACLE_BIN", oracle_bin()) };

    let source = PackageSource::new(dir.path(), MODULE1, "0.1.0");
    produce(&GoProducer, &source, &single_module_lineage(), &Unlinked)
        .expect("the single-module Go fixture must lower")
}

// ── G2: newtype interface satisfaction reaches super_types ──────────────────

#[test]
fn go_newtype_interface_satisfaction_reaches_super_types() {
    let produced = produce_single_module();
    let table = produced.table;

    let (_, celsius_entry) = table
        .iter()
        .find(|(_, e)| e.sym().name == "Celsius")
        .expect("Celsius must be declared");
    let record = celsius_entry
        .downcast::<Record>()
        .expect("Celsius (a newtype) must lower to a Record");

    let super_types: Vec<Type> = record.body().super_types.to_vec();
    assert!(
        !super_types.is_empty(),
        "Celsius satisfies Stringer via a directly-declared String() method; \
         super_types must record it, but lower_newtype used to never read \
         decl.implements at all (only lower_struct did) — got {super_types:?}"
    );

    // Stringer is an interface, which lowers to a Trait entry, not a Record —
    // find it by name alone (unique in this fixture).
    let stringer_id = find_id(&table, "Stringer", |_| true);
    let names_stringer = super_types.iter().any(|ty| match ty {
        Type::Nominal(Ref::Intro(id)) => *id == stringer_id,
        _ => false,
    });
    assert!(
        names_stringer,
        "Celsius's super_types must name the same-package Stringer interface \
         as a resolved Nominal(Intro), got {super_types:?}"
    );
}

// ── G4: method-body call is recorded, with a Member owner ───────────────────

#[test]
fn go_method_body_call_is_recorded_with_member_owner() {
    let produced = produce_single_module();
    let table = produced.table;

    let handle_id = find_id(&table, "Handle", |e| {
        // A method entry is a Function whose parent is Worker; disambiguate
        // via a downcast-free name check plus parent lookup below.
        e.sym().name == "Handle"
    });
    let worker_id = record_id(&table, "Worker");
    assert_eq!(
        table.parent_of(handle_id),
        Some(worker_id),
        "the `Handle` entry found must be Worker's method, not something else \
         named Handle"
    );
    let validate_id = find_id(&table, "validate", |e| e.sym().name == "validate");

    let hit = produced.occurrences.iter().find(|(owner, occ)| {
        *owner == handle_id
            && occ.target.intro == validate_id
            && occ.target.package == single_module_lineage()
            && occ.kind == ReferenceKind::FunctionCall
    });
    assert!(
        hit.is_some(),
        "a call to `validate()` from inside `Worker.Handle`'s body must be \
         recorded as a FunctionCall occurrence owned by the METHOD (Handle), \
         resolving to the same-package `validate` function; occurrences were: \
         {:?}",
        produced
            .occurrences
            .iter()
            .map(|(o, occ)| (*o, occ.target.intro, occ.kind))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        hit.unwrap().1.confidence,
        Confidence::Oracle,
        "an oracle-resolved call (go/types' Uses table) must carry Oracle confidence"
    );
}

// ── G6 (guard): same-module cross-package field type resolves to Intro ──────

#[test]
fn go_same_module_cross_package_field_resolves_to_intro() {
    let produced = produce_single_module();
    let table = produced.table;

    let bar_id = record_id(&table, "Bar");
    match field_ty(&table, "Foo", "B") {
        Type::Nominal(Ref::Intro(id)) => assert_eq!(
            id, bar_id,
            "Foo.B (typed `remote.Bar`, same module, different package) must \
             resolve to the same-Lowering-pass Bar record"
        ),
        other => panic!(
            "Foo.B must be a resolved same-module Nominal(Intro), got {other:?} — \
             a same-module cross-package type reference must never become Foreign"
        ),
    }
}

// ── G7 (guard + G1 end-to-end): embedded stdlib field is linkable Foreign ───

#[test]
fn go_embedded_stdlib_field_is_linkable_foreign() {
    let produced = produce_single_module();
    let table = produced.table;

    // Go's embedded-field implicit name is the type's bare name: `Mutex`.
    match field_ty(&table, "Server", "Mutex") {
        Type::Nominal(Ref::Foreign { key, .. }) => {
            assert!(
                key.origin.lineage().is_some(),
                "an embedded sync.Mutex must carry a linkable origin \
                 (lineage().is_some()), got {:?}",
                key.origin
            );
            match &key.origin {
                ForeignOrigin::Package(lineage) => {
                    assert_eq!(lineage.ecosystem.as_str(), "go-stdlib");
                    assert_eq!(lineage.name.as_str(), "sync");
                }
                other => panic!("expected ForeignOrigin::Package for embedded sync.Mutex, got {other:?}"),
            }
        }
        other => panic!(
            "Server's embedded sync.Mutex field must lower to Nominal(Ref::Foreign), \
             got {other:?}"
        ),
    }
}

// ── G3: cross-*module* call survives as a Foreign occurrence ────────────────
//
// A separate, real Go module (not merely a separate package in the same
// module — see G6 above for that case, which stays Intro) so `local` on the
// Rust side genuinely does not contain the target's import path. Wired via a
// `replace` directive to a sibling temp directory: no network access, no
// GOPROXY, no go.sum required for a locally-replaced module.

#[derive(Clone)]
struct ResolveEveryForeign(StableRef);
impl ForeignResolver for ResolveEveryForeign {
    fn resolve(&self, _key: &ForeignKey) -> Resolution {
        Resolution::Resolved(self.0.clone())
    }
}

const MAIN_MODULE: &str = "example.test/nudox-go-g3";
const DEP_MODULE: &str = "example.test/nudox-go-g3-dep";

const CALLER_GO: &str = r#"package caller

import "example.test/nudox-go-g3-dep/extlib"

func Caller() {
	extlib.Callee()
}
"#;

const EXTLIB_GO: &str = r#"package extlib

func Callee() {}
"#;

#[test]
fn go_cross_module_call_survives_as_foreign_occurrence() {
    let root = tempfile::tempdir().expect("create temporary root dir");
    let dep_dir = root.path().join("dep");
    let main_dir = root.path().join("main");
    fs::create_dir_all(dep_dir.join("extlib")).expect("create dep/extlib dir");
    fs::write(
        dep_dir.join("go.mod"),
        format!("module {DEP_MODULE}\n\ngo 1.22\n"),
    )
    .expect("write dep/go.mod");
    fs::write(dep_dir.join("extlib/extlib.go"), EXTLIB_GO).expect("write dep/extlib/extlib.go");

    fs::create_dir_all(main_dir.join("caller")).expect("create main/caller dir");
    fs::write(
        main_dir.join("go.mod"),
        format!(
            "module {MAIN_MODULE}\n\ngo 1.22\n\nrequire {DEP_MODULE} v0.0.0\n\nreplace {DEP_MODULE} => ../dep\n"
        ),
    )
    .expect("write main/go.mod");
    fs::write(main_dir.join("caller/caller.go"), CALLER_GO).expect("write main/caller/caller.go");

    unsafe { std::env::set_var("NUDOX_GO_ORACLE_BIN", oracle_bin()) };

    let lineage = PackageLineageId::new(EcosystemId::new("go"), PackageName::new(MAIN_MODULE));
    let source = PackageSource::new(&main_dir, MAIN_MODULE, "0.1.0");

    let foreign_target = StableRef::new(
        PackageLineageId::new(EcosystemId::new("go-stdlib"), PackageName::new("irrelevant")),
        IntroId::from_raw([0x5a; 32]),
    );
    let resolver = ResolveEveryForeign(foreign_target.clone());
    let produced = produce(&GoProducer, &source, &lineage, &resolver)
        .expect("the two-module Go fixture must lower");
    let table = produced.table;

    let caller_id = find_id(&table, "Caller", |e| e.sym().name == "Caller");

    let hit = produced.occurrences.iter().find(|(owner, occ)| {
        *owner == caller_id && occ.kind == ReferenceKind::FunctionCall
    });
    assert!(
        hit.is_some(),
        "pkgA (caller) calling a free function in pkgB (extlib), a genuinely \
         different Go MODULE the oracle never loaded declarations for, must \
         survive as a record_foreign_occurrence edge — previously the \
         producer recorded ONLY same-package function-level occurrences and \
         never called record_foreign_occurrence at all, so this edge did not \
         exist. Occurrences: {:?}",
        produced
            .occurrences
            .iter()
            .map(|(o, occ)| (*o, occ.kind))
            .collect::<Vec<_>>()
    );
    let (_, occ) = hit.unwrap();
    assert_eq!(
        occ.target, foreign_target,
        "with a populated resolver the foreign occurrence must LINK to the \
         resolver's target — proving record_foreign_occurrence actually ran \
         with a join-ready ForeignKey, not merely that some other edge exists"
    );
    assert_eq!(occ.confidence, Confidence::Oracle);
}
