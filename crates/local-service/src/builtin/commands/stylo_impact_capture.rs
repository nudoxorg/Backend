//! Capture bounded comparison material from the current owner-selected Stylo
//! root. Historical bytes are captured now and never reopened on later reads.
use super::super::{BuiltinModelError, BuiltinWorkspaceRelation};
use super::impact_retention::{Basis, Capture, Coverage, Freshness, Lease, Material, Worker};
use backend_engine::{
    WorkspaceSnapshot,
    builtin::{
        StyloAnalysisRecord, StyloAnalysisRelation, stylo_attempt_key, stylo_rows_digest,
        stylo_selection_key,
    },
};
use backend_version::Relation;
use std::collections::BTreeSet;

/// Independent scan and retention work bounds for the capture operation.
pub(super) struct CaptureLimits {
    pub rows: usize,
    pub scan_bytes: usize,
    pub retained_bytes: usize,
    pub source_bytes: usize,
    /// Conservative cap for temporary descriptors and indexes.
    pub temporary_bytes: usize,
}
struct Budget {
    rows: usize,
    scan: usize,
    retained: usize,
    source: usize,
    temporary: usize,
    limits: CaptureLimits,
}
impl Budget {
    fn add_record(
        &mut self,
        key: [u8; 32],
        row: &StyloAnalysisRecord,
        records: &mut Vec<Material>,
    ) -> bool {
        let mut encoded = Vec::new();
        StyloAnalysisRelation::encode_value(row, &mut encoded);
        let cost = encoded
            .len()
            .saturating_add(std::mem::size_of::<Material>());
        let Some(next) = self.retained.checked_add(cost) else {
            return false;
        };
        if next > self.limits.retained_bytes {
            return false;
        }
        self.retained = next;
        records.push(Material {
            key,
            bytes: encoded.into_boxed_slice(),
        });
        true
    }
}
fn error(message: &str) -> BuiltinModelError {
    BuiltinModelError(format!("CSS impact capture: {message}"))
}

/// Captures only the given selected project and exact native relation root.
/// The input budget is bounded independently from retained data and source
/// reads. Stopped or incomplete inventory always produces gapped coverage.
/// Retention accounting excludes temporary capture vectors and indexes;
/// a separate conservative descriptor/index budget limits them. Allocator
/// bookkeeping and total process RSS are outside this byte accounting.
/// This is an owner-internal step, not an arbitrary-root read API.
pub(super) fn capture<F: FnMut() -> bool>(
    snapshot: &WorkspaceSnapshot,
    project: [u8; 32],
    limits: CaptureLimits,
    mut cancelled: F,
) -> Result<Capture, BuiltinModelError> {
    if limits.rows == 0
        || limits.rows > 262_144
        || limits.scan_bytes == 0
        || limits.scan_bytes > 64 * 1024 * 1024
        || limits.retained_bytes == 0
        || limits.retained_bytes > 64 * 1024 * 1024
        || limits.source_bytes > limits.retained_bytes
        || limits.temporary_bytes < 131_072
        || limits.temporary_bytes > 67_108_864
    {
        return Err(error("invalid capture limits"));
    }
    if cancelled() {
        return Err(error("cancelled"));
    }
    let workspace = snapshot
        .relation::<BuiltinWorkspaceRelation>()
        .map_err(|_| error("workspace relation unavailable"))?;
    if !workspace
        .lookup(&project)
        .map_err(|_| error("project lookup failed"))?
        .as_ref()
        .and_then(|r| r.project_fields())
        .is_some()
    {
        return Err(error("project not selected"));
    }
    let relation = super::stylo_query::selected_native_relation(snapshot)?
        .ok_or_else(|| error("native root not selected"))?;
    let selection_key = stylo_selection_key(project);
    let selection = relation
        .lookup(&selection_key)
        .map_err(|_| error("selection lookup failed"))?
        .ok_or_else(|| error("selection unavailable"))?;
    let StyloAnalysisRecord::Selection {
        project: owner,
        source_root,
        producer,
        ..
    } = &selection
    else {
        return Err(error("invalid selection"));
    };
    if *owner != project {
        return Err(error("selection owner mismatch"));
    }
    let scope = stylo_rows_digest(&[(selection_key, &selection)])
        .map_err(|_| error("selection digest failed"))?;
    let mut basis = Basis {
        manifest: *snapshot.root().as_bytes(),
        native: *relation.root().as_bytes(),
        project,
        scope,
        recipe: *producer,
        freshness: Freshness::Unknown,
        worker: Worker::Unknown,
    };
    let mut budget = Budget {
        rows: 0,
        scan: 0,
        retained: std::mem::size_of::<Lease>(),
        source: 0,
        temporary: 131_072,
        limits,
    };
    let mut records = Vec::new();
    let mut selection_encoded = Vec::new();
    StyloAnalysisRelation::encode_value(&selection, &mut selection_encoded);
    if selection_encoded.len() > budget.limits.scan_bytes {
        return Err(error("selection exceeds scan limit"));
    }
    budget.rows = 1;
    budget.scan = selection_encoded.len();
    budget.temporary = budget
        .temporary
        .saturating_add(1024)
        .saturating_add(selection_encoded.len().saturating_mul(2));
    if budget.temporary > budget.limits.temporary_bytes {
        return Err(error("temporary budget exhausted"));
    }
    if !budget.add_record(selection_key, &selection, &mut records) {
        return Err(error("selection exceeds retention limit"));
    }
    // The attempt row is a separate selected-root fact. Classify it only if
    // its lookup and canonical retained bytes fit every capture budget; a
    // missing or unretained row remains explicitly Unknown.
    if budget.rows < budget.limits.rows
        && budget.scan.saturating_add(32 * 1024) <= budget.limits.scan_bytes
        && budget.temporary.saturating_add(66_560) <= budget.limits.temporary_bytes
    {
        if cancelled() {
            return Err(error("cancelled"));
        }
        budget.rows += 1;
        let attempt_key = stylo_attempt_key(project);
        let attempt = relation
            .lookup(&attempt_key)
            .map_err(|_| error("attempt lookup failed"))?;
        if let Some(attempt) = attempt {
            let mut encoded = Vec::new();
            StyloAnalysisRelation::encode_value(&attempt, &mut encoded);
            budget.scan = budget.scan.saturating_add(encoded.len());
            budget.temporary = budget
                .temporary
                .saturating_add(1024)
                .saturating_add(encoded.len().saturating_mul(2));
            if budget.temporary <= budget.limits.temporary_bytes
                && budget.add_record(attempt_key, &attempt, &mut records)
            {
                let (state, worker) =
                    super::stylo_query::classify_attempt(project, Some(&selection), Some(attempt))?;
                (basis.freshness, basis.worker) =
                    super::impact_freshness::from_query(state, worker);
            }
        } else {
            // Absence is observed at the exact selected native root, not
            // inferred from a truncated relation scan.
            let (state, worker) =
                super::stylo_query::classify_attempt(project, Some(&selection), None)?;
            (basis.freshness, basis.worker) = super::impact_freshness::from_query(state, worker);
        }
    }
    let mut hosts = BTreeSet::new();
    let mut source_hosts = Vec::new();
    let mut pass_complete = [false; 2];
    // Each pass is charged against the same scan budget, including irrelevant rows.
    'passes: for pass in 0..2 {
        let mut after = None;
        loop {
            if cancelled() {
                return Err(error("cancelled"));
            }
            if budget.rows >= budget.limits.rows
                || budget.scan.saturating_add(32 * 1024) > budget.limits.scan_bytes
            {
                break;
            }
            let page = relation
                .page(after.as_ref(), 1)
                .map_err(|_| error("native scan failed"))?;
            budget.rows += 1;
            let Some((key, row)) = page.entries().first() else {
                pass_complete[pass] = true;
                break;
            };
            let mut encoded = Vec::new();
            StyloAnalysisRelation::encode_value(row, &mut encoded);
            budget.scan = budget.scan.saturating_add(encoded.len());
            budget.temporary = budget
                .temporary
                .saturating_add(1024)
                .saturating_add(encoded.len().saturating_mul(2));
            if budget.temporary > budget.limits.temporary_bytes {
                break 'passes;
            }
            let include = match row {
                StyloAnalysisRecord::Host {
                    project: owner,
                    path,
                    content: Some(content),
                    source: Some(source),
                    byte_length: Some(length),
                    terminal: backend_engine::builtin::StyloTerminal::Parsed(_),
                    ..
                } if pass == 0 && *owner == project => {
                    hosts.insert(*key);
                    source_hosts.push((*key, path.clone(), *content, *source, *length));
                    true
                }
                StyloAnalysisRecord::Host { project: owner, .. }
                    if pass == 0 && *owner == project =>
                {
                    hosts.insert(*key);
                    true
                }
                StyloAnalysisRecord::Fact { host, .. } if pass == 1 && hosts.contains(host) => true,
                _ => false,
            };
            if include && !budget.add_record(*key, row, &mut records) {
                if pass == 0 {
                    hosts.remove(key);
                    source_hosts.retain(|(host, ..)| host != key);
                }
                break 'passes;
            }
            after = Some(*key);
        }
    }
    let mut sources = Vec::new();
    for (host, path, content, source, length) in source_hosts {
        if cancelled() {
            return Err(error("cancelled"));
        }
        let Ok(length) = usize::try_from(length) else {
            continue;
        };
        let attempted = length.saturating_add(1);
        if length > 4 * 1024 * 1024
            || budget.source.saturating_add(attempted) > budget.limits.source_bytes
            || budget
                .retained
                .saturating_add(length)
                .saturating_add(std::mem::size_of::<Material>())
                > budget.limits.retained_bytes
        {
            continue;
        }
        // Charge every attempt, including vanished, changed or oversized reads.
        budget.source += attempted;
        let Ok(bytes) = super::super::ingest::read_stylo_selected_source(
            std::path::Path::new(source_root),
            &path,
            length,
        ) else {
            continue;
        };
        if bytes.len() != length {
            continue;
        }
        let actual =
            backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(
                &bytes,
            );
        if actual.as_ref() != &source
            || backend_compile::VerifiedSourceText::admit_stored(&bytes, content, actual).is_err()
        {
            continue;
        }
        budget.retained += bytes.len() + std::mem::size_of::<Material>();
        sources.push(Material {
            key: host,
            bytes: bytes.into_boxed_slice(),
        });
    }
    if cancelled() {
        return Err(error("cancelled"));
    }
    // Completeness here covers enumeration of retained rows and captured host
    // images only. It never upgrades native semantic, DOM or source-site scope.
    let relation_coverage = if pass_complete.iter().all(|complete| *complete) {
        Coverage::Complete
    } else {
        Coverage::Gapped
    };
    let source_coverage = if pass_complete[0] && sources.len() == hosts.len() {
        Coverage::Complete
    } else {
        Coverage::Gapped
    };
    Ok(Capture {
        basis,
        relation_coverage,
        source_coverage,
        records,
        sources,
    })
}
