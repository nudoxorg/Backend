//! Bounded catalog projections and query cursor admission.

use super::Library;
use crate::arrangement::ArrangementPage;
use crate::{
    Cursor, Document, DocumentQuery, Freshness, Frontier, GraphNeighborhoodQuery, GraphRelation, LibraryError,
    NameQuery, Outline, OutlineExtent, OutlineNode, OutlineQuery, PackageKey, PageRequest,
    PageTerminal, ProjectionPage, Query, QueryLimit, RankedSearchSnapshot, ReadManifest, Row,
    RowId, SymbolKey, ViewRecipeId, ViewRevision, ViewRoot, ViewSnapshot, ViewStateRoot,
    view_identity_bytes,
};
use crate::{ReferenceFact, ReferenceRecord};
use std::collections::BTreeSet;

impl Library {
    /// Reads one root/query-bound package page.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError::WrongBasis`] for a stale revision,
    /// [`LibraryError::CursorMismatch`] for a foreign continuation, or
    /// [`LibraryError::View`] when the bounded snapshot cannot be built.
    pub fn packages_page(&self, page: PageRequest) -> Result<ProjectionPage, LibraryError> {
        self.check_basis(page.basis())?;
        let recipe_bytes = b"packages-page";
        let recipe = self.query_recipe(recipe_bytes, None);
        let limit = usize::from(page.limit().get());
        let cursor = page.continuation().map(crate::PageContinuation::cursor);
        let start = self.check_query_cursor(cursor, recipe, limit, |offset| {
            self.arrangement.packages_page_from(offset, limit)
        })?;
        let result = self.arrangement.packages_page_from(start, limit);
        self.projection_page(recipe_bytes, &result, start, limit)
    }

    /// Reads package outline membership as one bounded flat row page.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError::WrongBasis`] for a stale revision,
    /// [`LibraryError::NotFound`] for an unknown package,
    /// [`LibraryError::CursorMismatch`] for a foreign continuation, or
    /// [`LibraryError::View`] when the bounded snapshot cannot be built.
    pub fn outline_page(
        &self,
        package: PackageKey,
        page: PageRequest,
    ) -> Result<ProjectionPage, LibraryError> {
        self.check_basis(page.basis())?;
        if !self.arrangement.has_package(package) {
            return Err(LibraryError::NotFound);
        }
        let mut recipe_bytes = b"outline-page".to_vec();
        recipe_bytes.extend_from_slice(package.as_bytes());
        let recipe = self.query_recipe(&recipe_bytes, None);
        let limit = usize::from(page.limit().get());
        let cursor = page.continuation().map(crate::PageContinuation::cursor);
        let start = self.check_query_cursor(cursor, recipe, limit, |offset| {
            self.arrangement
                .package_symbols_page_from(package, offset, limit)
        })?;
        let result = self
            .arrangement
            .package_symbols_page_from(package, start, limit);
        self.projection_page(&recipe_bytes, &result, start, limit)
    }

    /// Looks up one document under an exact source basis.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError::WrongBasis`] when the query is stale and
    /// [`LibraryError::NotFound`] when no document is registered for the
    /// requested symbol.
    pub fn document(&self, query: DocumentQuery) -> Result<Document, LibraryError> {
        let basis = self.check_basis(query.basis)?;
        if let Some(source) = query.source_basis()
            && source != self.revision_basis()
        {
            return Err(LibraryError::WrongBasis {
                expected: self.revision_root(),
                observed: source.root.into(),
            });
        }
        let symbol = query
            .resolve_symbol(&self.view)
            .ok_or(LibraryError::NotFound)?;
        self.work.record_seek();
        let row = self
            .arrangement
            .document(symbol)
            .then(|| self.view.row(RowId::Symbol(symbol)))
            .flatten()
            .ok_or(LibraryError::NotFound)?;
        let mut document = Document::new(symbol, basis, row.document.clone())
            .with_source_basis(self.revision_basis())
            .with_location(row.source.clone())
            .with_excerpt(row.excerpt.clone());
        document.signature.clone_from(&row.signature);
        Ok(document)
    }

    /// Looks up names with a bounded stable-row result.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError::WrongBasis`] for a stale query and
    /// [`LibraryError::View`] when the bounded snapshot cannot be constructed.
    pub fn names(&self, query: &NameQuery) -> Result<ViewSnapshot, LibraryError> {
        self.check_basis(query.basis)?;
        let recipe = self.query_recipe(query.text.as_bytes(), query.read_manifest.as_ref());
        self.work.record_seek();
        let limit = usize::from(query.limit.get());
        let start = self.check_query_cursor(query.cursor, recipe, limit, |offset| {
            self.arrangement.names_page(
                query.text(),
                offset,
                limit,
                |id| self.view.row(id),
                &self.work,
            )
        })?;
        let page = self.arrangement.names_page(
            query.text(),
            start,
            limit,
            |id| self.view.row(id),
            &self.work,
        );
        if query.cursor.is_some() && page.ids.is_empty() {
            return Err(LibraryError::CursorMismatch);
        }
        let rows = page
            .ids
            .iter()
            .filter_map(|&id| self.view.row(id))
            .collect::<Vec<_>>();
        self.work.record_output(rows.len());
        self.snapshot_for(
            query.text.as_bytes(),
            rows,
            if page.has_more {
                Some(self.cursor)
            } else {
                None
            },
            query.read_manifest.as_ref(),
            Some(start.saturating_add(limit)),
        )
    }

    /// Looks up one package outline under an exact source basis.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError::WrongBasis`] for a stale query and
    /// [`LibraryError::NotFound`] when no outline is registered for the
    /// requested package.
    pub fn outline(&self, query: OutlineQuery) -> Result<Outline, LibraryError> {
        let basis = self.check_basis(query.basis)?;
        if let Some(source) = query.source_basis()
            && source != self.revision_basis()
        {
            return Err(LibraryError::WrongBasis {
                expected: self.revision_root(),
                observed: source.root.into(),
            });
        }
        self.work.record_seek();
        if !self.arrangement.has_package(query.package) {
            return Err(LibraryError::NotFound);
        }
        let symbol_page = self
            .arrangement
            .package_symbols_page(query.package, usize::from(QueryLimit::MAX));
        let mut truncated = symbol_page.has_more;
        let mut symbols = symbol_page
            .ids
            .iter()
            .filter_map(|&id| self.row_for_id(id))
            .filter_map(|row| match row.id {
                RowId::Symbol(symbol) => Some(symbol),
                RowId::Package(_) | RowId::Object(_) => None,
            })
            .collect::<Vec<_>>();
        // Existing single-package fixtures predate explicit package
        // membership. Keep their unambiguous result while ensuring an
        // unscoped symbol can never leak across a multi-package projection.
        if symbols.is_empty() && self.arrangement.package_count() == 1 {
            symbols = self
                .arrangement
                .unscoped_symbols_page(usize::from(QueryLimit::MAX))
                .ids
                .iter()
                .filter_map(|&id| self.row_for_id(id))
                .filter_map(|row| match row.id {
                    RowId::Symbol(symbol) if row.package.is_none() => Some(symbol),
                    RowId::Package(_) | RowId::Object(_) | RowId::Symbol(_) => None,
                })
                .collect();
        }
        if symbols.is_empty() {
            return Err(LibraryError::NotFound);
        }
        let mut roots = symbols
            .iter()
            .copied()
            .filter(|symbol| {
                self.row_for_id(RowId::Symbol(*symbol))
                    .is_some_and(|row| row.parent.is_none())
            })
            .collect::<Vec<_>>();
        if roots.is_empty() {
            roots.push(symbols[0]);
        }
        let mut budget = usize::from(QueryLimit::MAX);
        let mut nodes = Vec::new();
        for symbol in roots {
            if budget == 0 {
                truncated = true;
                break;
            }
            budget = budget.saturating_sub(1);
            let mut path = BTreeSet::new();
            path.insert(symbol);
            nodes.push(OutlineNode {
                symbol,
                children: self.outline_children(
                    query.package,
                    symbol,
                    &mut budget,
                    0,
                    &mut path,
                    &mut truncated,
                ),
            });
        }
        let root = nodes.remove(0);
        Ok(Outline::new(query.package, basis, root)
            .with_additional_roots(nodes)
            .with_extent(if truncated {
                OutlineExtent::Truncated
            } else {
                OutlineExtent::Complete
            })
            .with_source_basis(self.revision_basis()))
    }

    fn outline_children(
        &self,
        package: PackageKey,
        parent: SymbolKey,
        budget: &mut usize,
        depth: usize,
        path: &mut BTreeSet<SymbolKey>,
        truncated: &mut bool,
    ) -> Box<[OutlineNode]> {
        // Outline replies have no independent continuation shape, so enforce
        // a hard node/depth budget at the arrangement boundary. A malformed
        // cyclic parent relation is cut at the path edge rather than causing
        // unbounded recursion or a whole-view scan.
        const MAX_DEPTH: usize = 64;
        if *budget == 0 || depth >= MAX_DEPTH {
            *truncated = true;
            return Box::new([]);
        }
        let page = self
            .arrangement
            .children_page(package, Some(parent), *budget);
        *truncated |= page.has_more;
        let mut children = Vec::with_capacity(page.ids.len());
        for id in page.ids {
            if *budget == 0 {
                break;
            }
            let Some(Row {
                id: RowId::Symbol(symbol),
                ..
            }) = self.row_for_id(id)
            else {
                continue;
            };
            if !path.insert(symbol) {
                continue;
            }
            *budget = (*budget).saturating_sub(1);
            let nested = self.outline_children(package, symbol, budget, depth + 1, path, truncated);
            path.remove(&symbol);
            children.push(OutlineNode {
                symbol,
                children: nested,
            });
        }
        children.into_boxed_slice()
    }

    /// Searches registered names and document text with stable row identity.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError::WrongBasis`] for a stale query,
    /// [`LibraryError::InvalidQuery`] for empty search text, or
    /// [`LibraryError::View`] when the bounded snapshot cannot be constructed.
    pub fn search(&self, query: &Query) -> Result<ViewSnapshot, LibraryError> {
        self.search_ranked(query)
            .map(RankedSearchSnapshot::into_snapshot)
    }

    /// Searches while retaining relevance order separately from canonical
    /// view-relation order.
    ///
    /// # Errors
    ///
    /// Returns the same bounded query failures as the compatibility search.
    pub fn search_ranked(&self, query: &Query) -> Result<RankedSearchSnapshot, LibraryError> {
        self.check_basis(query.basis)?;
        if query.text.trim().is_empty() {
            return Err(LibraryError::InvalidQuery(
                "search text is empty".to_owned(),
            ));
        }
        let recipe = self.query_recipe(query.text.as_bytes(), query.read_manifest.as_ref());
        self.work.record_seek();
        let limit = usize::from(query.limit.get());
        let start = self.check_query_cursor(query.cursor, recipe, limit, |offset| {
            self.arrangement.search_page(
                query.text(),
                offset,
                limit,
                |id| self.view.row(id),
                &self.work,
            )
        })?;
        let page = self.arrangement.search_page(
            query.text(),
            start,
            limit,
            |id| self.view.row(id),
            &self.work,
        );
        if query.cursor.is_some() && page.ids.is_empty() {
            return Err(LibraryError::CursorMismatch);
        }
        let rows = page
            .ids
            .iter()
            .filter_map(|&id| self.view.row(id))
            .collect::<Vec<_>>();
        self.work.record_output(rows.len());
        let snapshot = self.snapshot_for(
            query.text.as_bytes(),
            rows,
            if page.has_more {
                Some(self.cursor)
            } else {
                None
            },
            query.read_manifest.as_ref(),
            Some(start.saturating_add(limit)),
        )?;
        Ok(RankedSearchSnapshot {
            snapshot,
            order: page.ids.into_boxed_slice(),
        })
    }

    /// Builds a search page from an owner-selected complete relevance order.
    ///
    /// This is the application-service seam for replaceable local search
    /// engines. Every identity is resolved from this library's immutable view;
    /// callers can choose relevance order but cannot inject row payloads or
    /// escape the request's root, recipe, page, or continuation bounds.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError::WrongBasis`] for a stale query,
    /// [`LibraryError::InvalidQuery`] for invalid text or foreign/duplicate
    /// identities, and [`LibraryError::CursorMismatch`] for an invalid page.
    pub fn search_from_ranked_ids(
        &self,
        query: &Query,
        ranked_ids: &[RowId],
    ) -> Result<ViewSnapshot, LibraryError> {
        self.check_basis(query.basis)?;
        if query.text.trim().is_empty() {
            return Err(LibraryError::InvalidQuery(
                "search text is empty".to_owned(),
            ));
        }
        let mut unique = BTreeSet::new();
        if ranked_ids.len() > usize::try_from(self.view.row_count()).unwrap_or(usize::MAX)
            || ranked_ids
                .iter()
                .any(|id| !unique.insert(*id) || self.view.row(*id).is_none())
        {
            return Err(LibraryError::InvalidQuery(
                "ranked search identities are not a subset of the selected view".to_owned(),
            ));
        }
        let recipe = self.query_recipe(query.text.as_bytes(), query.read_manifest.as_ref());
        let limit = usize::from(query.limit.get());
        let ranked_page = |start: usize| {
            let mut ids = ranked_ids
                .iter()
                .copied()
                .skip(start)
                .take(limit.saturating_add(1))
                .collect::<Vec<_>>();
            let has_more = ids.len() > limit;
            ids.truncate(limit);
            ArrangementPage { ids, has_more }
        };
        let start = self.check_query_cursor(query.cursor, recipe, limit, ranked_page)?;
        let page = ranked_page(start);
        if query.cursor.is_some() && page.ids.is_empty() {
            return Err(LibraryError::CursorMismatch);
        }
        let rows = page
            .ids
            .iter()
            .filter_map(|&id| self.view.row(id))
            .collect::<Vec<_>>();
        self.work.record_seek();
        self.work.record_output(rows.len());
        self.snapshot_for(
            query.text.as_bytes(),
            rows,
            page.has_more.then_some(self.cursor),
            query.read_manifest.as_ref(),
            page.has_more.then_some(start.saturating_add(limit)),
        )
    }

    /// Returns one bounded graph neighborhood rooted at a declaration.
    ///
    /// The retained package/parent arrangement supplies the root's parent and
    /// direct children. No graph result is fabricated when the declaration is
    /// absent, and the result is always pinned to this library's source root.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError::NotFound`] when the declaration is not in the
    /// accepted view, or [`LibraryError::View`] if the bounded snapshot cannot
    /// be constructed.
    pub fn graph(&self, query: GraphNeighborhoodQuery) -> Result<ViewSnapshot, LibraryError> {
        self.check_basis(query.basis())?;
        let symbol = query
            .resolve_symbol(&self.view)
            .ok_or(LibraryError::NotFound)?;
        self.work.record_seek();
        let root_id = RowId::Symbol(symbol);
        let root = self.view.row(root_id).ok_or(LibraryError::NotFound)?;
        let mut ids = Vec::with_capacity(1 + usize::from(QueryLimit::MAX));
        ids.push(root_id);
        if let Some(package) = root.package {
            if let Some(parent) = root.parent {
                let parent_id = RowId::Symbol(parent);
                if self.arrangement.document(parent) && self.view.row(parent_id).is_some() {
                    ids.push(parent_id);
                }
            }
            let remaining = usize::from(QueryLimit::MAX).saturating_sub(ids.len());
            let children = self
                .arrangement
                .children_page(package, Some(symbol), remaining);
            ids.extend(children.ids);
        }
        ids.sort_unstable();
        ids.dedup();
        let rows = ids
            .into_iter()
            .filter_map(|id| self.view.row(id))
            .collect::<Vec<_>>();
        self.work.record_output(rows.len());
        self.snapshot_for(b"graph", rows, None, None, None)
    }

    /// Builds a graph snapshot from compiler-proven semantic neighbor identities.
    ///
    /// The application owner may select graph edges from a richer authority such as a reopened
    /// compiler image. This method keeps row payload authority here: every supplied identity must
    /// be a member of this exact immutable view, the requested source must be present, and the
    /// fixed graph result bound is checked before any result root is built.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError`] when the query basis differs, the source or a target is absent,
    /// identities repeat, or the fixed result bound is exceeded.
    pub fn graph_from_semantic_ids(
        &self,
        query: GraphNeighborhoodQuery,
        ids: &[RowId],
    ) -> Result<ViewSnapshot, LibraryError> {
        self.check_basis(query.basis())?;
        let source = RowId::Symbol(
            query
                .resolve_symbol(&self.view)
                .ok_or(LibraryError::NotFound)?,
        );
        if ids.len() > 1 + usize::from(QueryLimit::MAX) || !ids.contains(&source) {
            return Err(LibraryError::InvalidQuery(
                "semantic graph identities violate the bounded source contract".to_owned(),
            ));
        }
        let mut unique = BTreeSet::new();
        let rows = ids
            .iter()
            .copied()
            .map(|id| {
                if !unique.insert(id) {
                    return Err(LibraryError::InvalidQuery(
                        "semantic graph contains a duplicate identity".to_owned(),
                    ));
                }
                self.view.row(id).ok_or_else(|| {
                    LibraryError::InvalidQuery(
                        "semantic graph identity is absent from the selected view".to_owned(),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.work.record_seek();
        self.work.record_output(rows.len());
        // The query recipe identifies the public graph operation. Whether its
        // neighbors came from compiler links or the structural fallback is a
        // coverage/evidence fact, not a second client-visible query identity.
        self.snapshot_for(b"graph", rows, None, None, None)
    }

    /// Builds a graph snapshot from compiler-proven semantic edges.
    ///
    /// This is the typed companion to [`Self::graph_from_semantic_ids`]. Row
    /// payloads and canonical view commitments remain owned by the library;
    /// the owner contributes only edges selected from its admitted semantic
    /// image. The edge sidecar is explicitly versioned at the wire boundary
    /// and does not alter the committed view root.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError`] when an endpoint is outside this immutable
    /// view, the source is absent, an edge is repeated, or the bounded graph
    /// contract is exceeded.
    pub fn graph_from_semantic_relations(
        &self,
        query: GraphNeighborhoodQuery,
        relations: &[GraphRelation],
    ) -> Result<ViewSnapshot, LibraryError> {
        self.check_basis(query.basis())?;
        let source = RowId::Symbol(
            query
                .resolve_symbol(&self.view)
                .ok_or(LibraryError::NotFound)?,
        );
        let mut ids = BTreeSet::from([source]);
        let mut unique = BTreeSet::new();
        for relation in relations {
            if !unique.insert(*relation) {
                return Err(LibraryError::InvalidQuery(
                    "semantic graph contains a duplicate typed relation".to_owned(),
                ));
            }
            if self.view.row(relation.from).is_none() || self.view.row(relation.to).is_none() {
                return Err(LibraryError::InvalidQuery(
                    "semantic graph relation endpoint is absent from the selected view"
                        .to_owned(),
                ));
            }
            ids.insert(relation.from);
            ids.insert(relation.to);
        }
        if ids.len() > 1 + usize::from(QueryLimit::MAX)
            || relations.len() > usize::from(QueryLimit::MAX)
        {
            return Err(LibraryError::InvalidQuery(
                "semantic graph relations violate the bounded source contract".to_owned(),
            ));
        }
        let rows = ids
            .into_iter()
            .map(|id| {
                self.view.row(id).ok_or_else(|| {
                    LibraryError::InvalidQuery(
                        "semantic graph identity is absent from the selected view".to_owned(),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.work.record_seek();
        self.work.record_output(rows.len());
        let mut snapshot = self.snapshot_for(b"graph", rows, None, None, None)?;
        snapshot.graph_relations = Some(relations.to_vec().into_boxed_slice());
        Ok(snapshot)
    }

    /// Builds the find-references answer from compiler-verified occurrence
    /// facts against this library's immutable view.
    ///
    /// This is the application-service seam for the occurrence plane, shaped
    /// exactly like [`Library::graph_from_semantic_ids`]: the owner selects
    /// the evidence from a compiler authority, while row-payload authority
    /// stays here. Every site must name a declaration in this exact view, the
    /// target must be present, and the bounded result contract holds, so a
    /// reference answer can never cite a declaration the published view
    /// cannot show.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError::NotFound`] when the target or a site is absent
    /// from the view, and [`LibraryError::InvalidQuery`] when the fact list
    /// or a resolved coordinate violates its bound.
    pub fn references(
        &self,
        target: &crate::ProductText,
        facts: &[ReferenceFact],
    ) -> Result<Box<[ReferenceRecord]>, LibraryError> {
        let target_present = self
            .view
            .rows()
            .iter()
            .any(|row| row.label == target.as_str());
        if !target_present {
            return Err(LibraryError::NotFound);
        }
        if facts.len() > crate::MAX_PRODUCT_ROWS {
            return Err(LibraryError::InvalidQuery(
                "reference facts exceed the bounded result contract".to_owned(),
            ));
        }
        let records = facts
            .iter()
            .map(|fact| {
                let row = self
                    .view
                    .row(RowId::Symbol(fact.site))
                    .ok_or(LibraryError::NotFound)?;
                Ok(ReferenceRecord {
                    site: crate::ProductText::new(row.label.clone()).map_err(|_| {
                        LibraryError::InvalidQuery(
                            "reference site coordinate violates the text bound".to_owned(),
                        )
                    })?,
                    target: fact.target.clone(),
                    relation: fact.relation,
                    evidence: fact.evidence.clone(),
                })
            })
            .collect::<Result<Vec<_>, LibraryError>>()?;
        self.work.record_seek();
        self.work.record_output(records.len());
        Ok(records.into_boxed_slice())
    }

    /// Reads one bounded graph-neighborhood page.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError::WrongBasis`] for a stale revision,
    /// [`LibraryError::NotFound`] for an unknown symbol,
    /// [`LibraryError::CursorMismatch`] for a foreign continuation, or
    /// [`LibraryError::View`] when the bounded snapshot cannot be built.
    pub fn graph_page(
        &self,
        symbol: SymbolKey,
        page: PageRequest,
    ) -> Result<ProjectionPage, LibraryError> {
        self.check_basis(page.basis())?;
        let root = self
            .view
            .row(RowId::Symbol(symbol))
            .ok_or(LibraryError::NotFound)?;
        let mut recipe_bytes = b"graph-page".to_vec();
        recipe_bytes.extend_from_slice(symbol.as_bytes());
        let recipe = self.query_recipe(&recipe_bytes, None);
        let limit = usize::from(page.limit().get());
        let cursor = page.continuation().map(crate::PageContinuation::cursor);
        let start = self.check_query_cursor(cursor, recipe, limit, |offset| {
            self.graph_page_ids(&root, symbol, offset, limit)
        })?;
        let result = self.graph_page_ids(&root, symbol, start, limit);
        self.projection_page(&recipe_bytes, &result, start, limit)
    }

    fn graph_page_ids(
        &self,
        root: &Row,
        symbol: SymbolKey,
        start: usize,
        limit: usize,
    ) -> ArrangementPage {
        let mut prefix = vec![RowId::Symbol(symbol)];
        if let Some(parent) = root.parent {
            let id = RowId::Symbol(parent);
            if self.view.row(id).is_some() {
                prefix.push(id);
            }
        }
        prefix.sort_unstable();
        prefix.dedup();
        let prefix_len = prefix.len();
        let mut ids = prefix
            .into_iter()
            .skip(start)
            .take(limit.saturating_add(1))
            .collect::<Vec<_>>();
        let mut children_have_more = false;
        if ids.len() <= limit
            && let Some(package) = root.package
        {
            let child_start = start.saturating_sub(prefix_len);
            let remaining = limit.saturating_add(1).saturating_sub(ids.len());
            let children =
                self.arrangement
                    .children_page_from(package, Some(symbol), child_start, remaining);
            children_have_more = children.has_more;
            ids.extend(children.ids);
        }
        let has_more = children_have_more || ids.len() > limit;
        ids.truncate(limit);
        ArrangementPage { ids, has_more }
    }

    fn projection_page(
        &self,
        recipe: &[u8],
        page: &ArrangementPage,
        start: usize,
        limit: usize,
    ) -> Result<ProjectionPage, LibraryError> {
        let rows = page
            .ids
            .iter()
            .filter_map(|&id| self.view.row(id))
            .collect::<Vec<_>>();
        let snapshot = self.snapshot_for(
            recipe,
            rows,
            page.has_more.then_some(self.cursor),
            None,
            page.has_more.then_some(start.saturating_add(limit)),
        )?;
        let terminal = snapshot.next.map_or(PageTerminal::Complete, |cursor| {
            PageTerminal::More(crate::PageContinuation::from_cursor(cursor))
        });
        Ok(ProjectionPage { snapshot, terminal })
    }
}

impl Library {
    fn check_basis(&self, basis: ViewRevision) -> Result<ViewStateRoot, LibraryError> {
        let expected = self.revision_root();
        if !basis.matches(expected) {
            return Err(LibraryError::WrongBasis {
                expected,
                observed: basis,
            });
        }
        Ok(expected)
    }

    pub(super) fn row_for_id(&self, id: RowId) -> Option<Row> {
        self.view.row(id)
    }

    pub(super) fn snapshot_for(
        &self,
        recipe: &[u8],
        rows: Vec<Row>,
        next: Option<Cursor>,
        read_manifest: Option<&ReadManifest>,
        next_offset: Option<usize>,
    ) -> Result<ViewSnapshot, LibraryError> {
        let manifest_bytes = read_manifest.map_or_else(
            || vec![0],
            |manifest| {
                let mut bytes = Vec::with_capacity(manifest.canonical_bytes().len() + 1);
                bytes.push(1);
                bytes.extend_from_slice(&manifest.canonical_bytes());
                bytes
            },
        );
        let id = self.query_recipe_with_manifest(recipe, &manifest_bytes);
        let coverage = self.view.coverage.to_vec();
        let basis = self.revision_basis();
        let frontier = self.revision_frontier();
        let rows = Self::rows_at_basis(rows, basis);
        let root = if let Some(capability) = self.view.capability() {
            ViewRoot::new_checked(id, basis, frontier, rows, coverage, capability)?
        } else {
            ViewRoot::new_incomplete(id, basis, frontier, rows, coverage)?
        };
        let next = next.map(|cursor| {
            let next = Cursor::for_view(
                root.recipe,
                root.version,
                Frontier::new(
                    cursor.branch(),
                    cursor.log(),
                    cursor.schema(),
                    root.root,
                    cursor.sequence(),
                ),
            );
            next_offset.map_or(next, |offset| next.with_query_offset(offset as u64))
        });
        Ok(ViewSnapshot {
            root,
            freshness: Freshness::Current,
            next,
            graph_relations: None,
        })
    }

    fn query_recipe(&self, recipe: &[u8], read_manifest: Option<&ReadManifest>) -> ViewRecipeId {
        let manifest_bytes = read_manifest.map_or_else(
            || vec![0],
            |manifest| {
                let mut bytes = Vec::with_capacity(manifest.canonical_bytes().len() + 1);
                bytes.push(1);
                bytes.extend_from_slice(&manifest.canonical_bytes());
                bytes
            },
        );
        self.query_recipe_with_manifest(recipe, &manifest_bytes)
    }

    fn query_recipe_with_manifest(&self, recipe: &[u8], manifest_bytes: &[u8]) -> ViewRecipeId {
        view_identity_bytes(&[
            b"query",
            recipe,
            self.revision_root().as_bytes(),
            manifest_bytes,
        ])
    }

    fn check_query_cursor(
        &self,
        cursor: Option<Cursor>,
        recipe: ViewRecipeId,
        limit: usize,
        fetch: impl Fn(usize) -> ArrangementPage,
    ) -> Result<usize, LibraryError> {
        let Some(cursor) = cursor else {
            return Ok(0);
        };
        if cursor.recipe() != recipe
            || cursor.branch() != self.cursor.branch()
            || cursor.log() != self.cursor.log()
            || cursor.schema() != self.cursor.schema()
            || cursor.sequence() > self.cursor.sequence()
        {
            return Err(LibraryError::CursorMismatch);
        }
        let offset =
            usize::try_from(cursor.query_offset()).map_err(|_| LibraryError::CursorMismatch)?;
        if offset == 0 && cursor.version() == self.view.version && cursor.root() == self.view.root {
            return Ok(0);
        }
        let page_size = limit;
        if offset < page_size {
            return Err(LibraryError::CursorMismatch);
        }
        let previous_start = offset - page_size;
        let previous_page = fetch(previous_start);
        if previous_page.ids.len() != page_size {
            return Err(LibraryError::CursorMismatch);
        }
        let previous_rows = previous_page
            .ids
            .iter()
            .filter_map(|&id| self.view.row(id))
            .collect::<Vec<_>>();
        let basis = self.revision_basis();
        let frontier = self.revision_frontier();
        let previous_rows = Self::rows_at_basis(previous_rows, basis);
        let expected = if let Some(capability) = self.view.capability() {
            ViewRoot::new_checked(
                recipe,
                basis,
                frontier,
                previous_rows,
                self.view.coverage.to_vec(),
                capability,
            )
            .map_err(LibraryError::View)?
        } else {
            ViewRoot::new_incomplete(
                recipe,
                basis,
                frontier,
                previous_rows,
                self.view.coverage.to_vec(),
            )
            .map_err(LibraryError::View)?
        };
        if cursor.version() != expected.version() || cursor.root() != expected.root() {
            return Err(LibraryError::CursorMismatch);
        }
        Ok(offset)
    }

    fn revision_basis(&self) -> crate::Basis {
        crate::Basis {
            root: self.view.root,
            ..self.view.basis
        }
    }

    fn revision_frontier(&self) -> Frontier {
        let basis = self.revision_basis();
        Frontier::new(
            basis.branch,
            basis.log,
            basis.schema,
            basis.root,
            self.view.frontier.sequence,
        )
    }

    fn rows_at_basis(mut rows: Vec<Row>, basis: crate::Basis) -> Vec<Row> {
        for row in &mut rows {
            row.basis = basis;
        }
        rows
    }
}
