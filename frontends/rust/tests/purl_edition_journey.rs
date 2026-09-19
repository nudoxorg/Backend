//! Terra journey oracle (R14): a located edition-2015 workspace member
//! analyzes under its own declared edition through the real authority.

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use backend_frontend_rust::legacy::{
    RustAnalysisControl, RustPackageUrl, RustProject, RustToolchain, SourceByteLimit,
};
use backend_semantic::vocabulary::RustEdition;

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[test]
fn located_edition_2015_member_analyzes_under_its_declared_edition() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("nudox-purl-journey-{nonce}-{sequence}"));
    fs::create_dir_all(root.join("src")).expect("fixture dir");
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"member\"\nversion = \"0.1.0\"\nedition = \"2015\"\n\n[workspace]\nmembers = [\".\"]\n",
    )
    .expect("fixture manifest");
    fs::write(root.join("src/lib.rs"), "pub fn answer() -> u32 { 42 }\n").expect("fixture source");

    let toolchain = RustToolchain::discover(PathBuf::from("rustc")).expect("toolchain");
    let cancelled = AtomicBool::new(false);
    let parsed = RustPackageUrl::parse("cargo:member@0.1.0").expect("purl");
    let located = parsed
        .locate(&root, &toolchain, None, &cancelled)
        .expect("locate");
    assert!(located.from_workspace());
    let project: &RustProject = located.project();
    assert_eq!(project.edition, RustEdition::Rust2015, "located edition");
    let outcome = project.analyze(
        RustAnalysisControl {
            cancelled: &cancelled,
            maximum_source_bytes: SourceByteLimit::from(u32::MAX),
            deadline: Instant::now() + Duration::from_secs(180),
        },
        |_authority| Ok(()),
    );
    fs::remove_dir_all(&root).expect("cleanup");
    match outcome {
        Ok(()) => {}
        Err(error) => panic!("locate→analyze journey failed: {error}"),
    }
}
