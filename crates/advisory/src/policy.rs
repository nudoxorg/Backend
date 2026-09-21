use serde::{Deserialize, Serialize};

use super::model::{
    Advisory, AdvisoryStatus, FreshnessState, MalwareCoverage, PackageIdentity, SeverityLevel,
};
use super::version::range_matches;

/// Offline behavior for stale or unavailable advisory feeds.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OfflinePolicy {
    /// Permit cached evidence, even when it is outside the freshness window, with a warning.
    AllowCached,
    /// Permit cached evidence only as a warning when stale.
    Warn,
    /// Reject installs when current evidence cannot be obtained.
    FailClosed,
}

/// Whether the selected advisory authorities cover the requested package.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AdvisoryCoverage {
    /// All configured authorities evaluated this ecosystem/package.
    Complete,
    /// At least one authority evaluated it, but coverage is incomplete.
    Partial,
    /// No authority could evaluate it.
    Unknown,
    /// An authority was configured, but its feed was unavailable for this decision.
    Unavailable,
}

/// Runtime state used by the acquisition gate.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AdvisoryObservation {
    /// Matched advisory objects.
    pub advisories: Box<[Advisory]>,
    /// Coverage of the package/equivalent ecosystem.
    pub coverage: AdvisoryCoverage,
    /// Whether the selected feed frontier is fresh.
    pub freshness: FreshnessState,
    /// Whether the decision is being made without a live source.
    pub offline: bool,
    /// Registry publisher withdrew this exact release.
    pub yanked: bool,
    /// Registry publisher hides this exact release from ordinary listings.
    pub unlisted: bool,
    /// Whether a source explicitly evaluates malicious-package claims.
    pub malware: MalwareCoverage,
}

/// An explicit, auditable override for an otherwise blocked installation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OverrideEvidence {
    /// Stable actor or automation identity.
    pub actor: String,
    /// Human reason, retained in the journal/audit event.
    pub reason: String,
    /// Policy version under which the override was granted.
    pub policy_version: u16,
    /// Optional expiry in seconds since the Unix epoch.
    pub expires_at: Option<u64>,
}

/// Typed reason explaining an acquisition decision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PolicyReason {
    /// One or more matching advisories.
    Advisory(AdvisoryStatus),
    /// Feed lacks complete coverage.
    IncompleteCoverage,
    /// Configured advisory authority could not be reached.
    UnavailableEvidence,
    /// Feed is stale.
    StaleEvidence,
    /// No cache exists while offline.
    NoCachedEvidence,
    /// User/automation override was accepted.
    OverrideAccepted(OverrideEvidence),
    /// Cached evidence was allowed by policy.
    CachedEvidenceAllowed,
}

/// Installation/acquisition result with no prose parsing required by callers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AcquisitionDecision {
    /// Safe under the configured evidence and policy.
    Allow,
    /// Proceed, but surface reasons to every product surface.
    Warn(Box<[PolicyReason]>),
    /// Block until evidence or explicit override changes the decision.
    Deny(Box<[PolicyReason]>),
}

/// Stateless gate shared by acquisition, CLI, MCP, and the desktop package view.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AcquisitionGate {
    /// Offline handling selected by the workspace.
    pub offline: OfflinePolicy,
}

impl AcquisitionGate {
    /// Evaluates an observation without allocating when it is clean and fresh.
    #[must_use]
    pub fn decide(&self, observation: &AdvisoryObservation) -> AcquisitionDecision {
        let mut reasons = Vec::new();
        let mut blocked = false;
        if observation.yanked {
            reasons.push(PolicyReason::Advisory(AdvisoryStatus::Yanked));
        }
        if observation.unlisted {
            reasons.push(PolicyReason::Advisory(AdvisoryStatus::Unlisted));
        }
        if observation.malware == MalwareCoverage::NotCovered
            && observation.coverage == AdvisoryCoverage::Complete
        {
            reasons.push(PolicyReason::IncompleteCoverage);
            if self.offline == OfflinePolicy::FailClosed {
                blocked = true;
            }
        }
        for advisory in &observation.advisories {
            if advisory.is_withdrawn() {
                reasons.push(PolicyReason::Advisory(AdvisoryStatus::Withdrawn));
                continue;
            }
            let statuses = advisory.statuses();
            for status in statuses {
                if matches!(
                    status,
                    AdvisoryStatus::Vulnerable
                        | AdvisoryStatus::Malicious
                        | AdvisoryStatus::Unsound
                ) {
                    blocked = true;
                }
                reasons.push(PolicyReason::Advisory(status));
            }
        }
        match observation.coverage {
            AdvisoryCoverage::Complete => {}
            AdvisoryCoverage::Partial | AdvisoryCoverage::Unknown => {
                reasons.push(PolicyReason::IncompleteCoverage);
                if self.offline == OfflinePolicy::FailClosed {
                    blocked = true;
                }
            }
            AdvisoryCoverage::Unavailable => {
                reasons.push(PolicyReason::UnavailableEvidence);
                if self.offline == OfflinePolicy::FailClosed {
                    blocked = true;
                }
            }
        }
        if matches!(
            observation.freshness,
            FreshnessState::Stale | FreshnessState::Unknown
        ) {
            reasons.push(PolicyReason::StaleEvidence);
            if self.offline == OfflinePolicy::FailClosed {
                blocked = true;
            }
        }
        if blocked {
            AcquisitionDecision::Deny(reasons.into_boxed_slice())
        } else if reasons.is_empty() {
            AcquisitionDecision::Allow
        } else {
            AcquisitionDecision::Warn(reasons.into_boxed_slice())
        }
    }

    /// Evaluates with a checked, auditable override.
    #[must_use]
    pub fn decide_with_override(
        &self,
        observation: &AdvisoryObservation,
        evidence: OverrideEvidence,
        now: u64,
    ) -> AcquisitionDecision {
        let decision = self.decide(observation);
        let valid = !evidence.actor.trim().is_empty()
            && !evidence.reason.trim().is_empty()
            && evidence.policy_version > 0
            && evidence.expires_at.is_none_or(|expiry| expiry >= now);
        if valid && matches!(decision, AcquisitionDecision::Deny(_)) {
            AcquisitionDecision::Warn(Box::new([PolicyReason::OverrideAccepted(evidence)]))
        } else {
            decision
        }
    }

    /// Resolves matching advisory objects for an exact package/version pair.
    pub fn matching<'a>(
        advisories: impl IntoIterator<Item = &'a Advisory>,
        package: &PackageIdentity,
        version: &str,
    ) -> Box<[&'a Advisory]> {
        advisories
            .into_iter()
            .filter(|advisory| {
                !advisory.is_withdrawn()
                    && advisory.affected.iter().any(|range| {
                        range.package.ecosystem == package.ecosystem
                            && range.package.name == package.name
                            && range_matches(range, version).is_ok_and(|matched| matched)
                    })
            })
            .collect()
    }

    /// Highest severity in a matched set, useful for compact product DTOs.
    #[must_use]
    pub fn maximum_severity(advisories: &[Advisory]) -> SeverityLevel {
        advisories
            .iter()
            .map(|advisory| advisory.severity.level)
            .max()
            .unwrap_or(SeverityLevel::Unknown)
    }

    /// Turns malware coverage into the explicit unknown status when the authority is absent.
    #[must_use]
    pub fn coverage_status(
        coverage: AdvisoryCoverage,
        malware: MalwareCoverage,
    ) -> Option<AdvisoryStatus> {
        (coverage != AdvisoryCoverage::Complete || malware == MalwareCoverage::NotCovered)
            .then_some(AdvisoryStatus::UnknownCoverage)
    }
}
