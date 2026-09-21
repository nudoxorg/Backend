use serde::{Deserialize, Serialize};

use super::model::{
    Advisory, AdvisoryCategory, AdvisoryStatus, AffectedRange, FreshnessState, NativeAdvisoryId,
    SeverityLevel,
};
use super::policy::{
    AcquisitionDecision, AdvisoryCoverage, AdvisoryObservation, OverrideEvidence, PolicyReason,
};

/// Versioned compact advisory projection for library, CLI, MCP, and desktop surfaces.
///
/// It carries source-backed facts only.  A surface may add a registry standing (yanked or
/// unlisted) from its release record, but it cannot manufacture a clean/security claim when the
/// advisory frontier is unknown.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdvisorySurfaceDto {
    /// DTO schema version.
    pub schema: u16,
    /// Canonical advisory identity.
    pub canonical_id: String,
    /// Source-qualified native identities which contributed this claim.
    pub source_ids: Box<[NativeAdvisoryId]>,
    /// All aliases retained by the source graph.
    pub aliases: Box<[String]>,
    /// Source-derived status labels.
    pub statuses: Box<[AdvisoryStatus]>,
    /// Security categories retained from the source object.
    pub categories: Box<[AdvisoryCategory]>,
    /// Package/range claims, including source matcher semantics.
    pub affected: Box<[AffectedRange]>,
    /// Fixed boundaries projected from the source matcher for compact clients.
    pub fixed_ranges: Box<[String]>,
    /// Normalized severity.
    pub severity: SeverityLevel,
    /// Coverage of the selected source frontier.
    pub coverage: AdvisoryCoverage,
    /// Freshness of the selected source frontier.
    pub freshness: FreshnessState,
    /// Source withdrawal timestamp.
    pub withdrawn: Option<String>,
    /// Registry publisher withdrew the selected release.
    pub yanked: bool,
    /// Registry publisher hides the selected release from ordinary listings.
    pub unlisted: bool,
}

/// Product-level policy projection paired with advisory facts.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdvisoryDecisionDto {
    /// Advisory fact projection.
    pub advisory: AdvisorySurfaceDto,
    /// Typed acquisition result.
    pub decision: AcquisitionDecision,
    /// Stable reasons suitable for logs and UI affordances.
    pub reasons: Box<[PolicyReason]>,
}

impl AdvisorySurfaceDto {
    /// Projects a source-backed advisory object without copying its range bodies.
    #[must_use]
    pub fn from_advisory(
        advisory: &Advisory,
        coverage: AdvisoryCoverage,
        freshness: FreshnessState,
    ) -> Self {
        Self {
            schema: 1,
            canonical_id: advisory.key.canonical.0.clone(),
            source_ids: Box::new([advisory.key.native.clone()]),
            aliases: advisory
                .aliases
                .iter()
                .map(|alias| alias.value.clone())
                .collect(),
            statuses: advisory.statuses(),
            categories: advisory.categories.clone(),
            affected: advisory.affected.clone(),
            fixed_ranges: advisory
                .affected
                .iter()
                .flat_map(|range| match &range.matcher {
                    super::model::VersionMatcher::Events { events, .. } => events
                        .iter()
                        .filter(|event| {
                            matches!(
                                event.kind,
                                super::model::VersionEventKind::Fixed
                                    | super::model::VersionEventKind::Limit
                            )
                        })
                        .map(|event| event.version.clone())
                        .collect::<Vec<_>>(),
                    super::model::VersionMatcher::RustSec { patched, .. } => patched.to_vec(),
                    super::model::VersionMatcher::Unsupported { .. } => Vec::new(),
                })
                .collect(),
            severity: advisory.severity.level,
            coverage,
            freshness,
            withdrawn: advisory.withdrawn.clone(),
            yanked: false,
            unlisted: false,
        }
    }

    /// Projects one observation while preserving unknown/stale coverage.
    #[must_use]
    pub fn from_observation(observation: &AdvisoryObservation) -> Box<[Self]> {
        observation
            .advisories
            .iter()
            .map(|advisory| {
                let mut dto =
                    Self::from_advisory(advisory, observation.coverage, observation.freshness);
                dto.yanked = observation.yanked;
                dto.unlisted = observation.unlisted;
                dto
            })
            .collect()
    }
}

/// Complete package/version security projection shared by registry, product, CLI, MCP, and GUI.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdvisoryPackageDto {
    /// DTO schema version.
    pub schema: u16,
    /// Advisory objects matching this exact package version.
    pub advisories: Box<[AdvisorySurfaceDto]>,
    /// Coverage of the configured advisory authorities.
    pub coverage: AdvisoryCoverage,
    /// Freshness of the selected advisory frontier.
    pub freshness: FreshnessState,
    /// Registry publisher withdrew this exact version.
    pub yanked: bool,
    /// Registry publisher hides this exact version from listings.
    pub unlisted: bool,
    /// Typed acquisition decision made before archive staging.
    pub decision: AcquisitionDecision,
    /// Stable policy reasons retained for audit and display.
    pub reasons: Box<[PolicyReason]>,
}

impl AdvisoryPackageDto {
    /// Constructs the safe absence state. Unknown coverage is never projected as clean.
    #[must_use]
    pub fn unknown() -> Self {
        let reasons = Box::new([
            PolicyReason::IncompleteCoverage,
            PolicyReason::StaleEvidence,
        ]);
        Self {
            schema: 1,
            advisories: Box::new([]),
            coverage: AdvisoryCoverage::Unknown,
            freshness: FreshnessState::Unknown,
            yanked: false,
            unlisted: false,
            decision: AcquisitionDecision::Warn(reasons.clone()),
            reasons,
        }
    }

    /// Projects one release observation and its already-evaluated policy result.
    #[must_use]
    pub fn from_observation(
        observation: &AdvisoryObservation,
        decision: AcquisitionDecision,
    ) -> Self {
        let advisories = AdvisorySurfaceDto::from_observation(observation);
        let reasons = match &decision {
            AcquisitionDecision::Allow => Box::new([]),
            AcquisitionDecision::Warn(reasons) | AcquisitionDecision::Deny(reasons) => {
                reasons.clone()
            }
        };
        Self {
            schema: 1,
            advisories,
            coverage: observation.coverage,
            freshness: observation.freshness,
            yanked: observation.yanked,
            unlisted: observation.unlisted,
            decision,
            reasons,
        }
    }

    /// Applies a checked, auditable override to a persisted decision for a read-only product
    /// projection. Invalid or expired evidence leaves the safe decision unchanged.
    #[must_use]
    pub fn with_override(mut self, evidence: OverrideEvidence, now: u64) -> Self {
        let valid = !evidence.actor.trim().is_empty()
            && !evidence.reason.trim().is_empty()
            && evidence.policy_version > 0
            && evidence.expires_at.is_none_or(|expiry| expiry >= now);
        if valid && matches!(self.decision, AcquisitionDecision::Deny(_)) {
            let reason = PolicyReason::OverrideAccepted(evidence);
            self.decision = AcquisitionDecision::Warn(Box::new([reason.clone()]));
            self.reasons = Box::new([reason]);
        }
        self
    }
}

impl AdvisoryDecisionDto {
    /// Creates the compact paired policy result.
    #[must_use]
    pub fn new(advisory: AdvisorySurfaceDto, decision: AcquisitionDecision) -> Self {
        let reasons = match &decision {
            AcquisitionDecision::Allow => Box::new([]),
            AcquisitionDecision::Warn(reasons) | AcquisitionDecision::Deny(reasons) => {
                reasons.clone()
            }
        };
        Self {
            advisory,
            decision,
            reasons,
        }
    }
}
