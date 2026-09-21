//! Deterministic screenshot, animation, and interaction infrastructure for
//! the Nudox GPUI CE desktop.
//!
//! The crate deliberately separates scenario description, GPUI driving, and
//! artifact comparison.  A desktop adapter supplies the real `Render` root
//! and data projection; this crate owns the clock, viewport, input sequence,
//! PNG provenance, and comparison oracle.

#![deny(unsafe_code)]

mod artifact;
mod diff;
mod input;
mod journey;
mod state;

mod gpui_driver;

use image::RgbaImage;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

pub use artifact::{
    ArtifactError, ArtifactWriter, CaptureManifest, FrameArtifact, FrameSequenceMetadata,
    RunManifest, VerificationReport, frame_artifact, hash_bytes, scenario_hash, verify_run,
};
pub use diff::{DiffBounds, DiffError, DiffMetrics, DiffPolicy, compare, diff_image, write_diff};
pub use gpui_driver::{
    GpuiCaptureOptions, capture_gpui_state, capture_gpui_state_with_adapters,
    capture_gpui_state_with_adapters_result, capture_gpui_state_with_hooks,
};
pub use input::{ActionDescriptor, ActionTarget, ActionTree};
pub use input::{InputError, InputStep, TransitionScript, modifiers, position};
pub use journey::{VisibleJourney, VisibleJourneyStep};
pub use state::{
    FocusState, GuiState, OverlayState, PageState, StateError, ThemeState, parse_state,
    validate_catalog,
};

/// The responsive viewport matrix required by the Nudox GUI gate.
pub const REQUIRED_VIEWPORTS: &[(u32, u32)] = &[
    (640, 480),
    (800, 600),
    (900, 600),
    (1024, 768),
    (1280, 800),
    (1440, 900),
    (1440, 1000),
    (1600, 1000),
    (1920, 1080),
    (2560, 1440),
];

/// SHA-256 of the ordered design-system font manifest (name + bytes).
pub const BUNDLED_FONT_SHA256: &str =
    "4a04e066c412c59da1a3c523d2b32edd3fa249806789c9e3c9721e79aa5df8cf";

/// Exact family set used by the desktop design lane.
pub const BUNDLED_FONT_FAMILIES: &str = "Instrument Sans|Archivo|Newsreader|Geist Mono";

/// Per-file byte hashes checked before a capture starts.
pub const BUNDLED_FONT_HASHES: [(&str, &str); 4] = [
    (
        "Archivo[wdth,wght].ttf",
        "0e094a7d3c7c4c25cf1310c4b30014f1dae9332220b1c2c88f4fa996f0b05053",
    ),
    (
        "InstrumentSans[wdth,wght].ttf",
        "b24f1812584816958afcf22e22d08e44318c5e51651e25d2438efdde389b33b1",
    ),
    (
        "Newsreader[opsz,wght].ttf",
        "8a08d13f8a6c0d51be379a60af84f945f65369a67e509ee3c3bdcc421254d7c1",
    ),
    (
        "GeistMono[wght].ttf",
        "87c2aff9723544a9adaea19d92e42a33705c9723624801b6e0224c2206a6af0d",
    ),
];

/// Stable logical source name for the bundled font asset.
pub const BUNDLED_FONT_SOURCE: &str = "bundled://nudox-design-system/fonts";

fn bundled_fonts() -> [(&'static str, &'static [u8]); 4] {
    [
        (
            BUNDLED_FONT_HASHES[0].0,
            include_bytes!("../assets/fonts/Archivo[wdth,wght].ttf"),
        ),
        (
            BUNDLED_FONT_HASHES[1].0,
            include_bytes!("../assets/fonts/InstrumentSans[wdth,wght].ttf"),
        ),
        (
            BUNDLED_FONT_HASHES[2].0,
            include_bytes!("../assets/fonts/Newsreader[opsz,wght].ttf"),
        ),
        (
            BUNDLED_FONT_HASHES[3].0,
            include_bytes!("../assets/fonts/GeistMono[wght].ttf"),
        ),
    ]
}

fn bundled_font_manifest_hash() -> String {
    let mut bytes = Vec::new();
    for (name, font) in bundled_fonts() {
        bytes.extend_from_slice(name.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(font);
    }
    hash_bytes(&bytes)
}

/// Returns the exact bundled font bytes for product registration.
#[must_use]
pub fn bundled_font_bytes() -> Vec<&'static [u8]> {
    bundled_fonts()
        .into_iter()
        .map(|(_, bytes)| bytes)
        .collect()
}

/// Verifies every design font byte hash and the aggregate manifest hash.
pub fn verify_bundled_fonts() -> Result<(), CaptureError> {
    for ((name, expected), (actual_name, bytes)) in BUNDLED_FONT_HASHES.iter().zip(bundled_fonts())
    {
        if name != &actual_name || hash_bytes(bytes) != *expected {
            return Err(CaptureError::InvalidConfig(format!(
                "bundled design font {name} failed byte-hash verification"
            )));
        }
    }
    if bundled_font_manifest_hash() != BUNDLED_FONT_SHA256 {
        return Err(CaptureError::InvalidConfig(
            "bundled design font manifest hash changed".to_owned(),
        ));
    }
    Ok(())
}

/// A fixed logical viewport and device scale.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Viewport {
    /// Logical width in points.
    pub width: u32,
    /// Logical height in points.
    pub height: u32,
    /// Device scale requested by the scenario.
    pub scale: u8,
}

impl Viewport {
    /// All required viewports at 1x.
    #[must_use]
    pub fn required() -> Vec<Self> {
        REQUIRED_VIEWPORTS
            .iter()
            .map(|&(width, height)| Self {
                width,
                height,
                scale: 1,
            })
            .collect()
    }

    /// A viewport with a validated positive scale.
    pub fn new(width: u32, height: u32, scale: u8) -> Result<Self, CaptureError> {
        let scale = u32::from(scale);
        if width == 0
            || height == 0
            || !matches!(scale, 1 | 2)
            || width.checked_mul(scale).is_none()
            || height.checked_mul(scale).is_none()
        {
            return Err(CaptureError::InvalidViewport {
                width,
                height,
                scale: scale as u8,
            });
        }
        Ok(Self {
            width,
            height,
            scale: scale as u8,
        })
    }

    /// Returns the requested image size in physical pixels.
    #[must_use]
    pub fn physical_size(self) -> (u32, u32) {
        (
            self.width * u32::from(self.scale),
            self.height * u32::from(self.scale),
        )
    }

    /// Stable artifact suffix.
    #[must_use]
    pub fn suffix(self) -> String {
        format!("{}x{}@{}x", self.width, self.height, self.scale)
    }
}

/// Fixed environmental inputs for one capture run.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CaptureConfig {
    /// Logical viewport and scale.
    pub viewport: Viewport,
    /// Semantic appearance name supplied to the adapter.
    pub theme: ThemeState,
    /// Exact font family selected by the adapter.
    pub font_family: String,
    /// Hash of the vendored/system font bytes used by the adapter.
    pub font_sha256: String,
    /// Source label for the font bytes (normally a Nix-vendored path).
    pub font_source: String,
    /// Locale used by number/date/text shaping paths.
    pub locale: String,
    /// Timezone used by all formatted timestamps.
    pub timezone: String,
    /// Seed supplied to GPUI's deterministic test dispatcher.
    pub seed: u64,
    /// Text direction used by the adapter.
    pub text_direction: String,
    /// IME mode used by the keyboard journey.
    pub ime_mode: String,
    /// Revision of the admitted data projection.
    pub data_revision: String,
    /// Virtual clock epoch, in milliseconds from a fixed run origin.
    pub epoch_ms: u64,
    /// Default animation duration to sample.
    pub animation_duration_ms: u64,
    /// Frame interval for animation capture.
    pub frame_interval_ms: u64,
    /// Pixel comparison policy.
    pub diff_policy: DiffPolicy,
}

impl CaptureConfig {
    /// Sensible deterministic defaults for one viewport.
    #[must_use]
    pub fn deterministic(viewport: Viewport) -> Self {
        Self {
            theme: ThemeState::Ink,
            viewport,
            font_family: BUNDLED_FONT_FAMILIES.to_owned(),
            font_sha256: BUNDLED_FONT_SHA256.to_owned(),
            font_source: BUNDLED_FONT_SOURCE.to_owned(),
            locale: "en-US".to_owned(),
            timezone: "UTC".to_owned(),
            seed: 0,
            text_direction: "ltr".to_owned(),
            ime_mode: "disabled".to_owned(),
            data_revision: "adapter-required".to_owned(),
            epoch_ms: 0,
            animation_duration_ms: 220,
            frame_interval_ms: 16,
            diff_policy: DiffPolicy::default(),
        }
    }

    /// Checks that every environment input has an explicit value.
    pub fn validate(&self) -> Result<(), CaptureError> {
        if self.font_family.trim().is_empty()
            || self.font_sha256.len() != 64
            || self.locale.trim().is_empty()
            || self.timezone.trim().is_empty()
            || self.data_revision.trim().is_empty()
            || !matches!(self.text_direction.as_str(), "ltr" | "rtl")
            || self.ime_mode.trim().is_empty()
        {
            return Err(CaptureError::InvalidConfig(
                "font, locale, timezone, revision, direction, and IME inputs must be explicit"
                    .to_owned(),
            ));
        }
        if self.font_sha256 != BUNDLED_FONT_SHA256 || self.font_source != BUNDLED_FONT_SOURCE {
            return Err(CaptureError::InvalidConfig(
                "captures must use the exact bundled Nudox design font manifest".to_owned(),
            ));
        }
        verify_bundled_fonts()?;
        Ok(())
    }

    /// The complete matrix at the requested scale.
    #[must_use]
    pub fn required_at_scale(scale: u8) -> Result<Vec<Self>, CaptureError> {
        REQUIRED_VIEWPORTS
            .iter()
            .map(|&(width, height)| Viewport::new(width, height, scale).map(Self::deterministic))
            .collect()
    }
}

/// Semantic points in an animation timeline.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AnimationFrame {
    /// Stable phase name.
    pub label: String,
    /// Virtual milliseconds from animation start.
    pub time_ms: u64,
}

/// Builds the mandated motion phases, including reversal and reduced motion.
#[must_use]
pub fn animation_frames(config: &CaptureConfig, reduced_motion: bool) -> Vec<AnimationFrame> {
    if reduced_motion {
        return vec![AnimationFrame {
            label: "reduced-motion".to_owned(),
            time_ms: 0,
        }];
    }
    let duration = config.animation_duration_ms;
    let midpoint = duration / 2;
    let first = config.frame_interval_ms.min(duration);
    let near_settled = duration.saturating_sub(config.frame_interval_ms.max(1));
    vec![
        AnimationFrame {
            label: "start".to_owned(),
            time_ms: 0,
        },
        AnimationFrame {
            label: "first-moving".to_owned(),
            time_ms: first,
        },
        AnimationFrame {
            label: "midpoint".to_owned(),
            time_ms: midpoint,
        },
        AnimationFrame {
            label: "retarget".to_owned(),
            time_ms: midpoint,
        },
        AnimationFrame {
            label: "reversal".to_owned(),
            time_ms: midpoint + first,
        },
        AnimationFrame {
            label: "near-settled".to_owned(),
            time_ms: near_settled,
        },
        AnimationFrame {
            label: "settled".to_owned(),
            time_ms: duration,
        },
    ]
}

/// One in-memory frame before it is written as an artifact.
#[derive(Clone, Debug)]
pub struct CaptureRecord {
    /// Stable semantic label.
    pub label: String,
    /// Virtual capture time.
    pub time_ms: u64,
    /// Image bytes.
    pub image: RgbaImage,
    /// Input step that produced it.
    pub input_index: Option<usize>,
    /// Baseline comparison result.
    pub diff: Option<DiffMetrics>,
}

/// Capture and comparison outcome for one state.
#[derive(Clone, Debug)]
pub struct CaptureSet {
    /// State identity.
    pub state: GuiState,
    /// Captured timeline frames.
    pub frames: Vec<CaptureRecord>,
}

/// A durable capture session with optional baseline comparison.
pub struct CaptureSession {
    /// Fixed run configuration.
    pub config: CaptureConfig,
    /// Artifact writer.
    pub writer: ArtifactWriter,
    baseline_root: Option<PathBuf>,
}

impl CaptureSession {
    /// Creates a run writer.
    pub fn new(
        config: CaptureConfig,
        output_root: impl Into<PathBuf>,
    ) -> Result<Self, CaptureError> {
        config.validate()?;
        Ok(Self {
            config,
            writer: ArtifactWriter::new(output_root)?,
            baseline_root: None,
        })
    }

    /// Sets an optional baseline root containing `frames/<state>/<label>.png`.
    #[must_use]
    pub fn with_baseline_root(mut self, baseline_root: impl Into<PathBuf>) -> Self {
        self.baseline_root = Some(baseline_root.into());
        self
    }

    /// Writes a capture set and its state manifest.
    pub fn write_set(
        &self,
        capture: &mut CaptureSet,
        script_id: Option<&str>,
    ) -> Result<CaptureManifest, CaptureError> {
        let mut frames = Vec::with_capacity(capture.frames.len());
        let expected_size = self.config.viewport.physical_size();
        for record in &mut capture.frames {
            if (record.image.width(), record.image.height()) != expected_size {
                return Err(CaptureError::InvalidConfig(format!(
                    "capture frame {} has physical size {}x{}, expected {}x{} for {}",
                    record.label,
                    record.image.width(),
                    record.image.height(),
                    expected_size.0,
                    expected_size.1,
                    self.config.viewport.suffix(),
                )));
            }
            let baseline = self.baseline_path(&capture.state.id, &record.label);
            if let Some(path) = baseline.as_deref().filter(|path| path.is_file()) {
                let image = image::open(path)?.into_rgba8();
                record.diff = Some(compare(&image, &record.image, self.config.diff_policy)?);
                if let Some(diff) = &record.diff {
                    if diff.changed() {
                        let relative = format!("diffs/{}/{}.png", capture.state.id, record.label);
                        write_diff(
                            &self.writer.root().join(&relative),
                            &image,
                            &record.image,
                            self.config.diff_policy.channel_tolerance,
                        )?;
                    }
                }
            }
            let (path, hash) =
                self.writer
                    .write_frame(&capture.state.id, &record.label, &record.image)?;
            frames.push(frame_artifact(record, path, hash));
        }
        let filmstrip_path = self
            .writer
            .write_filmstrip(&capture.state.id, &capture.frames)
            .map(Some)?;
        let scenario = (&capture.state, &self.config, script_id);
        let sequence_sha256 = scenario_hash(&frames)?;
        let sequence = FrameSequenceMetadata {
            encoding: "png-sequence".to_owned(),
            frame_count: frames.len(),
            duration_ms: frames.last().map_or(0, |frame| frame.time_ms),
            frame_interval_ms: self.config.frame_interval_ms,
            keyframes: frames.iter().map(|frame| frame.label.clone()).collect(),
            filmstrip_path,
        };
        let manifest = CaptureManifest {
            schema: 1,
            state: capture.state.clone(),
            config: self.config.clone(),
            frames,
            script_id: script_id.map(ToOwned::to_owned),
            scenario_sha256: scenario_hash(&scenario)?,
            sequence_sha256,
            sequence,
            source_revision: option_env!("NUDOX_SOURCE_REVISION").map(ToOwned::to_owned),
        };
        self.writer
            .write_json(&format!("manifests/{}.json", capture.state.id), &manifest)?;
        Ok(manifest)
    }

    fn baseline_path(&self, state: &str, label: &str) -> Option<PathBuf> {
        self.baseline_root
            .as_ref()
            .map(|root| root.join("frames").join(state).join(format!("{label}.png")))
    }
}

/// Errors emitted by the harness orchestration layer.
#[derive(Debug, Error)]
pub enum CaptureError {
    /// Viewport dimensions or scale are invalid.
    #[error("invalid capture viewport {width}x{height}@{scale}x")]
    InvalidViewport {
        /// Requested logical width.
        width: u32,
        /// Requested logical height.
        height: u32,
        /// Requested device scale.
        scale: u8,
    },
    /// State catalog validation failed.
    #[error(transparent)]
    State(#[from] StateError),
    /// Input script validation failed.
    #[error(transparent)]
    Input(#[from] InputError),
    /// Artifact writing failed.
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
    /// Image comparison failed.
    #[error(transparent)]
    Diff(#[from] DiffError),
    /// Image loading failed.
    #[error("baseline image loading failed: {0}")]
    Image(#[from] image::ImageError),
    /// A nondeterministic environment input was omitted.
    #[error("invalid deterministic capture configuration: {0}")]
    InvalidConfig(String),
    /// GPUI driver failed.
    #[error("GPUI capture failed: {0}")]
    Gpui(String),
    /// The platform does not expose a direct offscreen renderer.
    #[error("the current GPUI platform has no direct offscreen renderer")]
    NoRenderer,
}

/// Validates all stable harness inputs before starting a long capture run.
pub fn validate_run(states: &[GuiState], scripts: &[TransitionScript]) -> Result<(), CaptureError> {
    validate_catalog(states)?;
    for script in scripts {
        script.validate()?;
    }
    Ok(())
}

/// Returns whether the supplied path exists and is a regular baseline root.
#[must_use]
pub fn baseline_available(root: &Path) -> bool {
    root.is_dir() && root.join("frames").is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_matrix_includes_compact_and_large_widths() {
        let viewports = Viewport::required();
        assert_eq!(
            viewports.first().map(|v| (v.width, v.height)),
            Some((640, 480))
        );
        assert_eq!(
            viewports.last().map(|v| (v.width, v.height)),
            Some((2560, 1440))
        );
    }

    #[test]
    fn animation_timeline_contains_every_required_phase() {
        let config = CaptureConfig::deterministic(Viewport::new(1280, 800, 1).expect("viewport"));
        let labels: Vec<_> = animation_frames(&config, false)
            .into_iter()
            .map(|frame| frame.label)
            .collect();
        assert_eq!(
            labels,
            [
                "start",
                "first-moving",
                "midpoint",
                "retarget",
                "reversal",
                "near-settled",
                "settled"
            ]
        );
        assert_eq!(animation_frames(&config, true).len(), 1);
    }

    #[test]
    fn physical_size_tracks_logical_viewport_and_requested_scale() {
        assert_eq!(
            Viewport::new(1440, 1000, 1)
                .expect("viewport")
                .physical_size(),
            (1440, 1000)
        );
        assert_eq!(
            Viewport::new(1440, 1000, 2)
                .expect("viewport")
                .physical_size(),
            (2880, 2000)
        );
    }

    #[test]
    fn physical_size_rejects_overflowing_backing_dimensions() {
        assert!(Viewport::new(u32::MAX, 1, 2).is_err());
    }
}
