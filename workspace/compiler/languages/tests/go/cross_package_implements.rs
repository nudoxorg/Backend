//! Cross-package Go interface satisfaction must reach the `subtypes` graph
//! edge — RED test for the gap named in `oracle/go/serialize.go`'s
//! `Implements` doc comment:
//!
//! > Implements lists in-package interfaces this named type satisfies
//! > (method-set inclusion via `types.Implements`).
//!
//! # The incident this reproduces
//!
//! Field report, 2026-08-16: `main.AuthDeps` (an interface) is satisfied by
//! `auth.Client` (a struct in a *different* package), and `main.go` visibly
//! passes an `*auth.Client` where `AuthDeps` is expected. The Go-side
//! `subtypes` edge (`AuthDeps → auth.Client`) came back empty anyway,
//! because `implementsInterfaces` (oracle/go/serialize.go) only scans the
//! declaring type's *own* package scope (`pkg.Types.Scope()`) for candidate
//! interfaces — never the other packages the same module loaded. Go is
//! structurally typed, so a type never *names* the interface it satisfies;
//! this edge is the only way to answer "what satisfies this interface" for
//! Go at all, and it silently misses every cross-package case.
//!
//! This test builds a real two-package Go module — `iface` declares
//! `Shape`, `impl` declares `Circle`, neither imports the other, and
//! `Circle` satisfies `Shape` purely by having a matching `Area() float64`
//! method — runs the *real* `nudox-go-oracle` binary over it (not a
//! hand-authored JSON fixture; the gap is in the oracle's Go-side
//! `types.Implements` scan, and a hand fixture would just assume the fix
//! already happened), and asserts `Circle`'s lowered `super_types` names
//! `Shape`.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};

use nudox_ir::{
    change::{EcosystemId, IntroId, PackageLineageId, PackageName},
    index::Ref,
    kinds::{Record, ty::Type},
};
use nudox_languages::go::producer::GoProducer;
use nudox_languages::{PackageSource, produce};

const MODULE: &str = "example.test/nudox-cross-pkg";

const IFACE_GO: &str = r#"package iface

// Shape is implemented by any type reporting an area. Declared in its own
// package, deliberately separate from anything that satisfies it.
type Shape interface {
	Area() float64
}
"#;

const IMPL_GO: &str = r#"package impl

// Circle satisfies iface.Shape purely structurally: a matching Area()
// float64 method, no import of the iface package, and no `implements`
// keyword — exactly what Go's structural typing allows, and exactly the
// case the in-package-only oracle scan cannot see.
type Circle struct {
	Radius float64
}

func (c Circle) Area() float64 {
	return 3.14159 * c.Radius * c.Radius
}
"#;

/// Build the oracle binary once, into Cargo's per-test-binary scratch dir —
/// same pattern as `tests/go/real_package.rs` and `tests/go/test_packages.rs`.
fn oracle_bin() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let oracle_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("oracle/go");
        let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("nudox-go-oracle-cross-pkg");
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

/// A temporary two-package Go module: `iface` (the interface) and `impl`
/// (the structurally-satisfying struct), with no import between them.
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("create temporary Go module");
    std::fs::write(
        dir.path().join("go.mod"),
        format!("module {MODULE}\n\ngo 1.22\n"),
    )
    .expect("write go.mod");
    std::fs::create_dir_all(dir.path().join("iface")).expect("create iface/ package dir");
    std::fs::write(dir.path().join("iface/shape.go"), IFACE_GO).expect("write iface/shape.go");
    std::fs::create_dir_all(dir.path().join("impl")).expect("create impl/ package dir");
    std::fs::write(dir.path().join("impl/circle.go"), IMPL_GO).expect("write impl/circle.go");
    dir
}

#[test]
fn go_oracle_records_cross_package_interface_satisfaction() {
    let fixture = fixture();
    // SAFETY: this test process owns the oracle invocation and writes one
    // immutable binary path before the producer starts; no other thread in
    // this test binary touches this variable concurrently.
    unsafe { std::env::set_var("NUDOX_GO_ORACLE_BIN", oracle_bin()) };

    let source = PackageSource::new(fixture.path(), MODULE, "0.1.0");
    let lineage = PackageLineageId::new(EcosystemId::new("go"), PackageName::new(MODULE));
    let produced = produce(&GoProducer, &source, &lineage, &nudox_ir::foreign::Unlinked)
        .expect("the self-contained two-package Go module must lower through the oracle");
    let table = produced.table;

    let (_circle_id, circle_entry) = table
        .iter()
        .find(|(_, e)| e.sym().name == "Circle")
        .expect("impl.Circle must be declared");
    let record = circle_entry
        .downcast::<Record>()
        .expect("Circle must lower to a Record");

    let super_types: Vec<Type> = record.body().super_types.to_vec();
    assert!(
        !super_types.is_empty(),
        "Circle structurally satisfies iface.Shape (matching Area() float64 method), \
         declared in a DIFFERENT package with no import between them; super_types \
         must record it (this is exactly what the graph's `subtypes` edge reads), \
         but got {super_types:?}. This reproduces the field-report gap: \
         implementsInterfaces only scanned the declaring type's own package scope."
    );

    let shape_id: IntroId = super_types
        .iter()
        .find_map(|ty| match ty {
            Type::Nominal(Ref::Intro(id)) => Some(*id),
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!(
                "Circle's super_types must contain a same-table Nominal(Intro) \
                 reference once sealed (both packages are in the same module, so \
                 this must NOT be a Foreign ref); got {super_types:?}"
            )
        });

    let shape_entry = table
        .get(shape_id)
        .expect("Shape's IntroId must resolve to a live entry in the sealed table");
    assert_eq!(shape_entry.sym().name, "Shape");

    let shape_pkg = table
        .parent_of(shape_id)
        .and_then(|pid| table.get(pid))
        .expect("Shape must have a package/module parent");
    assert_eq!(
        shape_pkg.sym().name,
        format!("{MODULE}/iface"),
        "Shape's super_type reference must resolve into the `iface` package — proving \
         this is genuinely cross-package resolution, not a same-package coincidence"
    );
}
