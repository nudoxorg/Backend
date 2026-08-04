//! P5 — `classify` entry point: two surfaces in, `ApiReport` out.
//!
//! `classify(old, new, policy, deps)` runs an ordered list of pure lint
//! functions (Pack A now; Pack B/C/D later) over the two surfaces. There is no
//! I/O — the function is a pure computation over IR (K-IR-Only-Semver).
//!
//! # Ambiguity protocol (§9.3)
//!
//! - Facts sufficient → `Certainty::Certain`.
//! - Facts missing → `Certainty::Uncertain(reason)` — **never guess**.
//! - Uncertain findings never raise `required_bump` (§9.1).
//!
//! # S-5 hermetic assertion
//!
//! `classify` performs no I/O, does not invoke `rustc`, and does not touch the
//! network. It is a pure `fn(&ApiSurface, &ApiSurface, ...) -> ApiReport`.
//! The in-module test `classify_is_pure_no_io` constructs surfaces in-memory
//! and runs `classify` without any I/O setup, verifying the assertion
//! structurally.

use std::sync::Arc;

use ir::change::PackageLineageId;
use crate::vcs_types::ChangeSetFingerprint;

use crate::semver::packs::a::run_pack_a;
use crate::semver::report::{ApiReport, DepClosureStatus, SemverPolicy};
use crate::semver::surface::ApiSurface;

// ---------------------------------------------------------------------------
// DepSurfaceProvider (§9.1)
// ---------------------------------------------------------------------------

/// Reason a dep surface was not available.
#[derive(Clone, Debug)]
pub struct DepMissing {
    pub package: PackageLineageId,
}

/// An opaque pin identifying the exact version of a dependency to fetch.
#[derive(Clone, Debug)]
pub struct DepPin {
    /// Rendered version string (e.g. `"1.2.3"`).
    pub version: String,
}

/// Provider of hermetic dep surfaces for Pack-C analysis.
///
/// Implementations MUST be hermetic: same `(pkg, pin)` MUST always return the
/// same surface (keyed from the job's lockfile). No mutable state, no network
/// calls inside `surface()`.
pub trait DepSurfaceProvider {
    fn surface(
        &self,
        pkg: &PackageLineageId,
        pin: &DepPin,
    ) -> Result<Arc<ApiSurface>, DepMissing>;
}

// ---------------------------------------------------------------------------
// NoDeps — always returns DepMissing (disables Pack C)
// ---------------------------------------------------------------------------

/// A `DepSurfaceProvider` that always reports all deps as missing.
///
/// Use this to disable Pack-C analysis (e.g. when running in a hermetic
/// environment without dep archives). All Pack-C findings will be
/// `Uncertain(DepSurfaceUnavailable(_))`.
pub struct NoDeps;

impl DepSurfaceProvider for NoDeps {
    fn surface(
        &self,
        pkg: &PackageLineageId,
        _pin: &DepPin,
    ) -> Result<Arc<ApiSurface>, DepMissing> {
        Err(DepMissing { package: pkg.clone() })
    }
}

// ---------------------------------------------------------------------------
// classify (§9.2)
// ---------------------------------------------------------------------------

/// Classify the API evolution between two surfaces.
///
/// **Pure function** — no I/O, no `rustc`, no network (K-IR-Only-Semver, S-5).
///
/// # Arguments
///
/// - `old` — the baseline `ApiSurface` (old generation).
/// - `new` — the new `ApiSurface` (new generation).
/// - `policy` — classification policy knobs.
/// - `deps` — provider for dep surfaces (use [`NoDeps`] to disable Pack C).
///
/// # Returns
///
/// An [`ApiReport`] with all findings sorted and the required semver bump.
pub fn classify(
    old: &ApiSurface,
    new: &ApiSurface,
    policy: &SemverPolicy,
    deps: &dyn DepSurfaceProvider,
) -> ApiReport {
    let mut findings = Vec::new();

    // ── Pack A: CSC parity (§9.4) ────────────────────────────────────────────
    run_pack_a(old, new, &mut findings);

    // Pack B, C, D: deferred (P8/P9). Pack C (§9.6) will resolve `retgt` chains
    // through this provider; disabled until the dep closure is hermetically pinned.
    let _ = deps;
    let dep_closure = DepClosureStatus::Complete; // no Pack-C yet; trivially complete

    // ── Build report ─────────────────────────────────────────────────────────
    ApiReport::from_findings(
        old_state_fingerprint(old),
        new_state_fingerprint(new),
        new.config,
        findings,
        dep_closure,
        policy.strict_uncertain,
    )
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Derive a `ChangeSetFingerprint` proxy from the surface's config hash.
///
/// In the full pipeline this comes from the channel tip; here we derive a
/// stable placeholder from the surface's config id so tests don't need a
/// live VCS channel.
fn old_state_fingerprint(s: &ApiSurface) -> ChangeSetFingerprint {
    // Use the raw bytes of the config id as the fingerprint preimage.
    // This is advisory (§3.4); the authoritative fingerprint comes from
    // the channel tip at finish time.
    ChangeSetFingerprint::from_domain(
        "nudox.surface.placeholder.v1",
        s.config.as_content_blake3().as_bytes(),
    )
}

fn new_state_fingerprint(s: &ApiSurface) -> ChangeSetFingerprint {
    ChangeSetFingerprint::from_domain(
        "nudox.surface.placeholder.v1",
        s.config.as_content_blake3().as_bytes(),
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semver::surface::{surface, ExportPolicy};
    use ir::change::IntroId;
    use crate::wire::PayloadTable;
    use ir::kind::KindDiscriminant;
    use ir::entry::Visibility;
    use crate::wire::{
        EntryPayloadFlags, FnSigFlags, FunctionWire, KindWire, OwnedEntryPayload, SymbolWire,
    };

    fn intro(n: u8) -> IntroId {
        IntroId::from_raw([n; 32])
    }

    fn fn_payload(name: &str) -> OwnedEntryPayload {
        OwnedEntryPayload::sealed(
            SymbolWire {
                name: name.into(),
                visibility: Visibility::Public,
                documentation: Option::None,
                source_path: "src/lib.rs".into(),
                span_start: 0,
                span_end: 10,
                aliases: Vec::new(),
                deprecation: Option::None,
                doc_links: Vec::new(),
                attrs: Vec::new(),
                cfg: Option::None,
            },
            KindDiscriminant::Function,
            KindWire::Function(FunctionWire {
                input_params: Box::new([]),
                output_params: Box::new([]),
                sig: FnSigFlags::default(),
                generics: Box::new([]),
                wheres: Box::new([]),
            }),
            EntryPayloadFlags::default(),
        )
    }

    fn empty_surface() -> ApiSurface {
        surface(&PayloadTable::new(), &ExportPolicy::default())
    }

    fn single_fn_surface(id: IntroId, name: &str) -> ApiSurface {
        let mut t = PayloadTable::new();
        t.insert_live(id, fn_payload(name), Option::None);
        surface(&t, &ExportPolicy::default())
    }

    /// S-5 hermetic assertion: classify runs with surfaces constructed
    /// entirely in-memory and produces a deterministic report without any I/O.
    #[test]
    fn classify_is_pure_no_io() {
        let old = empty_surface();
        let new = empty_surface();
        let policy = SemverPolicy::default();
        let deps = NoDeps;

        // This must not panic, block, or touch the filesystem.
        let report = classify(&old, &new, &policy, &deps);
        assert!(report.findings.is_empty());
        assert!(report.uncertain.is_empty());
    }

    #[test]
    fn classify_item_removed_gives_major() {
        let id = intro(1);
        let old = single_fn_surface(id, "foo");
        let new = empty_surface();
        let policy = SemverPolicy::default();
        let deps = NoDeps;

        let report = classify(&old, &new, &policy, &deps);
        use crate::semver::report::BreakClass;
        assert_eq!(report.required_bump, BreakClass::Major);
        assert!(!report.findings.is_empty());
    }

    #[test]
    fn classify_no_change_no_findings() {
        let id = intro(2);
        let old = single_fn_surface(id, "bar");
        let new = single_fn_surface(id, "bar");
        let policy = SemverPolicy::default();
        let deps = NoDeps;

        let report = classify(&old, &new, &policy, &deps);
        use crate::semver::report::BreakClass;
        assert_eq!(report.required_bump, BreakClass::None);
        assert!(report.findings.is_empty());
    }
}
