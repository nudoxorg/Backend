//! Captures the real source frontier used by cross-file compiler fixtures.
use backend_frontend_rust::legacy::{
    RustAnalysisControl, RustCargoMetadataPolicy, RustFeatureControl, RustToolchain, RustWorkspace,
    RustWorkspaceFile, RustWorkspaceSessionKey, RustWorkspaceSessionLane, SourceByteLimit,
};
use backend_semantic::vocabulary::{RustEdition, Stage};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

pub(super) fn with_captured_workspace<Output>(
    root: &Path,
    toolchain: &RustToolchain,
    lower: impl FnOnce(&RustWorkspace) -> Result<Output, String>,
) -> Result<Output, String> {
    fn capture(
        root: &Path,
        directory: &Path,
        files: &mut Vec<(PathBuf, String)>,
    ) -> Result<(), String> {
        for entry in fs::read_dir(directory).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let kind = entry.file_type().map_err(|error| error.to_string())?;
            let path = entry.path();
            if kind.is_dir() {
                capture(root, &path, files)?;
            } else if kind.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
                let relative = path
                    .strip_prefix(root)
                    .map_err(|error| error.to_string())?
                    .to_path_buf();
                files.push((
                    relative,
                    fs::read_to_string(&path).map_err(|error| error.to_string())?,
                ));
            }
        }
        Ok(())
    }
    let root = root.canonicalize().map_err(|error| error.to_string())?;
    let mut captured = Vec::new();
    capture(&root, &root.join("src"), &mut captured)?;
    captured.sort_by(|left, right| left.0.cmp(&right.0));
    let paths = captured
        .iter()
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();
    let files = captured
        .iter()
        .map(|(path, source)| RustWorkspaceFile {
            relative_path: path,
            source,
        })
        .collect::<Vec<_>>();
    let cancelled = AtomicBool::new(false);
    let control = RustAnalysisControl {
        cancelled: &cancelled,
        maximum_source_bytes: SourceByteLimit::from(65_536),
        deadline: Instant::now() + Duration::from_secs(120),
    };
    let key = RustWorkspaceSessionKey::new(
        &root,
        toolchain,
        RustEdition::Rust2024,
        Stage::LowerIr,
        RustFeatureControl::default(),
        RustCargoMetadataPolicy::Offline,
        None,
        None,
        None,
        [0x46; 32],
        &paths,
    )
    .map_err(|error| format!("captured fixture key: {error:?}"))?;
    let mut lane = RustWorkspaceSessionLane::default();
    let lease = lane
        .begin(key, &files, control)
        .map_err(|error| format!("captured fixture frontier: {error:?}"))?;
    let result = lower(lease.workspace());
    if result.is_ok() {
        lease.commit();
    }
    result
}
