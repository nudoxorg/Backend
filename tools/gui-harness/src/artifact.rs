//! Durable screenshot artifacts and manifests.

use crate::{CaptureConfig, CaptureRecord, DiffMetrics, GuiState, ReferenceMetadata};
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{ExtendedColorType, GenericImage, ImageEncoder, Rgba, RgbaImage};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;
use thiserror::Error;

/// One PNG frame and its provenance.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FrameArtifact {
    /// Stable frame label, for example `midpoint` or `settled`.
    pub label: String,
    /// Virtual time at which the frame was captured.
    pub time_ms: u64,
    /// Logical viewport and device scale used for this frame. Older manifests
    /// may omit this and fall back to the manifest-level viewport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewport: Option<crate::Viewport>,
    /// Relative PNG path from the run root.
    pub path: String,
    /// SHA-256 of the encoded PNG bytes.
    pub sha256: String,
    /// Image width in physical pixels.
    pub width: u32,
    /// Image height in physical pixels.
    pub height: u32,
    /// Input step index that produced this frame, when applicable.
    pub input_index: Option<usize>,
    /// Baseline comparison, when a baseline was available.
    pub diff: Option<DiffMetrics>,
}

/// A physical-pixel crop used for close visual inspection of a frame.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CropRect {
    /// Left edge in physical pixels.
    pub left: u32,
    /// Top edge in physical pixels.
    pub top: u32,
    /// Crop width in physical pixels.
    pub width: u32,
    /// Crop height in physical pixels.
    pub height: u32,
}

impl CropRect {
    /// Creates a crop and rejects zero-sized rectangles.
    pub const fn new(left: u32, top: u32, width: u32, height: u32) -> Option<Self> {
        if width == 0 || height == 0 {
            return None;
        }
        Some(Self {
            left,
            top,
            width,
            height,
        })
    }

    fn fit(self, image: &RgbaImage) -> Option<Self> {
        let right = self.left.checked_add(self.width)?.min(image.width());
        let bottom = self.top.checked_add(self.height)?.min(image.height());
        (self.left < right && self.top < bottom).then_some(Self {
            left: self.left,
            top: self.top,
            width: right - self.left,
            height: bottom - self.top,
        })
    }
}

/// Durable metadata for one visual-inspection crop.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CropArtifact {
    /// Stable crop label.
    pub label: String,
    /// Source frame label.
    pub frame: String,
    /// Physical crop bounds.
    pub rect: CropRect,
    /// Relative PNG path from the run root.
    pub path: String,
    /// SHA-256 of the encoded crop PNG bytes.
    pub sha256: String,
}

/// Manifest for a complete state capture.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CaptureManifest {
    /// Manifest schema version.
    pub schema: u32,
    /// State rendered.
    pub state: GuiState,
    /// Deterministic capture inputs.
    pub config: CaptureConfig,
    /// Ordered captured frames.
    pub frames: Vec<FrameArtifact>,
    /// Input script, if one was applied.
    pub script_id: Option<String>,
    /// A stable hash of state, config, and script metadata.
    pub scenario_sha256: String,
    /// A stable hash of ordered frame labels, times, and PNG hashes.
    pub sequence_sha256: String,
    /// Machine-readable metadata for replaying the encoded PNG sequence.
    pub sequence: FrameSequenceMetadata,
    /// Git revision supplied by the caller, if available.
    pub source_revision: Option<String>,
    /// Complete environment provenance for this rendered artifact.
    #[serde(default)]
    pub provenance: CaptureProvenance,
    /// Semantic probe JSON written beside this manifest.
    pub semantic_artifact: Option<String>,
    /// SHA-256 of the semantic probe bytes referenced by `semantic_artifact`.
    #[serde(default)]
    pub semantic_sha256: Option<String>,
    /// Whether every requested baseline comparison satisfied its policy.
    #[serde(default)]
    pub baseline_within_policy: bool,
    /// Exact design contract identity used for this rendered capture.
    #[serde(default)]
    pub reference: Option<ReferenceMetadata>,
}

/// Environment identity attached to every capture manifest.
///
/// A pixel artifact without its renderer and checkout identity is not a
/// regression oracle. These values are collected at runtime so a copied
/// artifact can be rejected when it is verified from another checkout.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CaptureProvenance {
    /// Git commit that rendered the capture.
    pub source_revision: String,
    /// Whether the checkout had tracked or untracked changes at capture time.
    pub dirty: bool,
    /// Full rustc version and target information.
    pub rustc: String,
    /// Active rustup toolchain identity.
    pub toolchain: String,
    /// Resolved GPUI package identity.
    pub gpui_revision: String,
    /// Resolved GPUI CE package identity.
    pub gpui_ce_revision: String,
    /// Target operating system and architecture.
    pub os: String,
    /// Host hardware model or architecture fallback.
    pub hardware: String,
    /// Renderer identity used for the capture.
    pub renderer: String,
    /// Platform/backend identity used for the capture.
    pub backend: String,
    /// Deterministic image encoder configuration.
    pub encoder: String,
}

/// Encoded animation-sequence metadata retained beside a capture manifest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FrameSequenceMetadata {
    /// Sequence encoding. PNG keeps every frame lossless and independently inspectable.
    pub encoding: String,
    /// Number of encoded frames.
    pub frame_count: usize,
    /// Virtual duration covered by the sequence.
    pub duration_ms: u64,
    /// Nominal sampling interval from the deterministic capture config.
    pub frame_interval_ms: u64,
    /// Labels retained as semantic keyframes for animation review.
    pub keyframes: Vec<String>,
    /// Contact-sheet filmstrip for fast human review of the animation.
    pub filmstrip_path: Option<String>,
}

/// Run-level index over every state and transition capture.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct RunManifest {
    /// Manifest schema version.
    pub schema: u32,
    /// Harness package version.
    pub harness_version: String,
    /// All state manifests written in this run.
    pub captures: Vec<String>,
    /// All transition manifests written in this run.
    pub transitions: Vec<String>,
    /// Number of frame PNGs written.
    pub frame_count: usize,
    /// Number of failed baseline comparisons.
    pub failed_comparisons: usize,
}

/// Result of an independent artifact verification pass.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct VerificationReport {
    /// Manifests checked.
    pub manifests: usize,
    /// PNG frames checked.
    pub frames: usize,
    /// Stable relative paths that failed verification.
    pub failures: Vec<String>,
    /// Manifests whose baseline comparison or provenance was invalid.
    pub failed_manifests: usize,
}

/// Filesystem and encoding failures from artifact writing.
#[derive(Debug, Error)]
pub enum ArtifactError {
    /// Filesystem failure.
    #[error("screenshot artifact I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// PNG encoding failure.
    #[error("screenshot PNG encoding failed: {0}")]
    Image(#[from] image::ImageError),
    /// JSON manifest encoding failure.
    #[error("screenshot manifest encoding failed: {0}")]
    Json(#[from] serde_json::Error),
    /// A path attempted to escape the run root.
    #[error("unsafe screenshot artifact path {0:?}")]
    UnsafePath(String),
    /// A durable artifact failed an independent verification check.
    #[error("screenshot artifact verification failed: {0}")]
    Verification(String),
    /// Runtime provenance could not be established.
    #[error("screenshot provenance unavailable: {0}")]
    Provenance(String),
}

/// Verifies every manifest and PNG below a run root without trusting the
/// capture process. This is intentionally independent of GPUI and can be
/// used as a holdout gate in CI or after copying artifacts between machines.
pub fn verify_run(root: &Path) -> Result<VerificationReport, ArtifactError> {
    verify_run_inner(root, true)
}

/// Verifies a run before it is promoted to a baseline.
///
/// A first capture intentionally has no baseline (or contains an explicit
/// mismatch). It still has to satisfy every structural, provenance, semantic,
/// and hash check before its frames are allowed to become the oracle for
/// future runs. This mode is the only supported escape hatch for the baseline
/// policy gate.
pub fn verify_run_for_baseline_update(root: &Path) -> Result<VerificationReport, ArtifactError> {
    verify_run_inner(root, false)
}

/// Validates the ordered frame portion of a manifest before it is encoded as
/// an animation sequence. Equal timestamps are allowed because a retarget or
/// reversal can be sampled at the same virtual instant; labels still must be
/// unique so an FFmpeg-ready frame list cannot silently overwrite a PNG.
pub fn validate_frame_sequence(frames: &[FrameArtifact]) -> Result<(), ArtifactError> {
    if frames.is_empty() {
        return Err(ArtifactError::Verification(
            "frame sequence is empty".to_owned(),
        ));
    }
    if frames[0].time_ms != 0 {
        return Err(ArtifactError::Verification(
            "frame sequence must begin at virtual time zero".to_owned(),
        ));
    }
    let mut labels = std::collections::BTreeSet::new();
    for frame in frames {
        if frame.label.trim().is_empty() || !labels.insert(frame.label.as_str()) {
            return Err(ArtifactError::Verification(format!(
                "frame sequence has an empty or duplicate label {:?}",
                frame.label
            )));
        }
    }
    if frames
        .windows(2)
        .any(|pair| pair[0].time_ms > pair[1].time_ms)
    {
        return Err(ArtifactError::Verification(
            "frame sequence timestamps are not monotonic".to_owned(),
        ));
    }
    Ok(())
}

fn verify_run_inner(
    root: &Path,
    enforce_baseline_policy: bool,
) -> Result<VerificationReport, ArtifactError> {
    let mut manifests = Vec::new();
    collect_manifests(root, &mut manifests)?;
    if manifests.is_empty() {
        return Err(ArtifactError::Verification(
            "run contains no capture manifests".to_owned(),
        ));
    }
    let current_provenance = CaptureProvenance::collect()?;
    let mut report = VerificationReport::default();
    let mut referenced_frames = std::collections::BTreeSet::new();
    let mut referenced_semantics = std::collections::BTreeSet::new();
    let mut manifest_ids = std::collections::BTreeSet::new();
    for manifest_path in manifests {
        let bytes = std::fs::read(&manifest_path)?;
        let manifest: CaptureManifest = serde_json::from_slice(&bytes)?;
        report.manifests += 1;
        let manifest_id = manifest.state.id.clone();
        let filename_id = manifest_path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if filename_id != manifest_id || !manifest_ids.insert(manifest_id.clone()) {
            report.failures.push(format!(
                "{}: stale or duplicate manifest identity",
                manifest_path.display()
            ));
            report.failed_manifests += 1;
        }
        if manifest.frames.is_empty() || manifest.sequence.frame_count != manifest.frames.len() {
            report.failures.push(format!(
                "{}: manifest frame count is zero or inconsistent",
                manifest_path.display()
            ));
            report.failed_manifests += 1;
        }
        if let Err(error) = validate_frame_sequence(&manifest.frames) {
            report
                .failures
                .push(format!("{}: {error}", manifest_path.display()));
            report.failed_manifests += 1;
        }
        let expected_keyframes = manifest
            .frames
            .iter()
            .map(|frame| frame.label.clone())
            .collect::<Vec<_>>();
        if manifest.sequence.encoding != "png-sequence"
            || manifest.sequence.frame_interval_ms != manifest.config.frame_interval_ms
            || manifest.sequence.keyframes != expected_keyframes
        {
            report.failures.push(format!(
                "{}: ordered sequence metadata is stale",
                manifest_path.display()
            ));
            report.failed_manifests += 1;
        }
        let expected_scenario = scenario_hash(&(
            &manifest.state,
            &manifest.config,
            manifest.script_id.as_deref(),
        ))?;
        if expected_scenario != manifest.scenario_sha256 {
            report.failures.push(format!(
                "{}: scenario hash mismatch",
                manifest_path.display()
            ));
            report.failed_manifests += 1;
        }
        let expected_sequence = scenario_hash(&manifest.frames)?;
        if expected_sequence != manifest.sequence_sha256
            || manifest.sequence.duration_ms
                != manifest.frames.last().map_or(0, |frame| frame.time_ms)
        {
            report.failures.push(format!(
                "{}: frame sequence hash or duration mismatch",
                manifest_path.display()
            ));
            report.failed_manifests += 1;
        }
        if manifest.provenance.source_revision != current_provenance.source_revision
            || manifest.provenance.dirty != current_provenance.dirty
            || manifest.provenance.rustc != current_provenance.rustc
            || manifest.provenance.toolchain != current_provenance.toolchain
            || manifest.provenance.gpui_revision != current_provenance.gpui_revision
            || manifest.provenance.gpui_ce_revision != current_provenance.gpui_ce_revision
            || manifest.provenance.renderer != current_provenance.renderer
            || manifest.provenance.backend != current_provenance.backend
            || manifest.provenance.encoder != current_provenance.encoder
            || manifest.provenance.os != current_provenance.os
            || manifest.provenance.hardware != current_provenance.hardware
        {
            report.failures.push(format!(
                "{}: capture provenance does not match this checkout/runtime",
                manifest_path.display()
            ));
            report.failed_manifests += 1;
        }
        if enforce_baseline_policy && !manifest.baseline_within_policy {
            report.failures.push(format!(
                "{}: baseline comparison exceeded policy",
                manifest_path.display()
            ));
            report.failed_manifests += 1;
        }
        if let Some(semantic) = manifest.semantic_artifact.as_deref() {
            let semantic = safe_relative_path(semantic)?;
            let path = manifest_path
                .parent()
                .and_then(Path::parent)
                .ok_or_else(|| ArtifactError::Verification(manifest_path.display().to_string()))?
                .join(&semantic);
            referenced_semantics.insert(path.clone());
            if !path.is_file() {
                report.failures.push(format!(
                    "{}: missing semantic artifact {}",
                    manifest_path.display(),
                    semantic.display(),
                ));
                report.failed_manifests += 1;
            } else {
                let semantic_bytes = std::fs::read(&path)?;
                if manifest
                    .semantic_sha256
                    .as_deref()
                    .is_some_and(|expected| expected != hash_bytes(&semantic_bytes))
                {
                    report.failures.push(format!(
                        "{}: semantic artifact {} has a stale SHA-256",
                        manifest_path.display(),
                        semantic.display()
                    ));
                    report.failed_manifests += 1;
                }
                if serde_json::from_slice::<serde_json::Value>(&semantic_bytes).is_err() {
                    report.failures.push(format!(
                        "{}: invalid semantic artifact {}",
                        manifest_path.display(),
                        semantic.display()
                    ));
                    report.failed_manifests += 1;
                }
            }
        } else {
            report.failures.push(format!(
                "{}: semantic artifact is not recorded",
                manifest_path.display()
            ));
            report.failed_manifests += 1;
        }
        // Frame paths in a manifest are relative to the capture/session root,
        // while manifests live in its `manifests/` directory.
        let base = manifest_path
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| ArtifactError::Verification(manifest_path.display().to_string()))?;
        let mut manifest_frame_paths = std::collections::BTreeSet::new();
        for frame in manifest.frames {
            let expected_size = frame
                .viewport
                .unwrap_or(manifest.config.viewport)
                .physical_size();
            let frame_relative = safe_relative_path(&frame.path)?;
            let path = base.join(&frame_relative);
            if !manifest_frame_paths.insert(frame.path.clone())
                || !referenced_frames.insert(path.clone())
            {
                report.failures.push(format!(
                    "{}: duplicate frame reference {}",
                    manifest_path.display(),
                    frame.path
                ));
                report.failed_manifests += 1;
                continue;
            }
            let encoded = std::fs::read(&path)?;
            let relative = path
                .strip_prefix(root)
                .unwrap_or(path.as_path())
                .display()
                .to_string();
            let image = image::load_from_memory(&encoded)?.into_rgba8();
            let expected_size = frame
                .viewport
                .unwrap_or(manifest.config.viewport)
                .physical_size();
            report.frames += 1;
            if hash_bytes(&encoded) != frame.sha256
                || image.dimensions() != (frame.width, frame.height)
                || image.dimensions() != expected_size
                || (frame.width, frame.height) != expected_size
                || image.pixels().all(|pixel| pixel[3] == 0)
            {
                report.failures.push(relative);
            }
            if enforce_baseline_policy
                && frame.diff.as_ref().is_some_and(|diff| !diff.within_policy)
            {
                report.failures.push(format!(
                    "{}: frame {} is outside baseline policy",
                    manifest_path.display(),
                    frame.label
                ));
                report.failed_manifests += 1;
            }
        }
    }
    let mut actual_frames = Vec::new();
    collect_frames(root, &mut actual_frames)?;
    for frame in actual_frames {
        if !referenced_frames.contains(&frame) {
            report.failures.push(format!(
                "{}: orphan frame is not referenced by a manifest",
                frame.display()
            ));
        }
    }
    let mut semantic_files = Vec::new();
    collect_named_json(root, "semantics", &mut semantic_files)?;
    for semantic in semantic_files {
        if !referenced_semantics.contains(&semantic) {
            report.failures.push(format!(
                "{}: orphan semantic artifact is not referenced by a manifest",
                semantic.display()
            ));
        }
    }
    let mut journey_files = Vec::new();
    collect_named_json(root, "journeys", &mut journey_files)?;
    for journey in journey_files {
        let bytes = std::fs::read(&journey)?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
            ArtifactError::Verification(format!(
                "{}: invalid journey artifact: {error}",
                journey.display()
            ))
        })?;
        let valid = value
            .get("id")
            .and_then(serde_json::Value::as_str)
            .is_some()
            && value
                .get("from")
                .and_then(serde_json::Value::as_str)
                .is_some()
            && value
                .get("to")
                .and_then(serde_json::Value::as_str)
                .is_some()
            && value
                .get("observations")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|observations| !observations.is_empty())
            && value
                .get("steps")
                .and_then(serde_json::Value::as_array)
                .is_some();
        if !valid {
            report.failures.push(format!(
                "{}: stale or incomplete journey artifact",
                journey.display()
            ));
        }
    }
    if report.failures.is_empty() {
        Ok(report)
    } else {
        Err(ArtifactError::Verification(format!(
            "{} frame(s) failed",
            report.failures.len()
        )))
    }
}

fn collect_frames(root: &Path, output: &mut Vec<PathBuf>) -> Result<(), ArtifactError> {
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_frames(&path, output)?;
        } else if path
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "frames")
            && path.extension().is_some_and(|extension| extension == "png")
        {
            output.push(path);
        }
    }
    Ok(())
}

fn collect_named_json(
    root: &Path,
    directory: &str,
    output: &mut Vec<PathBuf>,
) -> Result<(), ArtifactError> {
    let directory_root = root.join(directory);
    if !directory_root.is_dir() {
        return Ok(());
    }
    collect_named_json_inner(&directory_root, output)
}

fn collect_named_json_inner(root: &Path, output: &mut Vec<PathBuf>) -> Result<(), ArtifactError> {
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_named_json_inner(&path, output)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            output.push(path);
        }
    }
    Ok(())
}

fn collect_manifests(root: &Path, output: &mut Vec<PathBuf>) -> Result<(), ArtifactError> {
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_manifests(&path, output)?;
        } else if path
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "manifests")
        {
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                output.push(path);
            }
        }
    }
    Ok(())
}

impl CaptureProvenance {
    /// Captures the checkout and renderer identity that produced a run.
    pub fn collect() -> Result<Self, ArtifactError> {
        let root = find_source_root()?;
        let source_revision = command_in(&root, "git", &["rev-parse", "HEAD"])?;
        let dirty = !command_in(&root, "git", &["status", "--porcelain"])?
            .trim()
            .is_empty();
        let rustc = command_output("rustc", &["--version", "--verbose"])?;
        let toolchain = match command_output("rustup", &["show", "active-toolchain"]) {
            Ok(toolchain) => toolchain,
            Err(_) => std::env::var("RUSTUP_TOOLCHAIN").unwrap_or_else(|_| "unknown".to_owned()),
        };
        let gpui_revision = lock_revision(&root, "gpui");
        let gpui_ce_revision = lock_revision(&root, "gpui-ce");
        let os = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
        let hardware = if cfg!(target_os = "macos") {
            command_output("sysctl", &["-n", "hw.model"])
                .or_else(|_| command_output("uname", &["-m"]))?
        } else {
            command_output("uname", &["-m"])?
        };
        Ok(Self {
            source_revision,
            dirty,
            rustc,
            toolchain,
            gpui_revision,
            gpui_ce_revision,
            os: os.trim().to_owned(),
            hardware: hardware.trim().to_owned(),
            renderer: std::env::var("NUDOX_GUI_RENDERER")
                .unwrap_or_else(|_| "gpui-headless".to_owned()),
            backend: std::env::var("NUDOX_GUI_BACKEND")
                .unwrap_or_else(|_| "gpui-platform-test".to_owned()),
            encoder: "png:rgba8:fast:sub".to_owned(),
        })
    }
}

fn find_source_root() -> Result<PathBuf, ArtifactError> {
    if let Ok(root) = std::env::var("NUDOX_SOURCE_ROOT") {
        let root = PathBuf::from(root);
        if root.join(".git").exists() {
            return Ok(root);
        }
    }
    let mut current = std::env::current_dir().map_err(|error| {
        ArtifactError::Provenance(format!("cannot determine current directory: {error}"))
    })?;
    loop {
        if current.join(".git").exists() {
            return Ok(current);
        }
        if !current.pop() {
            break;
        }
    }
    Err(ArtifactError::Provenance(
        "no git checkout found from current directory; set NUDOX_SOURCE_ROOT".to_owned(),
    ))
}

fn command_in(root: &Path, command: &str, args: &[&str]) -> Result<String, ArtifactError> {
    let output = Command::new(command)
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|error| ArtifactError::Provenance(format!("run {command}: {error}")))?;
    if !output.status.success() {
        return Err(ArtifactError::Provenance(format!(
            "{command} exited with {}",
            output.status
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn command_output(command: &str, args: &[&str]) -> Result<String, ArtifactError> {
    let output = Command::new(command)
        .args(args)
        .output()
        .map_err(|error| ArtifactError::Provenance(format!("run {command}: {error}")))?;
    if !output.status.success() {
        return Err(ArtifactError::Provenance(format!(
            "{command} exited with {}",
            output.status
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn lock_revision(root: &Path, package: &str) -> String {
    let lock = std::fs::read_to_string(root.join("Cargo.lock")).unwrap_or_default();
    let mut in_package = false;
    for line in lock.lines() {
        if line == "[[package]]" {
            in_package = false;
        } else if let Some(name) = line
            .strip_prefix("name = \"")
            .and_then(|line| line.strip_suffix('"'))
        {
            in_package = name == package;
        } else if in_package && line.starts_with("version = ") {
            return format!(
                "Cargo.lock:{package}:{}",
                line.trim_start_matches("version = ")
            );
        }
    }
    format!("unresolved:{package}")
}

/// Writes immutable PNGs and JSON manifests under a run directory.
pub struct ArtifactWriter {
    root: PathBuf,
}

impl ArtifactWriter {
    /// Creates a writer and its root directory.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, ArtifactError> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    /// Returns the run root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Writes a frame PNG and returns its relative path and encoded hash.
    pub fn write_frame(
        &self,
        state_id: &str,
        label: &str,
        image: &RgbaImage,
    ) -> Result<(String, String), ArtifactError> {
        let state = safe_component(state_id)?;
        let label = safe_component(label)?;
        let relative = PathBuf::from("frames")
            .join(state)
            .join(format!("{label}.png"));
        let path = self.root.join(&relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = encode_png(image)?;
        std::fs::write(&path, &bytes)?;
        Ok((relative.to_string_lossy().into_owned(), hash_bytes(&bytes)))
    }

    /// Writes a physical-pixel crop for close inspection of a captured frame.
    ///
    /// Crops are deliberately independent PNGs: a reviewer can open a small
    /// header, focus ring, or disclosure edge without scaling the full frame,
    /// and a malformed crop fails before it becomes an artifact.
    pub fn write_crop(
        &self,
        state_id: &str,
        frame_label: &str,
        crop_label: &str,
        image: &RgbaImage,
        rect: CropRect,
    ) -> Result<CropArtifact, ArtifactError> {
        let rect = rect
            .fit(image)
            .ok_or_else(|| ArtifactError::UnsafePath("crop is outside frame".to_owned()))?;
        let state = safe_component(state_id)?;
        let frame = safe_component(frame_label)?;
        let label = safe_component(crop_label)?;
        let relative = PathBuf::from("crops")
            .join(state)
            .join(format!("{frame}--{label}.png"));
        let path = self.root.join(&relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let crop = image::imageops::crop_imm(image, rect.left, rect.top, rect.width, rect.height)
            .to_image();
        let bytes = encode_png(&crop)?;
        std::fs::write(&path, &bytes)?;
        Ok(CropArtifact {
            label,
            frame: frame_label.to_owned(),
            rect,
            path: relative.to_string_lossy().into_owned(),
            sha256: hash_bytes(&bytes),
        })
    }

    /// Writes a deterministic contact-sheet filmstrip for an animation. Each
    /// frame stays independently inspectable as a PNG; the strip is a review
    /// aid and never participates in pixel baselines.
    pub fn write_filmstrip(
        &self,
        state_id: &str,
        frames: &[CaptureRecord],
    ) -> Result<String, ArtifactError> {
        if frames.is_empty() {
            return Err(ArtifactError::UnsafePath("empty filmstrip".to_owned()));
        }
        let state = safe_component(state_id)?;
        let columns = frames.len().min(4) as u32;
        let rows = (frames.len() as u32).div_ceil(columns);
        let cell_width = frames
            .iter()
            .map(|frame| frame.image.width())
            .max()
            .unwrap_or(1);
        let cell_height = frames
            .iter()
            .map(|frame| frame.image.height())
            .max()
            .unwrap_or(1);
        let mut strip = RgbaImage::from_pixel(
            cell_width.saturating_mul(columns),
            cell_height.saturating_mul(rows),
            Rgba([16, 18, 24, 255]),
        );
        for (index, frame) in frames.iter().enumerate() {
            let x = (index as u32 % columns).saturating_mul(cell_width);
            let y = (index as u32 / columns).saturating_mul(cell_height);
            strip.copy_from(&frame.image, x, y).map_err(|_| {
                ArtifactError::UnsafePath(format!("filmstrip frame {index} exceeds bounds"))
            })?;
        }
        let relative = PathBuf::from("animations").join(format!("{state}.filmstrip.png"));
        let path = self.root.join(&relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, encode_png(&strip)?)?;
        Ok(relative.to_string_lossy().into_owned())
    }

    /// Writes a JSON value under the run root.
    pub fn write_json<T: Serialize>(&self, relative: &str, value: &T) -> Result<(), ArtifactError> {
        let relative = safe_relative_path(relative)?;
        let path = self.root.join(&relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(value)?;
        std::fs::write(path, bytes)?;
        Ok(())
    }

    /// Reads a JSON manifest from the run root.
    pub fn read_json<T: for<'de> Deserialize<'de>>(
        &self,
        relative: &str,
    ) -> Result<T, ArtifactError> {
        let relative = safe_relative_path(relative)?;
        Ok(serde_json::from_slice(&std::fs::read(
            self.root.join(relative),
        )?)?)
    }
}

fn encode_png(image: &RgbaImage) -> Result<Vec<u8>, ArtifactError> {
    let mut bytes = Vec::new();
    // Matrix runs encode many high-resolution animation frames. A fixed fast
    // filter keeps output deterministic while avoiding adaptive Paeth search
    // turning a run into minutes of CPU time.
    let encoder = PngEncoder::new_with_quality(
        std::io::Cursor::new(&mut bytes),
        CompressionType::Fast,
        FilterType::Sub,
    );
    encoder.write_image(
        image.as_raw(),
        image.width(),
        image.height(),
        ExtendedColorType::Rgba8,
    )?;
    Ok(bytes)
}

/// Creates a stable hash over a serializable scenario description.
pub fn scenario_hash<T: Serialize>(value: &T) -> Result<String, ArtifactError> {
    Ok(hash_bytes(&serde_json::to_vec(value)?))
}

/// Hashes bytes with SHA-256 for artifact provenance.
#[must_use]
pub fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn safe_component(value: &str) -> Result<String, ArtifactError> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        || value.contains('\0')
    {
        return Err(ArtifactError::UnsafePath(value.to_owned()));
    }
    Ok(value.to_owned())
}

fn safe_relative_path(value: &str) -> Result<PathBuf, ArtifactError> {
    let path = Path::new(value);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::RootDir
            )
        })
    {
        return Err(ArtifactError::UnsafePath(value.to_owned()));
    }
    Ok(path.to_path_buf())
}

/// Converts a capture record into the frame portion of a manifest.
pub fn frame_artifact(record: &CaptureRecord, relative: String, sha256: String) -> FrameArtifact {
    FrameArtifact {
        label: record.label.clone(),
        time_ms: record.time_ms,
        viewport: Some(record.viewport),
        path: relative,
        sha256,
        width: record.image.width(),
        height: record.image.height(),
        input_index: record.input_index,
        diff: record.diff.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_hashes_png_bytes_and_rejects_escape_paths() {
        let root = std::env::temp_dir().join(format!(
            "backend-gui-harness-artifact-test-{}",
            std::process::id()
        ));
        let writer = ArtifactWriter::new(&root).expect("writer");
        let image = RgbaImage::new(1, 1);
        let (path, hash) = writer
            .write_frame("browse", "settled", &image)
            .expect("PNG");
        assert_eq!(path, "frames/browse/settled.png");
        assert_eq!(hash.len(), 64);
        assert!(matches!(
            writer.write_json("../escape.json", &()),
            Err(ArtifactError::UnsafePath(_))
        ));
        std::fs::remove_dir_all(root).expect("test artifact cleanup");
    }

    #[test]
    fn writer_emits_bounded_inspection_crops_with_exact_geometry() {
        let root = std::env::temp_dir().join(format!(
            "backend-gui-harness-crop-test-{}",
            std::process::id()
        ));
        let writer = ArtifactWriter::new(&root).expect("writer");
        let image = RgbaImage::from_pixel(8, 6, Rgba([8, 16, 24, 255]));
        let crop = writer
            .write_crop(
                "browse",
                "midpoint",
                "header",
                &image,
                CropRect::new(6, 4, 8, 8).expect("crop"),
            )
            .expect("crop");
        assert_eq!(crop.rect, CropRect::new(6, 4, 2, 2).expect("fit"));
        assert_eq!(
            image::open(root.join(&crop.path))
                .expect("encoded")
                .into_rgba8()
                .dimensions(),
            (2, 2)
        );
        assert!(matches!(
            writer.write_crop(
                "browse",
                "midpoint",
                "outside",
                &image,
                CropRect::new(8, 0, 1, 1).expect("crop")
            ),
            Err(ArtifactError::UnsafePath(_))
        ));
        std::fs::remove_dir_all(root).expect("test artifact cleanup");
    }

    #[test]
    fn frame_sequence_rejects_duplicate_labels_and_time_regressions() {
        let frame = |label: &str, time_ms: u64| FrameArtifact {
            label: label.to_owned(),
            time_ms,
            path: format!("frames/browse/{label}.png"),
            sha256: String::new(),
            width: 1,
            height: 1,
            viewport: None,
            input_index: None,
            diff: None,
        };
        assert!(validate_frame_sequence(&[frame("start", 0), frame("start", 1)]).is_err());
        assert!(validate_frame_sequence(&[frame("start", 10), frame("settled", 20)]).is_err());
        assert!(
            validate_frame_sequence(&[
                frame("start", 0),
                frame("settled", 2),
                frame("midpoint", 1),
            ])
            .is_err()
        );
        validate_frame_sequence(&[frame("start", 0), frame("retarget", 1), frame("settled", 1)])
            .expect("equal timestamp retarget is valid");
    }
}
