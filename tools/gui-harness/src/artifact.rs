//! Durable screenshot artifacts and manifests.

use crate::{CaptureConfig, CaptureRecord, DiffMetrics, GuiState};
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{ExtendedColorType, GenericImage, ImageEncoder, Rgba, RgbaImage};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// One PNG frame and its provenance.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FrameArtifact {
    /// Stable frame label, for example `midpoint` or `settled`.
    pub label: String,
    /// Virtual time at which the frame was captured.
    pub time_ms: u64,
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
}

/// Verifies every manifest and PNG below a run root without trusting the
/// capture process. This is intentionally independent of GPUI and can be
/// used as a holdout gate in CI or after copying artifacts between machines.
pub fn verify_run(root: &Path) -> Result<VerificationReport, ArtifactError> {
    let mut manifests = Vec::new();
    collect_manifests(root, &mut manifests)?;
    let mut report = VerificationReport::default();
    for manifest_path in manifests {
        let bytes = std::fs::read(&manifest_path)?;
        let manifest: CaptureManifest = serde_json::from_slice(&bytes)?;
        report.manifests += 1;
        // Frame paths in a manifest are relative to the capture/session root,
        // while manifests live in its `manifests/` directory.
        let base = manifest_path
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| ArtifactError::Verification(manifest_path.display().to_string()))?;
        for frame in manifest.frames {
            let path = base.join(&frame.path);
            let encoded = std::fs::read(&path)?;
            let relative = path
                .strip_prefix(root)
                .unwrap_or(path.as_path())
                .display()
                .to_string();
            let image = image::load_from_memory(&encoded)?.into_rgba8();
            report.frames += 1;
            if hash_bytes(&encoded) != frame.sha256
                || image.dimensions() != (frame.width, frame.height)
                || image.pixels().all(|pixel| pixel[3] == 0)
            {
                report.failures.push(relative);
            }
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
}
