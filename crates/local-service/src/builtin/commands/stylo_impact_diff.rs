//! Owner adapter for generic exact-ID diffs over retained Stylo material.
//! Native row absence is scoped to captured selected-root rows; semantic CSS
//! coverage, cascade and provider selection remain independently gapped.
use super::impact_diff::{self as diff, Entry};
use super::impact_retention::{Basis, ChangeSide, Coverage, Lease, LeaseId, LeaseStore};
use super::stylo_impact_adapter::{self, WalkResult};
use super::stylo_impact_walk;
use backend_engine::builtin::{StyloAnalysisRecord, StyloAnalysisRelation, StyloProjectedRow};
use backend_semantic::stylo_analysis::StyloSourceRole;
use backend_version::CanonicalRelation;
use std::time::Instant;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Domain {
    NativeFacts,
    VerifiedSourceImages,
}
#[derive(Clone, Copy)]
pub(super) struct Limits {
    pub scan_rows: usize,
    pub scan_bytes: usize,
    pub temporary_bytes: usize,
    pub diff: diff::Limits,
}
#[derive(Debug)]
pub(super) enum Error {
    Expired,
    Invalid,
    WorkLimit,
    Cancelled,
}
pub(super) struct Outcome {
    pub base: Basis,
    pub target: Basis,
    pub domain: Domain,
    pub base_complete: bool,
    pub target_complete: bool,
    pub scan_rows: usize,
    pub scan_bytes: usize,
    pub temporary_bytes: usize,
    pub result: diff::Outcome,
}
fn entries<'a, F: FnMut() -> bool>(
    lease: &'a Lease,
    domain: Domain,
    rows: &mut usize,
    bytes: &mut usize,
    temp: &mut usize,
    limits: Limits,
    cancelled: &mut F,
) -> Result<Vec<Entry<'a>>, Error> {
    let items = match domain {
        Domain::NativeFacts => &lease.records,
        Domain::VerifiedSourceImages => &lease.sources,
    };
    let mut result = Vec::new();
    for item in items.iter() {
        if cancelled() {
            return Err(Error::Cancelled);
        }
        *rows = rows.checked_add(1).ok_or(Error::WorkLimit)?;
        if *rows > limits.scan_rows {
            return Err(Error::WorkLimit);
        }
        if domain == Domain::NativeFacts {
            *bytes = bytes
                .checked_add(item.bytes.len())
                .ok_or(Error::WorkLimit)?;
            if *bytes > limits.scan_bytes {
                return Err(Error::WorkLimit);
            }
            let row =
                StyloAnalysisRelation::decode_value(&item.bytes).map_err(|_| Error::Invalid)?;
            if !matches!(row, StyloAnalysisRecord::Fact { .. }) {
                continue;
            }
        }
        *temp = temp
            .checked_add(std::mem::size_of::<Entry<'_>>() * 2)
            .ok_or(Error::WorkLimit)?;
        if *temp > limits.temporary_bytes {
            return Err(Error::WorkLimit);
        }
        result.push(Entry {
            id: item.key,
            bytes: &item.bytes,
        });
    }
    Ok(result)
}
/// Return exact native-row/image changes only when opposite enumeration is
/// complete. A missing row on a gapped side is a candidate, not a deletion.
pub(super) fn compare<F: FnMut() -> bool>(
    store: &mut LeaseStore,
    base: LeaseId,
    target: LeaseId,
    domain: Domain,
    limits: Limits,
    now: Instant,
    mut cancelled: F,
) -> Result<Outcome, Error> {
    let (base, target) = store
        .comparison_pair(base, target, now)
        .map_err(|_| Error::Expired)?;
    let mut rows = 0usize;
    let mut bytes = 0usize;
    // Reserve for geometric output-vector capacity in addition to decode scratch.
    let mut temp = 131_072usize
        .checked_add(
            limits
                .diff
                .output_bytes
                .checked_mul(2)
                .ok_or(Error::WorkLimit)?,
        )
        .ok_or(Error::WorkLimit)?;
    if temp > limits.temporary_bytes {
        return Err(Error::WorkLimit);
    }
    let old = entries(
        base,
        domain,
        &mut rows,
        &mut bytes,
        &mut temp,
        limits,
        &mut cancelled,
    )?;
    let new = entries(
        target,
        domain,
        &mut rows,
        &mut bytes,
        &mut temp,
        limits,
        &mut cancelled,
    )?;
    let coverage = |lease: &Lease| match domain {
        Domain::NativeFacts => lease.relation_coverage == Coverage::Complete,
        Domain::VerifiedSourceImages => lease.source_coverage == Coverage::Complete,
    };
    let (base_complete, target_complete) = (coverage(base), coverage(target));
    let result = diff::diff(
        &old,
        &new,
        base_complete,
        target_complete,
        limits.diff,
        cancelled,
    );
    Ok(Outcome {
        base: base.basis,
        target: target.basis,
        domain,
        base_complete,
        target_complete,
        scan_rows: rows,
        scan_bytes: bytes,
        temporary_bytes: temp,
        result,
    })
}
/// Traverse a changed authored custom-property occurrence on its actual side.
/// A change candidate stays candidate; no identity is joined by spelling.
fn walk_changed<F: FnMut() -> bool>(
    store: &mut LeaseStore,
    base: LeaseId,
    target: LeaseId,
    change: diff::Change,
    side: ChangeSide,
    budget: &mut WalkBudget,
    now: Instant,
    cancelled: F,
) -> Result<Option<(diff::Key, bool, WalkResult)>, Error> {
    let applicable = matches!(
        (change.side, side),
        (diff::Side::BaseOnly, ChangeSide::Base)
            | (diff::Side::TargetOnly, ChangeSide::Target)
            | (diff::Side::BothChanged, _)
    );
    if !applicable {
        return Err(Error::Invalid);
    }
    let lease = store
        .comparison_side(base, target, side, now)
        .map_err(|_| Error::Expired)?;
    let Some(item) = lease
        .records
        .binary_search_by_key(&change.id, |row| row.key)
        .ok()
        .map(|index| &lease.records[index])
    else {
        return Err(Error::Invalid);
    };
    if budget.input_rows == 0 || item.bytes.len() > budget.input_bytes {
        return Err(Error::WorkLimit);
    }
    budget.input_rows -= 1;
    budget.input_bytes -= item.bytes.len();
    let row = StyloAnalysisRelation::decode_value(&item.bytes).map_err(|_| Error::Invalid)?;
    let host = match row {
        StyloAnalysisRecord::Fact {
            host,
            fact: StyloProjectedRow::SourceOccurrence(ref site),
            ..
        } if site.role == StyloSourceRole::CustomPropertyDeclarationName => host,
        _ => return Ok(None),
    };
    let source_available = lease.historical_source(&host).is_some();
    let limits = stylo_impact_walk::Limits {
        input_rows: budget.input_rows,
        input_bytes: budget.input_bytes,
        edges: budget.edges,
        nodes: budget.nodes,
        depth: budget.depth,
        rows: budget.rows,
        witness_bytes: budget.witness_bytes,
        temporary_bytes: budget.temporary_bytes,
    };
    stylo_impact_adapter::walk_lease(lease, change.id, limits, cancelled)
        .map(|walk| Some((host, source_available, walk)))
        .map_err(|error| match error {
            stylo_impact_adapter::Error::Cancelled => Error::Cancelled,
            stylo_impact_adapter::Error::Input => Error::WorkLimit,
            _ => Error::Invalid,
        })
}

/// Causal rows remain candidates even when the input native-row delta is exact.
pub(super) struct LinkedImpact {
    pub change: diff::Change,
    pub side: ChangeSide,
    pub rows: Vec<stylo_impact_walk::Row>,
    pub paths: Vec<Option<String>>,
    pub seed_host: diff::Key,
    pub source_available: bool,
    pub context_gapped: bool,
    pub missing_owner: bool,
    pub stop: stylo_impact_walk::Stop,
}
#[derive(Default)]
pub(super) struct WalkUsage {
    pub seed_walks: usize,
    pub input_rows: usize,
    pub input_bytes: usize,
    pub edges: usize,
    pub nodes: usize,
    pub rows: usize,
    pub witness_bytes: usize,
    pub path_bytes: usize,
    pub temporary_peak: usize,
}
pub(super) struct ChangeAnalysis {
    pub comparison: Outcome,
    pub impacts: Vec<LinkedImpact>,
    pub walk_limited: bool,
    pub usage: WalkUsage,
}
/// Work budgets shared across all changed-seed traversals in this request.
#[derive(Clone, Copy)]
pub(super) struct WalkBudget {
    pub seed_walks: usize,
    pub input_rows: usize,
    pub input_bytes: usize,
    pub edges: usize,
    pub nodes: usize,
    pub rows: usize,
    pub witness_bytes: usize,
    pub path_bytes: usize,
    pub depth: usize,
    pub temporary_bytes: usize,
}

/// Derive native-fact deltas from both retained roots and walk only changed
/// custom-property source occurrences. A partial diff or walk is explicit.
pub(super) fn compare_and_walk<F: FnMut() -> bool>(
    store: &mut LeaseStore,
    base: LeaseId,
    target: LeaseId,
    limits: Limits,
    mut budget: WalkBudget,
    offset: usize,
    page_rows: usize,
    now: Instant,
    mut cancelled: F,
) -> Result<ChangeAnalysis, Error> {
    let initial_budget = budget;
    let mut temporary_peak = 0usize;
    let comparison = compare(
        store,
        base,
        target,
        Domain::NativeFacts,
        limits,
        now,
        &mut cancelled,
    )?;
    if comparison.result.stop == diff::Stop::Cancelled {
        return Err(Error::Cancelled);
    }
    let mut impacts = Vec::new();
    let mut walk_limited = !matches!(
        comparison.result.stop,
        diff::Stop::Complete | diff::Stop::OutputRows
    );
    let mut walks = 0usize;
    'changes: for change in comparison
        .result
        .changes
        .iter()
        .skip(offset)
        .take(page_rows)
    {
        if cancelled() {
            return Err(Error::Cancelled);
        }
        let sides: &[ChangeSide] = match change.side {
            diff::Side::BaseOnly => &[ChangeSide::Base],
            diff::Side::TargetOnly => &[ChangeSide::Target],
            diff::Side::BothChanged => &[ChangeSide::Base, ChangeSide::Target],
        };
        for &side in sides {
            if walks >= budget.seed_walks {
                walk_limited = true;
                break 'changes;
            }
            match walk_changed(
                store,
                base,
                target,
                *change,
                side,
                &mut budget,
                now,
                &mut cancelled,
            ) {
                Ok(None) => {}
                Ok(Some((seed_host, source_available, walk))) => {
                    walks += 1;
                    temporary_peak = temporary_peak.max(walk.temporary_bytes);
                    budget.input_rows = budget.input_rows.saturating_sub(walk.input_rows);
                    budget.input_bytes = budget.input_bytes.saturating_sub(walk.input_bytes);
                    budget.edges = budget.edges.saturating_sub(walk.outcome.edges);
                    budget.nodes = budget.nodes.saturating_sub(walk.outcome.nodes);
                    budget.rows = budget.rows.saturating_sub(walk.outcome.rows.len());
                    budget.witness_bytes = budget
                        .witness_bytes
                        .saturating_sub(walk.outcome.witness_bytes);
                    let mut context_gapped = false;
                    let paths = walk
                        .outcome
                        .rows
                        .iter()
                        .map(|row| match walk.paths.get(&row.host) {
                            Some(path) if path.len() <= budget.path_bytes => {
                                budget.path_bytes -= path.len();
                                Some(path.clone())
                            }
                            _ => {
                                context_gapped = true;
                                None
                            }
                        })
                        .collect();
                    let stop = walk.outcome.stop;
                    if stop != stylo_impact_walk::Stop::Complete {
                        walk_limited = true;
                    }
                    impacts.push(LinkedImpact {
                        change: *change,
                        side,
                        rows: walk.outcome.rows,
                        paths,
                        seed_host,
                        source_available,
                        context_gapped,
                        missing_owner: walk.outcome.missing_owner,
                        stop,
                    });
                    if stop != stylo_impact_walk::Stop::Complete {
                        break 'changes;
                    }
                }
                Err(Error::WorkLimit) => {
                    walk_limited = true;
                    break 'changes;
                }
                Err(error) => return Err(error),
            }
        }
        if walk_limited
            && (walks >= budget.seed_walks
                || budget.input_rows == 0
                || budget.input_bytes == 0
                || budget.edges == 0
                || budget.nodes == 0
                || budget.rows == 0
                || budget.witness_bytes == 0)
        {
            break;
        }
    }
    Ok(ChangeAnalysis {
        comparison,
        impacts,
        walk_limited,
        usage: WalkUsage {
            seed_walks: walks,
            input_rows: initial_budget.input_rows - budget.input_rows,
            input_bytes: initial_budget.input_bytes - budget.input_bytes,
            edges: initial_budget.edges - budget.edges,
            nodes: initial_budget.nodes - budget.nodes,
            rows: initial_budget.rows - budget.rows,
            witness_bytes: initial_budget.witness_bytes - budget.witness_bytes,
            path_bytes: initial_budget.path_bytes - budget.path_bytes,
            temporary_peak,
        },
    })
}
