//! # ir-vcs — the libpijul-linked IR patch engine / serve / diff / semver / transfer plane
//!
//! This crate owns everything **heavy** that sits on top of the light [`ir`]
//! data model: the libpijul-backed change engine ([`repo`]), the sealed
//! mmap-able [`archive`], the structural [`diff`], the [`semver`] classifier,
//! the iroh-based [`sync`] distribution plane, the guest→host streaming
//! [`protocol`] + host-side [`stream`] driver, and the serve/checkpoint/session
//! machinery. It reads the IR types (entry/symbol/kind/wire/change/view/vocab/…)
//! from the [`ir`] crate — libpijul and iroh live here, not there.
//!
//! Spec context: `docs/research/ir-vcs/design/SEMANTIC-IR-VCS-PLAN.md`.

// ── Sync plane (formerly the `ir-sync` crate) ───────────────────────────────
/// iroh-based distribution of libpijul change files to a trusted remote.
/// Implements `heart::sync::{ContentIo, ApplyHook}` for the IR VCS plane.
pub mod sync;

// ── VCS plane ───────────────────────────────────────────────────────────────
/// Sealed, mmap-able IR-only `PackageArchive` (formerly `nudox-ir-archive`):
/// header/index/seal/section/view.
pub mod archive;
/// Matcher-free structural delta over IR tables (formerly `nudox-ir-diff`):
/// `PackageDelta`, `IrOp`, diff/apply.
pub mod diff;
/// Guest→host IR streaming protocol (formerly `ir-stream`): frame grammar +
/// sink/receiver state machines. The host-side driver is [`stream`].
pub mod protocol;
/// IR-only API-surface projection + semver classification (formerly
/// `nudox-semver`): surface/classify/report/law-packs.
pub mod semver;

pub mod ascii;
pub mod checkout;
pub mod checkpoint;
pub mod error;
pub mod f1;
pub mod lower;
pub mod refs;
pub mod repo;
pub mod serialize;
pub mod serve_cache;
pub mod session;
pub mod stream;
pub mod subst;
pub mod vcs_types;
pub mod version;
pub mod wire;

// ── VCS-plane tests (formerly in nudox-ir-vcs) ──────────────────────────────
#[cfg(test)]
#[path = "vcs_tests.rs"]
mod vcs_tests;

#[cfg(test)]
mod ref_probes;

#[cfg(test)]
mod size_tests;

#[cfg(test)]
mod record_shape_tests;

pub use checkout::MaterializedIndex;
pub use checkpoint::{
    Checkpoint, CheckpointCache, CheckpointConfig, Retention, ServeStrategy, Served,
};
pub use error::Error as VcsError;
pub use f1::{
    ContinuityOp, ContinuitySummary, Error as F1Error, F1View, RenameEdge,
    compute_api_surface_hash, serialize_f1,
};
pub use refs::{BranchName, Ref, RefKind, ResolvedRef, TagName};
pub use repo::{ChangeHashHex, IrRepository, IrTip, VersionDiff};
pub use serve_cache::{ServeCache, ServeSource, ServedArchive};
pub use session::{FinishReport, GenerationMeta, RecordingSession, StageReport, StagedEntry};
pub use stream::{
    ProgressSnapshot, SourceDigestEntry, StreamPolicy, StreamedRecording, record_stream,
};
pub use version::{VersionLabel, VersionState};
