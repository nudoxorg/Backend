//! Report types: [`BreakClass`], [`Certainty`], [`Finding`], [`ApiReport`],
//! and rendering helpers.
//!
//! These are the normative shapes from §9.1. `ApiReport` is the output of
//! [`crate::semver::classify::classify`].

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use nudox_ir::change::{ChangeSetFingerprint, IntroId, PackageLineageId};
use nudox_ir::wire::{CfgExpr, FnSigFlags};

use crate::semver::config::ConfigId;
use crate::semver::surface::MonikerPath;

// ---------------------------------------------------------------------------
// BreakClass (§9.1)
// ---------------------------------------------------------------------------

/// Semver break classification for a finding or the overall report.
///
/// `Ord` is defined so that `Major > Minor > Patch > None`, enabling `max` over
/// slices of findings.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum BreakClass {
    /// Breaking change — requires a semver major bump.
    Major,
    /// Compatible addition — requires a minor bump.
    Minor,
    /// Internal or value-only change — patch bump sufficient.
    Patch,
    /// No API-surface change (e.g. doc-only edit, internal deletion).
    None,
}

impl BreakClass {
    fn rank(self) -> u8 {
        match self {
            BreakClass::Major => 3,
            BreakClass::Minor => 2,
            BreakClass::Patch => 1,
            BreakClass::None => 0,
        }
    }
}

impl PartialOrd for BreakClass {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for BreakClass {
    fn cmp(&self, other: &Self) -> Ordering {
        self.rank().cmp(&other.rank())
    }
}

// ---------------------------------------------------------------------------
// UncertainReason / Certainty (§9.1)
// ---------------------------------------------------------------------------

/// Reason why a finding's verdict is uncertain rather than definite.
// Holds `&'static str` (MissingFact / UnsupportedConstruct) — output-only, so
// `Serialize` only (a `&'static str` cannot be deserialized).
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub enum UncertainReason {
    /// A required IR fact is absent (e.g. `"rpit_captures"`, `"auto:cond"`).
    MissingFact(&'static str),
    /// A dep's `ApiSurface` was not available for Pack-C resolution.
    DepSurfaceUnavailable(PackageLineageId),
    /// The construct is not yet handled by any law pack in v1.
    UnsupportedConstruct(&'static str),
    /// The store is in a conflicted state; classify refuses.
    ConflictedState,
    /// The generation is a checkpoint (partial); publishing forbidden.
    PartialGeneration,
}

/// Certainty of a [`Finding`].
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub enum Certainty {
    /// The facts in the IR are sufficient to decide this verdict.
    Certain,
    /// Facts are missing or ambiguous; the finding is advisory.
    Uncertain(UncertainReason),
}

// ---------------------------------------------------------------------------
// LintId (§9.1)
// ---------------------------------------------------------------------------

/// A static lint identifier string (e.g. `"A-1"`, `"B-3"`).
///
/// This is a newtype over `&'static str` to make lint IDs type-distinguishable
/// from arbitrary strings without runtime allocation.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize)]
pub struct LintId(pub &'static str);

impl LintId {
    pub fn as_str(self) -> &'static str {
        self.0
    }
}

impl std::fmt::Display for LintId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

// ---------------------------------------------------------------------------
// FindingDetail (§9.1)
// ---------------------------------------------------------------------------

/// Typed old/new payload for rendering a finding and generating witnesses.
///
/// This is a pragmatic enum: each variant carries exactly the data needed to
/// render the human-readable detail for that lint class. Not exhaustive —
/// new packs extend it without breaking existing renderers.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub enum FindingDetail {
    /// Item was removed (A-1) or added (complementary).
    ItemPresence {
        /// `true` = was present, now absent; `false` = was absent, now present.
        was_present: bool,
    },
    /// Item kind changed (A-2).
    KindChanged {
        old_kind: SmolStr,
        new_kind: SmolStr,
    },
    /// Visibility was lowered (A-3).
    VisLowered {
        old_vis: SmolStr,
        new_vis: SmolStr,
    },
    /// A public struct field was removed or made private (A-4).
    FieldMissing {
        field_name: SmolStr,
    },
    /// An enum variant was removed (A-5) or added (A-6 / A-7).
    VariantPresence {
        variant_name: SmolStr,
        was_present: bool,
        non_exhaustive: bool,
    },
    /// A trait item was added (A-8 / A-9) or removed (A-10).
    TraitItemPresence {
        item_name: SmolStr,
        was_present: bool,
        defaulted: bool,
        sealed: bool,
    },
    /// `doc(hidden)` status toggled (A-11).
    DocHiddenToggle {
        now_hidden: bool,
    },
    /// `#[must_use]` was added (A-12) or removed.
    MustUseToggle {
        added: bool,
    },
    /// Deprecation was added or removed (A-13).
    DeprecationToggle {
        added: bool,
    },
    /// `#[non_exhaustive]` was added (A-14).
    NonExhaustiveAdded,
    /// A function signature flag changed (A-15).
    FnSigFlagsChanged {
        old_sig: FnSigFlags,
        new_sig: FnSigFlags,
    },
    /// `static mut` status changed, or `const`/`static` kind swapped (A-16).
    StaticMutToggle {
        now_mutable: bool,
    },
    /// `repr` attribute was added, changed, or removed (A-17).
    ReprChanged {
        old_repr: Option<SmolStr>,
        new_repr: Option<SmolStr>,
    },
    /// A struct with all-public fields gained a new field (A-18, L-5).
    AllPubStructGainedField {
        field_name: SmolStr,
    },
    /// A trait lost `dyn`-compatibility (A-19).
    DynCompatLost {
        old_compat: SmolStr,
        new_compat: SmolStr,
    },
    /// No structured detail available; human-readable message only.
    Plain(SmolStr),
}

// ---------------------------------------------------------------------------
// Finding (§9.1)
// ---------------------------------------------------------------------------

/// One classified finding from the linter.
#[derive(Clone, Debug, Serialize)]
pub struct Finding {
    /// The lint rule that fired (e.g. `LintId("A-1")`).
    pub lint: LintId,
    /// The intro of the item this finding is about, if determinable.
    pub item: Option<IntroId>,
    /// The canonical moniker path of the item (for presence lints: the path
    /// that appeared or disappeared).
    pub moniker: MonikerPath,
    /// Semver break classification.
    pub class: BreakClass,
    /// Certainty of the verdict.
    pub certainty: Certainty,
    /// If the finding is only triggered under a specific cfg, this names it
    /// (Pack D attribution, §9.7).
    pub when: Option<CfgExpr>,
    /// Typed details for rendering and witness generation.
    pub detail: FindingDetail,
}

impl Finding {
    /// True iff this finding is `Certainty::Certain`.
    pub fn is_certain(&self) -> bool {
        matches!(self.certainty, Certainty::Certain)
    }
}

// ---------------------------------------------------------------------------
// DepClosureStatus (§9.1)
// ---------------------------------------------------------------------------

/// Whether the dependency closure was fully available for Pack-C analysis.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum DepClosureStatus {
    /// All required dep surfaces were available.
    Complete,
    /// Some dep surfaces were missing; Pack-C findings may be incomplete.
    Missing(Vec<PackageLineageId>),
}

// ---------------------------------------------------------------------------
// Unchecked marker (§9.1)
// ---------------------------------------------------------------------------

/// Zero-size marker for the `toolchain_dimension` field of `ApiReport`.
///
/// In v1, MSRV / edition / toolchain-bump detection is explicitly a non-goal
/// (§3.6). This type is a placeholder that can later become a real enum without
/// changing the field name.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, Default)]
pub struct Unchecked;

// ---------------------------------------------------------------------------
// SemverPolicy (§9.1)
// ---------------------------------------------------------------------------

/// Policy knobs controlling classification behaviour.
///
/// All knobs are `false`/`true` per their CSC-parity defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemverPolicy {
    /// If `true`, `Uncertain` findings raise the required bump (CSC-strict
    /// mode). Default `false` (zero-FP discipline).
    pub strict_uncertain: bool,
    /// If `true`, re-exported dep items that had their own Major delta trigger
    /// a Pack-C4 Major finding. Default `false`.
    pub transitive_api: bool,
    /// If `true`, `sealed:pubapi` traits are treated as sealed for lint
    /// purposes. Default `true`.
    pub treat_pubapi_sealed_as_sealed: bool,
}

impl Default for SemverPolicy {
    fn default() -> Self {
        Self {
            strict_uncertain: false,
            transitive_api: false,
            treat_pubapi_sealed_as_sealed: true,
        }
    }
}

// ---------------------------------------------------------------------------
// ApiReport (§9.1)
// ---------------------------------------------------------------------------

/// The full semver classification report for a package API evolution.
///
/// Produced by [`crate::semver::classify::classify`].
#[derive(Clone, Debug, Serialize)]
pub struct ApiReport {
    /// Fingerprint of the old (baseline) generation.
    pub old_state: ChangeSetFingerprint,
    /// Fingerprint of the new generation.
    pub new_state: ChangeSetFingerprint,
    /// Config under which the surfaces were projected.
    pub config: ConfigId,
    /// The minimum required semver bump derived from **Certain** findings only.
    pub required_bump: BreakClass,
    /// All certain findings, sorted: class descending, then lint id, then moniker.
    pub findings: Vec<Finding>,
    /// All uncertain findings (never contribute to `required_bump`).
    pub uncertain: Vec<Finding>,
    /// Whether the dep closure was fully available.
    pub dep_closure: DepClosureStatus,
    /// MSRV / edition / toolchain-bump detection — always `Unchecked` in v1.
    pub toolchain_dimension: Unchecked,
}

impl ApiReport {
    /// Construct the report from a flat list of findings.
    ///
    /// - Certain and Uncertain findings are separated.
    /// - `required_bump` = max class over Certain findings.
    /// - Findings are sorted: class desc, lint id asc, moniker asc.
    pub(crate) fn from_findings(
        old_state: ChangeSetFingerprint,
        new_state: ChangeSetFingerprint,
        config: ConfigId,
        mut all_findings: Vec<Finding>,
        dep_closure: DepClosureStatus,
        // When `true` (policy `strict_uncertain`), Uncertain findings also raise
        // the bump; default `false` keeps the CSC-style zero-FP discipline (§9.1).
        strict_uncertain: bool,
    ) -> Self {
        // Sort: class DESC, lint id ASC, moniker ASC.
        all_findings.sort_by(|a, b| {
            b.class
                .cmp(&a.class)
                .then(a.lint.0.cmp(b.lint.0))
                .then(a.moniker.0.cmp(&b.moniker.0))
        });

        let mut findings = Vec::new();
        let mut uncertain = Vec::new();
        let mut required_bump = BreakClass::None;

        for f in all_findings {
            if f.is_certain() {
                required_bump = required_bump.max(f.class);
                findings.push(f);
            } else {
                if strict_uncertain {
                    required_bump = required_bump.max(f.class);
                }
                uncertain.push(f);
            }
        }

        ApiReport {
            old_state,
            new_state,
            config,
            required_bump,
            findings,
            uncertain,
            dep_closure,
            toolchain_dimension: Unchecked,
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering helpers
// ---------------------------------------------------------------------------

/// Render a `BreakClass` as a short human-readable string.
pub fn render_class(class: BreakClass) -> &'static str {
    match class {
        BreakClass::Major => "major",
        BreakClass::Minor => "minor",
        BreakClass::Patch => "patch",
        BreakClass::None => "none",
    }
}

/// Render a moniker path as `"crate::semver::module::Item"`.
pub fn render_moniker(path: &MonikerPath) -> String {
    path.0.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn break_class_ord_major_largest() {
        assert!(BreakClass::Major > BreakClass::Minor);
        assert!(BreakClass::Minor > BreakClass::Patch);
        assert!(BreakClass::Patch > BreakClass::None);
        let classes = [BreakClass::None, BreakClass::Patch, BreakClass::Major, BreakClass::Minor];
        let max = classes.into_iter().max().unwrap();
        assert_eq!(max, BreakClass::Major);
    }

    #[test]
    fn api_report_required_bump_from_certain_only() {
        let empty_csf = ChangeSetFingerprint::empty();
        let config = ConfigId::DEFAULT;

        // Mix: one certain Major, one uncertain Minor.
        let findings = vec![
            Finding {
                lint: LintId("A-1"),
                item: Option::None,
                moniker: MonikerPath::new(vec![SmolStr::new("foo")]),
                class: BreakClass::Major,
                certainty: Certainty::Certain,
                when: Option::None,
                detail: FindingDetail::ItemPresence { was_present: true },
            },
            Finding {
                lint: LintId("B-9"),
                item: Option::None,
                moniker: MonikerPath::new(vec![SmolStr::new("bar")]),
                class: BreakClass::Minor,
                certainty: Certainty::Uncertain(UncertainReason::MissingFact("lifetime")),
                when: Option::None,
                detail: FindingDetail::Plain(SmolStr::new("lifetime bound changed")),
            },
        ];

        let report = ApiReport::from_findings(
            empty_csf,
            empty_csf,
            config,
            findings,
            DepClosureStatus::Complete,
            false, // strict_uncertain
        );

        assert_eq!(report.required_bump, BreakClass::Major);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.uncertain.len(), 1);
    }

    #[test]
    fn render_helpers() {
        assert_eq!(render_class(BreakClass::Major), "major");
        let path = MonikerPath::new(vec![SmolStr::new("crate_name"), SmolStr::new("Error")]);
        assert_eq!(render_moniker(&path), "crate_name::Error");
    }
}
