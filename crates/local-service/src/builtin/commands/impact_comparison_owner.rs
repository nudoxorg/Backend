//! Native owner implementation of ephemeral comparison operands. No historical
//! filesystem reads are permitted; all source text comes from captured bytes.
use super::impact_retention::{Basis, Coverage, Lease, LeaseId, LeaseStore};
use super::stylo_impact_capture::{self, CaptureLimits};
use backend_engine::{
    WorkspaceSnapshot,
    builtin::{StyloAnalysisRecord, StyloAnalysisRelation, StyloProjectedRow},
};
use backend_library::impact_comparison::*;
use backend_semantic::stylo_analysis::StyloSourceRole;
use backend_version::CanonicalRelation;
use std::time::Instant;

#[derive(Debug)]
pub(super) enum Error {
    Invalid,
    Unavailable,
    Capacity,
    Cancelled,
    Expired,
    Deadline,
}
fn lease_id(claims: &ImpactLeaseClaims) -> LeaseId {
    LeaseId::from_parts(claims.epoch, claims.sequence)
}
fn same_basis(basis: Basis, claims: &ImpactLeaseClaims) -> bool {
    basis.manifest == claims.manifest
        && basis.native == claims.native
        && basis.project == claims.project
        && basis.scope == claims.scope
        && basis.recipe == claims.producer
        && super::impact_freshness::to_query(basis.freshness, basis.worker).is_some_and(
            |(attempt, worker)| {
                attempt == claims.attempt_at_capture && worker == claims.worker_at_capture
            },
        )
}
fn site(
    lease: &Lease,
    host: [u8; 32],
    seed: [u8; 32],
) -> Result<backend_semantic::stylo_analysis::StyloSourceOccurrence, Error> {
    let item = lease
        .records
        .binary_search_by_key(&seed, |row| row.key)
        .ok()
        .map(|index| &lease.records[index])
        .ok_or(Error::Unavailable)?;
    match StyloAnalysisRelation::decode_value(&item.bytes).map_err(|_| Error::Invalid)? {
        StyloAnalysisRecord::Fact {
            host: owner,
            fact: StyloProjectedRow::SourceOccurrence(site),
            ..
        } if owner == host && site.role == StyloSourceRole::CustomPropertyDeclarationName => {
            Ok(site)
        }
        _ => Err(Error::Invalid),
    }
}
fn verified_lease<'a>(
    store: &'a mut LeaseStore,
    token: &ImpactLease,
    secret: &[u8; 32],
    now: Instant,
) -> Result<&'a Lease, Error> {
    let claims = token.verify(secret).map_err(|_| Error::Invalid)?;
    let lease = store
        .get(lease_id(claims), now)
        .map_err(|_| Error::Expired)?;
    if !same_basis(lease.basis, claims)
        || claims.native_rows_complete != (lease.relation_coverage == Coverage::Complete)
        || claims.native_host_sources_complete != (lease.source_coverage == Coverage::Complete)
    {
        return Err(Error::Invalid);
    }
    match (claims.host, claims.seed) {
        (Some(host), Some(seed)) => {
            site(lease, host, seed)?;
        }
        (None, None) => {}
        _ => return Err(Error::Invalid),
    }
    Ok(lease)
}
fn alive(store: &mut LeaseStore, claims: &ImpactLeaseClaims, now: Instant) -> Result<(), Error> {
    let lease = store
        .get(lease_id(claims), now)
        .map_err(|_| Error::Expired)?;
    if !same_basis(lease.basis, claims)
        || claims.native_rows_complete != (lease.relation_coverage == Coverage::Complete)
        || claims.native_host_sources_complete != (lease.source_coverage == Coverage::Complete)
    {
        return Err(Error::Invalid);
    }
    Ok(())
}
/// Capture the currently selected owner snapshot and seal its exact seed.
pub(super) fn capture<F: FnMut() -> bool>(
    store: &mut LeaseStore,
    snapshot: &WorkspaceSnapshot,
    secret: &[u8; 32],
    request: &ImpactCaptureRequest,
    now: Instant,
    mut cancelled: F,
) -> Result<ImpactCaptureReply, Error> {
    request.admit().map_err(|_| Error::Invalid)?;
    let seed = request
        .seed
        .as_ref()
        .map(|seed| seed.verify(secret))
        .transpose()
        .map_err(|_| Error::Invalid)?;
    let project = backend_library::package_key(request.package.as_str()).to_bytes();
    if seed.as_ref().is_some_and(|seed| {
        seed.project != project || seed.manifest_root != *snapshot.root().as_bytes()
    }) {
        return Err(Error::Expired);
    }
    let deadline = now
        .checked_add(std::time::Duration::from_millis(u64::from(request.time_ms)))
        .ok_or(Error::Invalid)?;
    let mut timed_out = false;
    let mut interrupted = || {
        if cancelled() {
            return true;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            return true;
        }
        false
    };
    let result = stylo_impact_capture::capture(
        snapshot,
        project,
        CaptureLimits {
            rows: request.scan_rows as usize,
            scan_bytes: request.scan_bytes as usize,
            retained_bytes: request.retain_bytes as usize,
            source_bytes: request.source_bytes as usize,
            temporary_bytes: request.temporary_bytes as usize,
        },
        &mut interrupted,
    );
    drop(interrupted);
    let material = match result {
        Ok(material) if !timed_out => material,
        _ if timed_out => return Err(Error::Deadline),
        _ if cancelled() => return Err(Error::Cancelled),
        _ => return Err(Error::Unavailable),
    };
    if let Some(seed) = &seed {
        if material.basis.native != seed.native_root
            || material.basis.manifest != seed.manifest_root
        {
            return Err(Error::Expired);
        }
        // A requested seed missing from partial capture is unavailable, never known empty.
        let item = material
            .records
            .iter()
            .find(|row| row.key == seed.site)
            .ok_or(Error::Unavailable)?;
        match StyloAnalysisRelation::decode_value(&item.bytes).map_err(|_| Error::Invalid)? {
            StyloAnalysisRecord::Fact {
                host,
                fact: StyloProjectedRow::SourceOccurrence(ref site),
                ..
            } if host == seed.host
                && site.role == StyloSourceRole::CustomPropertyDeclarationName => {}
            _ => return Err(Error::Invalid),
        }
    }
    let basis = material.basis;
    let native_rows_complete = material.relation_coverage == Coverage::Complete;
    let native_host_sources_complete = material.source_coverage == Coverage::Complete;
    // Row/image enumeration is distinct from incomplete CSS semantics.
    let relation_gapped = true;
    let source_gapped = true;
    let has_source = seed.as_ref().is_some_and(|seed| {
        material
            .sources
            .iter()
            .any(|source| source.key == seed.host)
    });
    if Instant::now() >= deadline {
        return Err(Error::Deadline);
    }
    let id = store
        .insert(material, now, &mut || {
            cancelled() || Instant::now() >= deadline
        })
        .map_err(|error| match error {
            super::impact_retention::Error::Cancelled if Instant::now() >= deadline => {
                Error::Deadline
            }
            super::impact_retention::Error::Cancelled => Error::Cancelled,
            _ => Error::Capacity,
        })?;
    let (epoch, sequence) = id.parts();
    let (attempt_at_capture, worker_at_capture) =
        super::impact_freshness::to_query(basis.freshness, basis.worker).ok_or(Error::Invalid)?;
    let claims = ImpactLeaseClaims {
        epoch,
        sequence,
        manifest: basis.manifest,
        native: basis.native,
        project: basis.project,
        scope: basis.scope,
        producer: basis.recipe,
        seed: seed.as_ref().map(|seed| seed.site),
        host: seed.as_ref().map(|seed| seed.host),
        attempt_at_capture: attempt_at_capture.clone(),
        worker_at_capture,
        native_rows_complete,
        native_host_sources_complete,
    };
    let built = (|| {
        let lease = ImpactLease::issue(claims, secret).map_err(|_| Error::Invalid)?;
        let source = match (has_source, &seed) {
            (true, Some(seed)) => Some(
                ImpactSource::issue(
                    ImpactSourceClaims {
                        lease: lease.clone(),
                        host: seed.host,
                        site: seed.site,
                    },
                    secret,
                )
                .map_err(|_| Error::Invalid)?,
            ),
            _ => None,
        };
        Ok(ImpactCaptureReply {
            seed: request.seed.clone(),
            lease,
            source,
            relation_gapped,
            source_gapped,
            attempt_at_capture: attempt_at_capture.clone(),
            worker_at_capture,
        })
    })();
    if Instant::now() >= deadline {
        store.release(id);
        return Err(Error::Deadline);
    }
    if built.is_err() {
        store.release(id);
    }
    built
}
/// Reads only the copied bytes for the sealed historical source; missing bytes
/// stay unavailable even if a file with the same name exists today.
pub(super) fn read_source(
    store: &mut LeaseStore,
    secret: &[u8; 32],
    request: &ImpactReadSourceRequest,
    now: Instant,
) -> Result<ImpactSourceReply, Error> {
    let claims = request.source.verify(secret).map_err(|_| Error::Invalid)?;
    let lease = match verified_lease(store, &claims.lease, secret, now) {
        Ok(lease) => lease,
        Err(Error::Expired) => return Ok(ImpactSourceReply::Expired),
        Err(error) => return Err(error),
    };
    let site = site(lease, claims.host, claims.site)?;
    let host = lease
        .records
        .binary_search_by_key(&claims.host, |row| row.key)
        .ok()
        .map(|index| &lease.records[index])
        .ok_or(Error::Unavailable)?;
    let path = match StyloAnalysisRelation::decode_value(&host.bytes).map_err(|_| Error::Invalid)? {
        StyloAnalysisRecord::Host { project, path, .. }
            if project == claims.lease.claims().project =>
        {
            path
        }
        _ => return Err(Error::Invalid),
    };
    let Some(bytes) = lease.historical_source(&claims.host) else {
        return Ok(ImpactSourceReply::Unavailable);
    };
    let Ok(start) = usize::try_from(site.start) else {
        return Err(Error::Invalid);
    };
    let Ok(end) = usize::try_from(site.end) else {
        return Err(Error::Invalid);
    };
    let text = std::str::from_utf8(bytes)
        .map_err(|_| Error::Invalid)?
        .get(start..end)
        .ok_or(Error::Invalid)?;
    if text != site.raw {
        return Err(Error::Invalid);
    }
    let _ = path;
    Ok(ImpactSourceReply::Available {
        text: text.to_owned(),
        start: site.start,
        end: site.end,
    })
}

/// Recompute one deterministic page against both retained exact roots.
/// An authenticated offset binds the complete request; a computation limit
/// yields no continuation unless the page boundary is actually resumable.
pub(super) fn compare<F: FnMut() -> bool>(
    store: &mut LeaseStore,
    secret: &[u8; 32],
    request: &ImpactCompareRequest,
    now: Instant,
    mut cancelled: F,
) -> Result<ImpactCompareReply, Error> {
    use super::{
        impact_retention::ChangeSide,
        stylo_impact_adapter::{ChangeError, walk_change},
        stylo_impact_walk::{Limits, Stop},
    };
    request.limits.admit().map_err(|_| Error::Invalid)?;
    let base = request
        .base
        .verify(secret)
        .map_err(|_| Error::Invalid)?
        .clone();
    let target = request
        .target
        .verify(secret)
        .map_err(|_| Error::Invalid)?
        .clone();
    if base.project != target.project {
        return Err(Error::Invalid);
    }
    let offset = if let Some(cursor) = &request.continuation {
        let claims = cursor.verify(secret).map_err(|_| Error::Invalid)?;
        if claims.recipe != request.recipe().map_err(|_| Error::Invalid)?
            || claims.offset > request.limits.nodes
        {
            return Err(Error::Invalid);
        }
        claims.offset as usize
    } else {
        0
    };
    let empty = |terminal| ImpactCompareReply {
        base: request.base.clone(),
        target: request.target.clone(),
        side: request.side,
        rows: vec![],
        relation_gapped: true,
        source_gapped: true,
        base_attempt_at_capture: base.attempt_at_capture.clone(),
        target_attempt_at_capture: target.attempt_at_capture.clone(),
        base_worker_at_capture: base.worker_at_capture,
        target_worker_at_capture: target.worker_at_capture,
        provider_unknown: true,
        missing_owner: false,
        counters: ImpactCounters {
            input_rows: None,
            input_bytes: None,
            temporary_bytes: None,
            edges: None,
            nodes: None,
            witness_bytes: None,
        },
        terminal,
    };
    // A cancelled request still reports missing/expired retained material;
    // cancellation cannot conceal an owner restart or expired lease.
    for claims in [&base, &target] {
        match alive(store, claims, now) {
            Ok(()) => {}
            Err(Error::Expired) => return Ok(empty(ImpactTerminal::Expired)),
            Err(error) => return Err(error),
        }
    }
    if request.cancel || cancelled() {
        return Ok(empty(ImpactTerminal::Cancelled));
    }
    // Preflight seed reads are included in the input-row/byte budget before
    // decoding; expiration rechecks below do not decode or reread rows.
    let mut preflight_bytes = 0usize;
    let mut preflight_rows = 0usize;
    for claims in [&base, &target] {
        let lease = match store.get(lease_id(claims), now) {
            Ok(lease) => lease,
            Err(_) => return Ok(empty(ImpactTerminal::Expired)),
        };
        if !same_basis(lease.basis, claims) {
            return Err(Error::Invalid);
        }
        match (claims.host, claims.seed) {
            (None, None) => {}
            (Some(host), Some(seed)) => {
                preflight_rows += 1;
                let item = lease
                    .records
                    .binary_search_by_key(&seed, |row| row.key)
                    .ok()
                    .map(|index| &lease.records[index])
                    .ok_or(Error::Unavailable)?;
                preflight_bytes = preflight_bytes
                    .checked_add(item.bytes.len())
                    .ok_or(Error::Invalid)?;
                if preflight_bytes > request.limits.input_bytes as usize {
                    return Ok(empty(ImpactTerminal::WorkLimit {
                        reason: "input limit".into(),
                    }));
                }
                site(lease, host, seed)?;
            }
            _ => return Err(Error::Invalid),
        }
    }
    if request.limits.input_rows as usize <= preflight_rows {
        return Ok(empty(ImpactTerminal::WorkLimit {
            reason: "input limit".into(),
        }));
    }
    let side = match request.side {
        ImpactSide::Base => ChangeSide::Base,
        ImpactSide::Target => ChangeSide::Target,
    };
    let seed = match request.side {
        ImpactSide::Base => base.seed,
        ImpactSide::Target => target.seed,
    };
    let Some(seed) = seed else {
        return Ok(empty(ImpactTerminal::SeedUnavailable));
    };
    let cap = offset
        .checked_add(request.limits.page_rows as usize)
        .and_then(|n| n.checked_add(1))
        .ok_or(Error::Invalid)?;
    let limits = Limits {
        input_rows: request.limits.input_rows as usize - preflight_rows,
        input_bytes: request.limits.input_bytes as usize - preflight_bytes,
        edges: request.limits.edges as usize,
        nodes: request.limits.nodes as usize,
        depth: request.limits.depth as usize,
        rows: cap,
        witness_bytes: request.limits.witness_bytes as usize,
        temporary_bytes: request.limits.temporary_bytes as usize,
    };
    let deadline = now
        .checked_add(std::time::Duration::from_millis(u64::from(
            request.limits.time_ms,
        )))
        .ok_or(Error::Invalid)?;
    let mut timed_out = false;
    let mut interrupted = || {
        if cancelled() {
            return true;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            return true;
        }
        false
    };
    let walked = walk_change(
        store,
        lease_id(&base),
        lease_id(&target),
        side,
        seed,
        limits,
        now,
        &mut interrupted,
    );
    drop(interrupted);
    let outcome = match walked {
        Ok(value) => value,
        Err(ChangeError::Retention(super::impact_retention::Error::Expired)) => {
            return Ok(empty(ImpactTerminal::Expired));
        }
        Err(ChangeError::Walk(super::stylo_impact_adapter::Error::Input)) => {
            return Ok(empty(ImpactTerminal::WorkLimit {
                reason: "input limit".into(),
            }));
        }
        Err(ChangeError::Walk(super::stylo_impact_adapter::Error::Cancelled)) if timed_out => {
            return Ok(empty(ImpactTerminal::WorkLimit {
                reason: "deadline".into(),
            }));
        }
        Err(ChangeError::Walk(super::stylo_impact_adapter::Error::Cancelled)) => {
            return Ok(empty(ImpactTerminal::Cancelled));
        }
        Err(_) => return Err(Error::Invalid),
    };
    if timed_out {
        return Ok(empty(ImpactTerminal::WorkLimit {
            reason: "deadline".into(),
        }));
    }
    // A lease that expired during work cannot authorize a later result.
    for claims in [&base, &target] {
        match alive(store, claims, Instant::now()) {
            Ok(()) => {}
            Err(Error::Expired) => return Ok(empty(ImpactTerminal::Expired)),
            Err(error) => return Err(error),
        }
    }
    if !same_basis(outcome.base, &base) || !same_basis(outcome.target, &target) {
        return Err(Error::Invalid);
    }
    let result = outcome.result;
    if result.stop == Stop::Invalid {
        return Err(Error::Invalid);
    }
    if offset > result.rows.len() {
        return Err(Error::Invalid);
    }
    let counters = ImpactCounters {
        input_rows: Some((outcome.input_rows + preflight_rows) as u32),
        input_bytes: Some((outcome.input_bytes + preflight_bytes) as u32),
        temporary_bytes: Some(outcome.temporary_bytes as u32),
        edges: Some(result.edges as u32),
        nodes: Some(result.nodes as u32),
        witness_bytes: Some(result.witness_bytes as u32),
    };
    let mut rows = Vec::new();
    let mut used = 16_384usize;
    for row in result
        .rows
        .iter()
        .skip(offset)
        .take(request.limits.page_rows as usize)
    {
        if Instant::now() >= deadline {
            timed_out = true;
            break;
        }
        if cancelled() {
            return Ok(empty(ImpactTerminal::Cancelled));
        }
        let candidate = ImpactCandidate {
            strength: ImpactStrength::Candidate,
            dependency: row.dependency,
            path: outcome.paths.get(&row.host).cloned(),
            owner: row.owner,
            witness: row.witness.to_vec(),
        };
        let size = serde_json::to_vec(&candidate)
            .map_err(|_| Error::Invalid)?
            .len()
            .saturating_add(2);
        if used.saturating_add(size) > request.limits.output_bytes as usize {
            break;
        }
        used += size;
        rows.push(candidate);
    }
    for claims in [&base, &target] {
        match alive(store, claims, Instant::now()) {
            Ok(()) => {}
            Err(Error::Expired) => return Ok(empty(ImpactTerminal::Expired)),
            Err(error) => return Err(error),
        }
    }
    timed_out |= Instant::now() >= deadline;
    let next = offset + rows.len();
    let has_more = next < result.rows.len() || result.stop == Stop::Rows;
    let terminal = if timed_out {
        ImpactTerminal::WorkLimit {
            reason: "deadline".into(),
        }
    } else if result.stop == Stop::Cancelled || cancelled() {
        ImpactTerminal::Cancelled
    } else if has_more && !rows.is_empty() {
        ImpactTerminal::More {
            continuation: ImpactCursor::issue(
                ImpactCursorClaims {
                    recipe: request.recipe().map_err(|_| Error::Invalid)?,
                    offset: u32::try_from(next).map_err(|_| Error::Invalid)?,
                },
                secret,
            )
            .map_err(|_| Error::Invalid)?,
        }
    } else if has_more {
        ImpactTerminal::WorkLimit {
            reason: "output budget cannot fit one result".into(),
        }
    } else if result.stop == Stop::Complete {
        ImpactTerminal::Complete
    } else {
        ImpactTerminal::WorkLimit {
            reason: format!("{:?}", result.stop),
        }
    };
    Ok(ImpactCompareReply {
        base: request.base.clone(),
        target: request.target.clone(),
        side: request.side,
        rows,
        relation_gapped: true,
        source_gapped: true,
        base_attempt_at_capture: base.attempt_at_capture.clone(),
        target_attempt_at_capture: target.attempt_at_capture.clone(),
        base_worker_at_capture: base.worker_at_capture,
        target_worker_at_capture: target.worker_at_capture,
        provider_unknown: true,
        missing_owner: result.missing_owner,
        counters,
        terminal,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signed_historical_handle_cannot_invent_an_uncaptured_site() {
        use super::super::impact_retention::{Capture, Freshness, Worker};
        let now = Instant::now();
        let secret = [9; 32];
        let mut store = LeaseStore::new([1; 16]);
        let basis = Basis {
            manifest: [2; 32],
            native: [3; 32],
            project: [4; 32],
            scope: [5; 32],
            recipe: [6; 32],
            freshness: Freshness::Unknown,
            worker: Worker::Unknown,
        };
        let id = store
            .insert(
                Capture {
                    basis,
                    relation_coverage: Coverage::Gapped,
                    source_coverage: Coverage::Gapped,
                    records: vec![],
                    sources: vec![],
                },
                now,
                || false,
            )
            .unwrap();
        let (epoch, sequence) = id.parts();
        let lease = ImpactLease::issue(
            ImpactLeaseClaims {
                epoch,
                sequence,
                manifest: basis.manifest,
                native: basis.native,
                project: basis.project,
                scope: basis.scope,
                producer: basis.recipe,
                seed: None,
                host: None,
                attempt_at_capture: ImpactAttemptAtCapture::Unknown,
                worker_at_capture: backend_library::stylo_query::StyloWorkerCapability::Unknown,
                native_rows_complete: false,
                native_host_sources_complete: false,
            },
            &secret,
        )
        .unwrap();
        let source = ImpactSource::issue(
            ImpactSourceClaims {
                lease,
                host: [7; 32],
                site: [8; 32],
            },
            &secret,
        )
        .unwrap();
        assert!(matches!(
            read_source(
                &mut store,
                &secret,
                &ImpactReadSourceRequest { source },
                now
            ),
            Err(Error::Unavailable)
        ));
    }
    #[test]
    fn cancelled_request_reports_missing_retained_leases_as_expired() {
        let secret = [9; 32];
        let lease = ImpactLease::issue(
            ImpactLeaseClaims {
                epoch: [1; 16],
                sequence: 1,
                manifest: [2; 32],
                native: [3; 32],
                project: [4; 32],
                scope: [5; 32],
                producer: [6; 32],
                seed: Some([7; 32]),
                host: Some([8; 32]),
                attempt_at_capture:
                    backend_library::impact_comparison::ImpactAttemptAtCapture::Unknown,
                worker_at_capture: backend_library::stylo_query::StyloWorkerCapability::Unknown,
                native_rows_complete: false,
                native_host_sources_complete: false,
            },
            &secret,
        )
        .unwrap();
        let request = ImpactCompareRequest {
            base: lease.clone(),
            target: lease,
            side: ImpactSide::Base,
            limits: ImpactLimits {
                input_rows: 10,
                input_bytes: 131_072,
                edges: 10,
                nodes: 10,
                depth: 2,
                page_rows: 1,
                output_bytes: 131_072,
                witness_bytes: 1024,
                temporary_bytes: 131_072,
                time_ms: 1000,
            },
            continuation: None,
            cancel: true,
        };
        let mut store = LeaseStore::new([1; 16]);
        let result = compare(&mut store, &secret, &request, Instant::now(), || false).unwrap();
        assert_eq!(result.terminal, ImpactTerminal::Expired);
    }
}

/// Diff retained exact-key material and, for native authored custom-property
/// changes, attach only candidate paths witnessed on the corresponding side.
pub(super) fn diff<F: FnMut() -> bool>(
    store: &mut LeaseStore,
    secret: &[u8; 32],
    request: &ImpactDiffRequest,
    now: Instant,
    mut cancelled: F,
) -> Result<ImpactDiffReply, Error> {
    use super::{
        impact_diff::{self, Certainty, Side},
        impact_retention::ChangeSide,
        stylo_impact_diff::{self, Domain, WalkBudget},
    };
    request.limits.admit().map_err(|_| Error::Invalid)?;
    let base = request
        .base
        .verify(secret)
        .map_err(|_| Error::Invalid)?
        .clone();
    let target = request
        .target
        .verify(secret)
        .map_err(|_| Error::Invalid)?
        .clone();
    if base.project != target.project {
        return Err(Error::Invalid);
    }
    let offset = if let Some(cursor) = &request.continuation {
        let claims = cursor.verify(secret).map_err(|_| Error::Invalid)?;
        if claims.recipe != request.recipe().map_err(|_| Error::Invalid)?
            || claims.offset > request.limits.comparisons
        {
            return Err(Error::Invalid);
        }
        claims.offset as usize
    } else {
        0
    };
    let claimed_complete = |claims: &ImpactLeaseClaims| match request.domain {
        ImpactDiffDomain::NativeFacts => claims.native_rows_complete,
        ImpactDiffDomain::VerifiedSourceImages => claims.native_host_sources_complete,
    };
    let empty = |terminal| ImpactDiffReply {
        base: request.base.clone(),
        target: request.target.clone(),
        domain: request.domain,
        base_capture_complete: claimed_complete(&base),
        target_capture_complete: claimed_complete(&target),
        semantic_gapped: true,
        provider_unknown: true,
        walk_limited: true,
        context_gapped: true,
        missing_owner: false,
        rows: vec![],
        comparisons: None,
        compared_bytes: None,
        counters: None,
        terminal,
    };
    for claims in [&base, &target] {
        match alive(store, claims, now) {
            Ok(()) => {}
            Err(Error::Expired) => return Ok(empty(ImpactDiffTerminal::Expired)),
            Err(error) => return Err(error),
        }
    }
    if request.cancel || cancelled() {
        return Ok(empty(ImpactDiffTerminal::Cancelled));
    }
    let count = offset
        .checked_add(request.limits.page_rows as usize)
        .and_then(|n| n.checked_add(1))
        .ok_or(Error::Invalid)?;
    let logical_bytes = count
        .checked_mul(std::mem::size_of::<impact_diff::Change>())
        .ok_or(Error::Invalid)?;
    let limits = stylo_impact_diff::Limits {
        scan_rows: request.limits.scan_rows as usize,
        scan_bytes: request.limits.scan_bytes as usize,
        temporary_bytes: request.limits.temporary_bytes as usize,
        diff: impact_diff::Limits {
            input_rows: request.limits.scan_rows as usize,
            compared_bytes: request.limits.compared_bytes as usize,
            comparisons: request.limits.comparisons as usize,
            output_rows: count,
            output_bytes: logical_bytes,
        },
    };
    let deadline = now
        .checked_add(std::time::Duration::from_millis(u64::from(
            request.limits.time_ms,
        )))
        .ok_or(Error::Invalid)?;
    let mut timed_out = false;
    let mut interrupted = || {
        if cancelled() {
            return true;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            return true;
        }
        false
    };
    let walk_budget = WalkBudget {
        seed_walks: request.limits.seed_walks as usize,
        input_rows: request.limits.walk_input_rows as usize,
        input_bytes: request.limits.walk_input_bytes as usize,
        edges: request.limits.walk_edges as usize,
        nodes: request.limits.walk_nodes as usize,
        depth: request.limits.walk_depth as usize,
        rows: request.limits.walk_rows as usize,
        witness_bytes: request.limits.witness_bytes as usize,
        path_bytes: request.limits.walk_path_bytes as usize,
        temporary_bytes: request.limits.temporary_bytes as usize,
    };
    let computed = match request.domain {
        ImpactDiffDomain::NativeFacts => stylo_impact_diff::compare_and_walk(
            store,
            lease_id(&base),
            lease_id(&target),
            limits,
            walk_budget,
            offset,
            request.limits.page_rows as usize,
            now,
            &mut interrupted,
        ),
        ImpactDiffDomain::VerifiedSourceImages => stylo_impact_diff::compare(
            store,
            lease_id(&base),
            lease_id(&target),
            Domain::VerifiedSourceImages,
            limits,
            now,
            &mut interrupted,
        )
        .map(|comparison| stylo_impact_diff::ChangeAnalysis {
            comparison,
            impacts: Vec::new(),
            walk_limited: false,
            usage: Default::default(),
        }),
    };
    drop(interrupted);
    let computed = match computed {
        Ok(value) => value,
        Err(stylo_impact_diff::Error::Expired) => return Ok(empty(ImpactDiffTerminal::Expired)),
        Err(stylo_impact_diff::Error::Cancelled) if timed_out => {
            return Ok(empty(ImpactDiffTerminal::WorkLimit {
                reason: "deadline".into(),
            }));
        }
        Err(stylo_impact_diff::Error::Cancelled) => {
            return Ok(empty(ImpactDiffTerminal::Cancelled));
        }
        Err(stylo_impact_diff::Error::WorkLimit) => {
            return Ok(empty(ImpactDiffTerminal::WorkLimit {
                reason: "input or temporary limit".into(),
            }));
        }
        Err(_) => return Err(Error::Invalid),
    };
    if timed_out {
        return Ok(empty(ImpactDiffTerminal::WorkLimit {
            reason: "deadline".into(),
        }));
    }
    for claims in [&base, &target] {
        match alive(store, claims, Instant::now()) {
            Ok(()) => {}
            Err(Error::Expired) => return Ok(empty(ImpactDiffTerminal::Expired)),
            Err(error) => return Err(error),
        }
    }
    let diff = &computed.comparison.result;
    if diff.stop == impact_diff::Stop::Cancelled {
        return Ok(empty(ImpactDiffTerminal::Cancelled));
    }
    if diff.stop == impact_diff::Stop::Invalid {
        return Err(Error::Invalid);
    }
    if offset > diff.changes.len() {
        return Err(Error::Invalid);
    }
    let mut rows = Vec::new();
    let mut used = 16_384usize;
    let mut missing_owner = false;
    let mut context_gapped = false;
    for change in diff
        .changes
        .iter()
        .skip(offset)
        .take(request.limits.page_rows as usize)
    {
        if cancelled() {
            return Ok(empty(ImpactDiffTerminal::Cancelled));
        }
        if Instant::now() >= deadline {
            timed_out = true;
            break;
        }
        let mut impacts = Vec::new();
        let mut base_source = None;
        let mut target_source = None;
        for linked in computed
            .impacts
            .iter()
            .filter(|linked| linked.change == *change)
        {
            missing_owner |= linked.missing_owner;
            context_gapped |= linked.context_gapped;
            if linked.source_available {
                let lease = match linked.side {
                    ChangeSide::Base => request.base.clone(),
                    ChangeSide::Target => request.target.clone(),
                };
                let source = ImpactSource::issue(
                    ImpactSourceClaims {
                        lease,
                        host: linked.seed_host,
                        site: change.id,
                    },
                    secret,
                )
                .map_err(|_| Error::Invalid)?;
                match linked.side {
                    ChangeSide::Base => base_source = Some(source),
                    ChangeSide::Target => target_source = Some(source),
                }
            }
            for (row, path) in linked.rows.iter().zip(&linked.paths) {
                impacts.push(ImpactDiffPath {
                    side: match linked.side {
                        ChangeSide::Base => ImpactSide::Base,
                        ChangeSide::Target => ImpactSide::Target,
                    },
                    strength: ImpactStrength::Candidate,
                    dependency: row.dependency,
                    owner: row.owner,
                    path: path.clone(),
                    witness: row.witness.to_vec(),
                });
            }
        }
        let item = ImpactDiffItem {
            id: change.id,
            side: match change.side {
                Side::BaseOnly => ImpactDiffSide::BaseOnly,
                Side::TargetOnly => ImpactDiffSide::TargetOnly,
                Side::BothChanged => ImpactDiffSide::BothChanged,
            },
            certainty: match change.certainty {
                Certainty::Exact => ImpactDiffCertainty::ExactCapturedRow,
                Certainty::Candidate => ImpactDiffCertainty::CandidateMissing,
            },
            impacts,
            base_source,
            target_source,
        };
        let size = serde_json::to_vec(&item)
            .map_err(|_| Error::Invalid)?
            .len()
            .saturating_add(2);
        if used.saturating_add(size) > request.limits.output_bytes as usize {
            break;
        }
        used += size;
        rows.push(item);
    }
    for claims in [&base, &target] {
        match alive(store, claims, Instant::now()) {
            Ok(()) => {}
            Err(Error::Expired) => return Ok(empty(ImpactDiffTerminal::Expired)),
            Err(error) => return Err(error),
        }
    }
    timed_out |= Instant::now() >= deadline;
    let next = offset + rows.len();
    let more = next < diff.changes.len() || diff.stop == impact_diff::Stop::OutputRows;
    let work_limited = computed.walk_limited
        || !matches!(
            diff.stop,
            impact_diff::Stop::Complete | impact_diff::Stop::OutputRows
        );
    let terminal = if timed_out {
        ImpactDiffTerminal::WorkLimit {
            reason: "deadline".into(),
        }
    } else if work_limited {
        ImpactDiffTerminal::WorkLimit {
            reason: "diff or traversal limit".into(),
        }
    } else if more && !rows.is_empty() {
        ImpactDiffTerminal::More {
            continuation: ImpactDiffCursor::issue(
                ImpactDiffCursorClaims {
                    recipe: request.recipe().map_err(|_| Error::Invalid)?,
                    offset: u32::try_from(next).map_err(|_| Error::Invalid)?,
                },
                secret,
            )
            .map_err(|_| Error::Invalid)?,
        }
    } else if more {
        ImpactDiffTerminal::WorkLimit {
            reason: "output budget cannot fit one change".into(),
        }
    } else {
        ImpactDiffTerminal::Complete
    };
    Ok(ImpactDiffReply {
        base: request.base.clone(),
        target: request.target.clone(),
        domain: request.domain,
        base_capture_complete: computed.comparison.base_complete,
        target_capture_complete: computed.comparison.target_complete,
        semantic_gapped: true,
        provider_unknown: true,
        walk_limited: computed.walk_limited,
        context_gapped,
        missing_owner,
        rows,
        comparisons: Some(diff.comparisons as u32),
        compared_bytes: Some(diff.compared_bytes as u32),
        counters: Some(ImpactDiffCounters {
            scan_rows: computed.comparison.scan_rows as u32,
            scan_bytes: computed.comparison.scan_bytes as u32,
            temporary_bytes: computed.comparison.temporary_bytes as u32,
            seed_walks: computed.usage.seed_walks as u32,
            walk_input_rows: computed.usage.input_rows as u32,
            walk_input_bytes: computed.usage.input_bytes as u32,
            walk_edges: computed.usage.edges as u32,
            walk_nodes: computed.usage.nodes as u32,
            walk_rows: computed.usage.rows as u32,
            witness_bytes: computed.usage.witness_bytes as u32,
            walk_path_bytes: computed.usage.path_bytes as u32,
            walk_temporary_peak: computed.usage.temporary_peak as u32,
        }),
        terminal,
    })
}
