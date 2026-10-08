//! Producer-owned packed search over the existing admitted ranked prefix.
//! A cold continuation authenticates and rereads its exact predecessor range
//! and successor witness. One pending source row may survive a warm request;
//! all other reader state retires before the owner answers.

use super::{QueryCoordinator, RemoteSemantic, SearchPageError};
use backend_library::{
    Library, PackedCallContext, PackedPageCheckpoint, PackedPageContext, PackedPrefixCommitment,
    PackedQueryReceipt, Query, QueryPageRecipe, Row, SemanticSearchStatus, ViewSnapshot,
};
use backend_page_output::{
    PackedPageFailure, PackedPageWorker, measure_packed_search, packed_record_commitment,
    packed_search_records, packing_detail,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

pub(crate) struct PendingPackedSearch {
    token: String,
    row: Row,
    digest: [u8; 32],
}
pub(crate) struct PackedSearchOutcome {
    pub snapshot: ViewSnapshot,
    pub status: SemanticSearchStatus,
    pub receipt: PackedQueryReceipt,
    pub pending: Option<PendingPackedSearch>,
}

/// Refuses a provably unfit envelope before capturing or searching a corpus.
/// The required bytes are a canonical no-record lower bound; the actual
/// result may require more after its source and optional status are observed.
pub(crate) fn preflight_packed_search(
    query: &Query,
    coverage: &[backend_library::Coverage],
) -> Result<(), backend_library::PackedQueryFailure> {
    let Some(request) = query.packing() else {
        return Ok(());
    };
    let required =
        backend_page_output::minimum_packed_search_bytes(query.text(), coverage, request);
    let credit = request.policy().bytes();
    if required > credit as usize {
        return Err(
            backend_library::PackedQueryFailure::EnvelopeExceedsWireCredit {
                required_bytes: required as u64,
                credit_bytes: credit,
            },
        );
    }
    Ok(())
}

fn authenticate(secret: &[u8; 32], claims: &[u8]) -> [u8; 32] {
    let mut hash = blake3::Hasher::new_keyed(secret);
    hash.update(b"packed-search-owner-checkpoint-v1\0");
    hash.update(claims);
    *hash.finalize().as_bytes()
}

// Cost-only text, never returned to a caller as an authenticated MCP token.
// The actual frontend signs its real workspace/tool context. Every admitted
// MAC is exactly 32 safe hex characters, so this placeholder has identical
// escaped byte cost without copying that context through the service DTO.
fn cost_outer_token(request: &backend_library::PackedQueryRequest, token: &str) -> String {
    match request.call() {
        PackedCallContext::CliJson => token.to_owned(),
        PackedCallContext::Mcp { expiry, .. } => {
            let body = format!("{expiry}-{}", URL_SAFE_NO_PAD.encode(token.as_bytes()));
            format!("mcp1-{body}-{}", "0".repeat(32))
        }
    }
}

fn contract(error: impl core::fmt::Display) -> SearchPageError {
    SearchPageError::Projection(backend_library::LibraryError::InvalidQuery(
        error.to_string(),
    ))
}

pub(crate) fn search_packed(
    coordinator: &QueryCoordinator,
    library: &Library,
    remote: &mut RemoteSemantic,
    coverage: backend_version::CoverageWitness,
    query: &Query,
    secret: &[u8; 32],
    retained: Option<PendingPackedSearch>,
) -> Result<PackedSearchOutcome, SearchPageError> {
    let request = query
        .packing()
        .ok_or_else(|| contract("packed search omitted its policy"))?;
    preflight_packed_search(query, library.view().coverage()).map_err(SearchPageError::Packed)?;
    let policy = request.policy();
    let context = PackedPageContext::new(
        library.cursor(),
        query.basis(),
        QueryPageRecipe::search(library.revision_root(), query).identity(),
        policy,
    )
    .map_err(contract)?;
    let predecessor = request
        .predecessor()
        .map(|token| {
            PackedPageCheckpoint::decode_token_against(token, context, |claims, signature| {
                authenticate(secret, claims)
                    .iter()
                    .zip(signature)
                    .fold(0_u8, |difference, (left, right)| {
                        difference | (left ^ right)
                    })
                    == 0
            })
            .map_err(|_| SearchPageError::Projection(backend_library::LibraryError::CursorMismatch))
        })
        .transpose()?;
    if predecessor
        .as_ref()
        .is_some_and(|page| page.stop().next_row().is_none())
    {
        return Err(SearchPageError::Projection(
            backend_library::LibraryError::CursorMismatch,
        ));
    }
    let start = predecessor.as_ref().map_or(0, PackedPageCheckpoint::next);
    let start_at = usize::try_from(start)
        .map_err(|_| contract("packed search offset exceeds the existing rank space"))?;
    let local = coordinator
        .search_local_prefix(query, start_at)
        .map_err(SearchPageError::Local)?;
    let reconciliation_failed = remote.reconcile(coordinator, coverage).is_err();
    let (result, status) = remote.search_with_status(
        coordinator,
        coverage,
        local,
        query.text(),
        reconciliation_failed,
    );
    let ids = result
        .rows
        .iter()
        .map(|ranked| ranked.row.id)
        .collect::<Vec<_>>();
    let source = library
        .admit_ranked_search(query, &ids)
        .map_err(SearchPageError::Projection)?;
    if let Some(previous) = &predecessor {
        let mut prefix = PackedPrefixCommitment::new(context, previous.start());
        for ordinal in previous.start()..previous.next() {
            let at = usize::try_from(ordinal).map_err(contract)?;
            let row = source.row_at(at).ok_or_else(|| {
                contract("packed predecessor range is absent from the selected source")
            })?;
            prefix
                .push(
                    ordinal,
                    packed_record_commitment(&row, policy.detail()).map_err(contract)?,
                )
                .map_err(contract)?;
        }
        let next = source
            .row_at(start_at)
            .map(|row| packed_record_commitment(&row, policy.detail()))
            .transpose()
            .map_err(contract)?;
        previous.verify_source_range(&prefix, next).map_err(|_| {
            SearchPageError::Projection(backend_library::LibraryError::CursorMismatch)
        })?;
    }
    let retained = retained.filter(|pending| request.predecessor() == Some(pending.token.as_str()));
    let reused = retained.as_ref().is_some_and(|pending| {
        source
            .row_at(start_at)
            .and_then(|row| packed_record_commitment(&row, policy.detail()).ok())
            == Some(pending.digest)
    });
    let begin = start_at
        .checked_add(usize::from(reused))
        .ok_or_else(|| contract("packed rank overflow"))?;
    let end = start_at
        .checked_add(usize::from(query.limit().get()))
        .and_then(|end| end.checked_add(1))
        .ok_or_else(|| contract("packed rank overflow"))?;
    let rows = (begin..end).map_while(|at| source.row_at(at));
    let mut worker = PackedPageWorker::new(rows, context, start);
    if let Some(pending) = retained.filter(|_| reused) {
        worker = worker.with_pending_row(pending.row, pending.digest);
    }
    let page = worker
        .next_page(
            |row| packed_record_commitment(row, policy.detail()),
            |rows, checkpoint| {
                let more = checkpoint.stop().next_row().is_some();
                let owner_token = more.then(|| PackedPageCheckpoint::cost_token());
                let cursor = owner_token
                    .as_deref()
                    .map(|token| cost_outer_token(request, token));
                let answer = packed_search_records(
                    query.text(),
                    rows,
                    library.view().coverage(),
                    status,
                    more,
                );
                Ok::<_, String>(measure_packed_search(
                    &answer,
                    packing_detail(policy.detail()),
                    cursor.as_deref(),
                    request.call(),
                    policy.scope(),
                    policy.bytes() as usize,
                ))
            },
        )
        .map_err(|failure| match failure {
            PackedPageFailure::ItemExceedsWireCredit {
                ordinal,
                required_bytes,
                credit_bytes,
            } => SearchPageError::Packed(
                backend_library::PackedQueryFailure::ItemExceedsWireCredit {
                    ordinal,
                    required_bytes: required_bytes as u64,
                    credit_bytes: credit_bytes as u32,
                },
            ),
            PackedPageFailure::EnvelopeExceedsWireCredit {
                required_bytes,
                credit_bytes,
            } => SearchPageError::Packed(
                backend_library::PackedQueryFailure::EnvelopeExceedsWireCredit {
                    required_bytes: required_bytes as u64,
                    credit_bytes: credit_bytes as u32,
                },
            ),
            PackedPageFailure::Contract(error) => contract(error),
            PackedPageFailure::Projection(error) => contract(error),
            PackedPageFailure::Retired => {
                contract("packed search reader retired before completion")
            }
        })?;
    let receipt =
        PackedQueryReceipt::issue(&page.checkpoint, |claims| authenticate(secret, claims));
    let cursor = receipt
        .more()
        .then(|| cost_outer_token(request, receipt.token()));
    let answer = packed_search_records(
        query.text(),
        &page.rows,
        library.view().coverage(),
        status,
        receipt.more(),
    );
    let actual = measure_packed_search(
        &answer,
        packing_detail(policy.detail()),
        cursor.as_deref(),
        request.call(),
        policy.scope(),
        policy.bytes() as usize,
    );
    if actual != receipt.bytes() as usize || actual > policy.bytes() as usize {
        return Err(contract(
            "actual signed packed response differs from its admitted cost",
        ));
    }
    let pending = worker
        .into_pending()
        .map(|(row, digest)| PendingPackedSearch {
            token: receipt.token().to_owned(),
            row,
            digest,
        });
    let snapshot = source
        .snapshot(start_at, page.rows.len())
        .map_err(SearchPageError::Projection)?;
    Ok(PackedSearchOutcome {
        snapshot,
        status,
        receipt,
        pending,
    })
}
