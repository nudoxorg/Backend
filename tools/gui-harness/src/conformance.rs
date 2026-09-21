//! Independent rendered-evidence checks for GUI capture runs.
//!
//! The capture writer records facts about a render, but a writer-side check
//! cannot prove that the encoded PNG has the dimensions it claims or that a
//! stale run report was honest.  This module is deliberately a second pass:
//! it reads manifests, encoded pixels, semantic probes, and journey records
//! from disk and produces a machine-readable report.  It never creates a
//! reference image or treats a file's existence as visual evidence.

use crate::{
    CaptureManifest, CaptureMatrix, DesignContract, DiffMetrics, GuiState, OverlayState, PageState,
    ThemeState, Viewport, hash_bytes, scenario_hash,
};
use image::RgbaImage;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

/// Schema version for a rendered-evidence report.
pub const CONFORMANCE_SCHEMA: u32 = 1;

/// Independent policy knobs for a capture-evidence verification pass.
#[derive(Clone, Debug, Serialize)]
pub struct ConformancePolicy {
    /// Require a run report and require that it declares success.
    pub require_run_report: bool,
    /// Require every manifest to carry exact design-contract metadata.
    pub require_reference: bool,
    /// Require all dimensions declared by the design capture matrix.
    pub require_full_matrix: bool,
    /// Minimum number of frames for a full-motion transition.
    pub min_full_motion_frames: usize,
    /// Minimum number of changed pixels between targeted transition frames.
    pub min_changed_pixels: u64,
    /// Minimum fraction of a viewport that makes an exact uniform band unused.
    pub uniform_band_fraction: f64,
}

impl Default for ConformancePolicy {
    fn default() -> Self {
        Self {
            require_run_report: true,
            require_reference: true,
            require_full_matrix: true,
            min_full_motion_frames: 3,
            min_changed_pixels: 32,
            uniform_band_fraction: 0.25,
        }
    }
}

/// A failure category emitted by the conformance report.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ConformanceFailure {
    /// Stable category used by CI dashboards.
    pub category: String,
    /// Artifact or logical path that supplied the failing evidence.
    pub path: String,
    /// Human-readable failure detail.
    pub message: String,
}

/// An exact uniform band detected in an encoded frame.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UniformRegion {
    /// `row` or `column`.
    pub axis: String,
    /// Inclusive start coordinate.
    pub start: u32,
    /// Exclusive end coordinate.
    pub end: u32,
    /// Fraction of the relevant dimension occupied by this band.
    pub fraction: f64,
    /// Dominant color used for the exact uniform comparison.
    pub color: [u8; 4],
}

/// Pixel and geometry evidence for one encoded PNG.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FrameEvidence {
    /// Manifest-relative frame path.
    pub path: String,
    /// Logical dimensions declared by the manifest.
    pub logical_width: u32,
    /// Logical dimensions declared by the manifest.
    pub logical_height: u32,
    /// Device scale declared by the manifest.
    pub scale: u8,
    /// Exact physical dimensions expected from logical size and scale.
    pub expected_width: u32,
    /// Exact physical dimensions expected from logical size and scale.
    pub expected_height: u32,
    /// Encoded PNG width, when it could be decoded.
    pub actual_width: Option<u32>,
    /// Encoded PNG height, when it could be decoded.
    pub actual_height: Option<u32>,
    /// Encoded PNG hash observed on disk.
    pub sha256: Option<String>,
    /// Fraction of pixels outside the dominant background color.
    pub content_fraction: Option<f64>,
    /// Fraction of pixels with nonzero alpha.
    pub visible_fraction: Option<f64>,
    /// Large exact-color bands that look like an unused viewport remainder.
    pub uniform_regions: Vec<UniformRegion>,
    /// Number of pixels changed from the previous frame in this manifest.
    pub changed_from_previous: Option<u64>,
    /// Whether dimensions, hash, alpha, and coverage checks passed.
    pub passed: bool,
    /// External-baseline pixel comparison, when this capture was compared.
    pub baseline: Option<DiffMetrics>,
}

/// Manifest-level evidence retained in the report.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ManifestEvidence {
    /// Run-relative manifest path.
    pub path: String,
    /// Stable state id.
    pub state: String,
    /// Logical viewport width.
    pub logical_width: u32,
    /// Logical viewport height.
    pub logical_height: u32,
    /// Device scale.
    pub scale: u8,
    /// Number of frames checked.
    pub frame_count: usize,
    /// Whether exact reference metadata was present and matched.
    pub reference_matched: bool,
    /// Whether all external-baseline comparisons satisfied their policy.
    pub baseline_within_policy: bool,
    /// Whether semantic route/data/AX evidence was present.
    pub semantic_matched: bool,
    /// Whether this is a full-motion transition capture.
    pub transition: bool,
    /// Frame-level evidence.
    pub frames: Vec<FrameEvidence>,
}

/// Observed and missing dimensions of the contract's capture matrix.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MatrixEvidence {
    /// Matrix required by the extracted contract.
    pub required: CaptureMatrix,
    /// Viewport bands observed in manifests.
    pub observed_viewports: Vec<String>,
    /// Exact representative logical sizes observed in manifests.
    pub observed_viewport_sizes: Vec<String>,
    /// Scales observed in manifests.
    pub observed_scales: Vec<u8>,
    /// Themes observed in manifests.
    pub observed_themes: Vec<String>,
    /// Motion modes observed in manifests.
    pub observed_motion: Vec<String>,
    /// Interaction states observed in semantic evidence.
    pub observed_states: Vec<String>,
    /// Routes observed in state/semantic evidence.
    pub observed_routes: Vec<String>,
    /// Required overlays observed in state/semantic evidence.
    pub observed_overlays: Vec<String>,
    /// Keyboard/accessibility evidence observed in journeys and semantics.
    pub observed_keyboard: Vec<String>,
    /// Missing dimensions.  A nonempty value fails a full-matrix policy.
    pub missing: BTreeMap<String, Vec<String>>,
}

/// Complete machine-readable result of an independent capture verification.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ConformanceReport {
    /// Report schema.
    pub schema: u32,
    /// Run-relative root inspected.
    pub run_root: String,
    /// Contract identity, when one was supplied.
    pub contract_id: Option<String>,
    /// Contract hash, when one was supplied.
    pub contract_sha256: Option<String>,
    /// Whether the existing run report itself declared success.
    pub run_report_pass: bool,
    /// Number of manifests inspected.
    pub manifests: usize,
    /// Number of frame records inspected.
    pub frames: usize,
    /// Comparison dimensions covered by this report.
    pub comparison_categories: Vec<String>,
    /// Manifest and frame evidence.
    pub evidence: Vec<ManifestEvidence>,
    /// Matrix coverage.
    pub matrix: MatrixEvidence,
    /// Independent failures.
    pub failures: Vec<ConformanceFailure>,
    /// Final policy result.
    pub pass: bool,
}

/// Errors that prevent a conformance report from being read at all.
#[derive(Debug, Error)]
pub enum ConformanceError {
    /// Filesystem failure.
    #[error("capture conformance I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// JSON decoding failure.
    #[error("capture conformance JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    /// PNG decoding failure.
    #[error("capture conformance image failed: {0}")]
    Image(#[from] image::ImageError),
    /// Artifact hashing/serialization failure.
    #[error("capture conformance artifact check failed: {0}")]
    Artifact(String),
    /// Contract metadata was internally inconsistent.
    #[error("capture conformance contract check failed: {0}")]
    Contract(String),
}

/// Verifies physical capture evidence below `root`.
///
/// This pass checks the PNG bytes themselves, so a stale report that says a
/// run passed cannot hide a logical/physical scale mismatch or a cropped
/// scene surrounded by a large uniform remainder.
pub fn verify_capture_run(
    root: &Path,
    contract: Option<&DesignContract>,
    policy: ConformancePolicy,
) -> Result<ConformanceReport, ConformanceError> {
    let required = contract
        .map(|contract| contract.capture_matrix.clone())
        .unwrap_or_default();
    let (run_report_pass, mut failures) = inspect_run_report(root, policy.require_run_report)?;
    let mut manifest_paths = Vec::new();
    collect_manifests(root, &mut manifest_paths)?;
    manifest_paths.sort();

    let mut evidence = Vec::new();
    let mut observed_viewports = BTreeSet::new();
    let mut observed_viewport_sizes = BTreeSet::new();
    let mut observed_scales = BTreeSet::new();
    let mut observed_themes = BTreeSet::new();
    let mut observed_motion = BTreeSet::new();
    let mut observed_states = BTreeSet::new();
    let mut observed_routes = BTreeSet::new();
    let mut observed_overlays = BTreeSet::new();
    let mut observed_keyboard = BTreeSet::new();

    for manifest_path in &manifest_paths {
        let manifest_bytes = std::fs::read(manifest_path)?;
        let manifest: CaptureManifest = serde_json::from_slice(&manifest_bytes)?;
        let relative_manifest = display_relative(root, manifest_path);
        let capture_root = manifest_path
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| ConformanceError::Contract(relative_manifest.clone()))?;
        let viewport = manifest.config.viewport;
        let expected = viewport.physical_size();
        let band = viewport_band(viewport);
        let theme = theme_name(manifest.state.theme).to_owned();
        let motion = if manifest.state.reduced_motion {
            "reduced"
        } else {
            "full"
        };
        observed_viewports.insert(band.to_owned());
        observed_viewport_sizes.insert(format!("{}x{}", viewport.width, viewport.height));
        observed_scales.insert(viewport.scale);
        observed_themes.insert(theme.clone());
        observed_motion.insert(motion.to_owned());
        if manifest.schema != 1 {
            failures.push(failure(
                "manifest",
                &relative_manifest,
                format!("unsupported capture manifest schema {}", manifest.schema),
            ));
        }
        if !matches!(viewport.scale, 1 | 2) {
            failures.push(failure(
                "geometry",
                &relative_manifest,
                format!("unsupported device scale {}", viewport.scale),
            ));
        }
        if manifest.config.theme != manifest.state.theme {
            failures.push(failure(
                "color",
                &relative_manifest,
                "capture config theme differs from rendered state theme",
            ));
        }
        if let Some(route) = route_name(&manifest.state) {
            observed_routes.insert(route.to_owned());
        }
        if let Some(overlay) = overlay_name(manifest.state.overlay) {
            observed_overlays.insert(overlay.to_owned());
        }

        let reference_matched = match (&manifest.reference, contract) {
            (Some(reference), Some(contract)) => {
                let expected_reference = contract
                    .reference_metadata()
                    .map_err(|error| ConformanceError::Contract(error.to_string()))?;
                if reference != &expected_reference {
                    failures.push(failure(
                        "reference",
                        &relative_manifest,
                        "manifest reference metadata does not match the exact design contract",
                    ));
                    false
                } else {
                    true
                }
            }
            (Some(reference), None) => reference.capture_policy.starts_with("external-"),
            (None, _) => {
                if policy.require_reference {
                    failures.push(failure(
                        "reference",
                        &relative_manifest,
                        "manifest has no exact external design-contract metadata",
                    ));
                }
                false
            }
        };

        let semantic_matched;
        if !manifest.baseline_within_policy {
            failures.push(failure(
                "comparison",
                &relative_manifest,
                "external baseline comparison exceeded its pixel policy",
            ));
        }
        if let Some(semantic) = manifest.semantic_artifact.as_deref() {
            if !is_safe_relative_path(semantic) {
                failures.push(failure(
                    "semantic",
                    &relative_manifest,
                    "semantic artifact path escapes the capture root",
                ));
                semantic_matched = false;
            } else {
                let semantic_path = capture_root.join(semantic);
                match std::fs::read(&semantic_path)
                    .map_err(ConformanceError::Io)
                    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).map_err(Into::into))
                {
                    Ok(value) => {
                        let semantic_hash_matched =
                            if let Some(expected) = manifest.semantic_sha256.as_deref() {
                                let bytes = std::fs::read(&semantic_path)?;
                                if expected != hash_bytes(&bytes) {
                                    failures.push(failure(
                                        "live-data",
                                        &relative_manifest,
                                        "semantic artifact hash does not match the manifest",
                                    ));
                                    false
                                } else {
                                    true
                                }
                            } else {
                                failures.push(failure(
                                    "live-data",
                                    &relative_manifest,
                                    "semantic artifact has no content hash",
                                ));
                                false
                            };
                        let semantic_evidence = inspect_semantics(
                            &value,
                            &relative_manifest,
                            manifest.script_id.is_some(),
                            &mut failures,
                        );
                        semantic_matched = semantic_hash_matched && semantic_evidence.valid;
                        observed_states.extend(semantic_evidence.states);
                        observed_keyboard.extend(semantic_evidence.keyboard);
                        if semantic_evidence.focus {
                            observed_states.insert("focus".to_owned());
                        }
                        if semantic_evidence.hover {
                            observed_states.insert("hover".to_owned());
                        }
                        if semantic_evidence.pressed {
                            observed_states.insert("pressed".to_owned());
                        }
                        if semantic_evidence.disabled {
                            observed_states.insert("disabled".to_owned());
                        }
                    }
                    Err(error) => {
                        failures.push(failure(
                            "semantic",
                            &relative_manifest,
                            format!("cannot read semantic artifact: {error}"),
                        ));
                        semantic_matched = false;
                    }
                }
            }
        } else {
            failures.push(failure(
                "semantic",
                &relative_manifest,
                "manifest does not record a semantic artifact",
            ));
            semantic_matched = false;
        }

        if !provenance_is_rendered(&manifest) {
            failures.push(failure(
                "provenance",
                &relative_manifest,
                "manifest provenance is incomplete or identifies a synthetic capture",
            ));
        }
        if manifest.source_revision.as_deref() != Some(manifest.provenance.source_revision.as_str())
        {
            failures.push(failure(
                "provenance",
                &relative_manifest,
                "manifest source_revision differs from rendered provenance",
            ));
        }
        let mut manifest_frames = Vec::new();
        let mut images = Vec::new();
        let mut previous: Option<RgbaImage> = None;
        let mut frame_paths = BTreeSet::new();
        if manifest.frames.is_empty() {
            failures.push(failure(
                "geometry",
                &relative_manifest,
                "manifest contains no physical frames",
            ));
        }
        if manifest.sequence.frame_count != manifest.frames.len() {
            failures.push(failure(
                "animation",
                &relative_manifest,
                "sequence frame_count disagrees with manifest frames",
            ));
        }
        if manifest.sequence.encoding != "png-sequence" {
            failures.push(failure(
                "animation",
                &relative_manifest,
                format!(
                    "unsupported frame sequence encoding {:?}",
                    manifest.sequence.encoding
                ),
            ));
        }
        let frame_labels = manifest
            .frames
            .iter()
            .map(|frame| frame.label.as_str())
            .collect::<Vec<_>>();
        if manifest.sequence.keyframes
            != frame_labels
                .iter()
                .map(|label| (*label).to_owned())
                .collect::<Vec<_>>()
        {
            failures.push(failure(
                "animation",
                &relative_manifest,
                "sequence keyframes do not match ordered frame labels",
            ));
        }
        if manifest.sequence.duration_ms != manifest.frames.last().map_or(0, |frame| frame.time_ms)
        {
            failures.push(failure(
                "animation",
                &relative_manifest,
                "sequence duration does not match the last frame time",
            ));
        }
        let expected_sequence = scenario_hash(&manifest.frames)
            .map_err(|error| ConformanceError::Artifact(error.to_string()))?;
        if expected_sequence != manifest.sequence_sha256 {
            failures.push(failure(
                "animation",
                &relative_manifest,
                "sequence hash does not match the recorded frame sequence",
            ));
        }
        let expected_scenario = scenario_hash(&(
            &manifest.state,
            &manifest.config,
            manifest.script_id.as_deref(),
        ))
        .map_err(|error| ConformanceError::Artifact(error.to_string()))?;
        if expected_scenario != manifest.scenario_sha256 {
            failures.push(failure(
                "manifest",
                &relative_manifest,
                "scenario hash does not match state/config/script metadata",
            ));
        }

        for frame in &manifest.frames {
            let mut frame_evidence = FrameEvidence {
                path: frame.path.clone(),
                logical_width: viewport.width,
                logical_height: viewport.height,
                scale: viewport.scale,
                expected_width: expected.0,
                expected_height: expected.1,
                actual_width: None,
                actual_height: None,
                sha256: None,
                content_fraction: None,
                visible_fraction: None,
                uniform_regions: Vec::new(),
                changed_from_previous: None,
                passed: false,
                baseline: frame.diff.clone(),
            };
            if !frame_paths.insert(frame.path.clone()) || !is_safe_relative_path(&frame.path) {
                failures.push(failure(
                    "geometry",
                    &relative_manifest,
                    format!("unsafe or duplicate frame path {:?}", frame.path),
                ));
                manifest_frames.push(frame_evidence);
                continue;
            }
            let frame_path = capture_root.join(&frame.path);
            let encoded = match std::fs::read(&frame_path) {
                Ok(encoded) => encoded,
                Err(error) => {
                    failures.push(failure(
                        "geometry",
                        &relative_manifest,
                        format!("missing frame {}: {error}", frame.path),
                    ));
                    manifest_frames.push(frame_evidence);
                    continue;
                }
            };
            let actual_hash = hash_bytes(&encoded);
            frame_evidence.sha256 = Some(actual_hash.clone());
            if actual_hash != frame.sha256 {
                failures.push(failure(
                    "comparison",
                    &relative_manifest,
                    format!("frame {} has a stale PNG hash", frame.path),
                ));
            }
            if let Some(diff) = frame.diff.as_ref() {
                if !diff.same_dimensions {
                    failures.push(failure(
                        "comparison",
                        &relative_manifest,
                        format!(
                            "frame {} records a dimension-mismatched baseline",
                            frame.path
                        ),
                    ));
                }
                if !diff.within_policy {
                    failures.push(failure(
                        "comparison",
                        &relative_manifest,
                        format!(
                            "frame {} is outside its declared pixel comparison policy",
                            frame.path
                        ),
                    ));
                }
                if !diff.changed_fraction.is_finite()
                    || !diff.mean_channel_error.is_finite()
                    || !diff.perceptual_mean_error.is_finite()
                {
                    failures.push(failure(
                        "comparison",
                        &relative_manifest,
                        format!("frame {} records non-finite comparison metrics", frame.path),
                    ));
                }
            }
            let image = match image::load_from_memory(&encoded) {
                Ok(image) => image.into_rgba8(),
                Err(error) => {
                    failures.push(failure(
                        "geometry",
                        &relative_manifest,
                        format!("frame {} is not a readable PNG: {error}", frame.path),
                    ));
                    manifest_frames.push(frame_evidence);
                    continue;
                }
            };
            frame_evidence.actual_width = Some(image.width());
            frame_evidence.actual_height = Some(image.height());
            if image.dimensions() != expected
                || (frame.width, frame.height) != expected
                || image.dimensions() != (frame.width, frame.height)
            {
                failures.push(failure(
                    "geometry",
                    &relative_manifest,
                    format!(
                        "frame {} is {}x{} physical pixels; expected logical {}x{} at {}x ({}x{})",
                        frame.path,
                        image.width(),
                        image.height(),
                        viewport.width,
                        viewport.height,
                        viewport.scale,
                        expected.0,
                        expected.1
                    ),
                ));
            }
            let geometry = inspect_image_geometry(&image, policy.uniform_band_fraction);
            frame_evidence.content_fraction = Some(geometry.content_fraction);
            frame_evidence.visible_fraction = Some(geometry.visible_fraction);
            frame_evidence.uniform_regions = geometry.uniform_regions.clone();
            if geometry.visible_fraction == 0.0 {
                failures.push(failure(
                    "geometry",
                    &relative_manifest,
                    format!("frame {} is fully transparent", frame.path),
                ));
            }
            if geometry.content_fraction < 0.005 {
                failures.push(failure(
                    "clipping",
                    &relative_manifest,
                    format!("frame {} is nearly a single uniform color", frame.path),
                ));
            }
            if !geometry.uniform_regions.is_empty() {
                failures.push(failure(
                    "clipping",
                    &relative_manifest,
                    format!(
                        "frame {} contains a large uniform unused region: {}",
                        frame.path,
                        format_regions(&geometry.uniform_regions)
                    ),
                ));
            }
            let changed = previous
                .as_ref()
                .filter(|previous| previous.dimensions() == image.dimensions())
                .map(|previous| changed_pixels(previous, &image));
            frame_evidence.changed_from_previous = changed;
            previous = Some(image.clone());
            images.push(image);
            frame_evidence.passed = actual_hash == frame.sha256
                && frame_evidence.actual_width == Some(expected.0)
                && frame_evidence.actual_height == Some(expected.1)
                && (frame.width, frame.height) == expected
                && geometry.visible_fraction > 0.0
                && geometry.content_fraction >= 0.005
                && geometry.uniform_regions.is_empty();
            if let Some(diff) = &frame.diff
                && !diff.within_policy
            {
                failures.push(failure(
                    "color",
                    &relative_manifest,
                    format!(
                        "external baseline differs for {}: {} changed pixels, max channel error {}",
                        frame.path, diff.changed_pixels, diff.max_channel_error
                    ),
                ));
            }
            manifest_frames.push(frame_evidence);
        }
        inspect_animation(
            &manifest,
            &images,
            &relative_manifest,
            &policy,
            &mut failures,
        );
        if manifest.script_id.is_some() && !manifest.state.reduced_motion {
            observed_keyboard.insert("animation-frame".to_owned());
        }
        let path = relative_manifest;
        evidence.push(ManifestEvidence {
            path,
            state: manifest.state.id,
            logical_width: viewport.width,
            logical_height: viewport.height,
            scale: viewport.scale,
            frame_count: manifest_frames.len(),
            reference_matched,
            semantic_matched,
            baseline_within_policy: manifest.baseline_within_policy,
            transition: manifest.script_id.is_some(),
            frames: manifest_frames,
        });
    }

    if manifest_paths.is_empty() {
        failures.push(failure(
            "manifest",
            "manifests",
            "run contains no capture manifests",
        ));
    }

    inspect_journeys(
        root,
        &mut observed_keyboard,
        &mut observed_states,
        &mut failures,
    )?;
    if !observed_states.contains("rest") && !manifest_paths.is_empty() {
        observed_states.insert("rest".to_owned());
    }
    let missing = missing_matrix(
        &required,
        &observed_viewports,
        &observed_viewport_sizes,
        &observed_scales,
        &observed_themes,
        &observed_motion,
        &observed_states,
        &observed_routes,
        &observed_overlays,
        &observed_keyboard,
    );
    if policy.require_full_matrix && missing.values().any(|values| !values.is_empty()) {
        failures.push(failure(
            "matrix",
            "capture-matrix",
            format!("required capture dimensions are missing: {missing:?}"),
        ));
    }
    let matrix = MatrixEvidence {
        required,
        observed_viewports: observed_viewports.into_iter().collect(),
        observed_viewport_sizes: observed_viewport_sizes.into_iter().collect(),
        observed_scales: observed_scales.into_iter().collect(),
        observed_themes: observed_themes.into_iter().collect(),
        observed_motion: observed_motion.into_iter().collect(),
        observed_states: observed_states.into_iter().collect(),
        observed_routes: observed_routes.into_iter().collect(),
        observed_overlays: observed_overlays.into_iter().collect(),
        observed_keyboard: observed_keyboard.into_iter().collect(),
        missing,
    };
    let (contract_id, contract_sha256) = contract
        .map(|contract| (Some(contract.contract_id.clone()), contract.sha256().ok()))
        .unwrap_or((None, None));
    Ok(ConformanceReport {
        schema: CONFORMANCE_SCHEMA,
        run_root: root.display().to_string(),
        contract_id,
        contract_sha256,
        run_report_pass,
        manifests: evidence.len(),
        frames: evidence.iter().map(|manifest| manifest.frames.len()).sum(),
        comparison_categories: vec![
            "geometry".to_owned(),
            "color".to_owned(),
            "typography".to_owned(),
            "clipping".to_owned(),
            "focus".to_owned(),
            "animation".to_owned(),
        ],
        evidence,
        matrix,
        pass: failures.is_empty() && run_report_pass,
        failures,
    })
}

fn inspect_run_report(
    root: &Path,
    required: bool,
) -> Result<(bool, Vec<ConformanceFailure>), ConformanceError> {
    let path = root.join("run-report.json");
    if !path.is_file() {
        return Ok((
            !required,
            if required {
                vec![failure(
                    "run-report",
                    "run-report.json",
                    "run report is missing; capture evidence cannot pass on file existence",
                )]
            } else {
                Vec::new()
            },
        ));
    }
    let value: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let pass = value.get("pass").and_then(Value::as_bool).unwrap_or(false);
    let no_failures = value
        .get("failures")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty);
    if pass && no_failures {
        Ok((true, Vec::new()))
    } else {
        Ok((
            false,
            vec![failure(
                "run-report",
                "run-report.json",
                "run report does not declare a passing run with an empty failure list",
            )],
        ))
    }
}

fn inspect_animation(
    manifest: &CaptureManifest,
    images: &[RgbaImage],
    path: &str,
    policy: &ConformancePolicy,
    failures: &mut Vec<ConformanceFailure>,
) {
    let Some(_) = manifest.script_id.as_deref() else {
        return;
    };
    if manifest.state.reduced_motion {
        if images.windows(2).any(|frames| frames[0] != frames[1]) {
            failures.push(failure(
                "animation",
                path,
                "reduced-motion transition contains changing frames",
            ));
        }
        return;
    }
    if images.len() < policy.min_full_motion_frames {
        failures.push(failure(
            "animation",
            path,
            format!(
                "full-motion transition has {} frame(s); at least {} are required",
                images.len(),
                policy.min_full_motion_frames
            ),
        ));
    }
    let times_are_monotonic = manifest
        .frames
        .windows(2)
        .all(|frames| frames[0].time_ms <= frames[1].time_ms);
    let has_elapsed_time = manifest
        .frames
        .first()
        .zip(manifest.frames.last())
        .is_some_and(|(first, last)| last.time_ms > first.time_ms);
    if !times_are_monotonic || !has_elapsed_time {
        failures.push(failure(
            "animation",
            path,
            "full-motion frame times must be monotonic with positive elapsed duration",
        ));
    }
    let changed = images
        .windows(2)
        .map(|frames| changed_pixels(&frames[0], &frames[1]))
        .max()
        .unwrap_or(0);
    if changed < policy.min_changed_pixels {
        failures.push(failure(
            "animation",
            path,
            format!(
                "full-motion transition changed {changed} targeted pixel(s); at least {} are required",
                policy.min_changed_pixels
            ),
        ));
    }
}

#[derive(Default)]
struct SemanticEvidence {
    valid: bool,
    focus: bool,
    hover: bool,
    pressed: bool,
    disabled: bool,
    states: BTreeSet<String>,
    keyboard: BTreeSet<String>,
}

fn inspect_semantics(
    value: &Value,
    path: &str,
    transition: bool,
    failures: &mut Vec<ConformanceFailure>,
) -> SemanticEvidence {
    let mut evidence = SemanticEvidence {
        valid: true,
        ..SemanticEvidence::default()
    };
    let Some(probes) = value.as_array() else {
        failures.push(failure(
            "semantic",
            path,
            "semantic artifact must be an array of rendered probes",
        ));
        return evidence;
    };
    if probes.is_empty() {
        failures.push(failure(
            "semantic",
            path,
            "semantic artifact has no rendered probes",
        ));
        evidence.valid = false;
        return evidence;
    }
    for (index, probe) in probes.iter().enumerate() {
        let Some(object) = probe.as_object() else {
            failures.push(failure(
                "semantic",
                path,
                format!("probe {index} is not an object"),
            ));
            evidence.valid = false;
            continue;
        };
        let route = nonempty_string(object, "route").or_else(|| nonempty_string(object, "page"));
        if route.is_none() {
            failures.push(failure(
                "route",
                path,
                format!("probe {index} has no live route/page root"),
            ));
            evidence.valid = false;
        }
        for key in [
            "action_tree_route",
            "data_revision",
            "coordinate",
            "coordinate_revision",
        ] {
            if nonempty_string(object, key).is_none() {
                failures.push(failure(
                    "live-data",
                    path,
                    format!("probe {index} has no nonempty {key}"),
                ));
                evidence.valid = false;
            }
        }
        for key in ["action_tree_revision", "action_tree_generation"] {
            if object.get(key).and_then(Value::as_u64).is_none() {
                failures.push(failure(
                    "live-data",
                    path,
                    format!("probe {index} has no numeric {key}"),
                ));
                evidence.valid = false;
            }
        }
        if let (Some(data), Some(coordinate)) = (
            nonempty_string(object, "data_revision"),
            nonempty_string(object, "coordinate_revision"),
        ) {
            if data != coordinate {
                failures.push(failure(
                    "live-data",
                    path,
                    format!("probe {index} coordinate revision differs from data revision"),
                ));
                evidence.valid = false;
            }
        }
        let Some(actions) = object.get("actions").and_then(Value::as_array) else {
            failures.push(failure(
                "focus",
                path,
                format!("probe {index} has no accessibility action tree"),
            ));
            evidence.valid = false;
            continue;
        };
        if actions.is_empty() {
            failures.push(failure(
                "focus",
                path,
                format!("probe {index} accessibility action tree is empty"),
            ));
            evidence.valid = false;
        }
        let mut visible_enabled = false;
        for action in actions {
            let Some(action) = action.as_object() else {
                evidence.valid = false;
                continue;
            };
            for key in ["id", "label", "role"] {
                if nonempty_string(action, key).is_none() {
                    failures.push(failure(
                        "focus",
                        path,
                        format!("accessibility action omits {key}"),
                    ));
                    evidence.valid = false;
                }
            }
            if action.get("focus_order").and_then(Value::as_u64).is_none() {
                failures.push(failure(
                    "focus",
                    path,
                    "accessibility action has no numeric focus_order",
                ));
                evidence.valid = false;
            }
            let visible = action
                .get("visible")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let enabled = action
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            visible_enabled |= visible && enabled;
            evidence.focus |= action
                .get("focus")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            evidence.hover |= action
                .get("hover")
                .or_else(|| action.get("hovered"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            evidence.pressed |= action
                .get("pressed")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            evidence.disabled |= action
                .get("disabled")
                .and_then(Value::as_bool)
                .unwrap_or(!enabled);
        }
        if !visible_enabled {
            failures.push(failure(
                "focus",
                path,
                format!("probe {index} has no visible enabled action"),
            ));
            evidence.valid = false;
        }
        if nonempty_string(object, "focus_id").is_some()
            || nonempty_string(object, "focus").is_some()
        {
            evidence.focus = true;
        }
        if evidence.focus {
            evidence.keyboard.insert("focus-visible".to_owned());
        }
        evidence.keyboard.insert("accessibility-tree".to_owned());
        if let Some(input) = nonempty_string(object, "input") {
            for token in input.split_whitespace() {
                add_keyboard_token(&mut evidence.keyboard, token);
            }
        }
        if object.get("input_index").is_some() && transition {
            evidence.keyboard.insert("tab".to_owned());
        }
    }
    if !evidence.focus {
        failures.push(failure(
            "focus",
            path,
            "semantic timeline contains no focused action or focus owner",
        ));
        evidence.valid = false;
    }
    if evidence.hover {
        evidence.states.insert("hover".to_owned());
    }
    if evidence.pressed {
        evidence.states.insert("pressed".to_owned());
    }
    if evidence.disabled {
        evidence.states.insert("disabled".to_owned());
    }
    evidence
}

fn inspect_journeys(
    root: &Path,
    keyboard: &mut BTreeSet<String>,
    states: &mut BTreeSet<String>,
    failures: &mut Vec<ConformanceFailure>,
) -> Result<(), ConformanceError> {
    let mut paths = Vec::new();
    collect_named_json(root.join("journeys").as_path(), &mut paths)?;
    for path in paths {
        let relative = display_relative(root, &path);
        let value: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        let Some(steps) = value.get("steps").and_then(Value::as_array) else {
            failures.push(failure("keyboard", &relative, "journey has no input steps"));
            continue;
        };
        for step in steps {
            if let Some(kind) = step.get("type").and_then(Value::as_str) {
                if kind == "focus-next" {
                    keyboard.insert("tab".to_owned());
                } else if kind == "focus-previous" {
                    keyboard.insert("shift-tab".to_owned());
                } else if kind == "pointer-move" || kind == "hover" {
                    states.insert("hover".to_owned());
                } else if kind == "pointer-down" || kind == "click" {
                    states.insert("pressed".to_owned());
                } else if kind == "key" {
                    if let Some(value) = step.get("value").and_then(Value::as_str) {
                        add_keyboard_token(keyboard, value);
                    }
                }
            }
        }
        if value
            .get("observations")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
        {
            failures.push(failure(
                "keyboard",
                &relative,
                "journey has no rendered observations",
            ));
        }
    }
    Ok(())
}

fn add_keyboard_token(output: &mut BTreeSet<String>, token: &str) {
    let token = token.to_ascii_lowercase();
    if token == "tab" {
        output.insert("tab".to_owned());
    } else if token == "shift-tab" || token == "shift+tab" {
        output.insert("shift-tab".to_owned());
    } else if token == "enter" || token == "return" {
        output.insert("enter".to_owned());
    } else if token == "escape" || token == "esc" {
        output.insert("escape".to_owned());
    }
}

fn missing_matrix(
    required: &CaptureMatrix,
    viewports: &BTreeSet<String>,
    viewport_sizes: &BTreeSet<String>,
    scales: &BTreeSet<u8>,
    themes: &BTreeSet<String>,
    motion: &BTreeSet<String>,
    states: &BTreeSet<String>,
    routes: &BTreeSet<String>,
    overlays: &BTreeSet<String>,
    keyboard: &BTreeSet<String>,
) -> BTreeMap<String, Vec<String>> {
    let mut missing = BTreeMap::new();
    missing.insert(
        "viewports".to_owned(),
        required
            .viewports
            .iter()
            .filter(|item| !viewports.contains(*item))
            .cloned()
            .collect(),
    );
    let required_sizes = required
        .viewport_sizes
        .values()
        .map(|size| format!("{}x{}", size.width, size.height))
        .collect::<BTreeSet<_>>();
    missing.insert(
        "viewport_sizes".to_owned(),
        required_sizes
            .into_iter()
            .filter(|item| !viewport_sizes.contains(item))
            .collect(),
    );
    missing.insert(
        "scales".to_owned(),
        required
            .scales
            .iter()
            .map(u8::to_string)
            .filter(|item| !scales.contains(&item.parse().unwrap_or_default()))
            .collect(),
    );
    missing.insert(
        "themes".to_owned(),
        required
            .themes
            .iter()
            .filter(|item| !themes.contains(*item))
            .cloned()
            .collect(),
    );
    missing.insert(
        "motion".to_owned(),
        required
            .motion
            .iter()
            .filter(|item| !motion.contains(*item))
            .cloned()
            .collect(),
    );
    missing.insert(
        "states".to_owned(),
        required
            .states
            .iter()
            .filter(|item| !states.contains(*item))
            .cloned()
            .collect(),
    );
    missing.insert(
        "routes".to_owned(),
        required
            .routes
            .iter()
            .filter(|item| !routes.contains(*item))
            .cloned()
            .collect(),
    );
    missing.insert(
        "overlays".to_owned(),
        required
            .overlays
            .iter()
            .filter(|item| !overlays.contains(*item))
            .cloned()
            .collect(),
    );
    missing.insert(
        "keyboard".to_owned(),
        required
            .keyboard
            .iter()
            .filter(|item| !keyboard.contains(*item))
            .cloned()
            .collect(),
    );
    missing
}

fn route_name(state: &GuiState) -> Option<&'static str> {
    match state.page {
        Some(PageState::Browse | PageState::Project) => Some("orbit"),
        Some(PageState::Package) => Some("package"),
        Some(PageState::Declaration | PageState::Docs) => Some("page"),
        Some(PageState::Source | PageState::Code | PageState::CodeSearch) => Some("source"),
        Some(_) => Some("page"),
        None => None,
    }
}

fn overlay_name(overlay: Option<OverlayState>) -> Option<&'static str> {
    match overlay {
        Some(OverlayState::Loading | OverlayState::Indexing) => Some("loading"),
        Some(OverlayState::Stale) => Some("stale"),
        Some(OverlayState::Offline) => Some("offline"),
        Some(OverlayState::Fault) => Some("error"),
        _ => None,
    }
}

fn theme_name(theme: ThemeState) -> &'static str {
    match theme {
        ThemeState::Ink => "ink",
        ThemeState::Vellum => "glacier",
    }
}

fn viewport_band(viewport: Viewport) -> &'static str {
    if viewport.width >= 1280 {
        "wide"
    } else if viewport.width <= 800 {
        "narrow"
    } else {
        "compact"
    }
}

fn provenance_is_rendered(manifest: &CaptureManifest) -> bool {
    let provenance = &manifest.provenance;
    if provenance.source_revision.trim().is_empty()
        || provenance.rustc.trim().is_empty()
        || provenance.toolchain.trim().is_empty()
        || provenance.os.trim().is_empty()
        || provenance.hardware.trim().is_empty()
        || provenance.renderer.trim().is_empty()
        || provenance.backend.trim().is_empty()
        || provenance.encoder.trim().is_empty()
    {
        return false;
    }
    let identity = format!("{} {}", provenance.renderer, provenance.backend).to_ascii_lowercase();
    !["fixture", "synthetic", "generated", "mock"]
        .iter()
        .any(|marker| identity.contains(marker))
}

fn nonempty_string<'a>(object: &'a serde_json::Map<String, Value>, key: &str) -> Option<&'a str> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty() && *value != "undetermined")
}

fn inspect_image_geometry(image: &RgbaImage, uniform_band_fraction: f64) -> GeometryEvidence {
    let total = u64::from(image.width()) * u64::from(image.height());
    if total == 0 {
        return GeometryEvidence::default();
    }
    // Keep the tie-break deterministic. A hash map's randomized iteration
    // order can otherwise choose a different dominant colour for an image
    // with two equally sized surfaces, which would make clipping evidence
    // vary from process to process.
    let mut colors = HashMap::<[u8; 4], u64>::new();
    let mut visible = 0_u64;
    for pixel in image.pixels() {
        *colors.entry(pixel.0).or_default() += 1;
        if pixel[3] != 0 {
            visible += 1;
        }
    }
    let (dominant, dominant_count) =
        colors
            .into_iter()
            .fold(([0, 0, 0, 0], 0_u64), |current, candidate| {
                if candidate.1 > current.1 || (candidate.1 == current.1 && candidate.0 < current.0)
                {
                    candidate
                } else {
                    current
                }
            });
    let mut row_counts = vec![0_u32; image.height() as usize];
    let mut column_counts = vec![0_u32; image.width() as usize];
    for (index, pixel) in image.pixels().enumerate() {
        if pixel.0 == dominant {
            row_counts[index / image.width() as usize] += 1;
            column_counts[index % image.width() as usize] += 1;
        }
    }
    let row_uniform = row_counts
        .into_iter()
        .map(|count| f64::from(count) / f64::from(image.width()) >= 0.995)
        .collect::<Vec<_>>();
    let column_uniform = column_counts
        .into_iter()
        .map(|count| f64::from(count) / f64::from(image.height()) >= 0.995)
        .collect::<Vec<_>>();
    let mut uniform_regions = runs(&row_uniform, uniform_band_fraction, "row", dominant);
    uniform_regions.extend(runs(
        &column_uniform,
        uniform_band_fraction,
        "column",
        dominant,
    ));
    let content = total.saturating_sub(dominant_count);
    GeometryEvidence {
        content_fraction: content as f64 / total as f64,
        visible_fraction: visible as f64 / total as f64,
        uniform_regions,
    }
}

#[derive(Default)]
struct GeometryEvidence {
    content_fraction: f64,
    visible_fraction: f64,
    uniform_regions: Vec<UniformRegion>,
}

fn runs(values: &[bool], minimum_fraction: f64, axis: &str, color: [u8; 4]) -> Vec<UniformRegion> {
    let mut output = Vec::new();
    let mut start = None;
    for (index, uniform) in values
        .iter()
        .copied()
        .enumerate()
        .chain(std::iter::once((values.len(), false)))
    {
        if uniform && start.is_none() {
            start = Some(index);
        } else if !uniform {
            if let Some(start) = start.take() {
                let end = index;
                let fraction = (end - start) as f64 / values.len().max(1) as f64;
                if fraction >= minimum_fraction {
                    output.push(UniformRegion {
                        axis: axis.to_owned(),
                        start: start as u32,
                        end: end as u32,
                        fraction,
                        color,
                    });
                }
            }
        }
    }
    output
}

fn changed_pixels(left: &RgbaImage, right: &RgbaImage) -> u64 {
    if left.dimensions() != right.dimensions() {
        return u64::MAX;
    }
    left.pixels()
        .zip(right.pixels())
        .filter(|(left, right)| left != right)
        .count() as u64
}

fn format_regions(regions: &[UniformRegion]) -> String {
    regions
        .iter()
        .map(|region| format!("{}[{}..{}]", region.axis, region.start, region.end))
        .collect::<Vec<_>>()
        .join(", ")
}

fn failure(category: &str, path: &str, message: impl Into<String>) -> ConformanceFailure {
    ConformanceFailure {
        category: category.to_owned(),
        path: path.to_owned(),
        message: message.into(),
    }
}

fn collect_manifests(root: &Path, output: &mut Vec<PathBuf>) -> Result<(), ConformanceError> {
    if !root.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_manifests(&path, output)?;
        } else if path
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "manifests")
            && path
                .extension()
                .is_some_and(|extension| extension == "json")
        {
            output.push(path);
        }
    }
    Ok(())
}

fn collect_named_json(root: &Path, output: &mut Vec<PathBuf>) -> Result<(), ConformanceError> {
    if !root.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_named_json(&path, output)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            output.push(path);
        }
    }
    Ok(())
}

fn is_safe_relative_path(value: &str) -> bool {
    let path = Path::new(value);
    !path.is_absolute()
        && path
            .components()
            .all(|component| !matches!(component, Component::ParentDir | Component::RootDir))
}

fn display_relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ArtifactWriter, CaptureConfig, CaptureManifest, CaptureProvenance, CaptureSession,
        FrameArtifact,
    };
    use image::{Rgba, RgbaImage};

    fn provenance() -> CaptureProvenance {
        CaptureProvenance {
            source_revision: "test-revision".to_owned(),
            dirty: false,
            rustc: "rustc test".to_owned(),
            toolchain: "test-toolchain".to_owned(),
            gpui_revision: "test-gpui".to_owned(),
            gpui_ce_revision: "test-gpui-ce".to_owned(),
            os: "test-os".to_owned(),
            hardware: "test-hardware".to_owned(),
            renderer: "test-renderer".to_owned(),
            backend: "test-backend".to_owned(),
            encoder: "png:rgba8:fast:sub".to_owned(),
        }
    }

    #[test]
    fn detects_upper_left_scene_with_black_remainder() {
        let mut image = RgbaImage::from_pixel(16, 12, Rgba([0, 0, 0, 255]));
        for y in 0..6 {
            for x in 0..8 {
                image.put_pixel(x, y, Rgba([20, 30, 40, 255]));
            }
        }
        let evidence = inspect_image_geometry(&image, 0.25);
        assert!(
            evidence
                .uniform_regions
                .iter()
                .any(|region| region.axis == "column" && region.start >= 8)
        );
        assert!(
            evidence
                .uniform_regions
                .iter()
                .any(|region| region.axis == "row" && region.start >= 6)
        );
    }

    #[test]
    fn stale_two_x_manifest_fails_exact_physical_geometry() {
        let root = std::env::temp_dir().join(format!(
            "backend-gui-conformance-scale-regression-{}",
            std::process::id()
        ));
        let capture_root = root.join("1440x1000@1x");
        let writer = ArtifactWriter::new(&capture_root).expect("writer");
        let mut image = RgbaImage::from_pixel(2880, 2000, Rgba([0, 0, 0, 255]));
        for y in 0..1000 {
            for x in 0..1440 {
                image.put_pixel(x, y, Rgba([18, 20, 25, 255]));
            }
        }
        let (frame_path, frame_hash) = writer
            .write_frame("serde-package", "start", &image)
            .expect("frame");
        let config = CaptureConfig::deterministic(Viewport::new(1440, 1000, 1).expect("viewport"));
        let state = GuiState::new("serde-package", Some(PageState::Package), None);
        let frames = vec![FrameArtifact {
            label: "start".to_owned(),
            time_ms: 0,
            path: frame_path,
            sha256: frame_hash,
            width: 2880,
            height: 2000,
            input_index: None,
            diff: None,
        }];
        let manifest = CaptureManifest {
            schema: 1,
            state,
            config: config.clone(),
            frames: frames.clone(),
            script_id: None,
            scenario_sha256: scenario_hash(&(
                &GuiState::new("serde-package", Some(PageState::Package), None),
                &config,
                Option::<&str>::None,
            ))
            .expect("scenario"),
            sequence_sha256: scenario_hash(&frames).expect("sequence"),
            sequence: crate::FrameSequenceMetadata {
                encoding: "png-sequence".to_owned(),
                frame_count: 1,
                duration_ms: 0,
                frame_interval_ms: 16,
                keyframes: vec!["start".to_owned()],
                filmstrip_path: None,
            },
            source_revision: Some("test-revision".to_owned()),
            provenance: provenance(),
            semantic_artifact: None,
            semantic_sha256: None,
            baseline_within_policy: true,
            reference: None,
        };
        writer
            .write_json("manifests/serde-package.json", &manifest)
            .expect("manifest");
        let run_report = serde_json::json!({"pass": true, "failures": []});
        writer
            .write_json("run-report.json", &run_report)
            .expect("report");
        let mut policy = ConformancePolicy::default();
        policy.require_reference = false;
        policy.require_full_matrix = false;
        let report = verify_capture_run(&root, None, policy).expect("report");
        assert!(!report.pass);
        assert!(
            report
                .failures
                .iter()
                .any(|failure| failure.category == "geometry")
        );
        assert!(
            report
                .failures
                .iter()
                .any(|failure| failure.category == "clipping")
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn identical_full_motion_transition_fails_targeted_pixel_gate() {
        let root = std::env::temp_dir().join(format!(
            "backend-gui-conformance-motion-regression-{}",
            std::process::id()
        ));
        let config = CaptureConfig::deterministic(Viewport::new(16, 12, 1).expect("viewport"));
        let session = CaptureSession::new(config, root.join("640x480@1x")).expect("session");
        let writer = &session.writer;
        let image = RgbaImage::from_pixel(16, 12, Rgba([10, 20, 30, 255]));
        let mut frames = Vec::new();
        for (label, time_ms) in [("start", 0), ("midpoint", 16), ("settled", 32)] {
            let (path, hash) = writer.write_frame("motion", label, &image).expect("frame");
            frames.push(FrameArtifact {
                label: label.to_owned(),
                time_ms,
                path,
                sha256: hash,
                width: 16,
                height: 12,
                input_index: None,
                diff: None,
            });
        }
        let state = GuiState::new("motion", Some(PageState::Browse), None);
        let config = session.config.clone();
        let manifest = CaptureManifest {
            schema: 1,
            state: state.clone(),
            config: config.clone(),
            frames: frames.clone(),
            script_id: Some("transition".to_owned()),
            scenario_sha256: scenario_hash(&(&state, &config, Some("transition")))
                .expect("scenario"),
            sequence_sha256: scenario_hash(&frames).expect("sequence"),
            sequence: crate::FrameSequenceMetadata {
                encoding: "png-sequence".to_owned(),
                frame_count: frames.len(),
                duration_ms: 32,
                frame_interval_ms: 16,
                keyframes: frames.iter().map(|frame| frame.label.clone()).collect(),
                filmstrip_path: None,
            },
            source_revision: Some("test-revision".to_owned()),
            provenance: provenance(),
            semantic_artifact: None,
            semantic_sha256: None,
            baseline_within_policy: true,
            reference: None,
        };
        writer
            .write_json("manifests/motion.json", &manifest)
            .expect("manifest");
        writer
            .write_json(
                "run-report.json",
                &serde_json::json!({"pass": true, "failures": []}),
            )
            .expect("report");
        let mut policy = ConformancePolicy::default();
        policy.require_reference = false;
        policy.require_full_matrix = false;
        policy.min_changed_pixels = 1;
        let report = verify_capture_run(&root, None, policy).expect("report");
        assert!(
            report
                .failures
                .iter()
                .any(|failure| failure.category == "animation")
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }
}
