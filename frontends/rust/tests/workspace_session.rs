//! Exercises fail-closed rust-analyzer workspace admission and package transactions.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use backend_frontend_rust::legacy::{
    RustAnalysisControl, RustAuthorityError, RustFeatureControl, RustToolchain, RustWorkspace,
    RustWorkspaceFile, RustWorkspaceReadFrontierObserver, RustWorkspaceSessionKey,
    RustWorkspaceSessionLane, SourceByteLimit,
};
use backend_semantic::vocabulary::{RustEdition, Stage};
use ra_ap_syntax::AstNode;

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
struct RecordingReadFrontier {
    editor_buffers: std::collections::HashMap<PathBuf, Vec<u8>>,
    vfs_events: u64,
    rustdoc_input_events: u64,
    expected_root_vfs_path: PathBuf,
    expected_root_contents: Vec<u8>,
    root_content_paths: Vec<String>,
    expected_sibling_vfs_path: PathBuf,
    expected_sibling_contents: Vec<u8>,
    expected_rustdoc_input_path: PathBuf,
    expected_rustdoc_input_contents: Vec<u8>,
    saw_root_source: bool,
    saw_disk_sibling: bool,
    saw_rustdoc_input: bool,
    reject_rustdoc_input_once: bool,
    unresolved_candidates: Vec<(String, String, String)>,
    reject_candidate: Option<String>,
}

impl RustWorkspaceReadFrontierObserver for RecordingReadFrontier {
    fn observe_editor_buffer(&mut self, relative_path: &Path, contents: &[u8]) {
        self.editor_buffers
            .insert(relative_path.to_path_buf(), contents.to_vec());
    }

    fn observe_ra_vfs_file(&mut self, absolute_path: &str, contents: &[u8]) -> bool {
        self.vfs_events = self.vfs_events.saturating_add(1);
        if contents == self.expected_root_contents.as_slice() {
            if self.root_content_paths.len() < 16 {
                self.root_content_paths.push(absolute_path.to_owned());
            }
            self.saw_root_source =
                Path::new(absolute_path) == self.expected_root_vfs_path.as_path();
        }
        if Path::new(absolute_path) == self.expected_sibling_vfs_path.as_path()
            && contents == self.expected_sibling_contents.as_slice()
        {
            self.saw_disk_sibling = true;
        }
        true
    }

    fn observe_rustdoc_input(&mut self, absolute_path: &str, contents: &[u8]) -> bool {
        self.rustdoc_input_events = self.rustdoc_input_events.saturating_add(1);
        if Path::new(absolute_path) == self.expected_rustdoc_input_path.as_path()
            && contents == self.expected_rustdoc_input_contents.as_slice()
        {
            self.saw_rustdoc_input = true;
            return !std::mem::take(&mut self.reject_rustdoc_input_once);
        }
        true
    }

    fn observe_unresolved_module_candidate(
        &mut self,
        crate_root_file: &str,
        declaring_file: &str,
        candidate: &str,
    ) -> bool {
        if self.reject_candidate.as_deref() == Some(candidate) {
            self.reject_candidate = None;
            return false;
        }
        self.unresolved_candidates.push((
            crate_root_file.to_owned(),
            declaring_file.to_owned(),
            candidate.to_owned(),
        ));
        true
    }
}

fn package_frontier<'source>(
    root_source: &'source str,
    sibling: &'source str,
    extra: Option<(&'source Path, &'source str)>,
) -> Vec<RustWorkspaceFile<'source>> {
    let mut files = Vec::new();
    if let Some((relative_path, source)) = extra {
        files.push(RustWorkspaceFile {
            relative_path,
            source,
        });
    }
    files.push(RustWorkspaceFile {
        relative_path: Path::new("src/lib.rs"),
        source: root_source,
    });
    files.push(RustWorkspaceFile {
        relative_path: Path::new("src/sibling.rs"),
        source: sibling,
    });
    files
}

#[test]
fn workspace_lane_applies_selected_editor_buffers_and_discards_failed_transaction()
-> Result<(), Box<dyn std::error::Error>> {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root =
        std::env::temp_dir().join(format!("backend-rust-workspace-session-{nonce}-{sequence}"));
    fs::create_dir_all(root.join("src/foo"))?;
    fs::create_dir_all(root.join("docs"))?;
    #[cfg(unix)]
    let source_path_alias = {
        use std::os::unix::fs::symlink;
        let alias = root.with_extension("alias");
        symlink(&root, &alias)?;
        alias
    };
    #[cfg(not(unix))]
    let source_path_alias = root.clone();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"session_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )?;
    let root_source = concat!(
        "#![doc = include_str!(\"../docs/README.md\")]\n",
        "mod absent;\n",
        "#[path = \"generated.rs\"] mod generated;\n",
        "mod sibling;\n",
        "mod foo { mod bar; pub fn value() -> u8 { bar::value() } }\n",
        "pub fn value() -> u8 { generated::value().wrapping_add(sibling::value()).wrapping_add(foo::value()) }\n",
    );
    let disk_sibling = "pub fn value() -> u8 { 1 }\n";
    let generated_source = "pub fn value() -> u8 { 4 }\n";
    let rustdoc_input = b"session fixture documentation\n";
    fs::write(root.join("src/lib.rs"), root_source)?;
    fs::write(root.join("src/sibling.rs"), disk_sibling)?;
    fs::write(root.join("src/generated.rs"), generated_source)?;
    fs::write(root.join("docs/README.md"), rustdoc_input)?;

    let outcome = (|| {
        let toolchain = RustToolchain::discover(rustc_path())?;
        let source_paths = [PathBuf::from("src/lib.rs"), PathBuf::from("src/sibling.rs")];
        let overlay_sibling = "pub fn value() -> u8 { 2 }\n";
        let failed_sibling = "pub fn value() -> u8 { 3 }\n";
        let make_key = |source_paths: &[PathBuf]| {
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
                source_paths,
            )
        };
        let original_key = make_key(&source_paths)?;
        let unsaved_source_paths = [
            PathBuf::from("src/foo/bar.rs"),
            PathBuf::from("src/lib.rs"),
            PathBuf::from("src/sibling.rs"),
        ];
        assert!(!root.join("src/foo/bar.rs").exists());
        let unsaved_key = make_key(&unsaved_source_paths)?;
        assert_ne!(
            original_key, unsaved_key,
            "an unsaved source addition must change the operation key"
        );
        let running = AtomicBool::new(false);
        let control = || RustAnalysisControl {
            cancelled: &running,
            maximum_source_bytes: SourceByteLimit::from(8_192),
            deadline: Instant::now() + Duration::from_secs(180),
        };
        let key_for = |files: &[RustWorkspaceFile<'_>]| {
            let paths = files
                .iter()
                .map(|file| file.relative_path.to_path_buf())
                .collect::<Vec<_>>();
            make_key(&paths)
        };

        let mut lane = RustWorkspaceSessionLane::default();
        let initial = package_frontier(root_source, disk_sibling, None);
        {
            let mut observed_buffers = RecordingReadFrontier {
                expected_root_vfs_path: fs::canonicalize(root.join("src/lib.rs"))?,
                expected_root_contents: root_source.as_bytes().to_vec(),
                expected_sibling_vfs_path: fs::canonicalize(root.join("src/sibling.rs"))?,
                expected_sibling_contents: disk_sibling.as_bytes().to_vec(),
                expected_rustdoc_input_path: fs::canonicalize(root.join("docs/README.md"))?,
                expected_rustdoc_input_contents: rustdoc_input.to_vec(),
                reject_rustdoc_input_once: true,
                reject_candidate: Some(String::from("absent.rs")),
                ..RecordingReadFrontier::default()
            };
            let (lease, read_summary) = lane.begin_with_read_frontier_observer(
                key_for(&initial)?,
                &initial,
                control(),
                &mut observed_buffers,
            )?;
            assert_eq!(
                observed_buffers
                    .editor_buffers
                    .get(Path::new("src/lib.rs"))
                    .map(Vec::as_slice),
                Some(root_source.as_bytes()),
                "the observer must receive the exact bytes now present in RA's database"
            );
            assert_eq!(
                observed_buffers
                    .editor_buffers
                    .get(Path::new("src/sibling.rs"))
                    .map(Vec::as_slice),
                Some(disk_sibling.as_bytes()),
                "each selected buffer must be reported once from the RA database"
            );
            assert!(observed_buffers.vfs_events > 0);
            assert!(
                observed_buffers.saw_root_source,
                "expected exact root bytes at {:?}; matching contents arrived from {:?}",
                observed_buffers.expected_root_vfs_path, observed_buffers.root_content_paths
            );
            assert!(observed_buffers.saw_disk_sibling);
            assert!(observed_buffers.saw_rustdoc_input);
            assert!(read_summary.vfs_files_visited >= 3);
            assert!(read_summary.module_diagnostics_visited > 0);
            assert!(read_summary.rustdoc_inputs_visited > 0);
            assert_eq!(
                read_summary.rustdoc_input_events_delivered + 1,
                read_summary.rustdoc_inputs_visited,
                "the independent successful-read count must expose the injected dropped event"
            );
            assert_eq!(
                read_summary.vfs_events_delivered, read_summary.vfs_files_visited,
                "every representable loaded VFS file must be acknowledged"
            );
            assert!(read_summary.module_candidates_visited >= 2);
            assert_eq!(
                read_summary.module_candidate_events_delivered + 1,
                read_summary.module_candidates_visited,
                "the independent DefMap candidate count must expose the injected dropped event"
            );
            assert!(
                observed_buffers
                    .unresolved_candidates
                    .iter()
                    .any(|(_, _, candidate)| candidate == "absent/mod.rs")
            );
            let unresolved =
                nested_module_resolves(lease.workspace(), &root, root_source, control())?;
            assert!(!unresolved, "the nested module is absent on the first load");
            assert!(named_path_resolves(
                lease.workspace(),
                &root,
                root_source,
                "generated::value",
                control(),
            )?);
            assert!(
                !source_paths.contains(&PathBuf::from("src/generated.rs")),
                "the generated path is intentionally omitted from caller-selected buffers"
            );
            lease.commit();
        }
        let unsaved_module = "pub fn value() -> u8 { 9 }\n";
        {
            // The selected sources contain a module buffer that
            // has never existed on disk. RA must admit the FileId and source
            // root membership before resolving the parent module declaration.
            let current = package_frontier(
                root_source,
                disk_sibling,
                Some((Path::new("src/foo/bar.rs"), unsaved_module)),
            );
            let lease = lane.begin(key_for(&current)?, &current, control())?;
            assert!(
                nested_module_resolves(lease.workspace(), &root, root_source, control())?,
                "RA must resolve the current unsaved nested module buffer"
            );
            let observed = lease.workspace().analyze_source(
                source_path_alias.join("src/foo/bar.rs"),
                unsaved_module.as_bytes(),
                control(),
                |authority| Ok(authority.source == unsaved_module.as_bytes()),
            )?;
            assert!(
                observed,
                "the unsaved module must be bound to its exact buffer"
            );
            assert!(!root.join("src/foo/bar.rs").exists());
            lease.commit();
        }

        // PackageSourceSet does not prove that omission means deletion. A
        // disk-visible module stays in RA when this operation omits it.
        fs::write(root.join("src/foo/bar.rs"), unsaved_module)?;
        assert!(root.join("src/foo/bar.rs").exists());
        {
            let current = package_frontier(root_source, disk_sibling, None);
            let lease = lane.begin(key_for(&current)?, &current, control())?;
            assert!(
                nested_module_resolves(lease.workspace(), &root, root_source, control())?,
                "omission from the selected source list must not erase a disk module"
            );
            lease.commit();
        }

        // A new path in an editor rename resolves from its exact selected
        // buffer. The old disk path remains until a typed tombstone exists.
        let renamed_root_source = concat!(
            "mod sibling;\n",
            "mod foo { mod baz; pub fn value() -> u8 { baz::value() } }\n",
            "pub fn value() -> u8 { sibling::value().wrapping_add(foo::value()) }\n",
        );
        let renamed_module = "pub fn value() -> u8 { 11 }\n";
        {
            let current = package_frontier(
                renamed_root_source,
                disk_sibling,
                Some((Path::new("src/foo/baz.rs"), renamed_module)),
            );
            let lease = lane.begin(key_for(&current)?, &current, control())?;
            assert!(
                named_path_resolves(
                    lease.workspace(),
                    &root,
                    renamed_root_source,
                    "baz::value",
                    control(),
                )?,
                "a renamed module buffer must resolve at its selected lexical path"
            );
            assert!(!root.join("src/foo/baz.rs").exists());
            assert!(root.join("src/foo/bar.rs").exists());
            lease.commit();
        }

        {
            let changed = package_frontier(root_source, overlay_sibling, None);
            let lease = lane.begin(key_for(&changed)?, &changed, control())?;
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
            let unchanged = package_frontier(root_source, overlay_sibling, None);
            lane.begin(key_for(&unchanged)?, &unchanged, control())?
                .commit();
        }
        {
            let failed = package_frontier(root_source, failed_sibling, None);
            let _dropped_lease = lane.begin(key_for(&failed)?, &failed, control())?;
            // Dropping an uncommitted lease discards its mutated RA database.
        }
        {
            let retry = package_frontier(root_source, failed_sibling, None);
            let lease = lane.begin(key_for(&retry)?, &retry, control())?;
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
            let dependency_changed = package_frontier(root_source, failed_sibling, None);
            lane.begin(
                key_for(&dependency_changed)?,
                &dependency_changed,
                control(),
            )?
            .commit();
        }

        let stats = lane.stats();
        assert_eq!(stats.workspace_loads, 9);
        assert_eq!(stats.workspace_reuses, 0);
        assert_eq!(stats.workspace_reuse_disabled_requests, 9);
        assert_eq!(stats.source_updates, 8);
        assert_eq!(stats.overlay_sources_added, 2);
        assert_eq!(stats.overlay_sources_removed, 0);
        assert!(stats.overlay_root_entries_rebuilt > 0);
        assert_eq!(stats.failed_transactions, 1);
        assert!(stats.workspace_load_nanos > 0);
        assert!(stats.source_update_nanos > 0);
        Ok::<(), Box<dyn std::error::Error>>(())
    })();

    #[cfg(unix)]
    fs::remove_file(&source_path_alias)?;
    fs::remove_dir_all(&root)?;
    outcome
}

#[test]
fn production_session_overlay_keeps_backend_present_unconditional_module_owned()
-> Result<(), Box<dyn std::error::Error>> {
    // Exercise the whole package source frontier used by the live desktop
    // path. `assemble.rs` remains an ordinary module reached from the crate's
    // unconditional `mod assemble;` declaration in `lib.rs`.
    let repository_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or("Rust frontend manifest is not nested in the workspace")?;
    let package_root = repository_root.join("crates/present");
    let manifest = fs::read_to_string(package_root.join("Cargo.toml"))?;
    assert!(!manifest.lines().any(|line| line.trim() == "[features]"));
    let root_source = fs::read_to_string(package_root.join("lib.rs"))?;
    assert!(
        root_source
            .lines()
            .any(|line| line.trim() == "mod assemble;")
    );
    let assemble_source = fs::read_to_string(package_root.join("assemble.rs"))?;

    fn collect_rust_sources(
        package_root: &Path,
        directory: &Path,
        rows: &mut Vec<(PathBuf, String)>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let path = entry.path();
            if kind.is_dir() {
                collect_rust_sources(package_root, &path, rows)?;
            } else if kind.is_file() && path.extension().is_some_and(|extension| extension == "rs")
            {
                rows.push((
                    path.strip_prefix(package_root)?.to_path_buf(),
                    fs::read_to_string(path)?,
                ));
            }
        }
        Ok(())
    }

    let mut source_rows = Vec::new();
    collect_rust_sources(&package_root, &package_root, &mut source_rows)?;
    source_rows.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    assert!(
        source_rows.len() > 2,
        "fixture must cover the full package frontier"
    );
    assert!(
        source_rows.iter().any(|(path, source)| {
            path == Path::new("assemble.rs") && source == &assemble_source
        })
    );
    assert!(
        source_rows
            .iter()
            .any(|(path, source)| path == Path::new("lib.rs") && source == &root_source)
    );
    let source_paths = source_rows
        .iter()
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();
    let files = source_rows
        .iter()
        .map(|(path, source)| RustWorkspaceFile {
            relative_path: path,
            source,
        })
        .collect::<Vec<_>>();

    let toolchain = RustToolchain::discover(rustc_path())?;
    let features = RustFeatureControl {
        all_features: true,
        no_default_features: false,
        features: &[],
    };
    let key = RustWorkspaceSessionKey::new(
        &package_root,
        &toolchain,
        RustEdition::Rust2024,
        Stage::LowerIr,
        features,
        None,
        None,
        None,
        [0x51; 32],
        &source_paths,
    )?;
    let cancelled = AtomicBool::new(false);
    let control = RustAnalysisControl {
        cancelled: &cancelled,
        maximum_source_bytes: SourceByteLimit::from(64 * 1024 * 1024),
        deadline: Instant::now() + Duration::from_secs(300),
    };
    let mut lane = RustWorkspaceSessionLane::default();
    let mut observer = RecordingReadFrontier::default();
    let (lease, _) = lane.begin_with_read_frontier_observer(key, &files, control, &mut observer)?;
    assert_eq!(observer.editor_buffers.len(), source_rows.len());
    for (path, source) in &source_rows {
        assert_eq!(
            observer.editor_buffers.get(path).map(Vec::as_slice),
            Some(source.as_bytes()),
            "selected editor buffer must match the complete package frontier: {}",
            path.display()
        );
    }
    let admitted = lease.workspace().analyze_source(
        package_root.join("assemble.rs"),
        assemble_source.as_bytes(),
        control,
        |authority| {
            let has_page_lowering_entry = authority.declarations().any(|declaration| {
                authority
                    .declaration_name(&declaration)
                    .ok()
                    .and_then(|span| authority.source_at(span).ok())
                    == Some(b"page_from_document")
            });
            Ok((authority.source_scope, has_page_lowering_entry))
        },
    )?;
    assert_eq!(
        admitted.0,
        backend_frontend_rust::legacy::RustSourceScope::CargoModule
    );
    assert!(
        admitted.1,
        "HIR lowering must see the unconditional module's declarations"
    );
    assert!(
        observer
            .editor_buffers
            .get(Path::new("assemble.rs"))
            .is_some_and(|source| source.as_slice() == assemble_source.as_bytes())
    );
    lease.commit();
    Ok(())
}

#[cfg(unix)]
#[test]
fn workspace_overlay_preserves_symlinked_module_vfs_identity()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::symlink;

    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root =
        std::env::temp_dir().join(format!("backend-rust-workspace-symlink-{nonce}-{sequence}"));
    fs::create_dir_all(root.join("src"))?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"symlink_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )?;
    let root_source = concat!(
        "#[path = \"alias.rs\"] mod alias;\n",
        "#[path = \"real.rs\"] mod physical;\n",
        "pub fn value() -> u8 { alias::value().wrapping_add(physical::physical_value()) }\n",
    );
    let disk_target = "pub fn value() -> u8 { 1 }\n";
    let editor_buffer = "pub fn value() -> u8 { 9 }\n";
    let editor_target = "pub fn physical_value() -> u8 { 7 }\n";
    fs::write(root.join("src/lib.rs"), root_source)?;
    fs::write(root.join("src/real.rs"), disk_target)?;
    symlink("real.rs", root.join("src/alias.rs"))?;

    let outcome = (|| {
        let toolchain = RustToolchain::discover(rustc_path())?;
        let source_paths = [
            PathBuf::from("src/alias.rs"),
            PathBuf::from("src/lib.rs"),
            PathBuf::from("src/real.rs"),
        ];
        let key = RustWorkspaceSessionKey::new(
            &root,
            &toolchain,
            RustEdition::Rust2024,
            Stage::LowerIr,
            RustFeatureControl::default(),
            Some([5; 32]),
            Some([6; 32]),
            Some([7; 32]),
            [8; 32],
            &source_paths,
        )?;
        let files = [
            RustWorkspaceFile {
                relative_path: Path::new("src/alias.rs"),
                source: editor_buffer,
            },
            RustWorkspaceFile {
                relative_path: Path::new("src/lib.rs"),
                source: root_source,
            },
            RustWorkspaceFile {
                relative_path: Path::new("src/real.rs"),
                source: editor_target,
            },
        ];
        let cancelled = AtomicBool::new(false);
        let control = RustAnalysisControl {
            cancelled: &cancelled,
            maximum_source_bytes: SourceByteLimit::from(8_192),
            deadline: Instant::now() + Duration::from_secs(180),
        };
        let mut lane = RustWorkspaceSessionLane::default();
        let lease = lane.begin(key, &files, control)?;
        assert!(named_path_resolves(
            lease.workspace(),
            &root,
            root_source,
            "alias::value",
            control,
        )?);
        assert!(named_path_resolves(
            lease.workspace(),
            &root,
            root_source,
            "physical::physical_value",
            control,
        )?);
        assert!(lease.workspace().analyze_source(
            root.join("src/alias.rs"),
            editor_buffer.as_bytes(),
            control,
            |authority| Ok(authority.source == editor_buffer.as_bytes()),
        )?);
        assert!(lease.workspace().analyze_source(
            root.join("src/real.rs"),
            editor_target.as_bytes(),
            control,
            |authority| Ok(authority.source == editor_target.as_bytes()),
        )?);
        lease.commit();
        assert_eq!(lane.stats().overlay_sources_removed, 0);
        Ok::<(), Box<dyn std::error::Error>>(())
    })();

    fs::remove_dir_all(&root)?;
    outcome
}

#[test]
fn workspace_session_rejects_selected_file_without_active_hir_owner()
-> Result<(), Box<dyn std::error::Error>> {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "backend-rust-workspace-detached-{nonce}-{sequence}"
    ));
    fs::create_dir_all(root.join("src"))?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"detached_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )?;
    let root_source = "pub fn value() -> u8 { 1 }\n";
    let detached_source = "pub fn unowned() -> u8 { 2 }\n";
    fs::write(root.join("src/lib.rs"), root_source)?;
    fs::write(root.join("src/detached.rs"), detached_source)?;

    let outcome = (|| {
        let toolchain = RustToolchain::discover(rustc_path())?;
        let source_paths = [
            PathBuf::from("src/detached.rs"),
            PathBuf::from("src/lib.rs"),
        ];
        let files = [
            RustWorkspaceFile {
                relative_path: Path::new("src/detached.rs"),
                source: detached_source,
            },
            RustWorkspaceFile {
                relative_path: Path::new("src/lib.rs"),
                source: root_source,
            },
        ];
        let key = RustWorkspaceSessionKey::new(
            &root,
            &toolchain,
            RustEdition::Rust2024,
            Stage::LowerIr,
            RustFeatureControl::default(),
            None,
            None,
            None,
            [0x61; 32],
            &source_paths,
        )?;
        let cancelled = AtomicBool::new(false);
        let control = RustAnalysisControl {
            cancelled: &cancelled,
            maximum_source_bytes: SourceByteLimit::from(8_192),
            deadline: Instant::now() + Duration::from_secs(180),
        };
        let mut lane = RustWorkspaceSessionLane::default();
        let lease = lane.begin(key, &files, control)?;
        let result = lease.workspace().analyze_source(
            root.join("src/detached.rs"),
            detached_source.as_bytes(),
            control,
            |_| Ok(()),
        );
        assert!(
            matches!(result, Err(RustAuthorityError::DetachedSource { .. })),
            "an exactly bound VFS buffer without active Cargo HIR ownership must remain rejected"
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    })();

    fs::remove_dir_all(&root)?;
    outcome
}

#[test]
fn workspace_overlay_updates_selected_files_across_package_roots()
-> Result<(), Box<dyn std::error::Error>> {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "backend-rust-workspace-multi-root-{nonce}-{sequence}"
    ));
    fs::create_dir_all(root.join("crates/a/src"))?;
    fs::create_dir_all(root.join("crates/b/src"))?;
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/a\", \"crates/b\"]\nresolver = \"2\"\n",
    )?;
    for package in ["a", "b"] {
        fs::write(
            root.join(format!("crates/{package}/Cargo.toml")),
            format!(
                "[package]\nname = \"member_{package}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"
            ),
        )?;
    }
    let disk_a = "pub fn value() -> u8 { 1 }\n";
    let disk_b = "pub fn value() -> u8 { 2 }\n";
    let editor_a = "pub fn value() -> u8 { 11 }\n";
    let editor_b = "pub fn value() -> u8 { 22 }\n";
    fs::write(root.join("crates/a/src/lib.rs"), disk_a)?;
    fs::write(root.join("crates/b/src/lib.rs"), disk_b)?;

    let outcome = (|| {
        let toolchain = RustToolchain::discover(rustc_path())?;
        let source_paths = [
            PathBuf::from("crates/a/src/lib.rs"),
            PathBuf::from("crates/b/src/lib.rs"),
        ];
        let key = RustWorkspaceSessionKey::new(
            &root,
            &toolchain,
            RustEdition::Rust2024,
            Stage::LowerIr,
            RustFeatureControl::default(),
            Some([9; 32]),
            Some([10; 32]),
            Some([11; 32]),
            [12; 32],
            &source_paths,
        )?;
        let files = [
            RustWorkspaceFile {
                relative_path: Path::new("crates/a/src/lib.rs"),
                source: editor_a,
            },
            RustWorkspaceFile {
                relative_path: Path::new("crates/b/src/lib.rs"),
                source: editor_b,
            },
        ];
        let cancelled = AtomicBool::new(false);
        let control = RustAnalysisControl {
            cancelled: &cancelled,
            maximum_source_bytes: SourceByteLimit::from(8_192),
            deadline: Instant::now() + Duration::from_secs(180),
        };
        let mut lane = RustWorkspaceSessionLane::default();
        let lease = lane.begin(key, &files, control)?;
        for (path, expected) in [
            ("crates/a/src/lib.rs", editor_a),
            ("crates/b/src/lib.rs", editor_b),
        ] {
            assert!(lease.workspace().analyze_source(
                root.join(path),
                expected.as_bytes(),
                control,
                |authority| Ok(authority.source == expected.as_bytes()),
            )?);
        }
        lease.commit();
        assert_eq!(lane.stats().source_updates, 2);
        assert_eq!(lane.stats().selected_source_roots_touched, 2);
        assert_eq!(lane.stats().workspace_reuses, 0);
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
    named_path_resolves(workspace, root, source, "bar::value", control)
}

fn named_path_resolves(
    workspace: &RustWorkspace,
    root: &Path,
    source: &str,
    expected_path: &str,
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
                    .is_some_and(|spelling| spelling == expected_path.as_bytes())
                    && authority.resolve_path(&path).is_some()
            }))
        },
    )
}

fn rustc_path() -> PathBuf {
    std::env::var_os("RUSTC").map_or_else(|| PathBuf::from("rustc"), PathBuf::from)
}
