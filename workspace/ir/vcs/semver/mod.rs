//! # nudox-semver — IR-only API surface projection and semver classification.
//!
//! Implements P4 and P5 of SEMANTIC-IR-VCS-PLAN Rev 2.1 (§8.4, §9).
//!
//! ## Architecture
//!
//! ```text
//! PristineIntroTable
//!   │
//!   ▼  surface(table, policy)
//! ApiSurface  ──────────────────────────────┐
//!   │                                       │
//!   │  api_surface_hash → semver/graph gate │
//!   │  embed_hash       → re-embed gate     │
//!   │  payload_hash     → exact dedup       │
//!   │                                       │
//!   ▼  classify(old, new, policy, deps)     │
//! ApiReport   ◀────────────────────────────┘
//! ```
//!
//! ## Key invariants
//!
//! - **K-IR-Only-Semver** — `classify` is a pure function over IR. No rustc,
//!   no network, no CSC at classify time.
//! - **K-Hash-Classes** — three hashes, three consumers (§3.3). `api_surface_hash`
//!   gates semver/graph; `embed_hash` gates re-embedding; `payload_hash` is exact
//!   identity / dedup.
//! - **Ambiguity protocol** — if IR facts decide → `Certain`; if facts are missing
//!   → `Uncertain(reason)`. Never guess (§9.3).
//!
//! ## Module layout
//!
//! | Module | Contents |
//! |---|---|
//! | [`config`] | `ConfigId` (config-content address) |
//! | [`surface`] | `ExportPolicy`, `ApiSurface`, `surface()`, hash-class functions |
//! | [`report`] | `BreakClass`, `Certainty`, `Finding`, `ApiReport`, `SemverPolicy` |
//! | [`classify`] | `classify()`, `DepSurfaceProvider`, `NoDeps` |
//! | [`packs::a`] | Pack A: A-1 … A-19 (CSC parity, §9.4) |

pub mod classify;
pub mod config;
pub mod packs;
pub mod report;
pub mod surface;

// ---------------------------------------------------------------------------
// Flat re-exports (the crate's public API surface)
// ---------------------------------------------------------------------------

// Config
pub use config::ConfigId;

// Surface
pub use surface::{
    ApiItem, ApiSurface, ExportPolicy, MonikerPath, api_surface_hash, embed_hash, payload_hash,
    surface, surface_with_config,
};

// Report
pub use report::{
    ApiReport, BreakClass, Certainty, DepClosureStatus, Finding, FindingDetail, LintId,
    SemverPolicy, UncertainReason, Unchecked, render_class, render_moniker,
};

// Classify
pub use classify::{DepMissing, DepPin, DepSurfaceProvider, NoDeps, classify};
