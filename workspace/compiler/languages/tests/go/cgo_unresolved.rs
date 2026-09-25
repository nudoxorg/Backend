//! Incomplete cgo types must seal, and the names the oracle could not expand
//! must stay visible. Swallowing the panic and emitting an empty package is
//! the failure this guards against.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};

use nudox_ir::{
    change::{EcosystemId, PackageLineageId, PackageName},
    id::PackageId,
    prelude::Kind,
};
use nudox_languages::{PackageSource, go::producer::GoProducer, produce};

const PAYLOAD: &str = r#"{
  "schemaVersion": 3,
  "packages": [
    {
      "importPath": "example.com/cgo",
      "name": "cgo",
      "decls": [
        {
          "kind": "type",
          "name": "Conn",
          "underlying": { "kind": "invalid", "name": "example.com/cgo.Conn" }
        }
      ],
      "unresolvedCgo": ["example.com/cgo.Conn", "C.sqlite3"]
    }
  ]
}"#;

#[test]
fn unresolved_cgo_seals_and_lists_the_foreign_name() {
    let lineage =
        PackageLineageId::new(EcosystemId::new("go"), PackageName::new("example.com/cgo"));
    let pkg = GoProducer
        .lower_bytes(
            PAYLOAD.as_bytes(),
            PackageId::path("example.com/cgo"),
            &lineage,
        )
        .expect("an incomplete cgo type must seal, not fail finish");

    let names: Vec<&str> = pkg
        .iter()
        .map(|(_, entry)| entry.sym().name.as_str())
        .collect();
    assert_eq!(
        names.iter().filter(|name| **name == "Conn").count(),
        1,
        "the local type is already declared; listing it again is Duplicate: {names:?}"
    );
    assert!(
        names.contains(&"C.sqlite3"),
        "the foreign cgo name must be a symbol in the sealed package: {names:?}"
    );

    let foreign = pkg
        .iter()
        .find(|(_, entry)| {
            entry.sym().name == "(inner)" && entry.sym().documentation == "unresolved cgo type"
        })
        .map(|(_, entry)| entry)
        .expect("C.sqlite3 must carry a type, not only a name");
    let Kind::Field(field) = foreign.kind().as_owned_kind().expect("owned field") else {
        panic!("unresolved cgo inner must be a field");
    };
    let rendered = field
        .ty
        .as_ref()
        .map(|ty| ty.to_string())
        .unwrap_or_default();
    assert!(
        rendered.contains("sqlite3"),
        "the foreign name must survive on the type, not collapse to a bare gap: {rendered}"
    );

    let local = pkg
        .iter()
        .find(|(_, entry)| {
            entry.sym().name == "(inner)" && entry.sym().documentation == "(underlying type field)"
        })
        .map(|(_, entry)| entry)
        .expect("Conn's own underlying type must still be lowered");
    let Kind::Field(field) = local.kind().as_owned_kind().expect("owned field") else {
        panic!("Conn inner must be a field");
    };
    let rendered = field
        .ty
        .as_ref()
        .map(|ty| ty.to_string())
        .unwrap_or_default();
    assert!(
        rendered.contains("oracle-gap"),
        "a local incomplete type's underlying must be an oracle gap: {rendered}"
    );
}

/// Go allows several blank fields in one struct (`_ int; _ string`). They do
/// not bind a name, so the field id cannot be the name `_` or `finish`
/// returns Duplicate.
#[test]
fn blank_struct_fields_do_not_share_one_id() {
    let payload = r#"{
      "schemaVersion": 3,
      "packages": [{
        "importPath": "example.com/pad",
        "name": "pad",
        "decls": [{
          "kind": "type",
          "name": "Pad",
          "underlying": {
            "kind": "struct",
            "fields": [
              { "name": "_", "type": { "kind": "basic", "name": "int" } },
              { "name": "_", "type": { "kind": "basic", "name": "string" } }
            ]
          }
        }]
      }]
    }"#;
    let lineage = PackageLineageId::new(EcosystemId::new("go"), PackageName::new("example.com/pad"));
    let pkg = GoProducer
        .lower_bytes(
            payload.as_bytes(),
            PackageId::path("example.com/pad"),
            &lineage,
        )
        .expect("blank fields must seal, not Duplicate");
    let blanks = pkg
        .iter()
        .filter(|(_, entry)| entry.sym().name == "_")
        .count();
    assert_eq!(blanks, 2, "both blank fields must be declared");
}

fn oracle_bin() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let oracle_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("oracle/go");
        let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("nudox-go-oracle-cgo");
        let status = Command::new("go")
            .current_dir(&oracle_dir)
            .args(["build", "-o"])
            .arg(&out)
            .arg(".")
            .status()
            .expect("go must be on PATH to build the oracle");
        assert!(status.success(), "go build of the oracle failed");
        out
    })
}

/// A module that imports "C" and names an incomplete C struct must seal.
/// Disabling cgo is not the fix: the C names stay in the package as gaps.
#[test]
fn a_real_cgo_module_seals() {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("cgo-mod");
    std::fs::create_dir_all(&root).expect("tmpdir");
    std::fs::write(root.join("go.mod"), "module example.com/cgo\n\ngo 1.22\n").expect("go.mod");
    std::fs::write(
        root.join("conn.go"),
        r#"package cgo

/*
typedef struct sqlite3 sqlite3;
*/
import "C"

// Conn wraps an incomplete cgo pointer.
type Conn struct {
	db *C.sqlite3
}
"#,
    )
    .expect("conn.go");

    // SAFETY: this test binary is the only one that sets the variable, and
    // every test in it points at the same oracle.
    unsafe {
        std::env::set_var("NUDOX_GO_ORACLE_BIN", oracle_bin());
    }

    let src = PackageSource::new(&root, "example.com/cgo", "0.0.0");
    let lineage =
        PackageLineageId::new(EcosystemId::new("go"), PackageName::new("example.com/cgo"));
    let table = produce(&GoProducer, &src, &lineage, &nudox_ir::foreign::Unlinked)
        .expect("a cgo package must seal with its C names unresolved, not panic the oracle")
        .table;

    let names: Vec<&str> = table
        .iter()
        .map(|(_, entry)| entry.sym().name.as_str())
        .collect();
    assert!(names.contains(&"Conn"), "Conn must be declared: {names:?}");
    assert!(
        names
            .iter()
            .any(|name| name.contains("_Ctype_") || name.contains("sqlite3")),
        "the cgo type name must be listed, not dropped: {names:?}"
    );
}
