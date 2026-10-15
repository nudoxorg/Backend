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

/// Whether this deployment configured optional vector search.
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
/// Pending captures and publications awaiting activation can move; only they
/// produce a fraction. Publications with settled source gaps and refused
/// captures are terminal observed results and must never produce a fraction
/// that tells the user to wait forever.
///
/// A publication with source gaps (for example a source the configured
/// compiler program excludes) is usable, so it counts as published and
/// completed like any other readable generation and additionally marks the
/// tally `gapped`. A refusal or stale capture marks it `incomplete`. Which
/// words the lane and the project row use for those facts is decided once, in
/// [`SemanticTally::coverage`] and [`merge_package_state`], so the two cannot
/// disagree.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct SemanticTally {
    published: u16,
    completed: u16,
    terminal: Option<SemanticUnavailableReason>,
    incomplete: bool,
    gapped: bool,
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
                if matches!(coverage, SemanticPublicationCoverage::Partial(_)) {
                    // Every published artifact is complete; the authority
                    // checked only part of the declared scope. Readable and
                    // settled, so it completes like any generation and marks
                    // the lane partial instead of failing the project.
                    self.gapped = true;
                }
                if activated.contains(&(key.package_key(), key.profile())) {
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
                // A stale capture promises no progress and certifies nothing:
                // the scratch tally validates the claim, and only its terminal
                // reason survives, so the stale profile is neither in flight
                // nor usable.
                let mut scratch = Self::default();
                scratch.observe_current(key, selected, Some(capture.outcome()), activated)?;
                if let Some(reason) = scratch.terminal {
                    self.retain_terminal(reason);
                }
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
    /// Completeness refers to the indexed source inventory. Precedence is
    /// fixed and independent of page or iteration order:
    ///
    /// 1. Work still moving (a pending capture, or a publication awaiting
    ///    activation) reads as the exact fraction. A reader may wait.
    /// 2. Once everything settled, a readable publication stands for the
    ///    lane. Source gaps, or a profile refused while another published,
    ///    make it `Gapped`: usable, and nothing further will change it.
    /// 3. With no readable publication, a refused or stale scope is
    ///    `Unavailable`.
    /// 4. With no native refusal, the observed compiler scope is complete.
    /// Optional vector configuration has its own SemanticSearchStatus and
    /// never changes this compiler tally.
    pub(super) fn coverage(self) -> Vec<ViewCoverage> {
        let mut coverage = vec![ViewCoverage::Complete];
        if self.completed < self.published {
            coverage.push(ViewCoverage::Partial {
                lane: Lane::Semantic,
                completed: self.completed,
                total: self.published,
            });
            return coverage;
        }
        let refused = self.incomplete || self.terminal.is_some();
        if self.published > 0 {
            if self.gapped || refused {
                coverage.push(ViewCoverage::Gapped {
                    lane: Lane::Semantic,
                });
                return coverage;
            }
        } else if refused {
            let reason = self.terminal.map(terminal_reason).unwrap_or(Reason::Incomplete);
            coverage.push(ViewCoverage::Unavailable {
                lane: Lane::Semantic,
                reason,
            });
            return coverage;
        }
        coverage
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
        | ProductSemanticCaptureOutcome::Unavailable { .. } => RowState::Failed,
        // A publication with source gaps is readable and settled. The gaps are
        // reported by the lane (`Gapped`), not by failing the project.
        ProductSemanticCaptureOutcome::Published { .. } => RowState::Ready,
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
        ProductSemanticPublicationRecord::Unavailable(_) => RowState::Failed,
        ProductSemanticPublicationRecord::Published { .. } => RowState::Ready,
    }
}

/// Combines the lifecycles of two profiles of one package.
///
/// Work still moving dominates: while any profile is pending the project is
/// indexing, whatever its siblings reported. Once nothing moves, a readable
/// profile keeps the project usable, so a project fails only when every
/// profile failed. This is the row-level twin of [`SemanticTally::coverage`],
/// which applies the same precedence to the lane, and the result does not
/// depend on relation order.
fn merge_package_state(
    current: backend_engine::RowState,
    next: backend_engine::RowState,
) -> backend_engine::RowState {
    use backend_engine::RowState;
    match (current, next) {
        (RowState::Loading, _) | (_, RowState::Loading) => RowState::Loading,
        (RowState::Ready, _) | (_, RowState::Ready) => RowState::Ready,
        (RowState::Failed, RowState::Failed) => RowState::Failed,
    }
}

/// Applies package-scoped observations without erasing retained declarations.
/// The merge is order independent: see [`merge_package_state`].
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
                        *current = merge_package_state(*current, state);
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
                    *current = merge_package_state(*current, state);
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
            state = Some(state.map_or(current, |prior| merge_package_state(prior, current)));
            Ok(())
        })?;
    }
    let relation = snapshot
        .relation::<super::BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    visit_selected_package(&relation, package, |key, record| {
        if !observed.contains(key) {
            let current = publication_row_state(record);
            state = Some(state.map_or(current, |prior| merge_package_state(prior, current)));
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
#[cfg(test)]
fn is_semantic_lane(coverage: ViewCoverage) -> bool {
    matches!(
        coverage,
        ViewCoverage::Partial {
            lane: Lane::Semantic,
            ..
        } | ViewCoverage::Gapped {
            lane: Lane::Semantic,
        } | ViewCoverage::Unavailable {
            lane: Lane::Semantic,
            ..
        }
    )
}

/// Reads the selected semantic publication rows and reports the view coverage.
///
/// # Errors
/// Returns an error when the semantic relation cannot be opened or paged, or
/// when the selected row count exceeds the wire width of a fraction.
pub(super) fn view_coverage(
    snapshot: &WorkspaceSnapshot,
    activated: &ActivatedProfiles,
    _deployment: SemanticDeployment,
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
    Ok(tally.coverage())
}

#[cfg(test)]
#[path = "coverage/tests.rs"]
mod tests;
