//! W-Index: what a compiler-indexed multi-module crate reads as on its package
//! page.
//!
//! A semantic declaration only carries its file and line when the owner pairs
//! it with its structural twin, and the outline the compiler lane serves is
//! flat (no file-module node holds a file's names). Two things went wrong
//! together once Rust compiled for real: an `impl` block counted against its
//! type's name and cost the type its place, so a type with any `impl` carried no
//! path and no line (the `desktop-record-rust` boot panic, "no exact candidate");
//! and every name of the crate then read as one module. This indexes a real
//! crate through the real owner and asserts both, by name.

#![allow(clippy::expect_used, clippy::panic)]

use super::data;
use crate::runtime::CancellationToken;
use crate::runtime::reads::{OutlineCache, PageReader, ReadContext, ReadRequest, SessionReader};
use backend_client::Session;
use backend_library::DeclarationKind;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// `(file, contents)` of a crate with four modules, types with `impl` blocks,
/// a trait, an enum and free functions.
const FILES: [(&str, &str); 5] = [
    (
        "Cargo.toml",
        "[package]\nname = \"w-index-multi\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    ),
    (
        "src/lib.rs",
        "//! Top.\npub mod alpha;\npub mod beta;\n/// Top.\npub struct Top;\nimpl Top {\n    pub fn new() -> Self {\n        Top\n    }\n}\nimpl Default for Top {\n    fn default() -> Self {\n        Top\n    }\n}\n",
    ),
    (
        "src/alpha.rs",
        "/// A widget.\npub struct Widget;\nimpl Widget {\n    pub fn make() -> Self {\n        Widget\n    }\n}\nimpl std::fmt::Display for Widget {\n    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {\n        f.write_str(\"w\")\n    }\n}\nimpl std::fmt::Debug for Widget {\n    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {\n        f.write_str(\"W\")\n    }\n}\n/// Help.\npub fn helper() {}\n",
    ),
    (
        "src/beta/mod.rs",
        "pub mod inner;\n/// Speaks.\npub trait Speak {\n    fn speak(&self);\n}\n/// Mood.\npub enum Mood {\n    Happy,\n    Sad,\n}\n",
    ),
    (
        "src/beta/inner.rs",
        "/// Deep.\npub fn deep() {}\npub fn deeper() {}\n",
    ),
];

#[test]
fn a_multi_module_crate_reads_as_a_region_per_module_with_every_name_at_its_line() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // `/tmp`, not `temp_dir()`: under nix the latter makes the socket path too long.
    let root =
        PathBuf::from("/tmp").join(format!("nx-w-index-regions-{}-{nonce}", std::process::id()));
    crate::host::private_dir(&root).expect("private scratch root");
    let project = root.join("multi");
    for (path, contents) in FILES {
        let file = project.join(path);
        std::fs::create_dir_all(file.parent().expect("parent")).expect("directory");
        std::fs::write(file, contents).expect("file");
    }
    let paths = backend_runtime::WorkspacePaths::discover(
        Some(project.clone()),
        Some(root.join("data")),
        Some(root.with_extension("sock")),
    )
    .expect("paths");
    let host = crate::host::lease::DesktopHost::start_with_paths(paths).expect("the owner starts");
    let mut session = Session::connect(host.endpoint()).expect("session");
    session
        .index(project.to_str().expect("UTF-8"))
        .expect("the crate compiles through the real owner");

    let mut reader = SessionReader::connect(host.endpoint());
    let cancel = CancellationToken::new();
    let outlines = OutlineCache::default();
    let context = ReadContext {
        worker: 0,
        cancel: &cancel,
        outlines: &outlines,
    };
    let package = crate::model::pages::PackageRef::parse(
        &project.canonicalize().expect("canonical").to_string_lossy(),
    )
    .expect("package");
    let crate::model::pages::PageValue::Package(dossier) = reader
        .read(&ReadRequest::Package(package), &context)
        .expect("dossier")
    else {
        panic!("the package read answered with another page")
    };
    let outline = dossier.outline.known().expect("the outline is known");
    assert!(outline.complete, "the whole outline is served");

    // Every declaration says where it is: the type that has `impl` blocks as
    // much as the one that has none. Before: `path: None, line: None` on `Top`
    // and `Widget`, which is what the scene resolver could not match.
    let site = |name: &str, kind: DeclarationKind| {
        let found: Vec<_> = outline
            .walk()
            .filter(|node| {
                node.decl.name.as_ref() == name
                    && node.decl.kind == Some(kind)
                    && node.decl.path.is_some()
            })
            .collect();
        assert_eq!(
            found.len(),
            1,
            "exactly one {kind:?} `{name}` carries its place: {}",
            found.len()
        );
        (
            found[0].decl.path.as_deref().expect("path").to_owned(),
            found[0].decl.line.expect("line"),
        )
    };
    for (name, kind, file, line) in [
        ("Top", DeclarationKind::Struct, "src/lib.rs", 5),
        ("Widget", DeclarationKind::Struct, "src/alpha.rs", 2),
        ("helper", DeclarationKind::Function, "src/alpha.rs", 19),
        ("Speak", DeclarationKind::Trait, "src/beta/mod.rs", 3),
        ("Mood", DeclarationKind::Enum, "src/beta/mod.rs", 7),
        ("deep", DeclarationKind::Function, "src/beta/inner.rs", 2),
        ("deeper", DeclarationKind::Function, "src/beta/inner.rs", 3),
    ] {
        assert_eq!(
            site(name, kind),
            (file.to_owned(), line),
            "`{name}` is where the source says"
        );
    }

    // The page: a region per module, each holding its own names. Before: one
    // module named for the package, holding all of them and the `impl` blocks
    // again (`Top` three times, `Widget` four).
    let regions: Vec<(String, Vec<String>)> = {
        let mut regions: Vec<_> = data::modules(outline, "w-index-multi", None)
            .into_iter()
            .map(|module| {
                let mut names: Vec<String> = module
                    .items
                    .iter()
                    .map(|item| item.name.to_string())
                    .collect();
                names.sort();
                (module.name.to_string(), names)
            })
            .collect();
        regions.sort();
        regions
    };
    let region = |name: &str, names: &[&str]| {
        (
            name.to_owned(),
            names
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>(),
        )
    };
    assert_eq!(
        regions,
        vec![
            region("alpha", &["Widget", "helper"]),
            region("beta", &["Mood", "Speak"]),
            region("beta::inner", &["deep", "deeper"]),
            region("lib", &["Top"]),
        ],
        "one region per module, by name"
    );
    drop(session);
    drop(host);
    let _ = std::fs::remove_dir_all(&root);
}
