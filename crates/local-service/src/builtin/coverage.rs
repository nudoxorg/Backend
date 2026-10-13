//! Coverage of the indexed snapshot and its latest authenticated semantic capture.
//!
//! Retained generations remain readable after a refused refresh, but cannot
//! certify that refresh as complete. Optional embedding configuration also
//! cannot erase a native compiler refusal. No claim here observes filesystem
//! changes that have not been captured by an indexing operation.

use super::BuiltinModelError;
use backend_engine::builtin::{
    ProductSemanticCaptureOutcome, ProductSemanticCaptureRecord, ProductSemanticPublicationKey,
    ProductSemanticPublicationRecord, SemanticPublicationCoverage, SemanticUnavailableReason,
    semantic_capture_relation,
};
use backend_engine::{Lane, Reason, ViewCoverage, WorkspaceSnapshot};
use std::collections::BTreeSet;

/// Package/profile pairs whose semantic publication was activated in process.
pub(super) type ActivatedProfiles = BTreeSet<(
    backend_engine::PackageKey,
    backend_semantic::vocabulary::LanguageProfile,
)>;

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
    pub(super) fn from_remote(remote: &super::query::RemoteSemantic) -> Self {
        if remote.is_unconfigured() {
            Self::Unconfigured
        } else {
            Self::Configured
        }
    }
}

/// Counts of selected semantic publication rows, partitioned by record variant.
///
/// Pending captures and complete publications awaiting activation can move.
/// Partial publications and refused captures are terminal observed results;
/// they must never produce a fraction that tells the user to wait forever.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct SemanticTally {
    published: u16,
    completed: u16,
    terminal: Option<SemanticUnavailableReason>,
    incomplete: bool,
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
                if matches!(coverage, SemanticPublicationCoverage::Partial(_)) {
                    self.incomplete = true;
                    return Ok(());
                }
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

    /// Folds the latest capture before consulting a retained generation.
    /// A published capture must name the exact selected publication. Pending
    /// and failed captures deliberately do not activate their prior generation.
    pub(super) fn observe_current(
        &mut self,
        key: &ProductSemanticPublicationKey,
        selected: Option<&ProductSemanticPublicationRecord>,
        outcome: Option<ProductSemanticCaptureOutcome>,
        activated: &ActivatedProfiles,
    ) -> Result<(), BuiltinModelError> {
        if !key.is_selected() {
            return Ok(());
        }
        match outcome {
            Some(ProductSemanticCaptureOutcome::Pending { .. }) => {
                self.published = self.published.checked_add(1).ok_or_else(count_overflow)?;
            }
            Some(
                ProductSemanticCaptureOutcome::Failed { reason, .. }
                | ProductSemanticCaptureOutcome::Unavailable { reason },
            ) => {
                // This was an actually requested capture, including when its
                // compiler/project authority was unavailable. Optional
                // deployment scope cannot make that request successful.
                self.incomplete |= !matches!(reason, SemanticUnavailableReason::Cancelled);
                self.retain_terminal(reason);
            }
            Some(ProductSemanticCaptureOutcome::Published { coverage, claim }) => {
                if selected
                    != Some(&ProductSemanticPublicationRecord::Published { coverage, claim })
                {
                    return Err(BuiltinModelError(
                        "semantic capture differs from its selected publication".to_owned(),
                    ));
                }
                self.observe(
                    key,
                    &ProductSemanticPublicationRecord::Published { coverage, claim },
                    activated,
                )?;
            }
            None => {
                if let Some(record) = selected {
                    self.observe(key, record, activated)?;
                }
            }
        }
        Ok(())
    }

    /// Correlates a current capture with the authenticated package frontier.
    /// A stale pending operation no longer promises progress for current source.
    /// Retained publications remain readable but cannot certify another frontier.
    fn observe_scoped(
        &mut self,
        key: &ProductSemanticPublicationKey,
        selected: Option<&ProductSemanticPublicationRecord>,
        capture: Option<&ProductSemanticCaptureRecord>,
        source: &backend_engine::WorkspaceRelationHandle<super::BuiltinWorkspaceRelation>,
        activated: &ActivatedProfiles,
    ) -> Result<(), BuiltinModelError> {
        if !key.is_selected() {
            return Ok(());
        }
        // The current authenticated inventory declares current workspace
        // scope. Retained capture/history for a removed package belongs to
        // explicit historical reads; it cannot poison every remaining package.
        if source
            .lookup(&key.package_key().to_bytes())
            .map_err(|error| BuiltinModelError(format!("read current package scope: {error}")))?
            .as_ref()
            .and_then(|record| record.project_fields())
            .is_none()
        {
            return Ok(());
        }
        if let Some(capture) = capture {
            if !capture_matches_source(source, key, capture)? {
                // Still check a Published claim against its retained selection.
                // Source staleness must not launder an unrelated selected claim.
                self.observe_current(key, selected, Some(capture.outcome()), activated)?;
                self.incomplete = true;
                return Ok(());
            }
        }
        self.observe_current(
            key,
            selected,
            capture.map(|capture| capture.outcome()),
            activated,
        )
    }

    /// Refused declared scope outranks missing optional configuration, with
    /// deterministic precedence independent of page or iteration order.
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
    /// Completeness refers to the indexed source inventory. A refused or
    /// published partial native scope prevents readiness even when another
    /// profile remains pending. Pending captures retain their exact fraction
    /// only when no declared native scope has already failed.
    pub(super) fn coverage(self, deployment: SemanticDeployment) -> Vec<ViewCoverage> {
        let mut coverage = vec![ViewCoverage::Complete];
        let refused = match self.terminal {
            Some(SemanticUnavailableReason::Rejected) => Some(Reason::Incomplete),
            Some(SemanticUnavailableReason::Cancelled) => Some(Reason::Cancelled),
            _ => None,
        };
        if let Some(reason) = if self.incomplete {
            Some(Reason::Incomplete)
        } else {
            refused
        } {
            coverage.push(ViewCoverage::Unavailable {
                lane: Lane::Semantic,
                reason,
            });
            return coverage;
        }
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

    /// Native outcomes qualify declared scope even without embeddings.
    fn terminal_reason(self, deployment: SemanticDeployment) -> Option<Reason> {
        self.terminal.map(terminal_reason).or_else(|| {
            matches!(deployment, SemanticDeployment::Unconfigured).then_some(Reason::Unconfigured)
        })
    }
}

/// Deterministic severity of native outcomes, independent of encoding ordinals.
const fn terminal_rank(reason: SemanticUnavailableReason) -> u8 {
    match reason {
        SemanticUnavailableReason::Rejected => 1,
        SemanticUnavailableReason::Cancelled => 2,
        SemanticUnavailableReason::ProjectAuthority => 3,
        SemanticUnavailableReason::Toolchain => 4,
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

/// Package lifecycle from an authenticated observed capture. Readable rows
/// remain present for every state; this qualifies the latest indexing intent.
pub(super) fn capture_row_state(
    outcome: ProductSemanticCaptureOutcome,
) -> backend_engine::RowState {
    use backend_engine::RowState;
    match outcome {
        ProductSemanticCaptureOutcome::Pending { .. } => RowState::Loading,
        ProductSemanticCaptureOutcome::Failed { .. }
        | ProductSemanticCaptureOutcome::Unavailable { .. }
        | ProductSemanticCaptureOutcome::Published {
            coverage: SemanticPublicationCoverage::Partial(_),
            ..
        } => RowState::Failed,
        ProductSemanticCaptureOutcome::Published {
            coverage: SemanticPublicationCoverage::Complete,
            ..
        } => RowState::Ready,
    }
}

/// Package-local source correlation, without consulting global root equality
/// or the filesystem. A removed/missing package cannot certify its old capture.
pub(super) fn capture_matches_source(
    source: &backend_engine::WorkspaceRelationHandle<super::BuiltinWorkspaceRelation>,
    key: &ProductSemanticPublicationKey,
    capture: &ProductSemanticCaptureRecord,
) -> Result<bool, BuiltinModelError> {
    let project = source
        .lookup(&key.package_key().to_bytes())
        .map_err(|error| BuiltinModelError(format!("read captured package frontier: {error}")))?;
    Ok(project
        .as_ref()
        .and_then(|record| record.project_fields())
        .is_some_and(|project| &project.source_version == capture.capture().source_version()))
}

fn captured_package_state(
    source: &backend_engine::WorkspaceRelationHandle<super::BuiltinWorkspaceRelation>,
    key: &ProductSemanticPublicationKey,
    capture: &ProductSemanticCaptureRecord,
) -> Result<backend_engine::RowState, BuiltinModelError> {
    Ok(if capture_matches_source(source, key, capture)? {
        capture_row_state(capture.outcome())
    } else {
        backend_engine::RowState::Failed
    })
}

/// Legacy selected evidence, when no current capture exists for that key.
fn publication_row_state(record: &ProductSemanticPublicationRecord) -> backend_engine::RowState {
    use backend_engine::RowState;
    match record {
        ProductSemanticPublicationRecord::Published {
            coverage: SemanticPublicationCoverage::Partial(_),
            ..
        }
        | ProductSemanticPublicationRecord::Unavailable(
            SemanticUnavailableReason::Rejected | SemanticUnavailableReason::Cancelled,
        ) => RowState::Failed,
        ProductSemanticPublicationRecord::Published {
            coverage: SemanticPublicationCoverage::Complete,
            ..
        }
        | ProductSemanticPublicationRecord::Unavailable(
            SemanticUnavailableReason::Toolchain | SemanticUnavailableReason::ProjectAuthority,
        ) => RowState::Ready,
    }
}

/// Applies package-scoped observations without erasing retained declarations.
/// Failure dominates pending and ready profiles regardless of relation order.
fn package_states(
    snapshot: &WorkspaceSnapshot,
) -> Result<
    std::collections::BTreeMap<backend_engine::PackageKey, backend_engine::RowState>,
    BuiltinModelError,
> {
    let source = snapshot
        .relation::<super::BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    let captures = semantic_capture_relation(snapshot)
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    let mut states = std::collections::BTreeMap::new();
    let mut observed = BTreeSet::new();
    if let Some(captures) = captures {
        let mut after = None;
        loop {
            let page = captures
                .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
                .map_err(|error| BuiltinModelError(error.to_string()))?;
            for (key, capture) in page.entries() {
                if !key.is_selected() {
                    continue;
                }
                observed.insert(key.clone());
                let state = captured_package_state(&source, key, capture)?;
                states
                    .entry(key.package_key())
                    .and_modify(|current: &mut backend_engine::RowState| {
                        *current = (*current).max(state);
                    })
                    .or_insert(state);
            }
            let Some(next) = page.next().cloned() else {
                break;
            };
            after = Some(next);
        }
    }
    // Legacy selected publications have no companion. Preserve their native
    // refusal/partial scope too, while optional missing authority remains an
    // ordinary configuration gap until an actual capture requests that scope.
    let relation = snapshot
        .relation::<super::BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    let mut after = None;
    loop {
        let page = relation
            .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
            .map_err(|error| BuiltinModelError(error.to_string()))?;
        for (key, record) in page.entries() {
            if !key.is_selected() || observed.contains(key) {
                continue;
            }
            let state = publication_row_state(record);
            states
                .entry(key.package_key())
                .and_modify(|current: &mut backend_engine::RowState| {
                    *current = (*current).max(state)
                })
                .or_insert(state);
        }
        let Some(next) = page.next().cloned() else {
            break;
        };
        after = Some(next);
    }
    Ok(states)
}

#[cfg(test)]
thread_local! {
    static PACKAGE_STATE_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn reset_package_state_visits() {
    PACKAGE_STATE_VISITS.with(|visits| visits.set(0));
}

#[cfg(test)]
pub(super) fn package_state_visits() -> usize {
    PACKAGE_STATE_VISITS.with(std::cell::Cell::get)
}

/// Seeks one typed package prefix rather than collecting workspace keys.
fn visit_selected_package<R>(
    relation: &backend_engine::WorkspaceRelationHandle<R>,
    package: &backend_engine::PackageReference,
    mut visit: impl FnMut(&ProductSemanticPublicationKey, &R::Value) -> Result<(), BuiltinModelError>,
) -> Result<(), BuiltinModelError>
where
    R: backend_version::CanonicalRelation<Key = ProductSemanticPublicationKey>,
{
    let start = ProductSemanticPublicationKey::package_lower_bound(package.clone());
    let mut after = None;
    loop {
        let page = match after.as_ref() {
            None => relation.page_from(&start, backend_engine::MAX_SNAPSHOT_PAGE_ROWS),
            Some(after) => relation.page(Some(after), backend_engine::MAX_SNAPSHOT_PAGE_ROWS),
        }
        .map_err(|error| BuiltinModelError(error.to_string()))?;
        for (key, record) in page.entries() {
            if key.package() != package {
                return Ok(());
            }
            if key.is_selected() {
                #[cfg(test)]
                PACKAGE_STATE_VISITS.with(|visits| visits.set(visits.get() + 1));
                visit(key, record)?;
            }
        }
        let Some(next) = page.next().cloned() else {
            return Ok(());
        };
        after = Some(next);
    }
}

/// Qualifies one package against its exact typed capture/publication prefix.
pub(super) fn package_row_state(
    snapshot: &WorkspaceSnapshot,
    package: &backend_engine::PackageReference,
) -> Result<Option<backend_engine::RowState>, BuiltinModelError> {
    let source = snapshot
        .relation::<super::BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    let captures = semantic_capture_relation(snapshot)
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    let mut observed = BTreeSet::new();
    let mut state: Option<backend_engine::RowState> = None;
    if let Some(captures) = captures {
        visit_selected_package(&captures, package, |key, capture| {
            observed.insert(key.clone());
            let current = captured_package_state(&source, key, capture)?;
            state = Some(state.map_or(current, |prior| prior.max(current)));
            Ok(())
        })?;
    }
    let relation = snapshot
        .relation::<super::BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    visit_selected_package(&relation, package, |key, record| {
        if !observed.contains(key) {
            let current = publication_row_state(record);
            state = Some(state.map_or(current, |prior| prior.max(current)));
        }
        Ok(())
    })?;
    Ok(state)
}

/// Applies package-scoped observations without changing retained declarations.
pub(super) fn qualify_package_rows(
    snapshot: &WorkspaceSnapshot,
    rows: &mut [backend_engine::Row],
) -> Result<(), BuiltinModelError> {
    let states = package_states(snapshot)?;
    for row in rows {
        if let backend_engine::RowId::Package(package) = row.id {
            if let Some(state) = states.get(&package) {
                row.state = *state;
            }
        }
    }
    Ok(())
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
    let captures = semantic_capture_relation(snapshot)
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    let source = snapshot
        .relation::<super::BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    let mut tally = SemanticTally::default();
    let mut after = None;
    loop {
        let page = relation
            .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
            .map_err(|error| BuiltinModelError(error.to_string()))?;
        for (key, record) in page.entries() {
            if !key.is_selected() {
                continue;
            }
            let capture = captures
                .as_ref()
                .map(|captures| captures.lookup(key))
                .transpose()
                .map_err(|error| BuiltinModelError(error.to_string()))?
                .flatten();
            tally.observe_scoped(key, Some(record), capture.as_ref(), &source, activated)?;
        }
        let Some(next) = page.next().cloned() else {
            break;
        };
        after = Some(next);
    }
    // A first source-first capture may precede any selected generation.
    // Include that requested scope without counting selected rows twice.
    if let Some(captures) = captures {
        let mut after = None;
        loop {
            let page = captures
                .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
                .map_err(|error| BuiltinModelError(error.to_string()))?;
            for (key, capture) in page.entries() {
                if !key.is_selected() {
                    continue;
                }
                let selected = relation
                    .lookup(key)
                    .map_err(|error| BuiltinModelError(error.to_string()))?;
                if selected.is_none() {
                    tally.observe_scoped(key, None, Some(capture), &source, activated)?;
                }
            }
            let Some(next) = page.next().cloned() else {
                break;
            };
            after = Some(next);
        }
    }
    Ok(tally.coverage(deployment))
}

#[cfg(test)]
#[path = "coverage/tests.rs"]
mod tests;
