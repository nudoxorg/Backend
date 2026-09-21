use serde::{Deserialize, Serialize};

use super::model::{Advisory, AdvisoryStatus, FreshnessState, SeverityLevel};
use super::policy::{AcquisitionDecision, AdvisoryCoverage, AdvisoryObservation, PolicyReason};

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
    /// All aliases retained by the source graph.
    pub aliases: Box<[String]>,
    /// Source-derived status labels.
    pub statuses: Box<[AdvisoryStatus]>,
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
            aliases: advisory
                .aliases
                .iter()
                .map(|alias| alias.value.clone())
                .collect(),
            statuses: advisory.statuses(),
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
