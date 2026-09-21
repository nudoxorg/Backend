//! Durable screenshot artifacts and manifests.

use crate::{CaptureConfig, CaptureRecord, DiffMetrics, GuiState};
use image::RgbaImage;
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
        let mut bytes = Vec::new();
        {
            let mut cursor = std::io::Cursor::new(&mut bytes);
            image.write_to(&mut cursor, image::ImageFormat::Png)?;
        }
        std::fs::write(&path, &bytes)?;
        Ok((relative.to_string_lossy().into_owned(), hash_bytes(&bytes)))
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
