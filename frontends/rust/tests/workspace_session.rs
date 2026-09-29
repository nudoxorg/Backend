//! Exercises fail-closed rust-analyzer workspace admission and package transactions.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use backend_frontend_rust::legacy::{
    RustAnalysisControl, RustFeatureControl, RustToolchain, RustWorkspace, RustWorkspaceFile,
    RustWorkspaceSessionKey, RustWorkspaceSessionLane, SourceByteLimit,
};
use backend_semantic::vocabulary::{RustEdition, Stage};
use ra_ap_syntax::AstNode;

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[test]
fn workspace_lane_reloads_for_nested_module_and_discards_failed_transaction()
-> Result<(), Box<dyn std::error::Error>> {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root =
        std::env::temp_dir().join(format!("backend-rust-workspace-session-{nonce}-{sequence}"));
    fs::create_dir_all(root.join("src/foo"))?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"session_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )?;
    let root_source = concat!(
        "mod sibling;\n",
        "mod foo { mod bar; pub fn value() -> u8 { bar::value() } }\n",
        "pub fn value() -> u8 { sibling::value().wrapping_add(foo::value()) }\n",
    );
    let disk_sibling = "pub fn value() -> u8 { 1 }\n";
    fs::write(root.join("src/lib.rs"), root_source)?;
    fs::write(root.join("src/sibling.rs"), disk_sibling)?;

    let outcome = (|| {
        let toolchain = RustToolchain::discover(rustc_path())?;
        let source_paths = [PathBuf::from("src/lib.rs"), PathBuf::from("src/sibling.rs")];
        let overlay_sibling = "pub fn value() -> u8 { 2 }\n";
        let failed_sibling = "pub fn value() -> u8 { 3 }\n";
        let make_key = || {
            RustWorkspaceSessionKey::new(
                &root,
                &toolchain,
                RustEdition::Rust2024,
                Stage::LowerIr,
                RustFeatureControl::default(),
                Some([1; 32]),
                Some([2; 32]),
                Some([3; 32]),
                [4; 32],
                &source_paths,
            )
        };
        let original_key = make_key()?;
        let missing_source_paths = [
            PathBuf::from("src/lib.rs"),
            PathBuf::from("src/not-created.rs"),
        ];
        assert!(
            RustWorkspaceSessionKey::new(
                &root,
                &toolchain,
                RustEdition::Rust2024,
                Stage::LowerIr,
                RustFeatureControl::default(),
                Some([1; 32]),
                Some([2; 32]),
                Some([3; 32]),
                [4; 32],
                &missing_source_paths,
            )
            .is_err()
        );
        fs::write(root.join("src/sibling_renamed.rs"), disk_sibling)?;
        let renamed_source_paths = [
            PathBuf::from("src/lib.rs"),
            PathBuf::from("src/sibling_renamed.rs"),
        ];
        let renamed_key = RustWorkspaceSessionKey::new(
            &root,
            &toolchain,
            RustEdition::Rust2024,
            Stage::LowerIr,
            RustFeatureControl::default(),
            Some([1; 32]),
            Some([2; 32]),
            Some([3; 32]),
            [4; 32],
            &renamed_source_paths,
        )?;
        assert_ne!(
            original_key, renamed_key,
            "a source rename must change the session key"
        );
        let running = AtomicBool::new(false);
        let control = || RustAnalysisControl {
            cancelled: &running,
            maximum_source_bytes: SourceByteLimit::from(8_192),
            deadline: Instant::now() + Duration::from_secs(180),
        };
        let package_frontier = |sibling: &str| {
            [
                RustWorkspaceFile {
                    relative_path: Path::new("src/lib.rs"),
                    source: root_source,
                },
                RustWorkspaceFile {
                    relative_path: Path::new("src/sibling.rs"),
                    source: sibling,
                },
            ]
        };

        let mut lane = RustWorkspaceSessionLane::default();
        let initial = package_frontier(disk_sibling);
        {
            let lease = lane.begin(make_key()?, &initial, control())?;
            let unresolved =
                nested_module_resolves(lease.workspace(), &root, root_source, control())?;
            assert!(!unresolved, "the nested module is absent on the first load");
            lease.commit();
        }
        fs::write(root.join("src/foo/bar.rs"), "pub fn value() -> u8 { 9 }\n")?;
        {
            // The package frontier is identical. A nested file appeared below
            // an existing, previously empty directory after the prior load.
            let unchanged_frontier = package_frontier(disk_sibling);
            let lease = lane.begin(make_key()?, &unchanged_frontier, control())?;
            assert!(
                nested_module_resolves(lease.workspace(), &root, root_source, control())?,
                "a fresh workspace must see a newly created nested module"
            );
            lease.commit();
        }
        assert_eq!(lane.stats().workspace_loads, 2);
        assert_eq!(lane.stats().workspace_reuses, 0);
        {
            let changed = package_frontier(overlay_sibling);
            let lease = lane.begin(make_key()?, &changed, control())?;
            let observed = lease.workspace().analyze_source(
                root.join("src/sibling.rs"),
                overlay_sibling.as_bytes(),
                control(),
                |authority| Ok(authority.source == overlay_sibling.as_bytes()),
            )?;
            assert!(observed, "RA must observe the exact admitted editor buffer");
            lease.commit();
        }
        {
            let unchanged = package_frontier(overlay_sibling);
            lane.begin(make_key()?, &unchanged, control())?.commit();
        }
        {
            let failed = package_frontier(failed_sibling);
            let _dropped_lease = lane.begin(make_key()?, &failed, control())?;
            // Dropping an uncommitted lease discards its mutated RA database.
        }
        {
            let retry = package_frontier(failed_sibling);
            let lease = lane.begin(make_key()?, &retry, control())?;
            let observed = lease.workspace().analyze_source(
                root.join("src/sibling.rs"),
                failed_sibling.as_bytes(),
                control(),
                |authority| Ok(authority.source == failed_sibling.as_bytes()),
            )?;
            assert!(
                observed,
                "a failed prior overlay must not contaminate the fresh retry workspace"
            );
            lease.commit();
        }
        {
            fs::write(
                root.join("Cargo.toml"),
                "[package]\nname = \"session_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[package.metadata]\nrevision = \"changed\"\n",
            )?;
            let dependency_changed = package_frontier(failed_sibling);
            lane.begin(make_key()?, &dependency_changed, control())?
                .commit();
        }

        let stats = lane.stats();
        assert_eq!(stats.workspace_loads, 7);
        assert_eq!(stats.workspace_reuses, 0);
        assert_eq!(stats.workspace_reuse_disabled_requests, 7);
        assert_eq!(stats.source_updates, 5);
        assert_eq!(stats.failed_transactions, 1);
        assert!(stats.workspace_load_nanos > 0);
        assert!(stats.source_update_nanos > 0);
        Ok::<(), Box<dyn std::error::Error>>(())
    })();

    fs::remove_dir_all(&root)?;
    outcome
}

fn nested_module_resolves(
    workspace: &RustWorkspace,
    root: &Path,
    source: &str,
    control: RustAnalysisControl<'_>,
) -> Result<bool, backend_frontend_rust::legacy::RustAuthorityError> {
    workspace.analyze_source(
        root.join("src/lib.rs"),
        source.as_bytes(),
        control,
        |authority| {
            Ok(authority.paths().any(|path| {
                authority
                    .span(path.syntax())
                    .ok()
                    .and_then(|span| authority.source_at(span).ok())
                    .is_some_and(|spelling| spelling == b"bar::value")
                    && authority.resolve_path(&path).is_some()
            }))
        },
    )
}

fn rustc_path() -> PathBuf {
    std::env::var_os("RUSTC").map_or_else(|| PathBuf::from("rustc"), PathBuf::from)
}
