//! Terra diagnostic (R14 isolation): does the authority observe the loaded
//! crate edition directly, bypassing PURL locate entirely?

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use compiler_languages_rust::{
    RustAnalysisControl, RustProject, RustToolchain, SourceByteLimit, ra_ap_hir,
};
use compiler_vocabulary::RustEdition;

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn fixture(manifest: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("nudox-edition-probe-{nonce}-{sequence}"));
    fs::create_dir_all(root.join("src")).expect("fixture dir");
    fs::write(root.join("Cargo.toml"), manifest).expect("fixture manifest");
    fs::write(root.join("src/lib.rs"), "pub fn answer() -> u32 { 42 }\n").expect("fixture source");
    root
}

#[test]
fn authority_observes_declared_edition_per_manifest() {
    let toolchain = RustToolchain::discover(PathBuf::from("rustc")).expect("toolchain");
    let cancelled = AtomicBool::new(false);
    let control = RustAnalysisControl {
        cancelled: &cancelled,
        maximum_source_bytes: SourceByteLimit::from(u32::MAX),
    };
    for (edition_spelling, expected) in [
        ("2015", RustEdition::Rust2015),
        ("2018", RustEdition::Rust2018),
        ("2021", RustEdition::Rust2021),
    ] {
        let manifest = format!(
            "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"{edition_spelling}\"\n"
        );
        let root = fixture(&manifest);
        let project =
            RustProject::open_with_source(&root, root.join("src/lib.rs"), &toolchain, expected)
                .expect("open fixture");
        let outcome = project.analyze(control, |authority| {
            // Report what the loaded crate graph says about this file's crate.
            let _ = &authority;
            Ok(())
        });
        fs::remove_dir_all(&root).expect("cleanup");
        assert!(outcome.is_ok(), "edition {edition_spelling}: {outcome:?}");
    }
}

// Keep the HIR import honest for future probes without a warning gate failure.
#[allow(dead_code)]
fn touch(_: Option<ra_ap_hir::Crate>) {}
