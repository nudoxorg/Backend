//! Honest semantic-lane coverage for the compiled product view.
//!
//! Every surface reads the two non-complete coverage shapes differently.
//! `Coverage::Partial { .. }` means "work is running right now; wait and ask
//! again", and `Coverage::Unavailable { .. }` means "this lane will not answer
//! until the deployment or the source changes". Rendering a terminal semantic
//! record as a fraction therefore instructs every surface to wait forever: a
//! selected row is written once as
//! `ProductSemanticPublicationRecord::Unavailable(SemanticUnavailableReason)`
//! and nothing in the product ever rewrites it, so a `0/7` derived from those
//! rows can never advance. There is no timer, retry, or promotion path behind
//! it. This module therefore partitions selected rows by record variant so a
//! terminal cause leaves the numerator and the denominator entirely and is
//! reported as the typed reason it already is.
//!
//! The vector produced here is also the vector a health report must carry:
//! `projection::readiness_certificate` refuses a report whose coverage differs
//! from the published view root, so the deployment's own embedding terminal is
//! folded in here rather than injected into the reply on the way out.

use super::BuiltinModelError;
use backend_engine::builtin::{
    ProductSemanticPublicationKey, ProductSemanticPublicationRecord, SemanticPublicationCoverage,
    SemanticUnavailableReason,
};
use backend_engine::{Lane, Reason, ViewCoverage, WorkspaceSnapshot};
use std::collections::BTreeSet;

/// Package/profile pairs whose semantic publication was activated in process.
pub(super) type ActivatedProfiles =
    BTreeSet<(backend_engine::PackageKey, backend_semantic::vocabulary::LanguageProfile)>;

/// Whether this deployment configured the embedding half of the semantic lane.
///
/// The embedding provider is read from the environment once at startup and
/// never changes while the owner runs, so an absent provider is a terminal
/// fact about the deployment, not a transient step of an index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SemanticDeployment {
    /// An embedding endpoint was configured, or its configuration was
    /// malformed and is reported separately by the capability inventory.
    Configured,
    /// No embedding endpoint is configured for this deployment.
    Unconfigured,
}

impl SemanticDeployment {
    /// Classifies the process-wide optional remote semantic provider.
    pub(super) const fn from_remote(remote: &super::query::RemoteSemantic) -> Self {
        if remote.is_unconfigured() {
            Self::Unconfigured
        } else {
            Self::Configured
        }
    }
}

/// Counts of selected semantic publication rows, partitioned by record variant.
///
/// Published rows are the only rows that can still move, so they are the only
/// rows that may appear in a fraction. Terminal rows are retained as one
/// deterministic reason instead.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct SemanticTally {
    published: u16,
    completed: u16,
    terminal: Option<SemanticUnavailableReason>,
}

/// One exact fault for a selected row count that cannot fit the wire fraction.
fn count_overflow() -> BuiltinModelError {
    BuiltinModelError("semantic publication count exceeds wire width".to_owned())
}

impl SemanticTally {
    /// Folds one relation row into the tally, ignoring immutable history rows.
    ///
    /// # Errors
    /// Returns an error when the selected row count exceeds the wire width of
    /// a coverage fraction.
    pub(super) fn observe(
        &mut self,
        key: &ProductSemanticPublicationKey,
        record: &ProductSemanticPublicationRecord,
        activated: &ActivatedProfiles,
    ) -> Result<(), BuiltinModelError> {
        if !key.is_selected() {
            return Ok(());
        }
        match record {
            ProductSemanticPublicationRecord::Published { coverage, .. } => {
                self.published = self.published.checked_add(1).ok_or_else(count_overflow)?;
                if matches!(coverage, SemanticPublicationCoverage::Complete)
                    && activated.contains(&(key.package_key(), key.profile()))
                {
                    self.completed = self.completed.checked_add(1).ok_or_else(count_overflow)?;
                }
            }
            ProductSemanticPublicationRecord::Unavailable(reason) => self.retain_terminal(*reason),
        }
        Ok(())
    }

    /// Retains the lowest-ordinal terminal reason observed so far.
    ///
    /// Several selected rows may disagree. Choosing by the relation's own
    /// declared discriminant order — `Toolchain`, `ProjectAuthority`,
    /// `Cancelled`, `Rejected` — makes the reported reason a pure function of
    /// the relation contents rather than of page or iteration order.
    fn retain_terminal(&mut self, reason: SemanticUnavailableReason) {
        let keep = match self.terminal {
            None => true,
            Some(current) => terminal_rank(reason) < terminal_rank(current),
        };
        if keep {
            self.terminal = Some(reason);
        }
    }

    /// Renders the tally as the view's coverage vector.
    ///
    /// The source lane is always complete; only the semantic lane can be
    /// qualified. In-flight published work outranks every terminal cause
    /// because that fraction really does advance, and a terminal cause
    /// outranks a claim of completeness because part of the declared scope was
    /// never covered.
    pub(super) fn coverage(self, deployment: SemanticDeployment) -> Vec<ViewCoverage> {
        let mut coverage = vec![ViewCoverage::Complete];
        if self.completed < self.published {
            coverage.push(ViewCoverage::Partial {
                lane: Lane::Semantic,
                completed: self.completed,
                total: self.published,
            });
            return coverage;
        }
        if let Some(reason) = self.terminal_reason(deployment) {
            coverage.push(ViewCoverage::Unavailable {
                lane: Lane::Semantic,
                reason,
            });
        }
        coverage
    }

    /// Deployment terminals outrank relation terminals: an absent embedding
    /// provider disables the whole lane, while a relation terminal disables
    /// one compilation within it.
    fn terminal_reason(self, deployment: SemanticDeployment) -> Option<Reason> {
        match deployment {
            SemanticDeployment::Unconfigured => Some(Reason::Unconfigured),
            SemanticDeployment::Configured => self.terminal.map(terminal_reason),
        }
    }
}

/// Declared discriminant order of the relation's terminal vocabulary.
const fn terminal_rank(reason: SemanticUnavailableReason) -> u8 {
    match reason {
        SemanticUnavailableReason::Toolchain => 1,
        SemanticUnavailableReason::ProjectAuthority => 2,
        SemanticUnavailableReason::Cancelled => 3,
        SemanticUnavailableReason::Rejected => 4,
    }
}

/// Maps a relation terminal onto the surface-facing reason vocabulary.
///
/// A missing compiler or a missing project authority are both deployment
/// configuration gaps, so both read as `Unconfigured`. A rejected source is a
/// coverage gap in facts that do exist, so it reads as `Incomplete`.
const fn terminal_reason(reason: SemanticUnavailableReason) -> Reason {
    match reason {
        SemanticUnavailableReason::Toolchain | SemanticUnavailableReason::ProjectAuthority => {
            Reason::Unconfigured
        }
        SemanticUnavailableReason::Cancelled => Reason::Cancelled,
        SemanticUnavailableReason::Rejected => Reason::Incomplete,
    }
}

/// Returns whether one coverage entry qualifies the semantic lane.
fn is_semantic_lane(coverage: ViewCoverage) -> bool {
    matches!(
        coverage,
        ViewCoverage::Partial {
            lane: Lane::Semantic,
            ..
        } | ViewCoverage::Unavailable {
            lane: Lane::Semantic,
            ..
        }
    )
}

/// Folds the deployment's embedding terminal into a coverage vector produced
/// elsewhere, leaving the semantic lane with at most one entry.
///
/// The vector already carries the deployment terminal when it came from
/// [`view_coverage`]; this reconciliation exists so a reply assembled from any
/// other path cannot silently drop the fact that no embedding lane is
/// configured, and so the lane can never be described twice.
pub(super) fn reconcile_semantic_lane(
    coverage: &[ViewCoverage],
    deployment: SemanticDeployment,
) -> Vec<ViewCoverage> {
    let mut reconciled = coverage.to_vec();
    if matches!(deployment, SemanticDeployment::Configured)
        || reconciled.iter().copied().any(is_semantic_lane)
    {
        return reconciled;
    }
    reconciled.push(ViewCoverage::Unavailable {
        lane: Lane::Semantic,
        reason: Reason::Unconfigured,
    });
    reconciled
}

/// Reads the selected semantic publication rows and reports the view coverage.
///
/// # Errors
/// Returns an error when the semantic relation cannot be opened or paged, or
/// when the selected row count exceeds the wire width of a fraction.
pub(super) fn view_coverage(
    snapshot: &WorkspaceSnapshot,
    activated: &ActivatedProfiles,
    deployment: SemanticDeployment,
) -> Result<Vec<ViewCoverage>, BuiltinModelError> {
    let relation = snapshot
        .relation::<super::BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    let mut tally = SemanticTally::default();
    let mut after = None;
    loop {
        let page = relation
            .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
            .map_err(|error| BuiltinModelError(error.to_string()))?;
        for (key, record) in page.entries() {
            tally.observe(key, record, activated)?;
        }
        let Some(next) = page.next().cloned() else {
            break;
        };
        after = Some(next);
    }
    Ok(tally.coverage(deployment))
}

#[cfg(test)]
#[path = "coverage/tests.rs"]
mod tests;
