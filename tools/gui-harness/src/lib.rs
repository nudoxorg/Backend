//! Deterministic screenshot, animation, and interaction infrastructure for
//! the Nudox GPUI CE desktop.
//!
//! The crate deliberately separates scenario description, GPUI driving, and
//! artifact comparison.  A desktop adapter supplies the real `Render` root
//! and data projection; this crate owns the clock, viewport, input sequence,
//! PNG provenance, and comparison oracle.

#![deny(unsafe_code)]

mod artifact;
mod conformance;
mod design_contract;
mod diff;
mod input;
mod journey;
mod semantics;
mod state;

mod gpui_driver;

use image::RgbaImage;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

pub use artifact::{
    ArtifactError, ArtifactWriter, CaptureManifest, CaptureProvenance, FrameArtifact,
    FrameSequenceMetadata, RunManifest, VerificationReport, frame_artifact, hash_bytes,
    scenario_hash, validate_frame_sequence, verify_run, verify_run_for_baseline_update,
};
pub use conformance::{
    CONFORMANCE_SCHEMA, ConformanceError, ConformanceFailure, ConformancePolicy, ConformanceReport,
    FrameEvidence, ManifestEvidence, MatrixEvidence, UniformRegion, verify_capture_run,
};
pub use design_contract::{
    Artboard, CaptureMatrix, DESIGN_CONTRACT_SCHEMA, DesignArtifact, DesignContract,
    DesignContractError, DesignSemantics, REQUIRED_ARTIFACTS, ReferenceArtifact, ReferenceMetadata,
    StyleDeclaration, SvgGlyph, TokenDeclaration, resolve_contract_root, summarize_contract,
};
pub use diff::{DiffBounds, DiffError, DiffMetrics, DiffPolicy, compare, diff_image, write_diff};
pub use gpui_driver::{
    GpuiCaptureOptions, capture_gpui_state, capture_gpui_state_with_adapters,
    capture_gpui_state_with_adapters_result, capture_gpui_state_with_adapters_result_and_semantics,
    capture_gpui_state_with_hooks, capture_gpui_state_with_timed_adapters_result,
};
pub use input::{ActionDescriptor, ActionTarget, ActionTree};
pub use input::{InputError, InputStep, TransitionScript, modifiers, position};
pub use journey::{VisibleJourney, VisibleJourneyStep};
pub use semantics::{
    SEMANTIC_SCHEMA, SemanticAnnouncement, SemanticBounds, SemanticError, SemanticNode,
    SemanticProbe, SemanticRelations, SemanticRole, SemanticSource, SemanticState, changed_pixels,
    contrast_ratio, crop_focus_ring, hash_png_pixels, meets_wcag_aa, relative_luminance,
};
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

/// Resolves viewport changes that happen before the first captured frame.
///
/// Input journeys can resize a window or change its device scale at time zero.
/// GPUI applies those changes before the first screenshot, so the capture
/// contract must use the resulting logical size and backing scale when it
/// crops, writes PNGs, and records a manifest. Later viewport changes are
/// dispatched by the driver and recorded on the individual frame; keeping the
/// preflight pass limited to the initial viewport avoids pretending that one
/// manifest-level size describes a mixed-size sequence.
pub fn preflight_viewport(
    viewport: Viewport,
    actions: &[InputStep],
    frames: &[AnimationFrame],
) -> Result<Viewport, CaptureError> {
    let first_frame = frames.first().map_or(0, |frame| frame.time_ms);
    let mut elapsed = 0_u64;
    let mut effective = viewport;
    for step in actions {
        match step {
            InputStep::Wait { milliseconds } => {
                elapsed = elapsed.saturating_add(u64::from(*milliseconds));
            }
            InputStep::Resize { width, height } if elapsed <= first_frame => {
                effective = Viewport::new(*width, *height, effective.scale)?;
            }
            InputStep::Scale { factor } if elapsed <= first_frame => {
                effective = Viewport::new(effective.width, effective.height, *factor)?;
            }
            InputStep::Resize { .. } | InputStep::Scale { .. } => {}
            _ => {}
        }
    }
    Ok(effective)
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

    /// Complete required logical-size and device-scale matrix.
    #[must_use]
    pub fn required_matrix() -> Vec<Self> {
        [1_u8, 2]
            .into_iter()
            .flat_map(|scale| {
                REQUIRED_VIEWPORTS.iter().map(move |&(width, height)| Self {
                    width,
                    height,
                    scale,
                })
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
            theme: ThemeState::Abyss,
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

/// Named design-system beats used by the deterministic capture schedule.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnimationBeat {
    /// Hover, press, and selection response.
    Touch90,
    /// Quiet reveal or pane transition.
    Reveal160,
    /// Disclosure or sheet transition.
    Unfold240,
    /// Toast/status emphasis.
    Emphasis380,
    /// Route-scale scene choreography.
    Scene620,
}

impl AnimationBeat {
    /// Returns the canonical duration in milliseconds.
    #[must_use]
    pub const fn duration_ms(self) -> u64 {
        match self {
            Self::Touch90 => 90,
            Self::Reveal160 => 160,
            Self::Unfold240 => 240,
            Self::Emphasis380 => 380,
            Self::Scene620 => 620,
        }
    }
}

/// Builds the mandated motion phases, including reversal and reduced motion.
#[must_use]
pub fn animation_frames(config: &CaptureConfig, reduced_motion: bool) -> Vec<AnimationFrame> {
    animation_frames_for_duration(config, reduced_motion, config.animation_duration_ms)
}

/// Returns the canonical 0/25/50/75/100% frame sequence for one beat.
#[must_use]
pub fn canonical_motion_frames(
    config: &CaptureConfig,
    reduced_motion: bool,
    beat: AnimationBeat,
) -> Vec<AnimationFrame> {
    if reduced_motion {
        return vec![AnimationFrame {
            label: "reduced-motion".to_owned(),
            time_ms: 0,
        }];
    }
    let duration = beat.duration_ms().max(config.animation_duration_ms);
    vec![
        AnimationFrame {
            label: "start".to_owned(),
            time_ms: 0,
        },
        AnimationFrame {
            label: "quarter".to_owned(),
            time_ms: duration / 4,
        },
        AnimationFrame {
            label: "midpoint".to_owned(),
            time_ms: duration / 2,
        },
        AnimationFrame {
            label: "three-quarter".to_owned(),
            time_ms: duration.saturating_mul(3) / 4,
        },
        AnimationFrame {
            label: "settled".to_owned(),
            time_ms: duration,
        },
    ]
}

/// Returns an interruption/reversal sequence for stress captures.
#[must_use]
pub fn stress_motion_frames(
    config: &CaptureConfig,
    reduced_motion: bool,
    beat: AnimationBeat,
) -> Vec<AnimationFrame> {
    if reduced_motion {
        return canonical_motion_frames(config, true, beat);
    }
    let duration = beat.duration_ms().max(config.animation_duration_ms);
    let quarter = duration / 4;
    vec![
        AnimationFrame {
            label: "start".to_owned(),
            time_ms: 0,
        },
        AnimationFrame {
            label: "quarter".to_owned(),
            time_ms: quarter,
        },
        AnimationFrame {
            label: "interrupt".to_owned(),
            time_ms: duration / 2,
        },
        AnimationFrame {
            label: "reverse".to_owned(),
            time_ms: duration / 2,
        },
        AnimationFrame {
            label: "three-quarter".to_owned(),
            time_ms: duration.saturating_mul(3) / 4,
        },
        AnimationFrame {
            label: "settled".to_owned(),
            time_ms: duration,
        },
    ]
}

/// Checks that an ordered frame schedule is deterministic and monotonic.
pub fn validate_animation_frames(frames: &[AnimationFrame]) -> Result<(), CaptureError> {
    if frames.is_empty() || frames.first().is_some_and(|frame| frame.time_ms != 0) {
        return Err(CaptureError::InvalidConfig(
            "animation sequence must start at virtual time zero".to_owned(),
        ));
    }
    let mut labels = std::collections::HashSet::new();
    if frames.iter().any(|frame| {
        !labels.insert(frame.label.as_str())
            || frames
                .windows(2)
                .any(|pair| pair[0].time_ms > pair[1].time_ms)
    }) {
        return Err(CaptureError::InvalidConfig(
            "animation sequence labels and timestamps must be ordered and unique".to_owned(),
        ));
    }
    Ok(())
}

/// Builds a route-aware timeline using the same duration family as the
/// production motion tokens: sheets and error/feedback surfaces get the
/// emphasis duration while ordinary navigation uses the standard duration.
#[must_use]
pub fn animation_frames_for_state(config: &CaptureConfig, state: &GuiState) -> Vec<AnimationFrame> {
    let beat = match state.overlay {
        Some(OverlayState::SettingsAppearance)
        | Some(OverlayState::SettingsEditor)
        | Some(OverlayState::SettingsAgents)
        | Some(OverlayState::SettingsDiagnostics)
        | Some(OverlayState::SettingsLegend)
        | Some(OverlayState::Fault)
        | Some(OverlayState::Vulnerable)
        | Some(OverlayState::Yanked)
        | Some(OverlayState::Indexing) => AnimationBeat::Unfold240,
        _ => AnimationBeat::Reveal160,
    };
    let frames = canonical_motion_frames(config, state.reduced_motion, beat);
    validate_animation_frames(&frames).expect("canonical animation schedule is valid");
    frames
}

fn animation_frames_for_duration(
    config: &CaptureConfig,
    reduced_motion: bool,
    duration: u64,
) -> Vec<AnimationFrame> {
    if reduced_motion {
        return vec![AnimationFrame {
            label: "reduced-motion".to_owned(),
            time_ms: 0,
        }];
    }
    let midpoint = duration / 2;
    let cadence = config.frame_interval_ms.max(1);
    let first = config.frame_interval_ms.min(duration / 2);
    let reversal = duration / 2 + cadence;
    let near_settled = duration.saturating_sub(cadence).max(reversal.min(duration));
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
            time_ms: reversal.min(duration),
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
    /// Logical viewport and device scale used for this frame.
    pub viewport: Viewport,
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
    /// Effective logical viewport and device scale after preflight inputs.
    pub viewport: Viewport,
    /// Captured timeline frames.
    pub frames: Vec<CaptureRecord>,
    /// Semantic probes captured at the same frame labels and timestamps.
    ///
    /// Keeping these beside the in-memory pixel sequence lets the artifact
    /// writer hash and verify the exact screenshot each probe describes.
    pub semantic_probes: Vec<SemanticProbe>,
}

/// A durable capture session with optional baseline comparison.
pub struct CaptureSession {
    /// Fixed run configuration.
    pub config: CaptureConfig,
    /// Artifact writer.
    pub writer: ArtifactWriter,
    baseline_root: Option<PathBuf>,
    provenance: CaptureProvenance,
    reference: Option<ReferenceMetadata>,
}

impl CaptureSession {
    /// Creates a run writer.
    pub fn new(
        config: CaptureConfig,
        output_root: impl Into<PathBuf>,
    ) -> Result<Self, CaptureError> {
        config.validate()?;
        let provenance = CaptureProvenance::collect()?;
        Ok(Self {
            config,
            writer: ArtifactWriter::new(output_root)?,
            baseline_root: None,
            provenance,
            reference: None,
        })
    }

    /// Sets an optional baseline root containing `frames/<state>/<label>.png`.
    #[must_use]
    pub fn with_baseline_root(mut self, baseline_root: impl Into<PathBuf>) -> Self {
        self.baseline_root = Some(baseline_root.into());
        self
    }

    /// Attaches the exact source-contract identity to future manifests.
    pub fn with_design_contract(mut self, contract: &DesignContract) -> Result<Self, CaptureError> {
        self.reference = Some(
            contract
                .reference_metadata()
                .map_err(|error| CaptureError::InvalidConfig(error.to_string()))?,
        );
        Ok(self)
    }

    /// Writes a capture set and its state manifest.
    pub fn write_set(
        &self,
        capture: &mut CaptureSet,
        script_id: Option<&str>,
        semantic_artifact: Option<&str>,
    ) -> Result<CaptureManifest, CaptureError> {
        let mut frames = Vec::with_capacity(capture.frames.len());
        let mut baseline_within_policy = true;
        let mut missing_baseline = false;
        for record in &mut capture.frames {
            let expected_size = record.viewport.physical_size();
            if (record.image.width(), record.image.height()) != expected_size {
                return Err(CaptureError::InvalidConfig(format!(
                    "capture frame {} has physical size {}x{}, expected {}x{} for {}",
                    record.label,
                    record.image.width(),
                    record.image.height(),
                    expected_size.0,
                    expected_size.1,
                    record.viewport.suffix(),
                )));
            }
            let baseline = self.baseline_path(&capture.state.id, &record.label);
            if let Some(path) = baseline {
                if !path.is_file() {
                    missing_baseline = true;
                } else {
                    let image = image::open(path)?.into_rgba8();
                    record.diff = Some(compare(&image, &record.image, self.config.diff_policy)?);
                    if let Some(diff) = &record.diff {
                        baseline_within_policy &= diff.within_policy;
                        if diff.changed() {
                            let relative =
                                format!("diffs/{}/{}.png", capture.state.id, record.label);
                            write_diff(
                                &self.writer.root().join(&relative),
                                &image,
                                &record.image,
                                self.config.diff_policy.channel_tolerance,
                            )?;
                        }
                    }
                }
            }
            let (path, hash) =
                self.writer
                    .write_frame(&capture.state.id, &record.label, &record.image)?;
            frames.push(frame_artifact(record, path, hash));
        }
        validate_frame_sequence(&frames)?;
        let filmstrip_path = self
            .writer
            .write_filmstrip(&capture.state.id, &capture.frames)
            .map(Some)?;
        let mut manifest_config = self.config.clone();
        manifest_config.viewport = capture.viewport;
        let scenario = (&capture.state, &manifest_config, script_id);
        let scenario_sha256 = scenario_hash(&scenario)?;
        let sequence_sha256 = scenario_hash(&frames)?;
        let sequence = FrameSequenceMetadata {
            encoding: "png-sequence".to_owned(),
            frame_count: frames.len(),
            duration_ms: frames.last().map_or(0, |frame| frame.time_ms),
            frame_interval_ms: self.config.frame_interval_ms,
            keyframes: frames.iter().map(|frame| frame.label.clone()).collect(),
            filmstrip_path,
        };
        let semantic_path = semantic_artifact.map(ToOwned::to_owned).or_else(|| {
            (!capture.semantic_probes.is_empty())
                .then(|| format!("semantics/{}.json", capture.state.id))
        });
        if !capture.semantic_probes.is_empty() {
            if capture.semantic_probes.len() != capture.frames.len() {
                return Err(CaptureError::InvalidConfig(format!(
                    "semantic probe count {} does not match frame count {}",
                    capture.semantic_probes.len(),
                    capture.frames.len()
                )));
            }
            for (probe, frame) in capture.semantic_probes.iter().zip(&capture.frames) {
                if probe.frame != frame.label
                    || probe.time_ms != frame.time_ms
                    || probe.viewport != (frame.viewport.width, frame.viewport.height)
                    || probe.screenshot_sha256 != hash_png_pixels(&frame.image)
                {
                    return Err(CaptureError::InvalidConfig(format!(
                        "semantic probe {} is not paired with its rendered frame",
                        probe.frame
                    )));
                }
            }
            for probe in &capture.semantic_probes {
                probe
                    .validate()
                    .map_err(|error| CaptureError::InvalidConfig(error.to_string()))?;
            }
            self.writer.write_json(
                semantic_path.as_deref().unwrap_or_default(),
                &capture.semantic_probes,
            )?;
        }
        let semantic_sha256 = semantic_path
            .as_deref()
            .map(|relative| {
                std::fs::read(self.writer.root().join(relative))
                    .map(|bytes| hash_bytes(&bytes))
                    .map_err(ArtifactError::Io)
            })
            .transpose()?;
        let manifest = CaptureManifest {
            schema: 1,
            state: capture.state.clone(),
            config: manifest_config,
            frames,
            script_id: script_id.map(ToOwned::to_owned),
            scenario_sha256,
            sequence_sha256,
            sequence,
            source_revision: Some(self.provenance.source_revision.clone()),
            provenance: self.provenance.clone(),
            semantic_artifact: semantic_path,
            semantic_sha256,
            baseline_within_policy,
            reference: self.reference.clone(),
        };
        self.writer
            .write_json(&format!("manifests/{}.json", capture.state.id), &manifest)?;
        if missing_baseline {
            return Err(CaptureError::BaselineMissing(capture.state.id.clone()));
        }
        if !baseline_within_policy {
            return Err(CaptureError::BaselineMismatch(capture.state.id.clone()));
        }
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
    /// A configured baseline frame was absent.
    #[error("baseline is missing for capture {0:?}")]
    BaselineMissing(String),
    /// One or more baseline comparisons exceeded policy. The manifest and
    /// diff artifacts are still written before this error is returned.
    #[error("baseline comparison exceeded policy for capture {0:?}")]
    BaselineMismatch(String),
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
    fn named_motion_beats_produce_ordered_keyframes_and_reduced_snap() {
        let config = CaptureConfig::deterministic(Viewport::new(1280, 800, 1).expect("viewport"));
        let frames = canonical_motion_frames(&config, false, AnimationBeat::Unfold240);
        assert_eq!(frames.last().map(|frame| frame.time_ms), Some(240));
        validate_animation_frames(&frames).expect("canonical schedule");
        assert_eq!(
            canonical_motion_frames(&config, true, AnimationBeat::Scene620).len(),
            1
        );
    }

    #[test]
    fn stress_motion_schedule_keeps_equal_time_retarget_and_reversal_ordered() {
        let config = CaptureConfig::deterministic(Viewport::new(1280, 800, 1).expect("viewport"));
        let frames = stress_motion_frames(&config, false, AnimationBeat::Reveal160);
        validate_animation_frames(&frames).expect("stress schedule");
        assert_eq!(frames[2].time_ms, frames[3].time_ms);
        assert_eq!(frames.last().map(|frame| frame.time_ms), Some(220));
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

    #[test]
    fn one_pixel_baseline_mismatch_writes_evidence_and_fails_closed() {
        let root = std::env::temp_dir().join(format!(
            "backend-gui-harness-baseline-test-{}",
            std::process::id()
        ));
        let baseline = root.join("baseline");
        let output = root.join("output");
        std::fs::create_dir_all(baseline.join("frames/browse")).expect("baseline directory");
        let image = RgbaImage::from_pixel(640, 480, image::Rgba([0, 0, 0, 255]));
        image
            .save(baseline.join("frames/browse/start.png"))
            .expect("baseline image");
        let mut actual = image.clone();
        actual.put_pixel(0, 0, image::Rgba([1, 0, 0, 255]));
        let config = CaptureConfig::deterministic(Viewport::new(640, 480, 1).expect("viewport"));
        let session = CaptureSession::new(config, &output)
            .expect("capture session")
            .with_baseline_root(&baseline);
        session
            .writer
            .write_json("semantics/browse.json", &serde_json::json!([]))
            .expect("semantic artifact");
        let mut capture = CaptureSet {
            state: GuiState::new("browse", Some(PageState::Browse), None),
            viewport: Viewport::new(640, 480, 1).expect("viewport"),
            frames: vec![CaptureRecord {
                label: "start".to_owned(),
                time_ms: 0,
                viewport: Viewport::new(640, 480, 1).expect("viewport"),
                image: actual,
                input_index: None,
                diff: None,
            }],
            semantic_probes: Vec::new(),
        };
        let result = session.write_set(&mut capture, None, Some("semantics/browse.json"));
        assert!(matches!(result, Err(CaptureError::BaselineMismatch(_))));
        assert!(output.join("manifests/browse.json").is_file());
        assert!(output.join("diffs/browse/start.png").is_file());
        std::fs::remove_dir_all(root).expect("test cleanup");
    }

    #[test]
    fn manifest_and_verifier_follow_the_effective_capture_viewport() {
        let root = std::env::temp_dir().join(format!(
            "backend-gui-harness-effective-viewport-{}",
            std::process::id()
        ));
        let config = CaptureConfig::deterministic(Viewport::new(4, 3, 1).expect("viewport"));
        let session = CaptureSession::new(config, &root).expect("capture session");
        session
            .writer
            .write_json("semantics/edge.json", &serde_json::json!([]))
            .expect("semantic artifact");
        let mut image = RgbaImage::from_pixel(8, 6, image::Rgba([32, 32, 32, 255]));
        image.put_pixel(7, 5, image::Rgba([255, 59, 48, 255]));
        let mut capture = CaptureSet {
            state: GuiState::new("edge", None, None),
            viewport: Viewport::new(4, 3, 2).expect("effective viewport"),
            frames: vec![CaptureRecord {
                label: "start".to_owned(),
                time_ms: 0,
                viewport: Viewport::new(4, 3, 2).expect("effective viewport"),
                image,
                input_index: None,
                diff: None,
            }],
            semantic_probes: Vec::new(),
        };
        session
            .write_set(&mut capture, None, Some("semantics/edge.json"))
            .expect("write effective capture");
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.join("manifests/edge.json")).expect("manifest"),
        )
        .expect("manifest json");
        assert_eq!(manifest["config"]["viewport"]["scale"], 2);
        let verified = verify_run(&root).expect("verified run");
        assert_eq!(verified.manifests, 1);
        assert_eq!(verified.frames, 1);
        std::fs::remove_dir_all(root).expect("test cleanup");
    }
}
