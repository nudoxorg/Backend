//! docs/LIMITATIONS.md L3 / docs/ISSUES.md: enum variants (and several other
//! construction sites) hardcode `cfg: None` instead of going through
//! `symbol_parts()`, so a real `#[cfg(feature = "…")]` on a variant is dropped
//! even though the producer already knows how to compute cfg for other items.

use std::path::{Path, PathBuf};

use nudox_ir::{
    change::{EcosystemId, PackageLineageId, PackageName},
    entry::{EntryInner, Symbol, Visibility},
    foreign::Unlinked,
    kind::Kind,
    lower::Lowering,
    package::PackageId,
};
use nudox_languages::rust::RustProducer;
use nudox_languages::{PackageSource, Producer};

fn write_fixture(dir: &str, pkg_name: &str, body: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(dir);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).expect("create fixture src dir");
    std::fs::write(
        root.join("Cargo.toml"),
        format!(
            "[package]\n\
             name = \"{pkg_name}\"\n\
             version = \"0.1.0\"\n\
             edition = \"2021\"\n\
             \n\
             [lib]\n\
             path = \"src/lib.rs\"\n\
             \n\
             [features]\n\
             extra = []\n\
             \n\
             [workspace]\n"
        ),
    )
    .expect("write fixture manifest");
    std::fs::write(root.join("src/lib.rs"), body).expect("write fixture lib.rs");
    root
}

#[test]
fn cfg_on_an_enum_variant_survives_lowering() {
    let body = r#"
pub enum Choice {
    /// See [`Choice::Always`].
    #[cfg(feature = "extra")]
    Gated,
    Always,
}

pub struct Holder {
    /// See [`Choice::Always`].
    pub choice: Choice,
}
"#;
    let root = write_fixture("cfg_on_enum_variants", "cfg_enum_fix", body);
    let src = PackageSource::new(&root, "cfg_enum_fix", "0.1.0");
    let producer = RustProducer { direct_repo: false };

    let oracle = producer
        .invoke(&src)
        .unwrap_or_else(|e| panic!("fixture must load: {e}"));

    let root_sym = Symbol {
        name: "cfg_enum_fix".to_owned(),
        visibility: Visibility::Public,
        documentation: String::new(),
        source: src.root.clone(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    };
    let mut sink: Lowering<_> = Lowering::new(PackageId::path(src.root()), root_sym);
    producer
        .lower(&oracle, &mut sink)
        .unwrap_or_else(|e| panic!("fixture must lower: {e}"));
    let package = sink
        .finish()
        .unwrap_or_else(|e| panic!("fixture must be structurally sound: {e}"));

    let lineage = PackageLineageId::new(
        EcosystemId::new("cargo"),
        PackageName::new("cfg_enum_fix".to_owned()),
    );
    let table = package.seal(&lineage, &Unlinked).table;

    let gated = table
        .iter()
        .find(|(_, e)| e.sym().name == "Gated")
        .expect("variant Gated must be present (CargoFeatures::All includes feature extra)");
    assert!(
        matches!(gated.1.kind(), EntryInner::Owned(Kind::Variant(_))),
        "Gated must be a Variant"
    );
    assert!(
        gated.1.sym().cfg.is_some(),
        "#[cfg(feature = \"extra\")] on variant Gated must reach Symbol.cfg, not be hardcoded None"
    );
    assert!(
        !gated.1.sym().doc_links.is_empty(),
        "doc links on enum variants must reach Symbol.doc_links"
    );

    let field = table
        .iter()
        .find(|(_, e)| e.sym().name == "choice")
        .expect("Holder::choice field must be present");
    assert!(
        !field.1.sym().doc_links.is_empty(),
        "doc links on fields must reach Symbol.doc_links"
    );
}
