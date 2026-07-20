//! The package-search read flow (`Target::Packages`).
//!
//! One [`heart::query::Query`] in, one ranked [`heart::search::Page`] of
//! [`crate::registry::GlobalPackage`] out. Pagination lives *here*, inside the
//! engine after ranking (INDEX-PLAN §9) — the HTTP handler never touches a
//! cursor.

use std::sync::Arc;

use futures::TryStreamExt;
use heart::query::{PageSpecification, Query as WireQuery};
use heart::{Cursor, Page, Scored};

use registry::vector::EmbeddingModel;

use crate::Server;
use crate::authz::ReadCap;
use crate::error::{BadRequestReason, ServerError, ServerResult};
use crate::registry::GlobalPackage;
use crate::registry::search::SearchKey;
use crate::registry::search::usages::{Usage, UsageQueryBackend};
use crate::search::query::{LiteralQuery, Query as ExecutionQuery};
use crate::search::registry::RegistrySearchSurface;

impl<M: EmbeddingModel> Server<M> {
    /// Search the registry for packages matching one wire [`WireQuery`].
    ///
    /// Runs the frozen fusion ranking (ID-8) across every federation source,
    /// merges overlay-over-base, and pages the single ranked order out with one
    /// keyset cursor anchored to the definitive base's index snapshot. The
    /// caller must hold a [`ReadCap`] proving authorization already happened at
    /// the HTTP boundary.
    ///
    /// Only the wire query's `text`, ecosystem `scope`, and `page` are
    /// consulted here; symbol/usage-only fields are ignored for package search.
    pub async fn search_packages(
        &self,
        _cap: &ReadCap,
        query: &WireQuery,
    ) -> ServerResult<Page<GlobalPackage>> {
        let page = &query.page;
        let limit = page_limit(page);

        let execution_query =
            ExecutionQuery::Literal(LiteralQuery::parse(&query.text).map_err(BadRequestReason::from)?);

        // Package search scopes to at most one ecosystem for v1:
        // - empty `scope.ecosystems` → unscoped (all ecosystems; `lang:` tokens
        //   in the query text still apply)
        // - one or more → the first listed only; remaining ecosystems are
        //   ignored (prefer a single-ecosystem scope, or `lang:` tokens).
        let ecosystem = query.scope.ecosystems.first().copied();

        // Synonym expansion rides the process-wide heuristics tables when loaded.
        let synonyms = self.heuristics().map(|heuristics| heuristics.synonyms());

        // The cursor is anchored to the base's snapshot; decode it once here so
        // an anchor-mismatch is reported as a client error, not swallowed.
        let after = self.decode_package_cursor(page)?;

        let mut groups = Vec::new();
        for sourced in self.federation().in_precedence() {
            let surface = RegistrySearchSurface::new(Arc::clone(&sourced.value.packages));
            let hits = surface
                .search(&execution_query, page, ecosystem, synonyms)
                .await?;
            groups.push(hits.try_collect().await?);
        }
        let items = crate::search::merge_overlay_first(groups, |package| package.id, limit);

        // Resume strictly after the decoded cursor key under (score desc, id asc),
        // then take one page. A full page may have more behind it.
        let snapshot = self.base().packages.snapshot().await;
        let resumed: Vec<Scored<GlobalPackage>> = items
            .into_iter()
            .filter(|hit| {
                crate::registry::search::keyset_is_after(after, hit.score, hit.value.id)
            })
            .take(limit)
            .collect();

        let next = (resumed.len() == limit)
            .then(|| resumed.last())
            .flatten()
            .map(|last| Cursor::<SearchKey>::new((last.score, last.value.id), snapshot).encode());

        tracing::debug!(hits = resumed.len(), "package search served");
        Ok(Page { items: resumed, next })
    }

    /// Answer a `Target::Usages` query — every recorded use of one symbol, from
    /// the reverse `occ` index ([`registry::graph::ReversePositionIndex`],
    /// INDEX-PLAN §5.5). The caller must hold a [`ReadCap`].
    ///
    /// Delegates to the server's [`ReverseIndexUsageBackend`]. When an IR view +
    /// reverse index are loaded for the queried package, this returns the real
    /// paged uses; when none is loaded for the scope it returns the typed
    /// `IndexUnavailable` (surfaced as `503`), never a `501` and never a fake
    /// empty page — a client can tell "not materialized here yet" apart from
    /// "this symbol genuinely has no uses" (a populated empty page).
    pub async fn usages(
        &self,
        _cap: &ReadCap,
        query: &WireQuery,
    ) -> ServerResult<Page<Usage>> {
        let heart::query::Target::Usages { of } = &query.target else {
            // The router only dispatches `Target::Usages` here.
            return Err(BadRequestReason::MissingField { field: "target.Usages.of" }.into());
        };
        self.usage_backend()
            .usages(of, &query.page)
            .map_err(ServerError::from)
    }

    /// Decode the opaque package-search resume token, if any, as its keyset.
    ///
    /// A malformed token is a typed client error; a token anchored to an older
    /// snapshot is tolerated (the newer order is served), matching the symbol
    /// paginator's defence-in-depth posture.
    fn decode_package_cursor(
        &self,
        page: &PageSpecification,
    ) -> ServerResult<Option<SearchKey>> {
        let Some(token) = page.cursor.as_deref() else {
            return Ok(None);
        };
        let cursor = Cursor::<SearchKey>::decode(token).map_err(|source| {
            ServerError::from(BadRequestReason::InvalidCursor {
                token: token.to_owned(),
                source,
            })
        })?;
        Ok(Some(cursor.after))
    }
}

/// The requested page size as a `usize`, clamped so a zero limit still returns
/// at least one hit.
fn page_limit(page: &PageSpecification) -> usize {
    (page.limit as usize).max(1)
}
