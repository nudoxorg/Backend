//! Bounded catalog projections and query cursor admission.

use super::Library;
use crate::arrangement::ArrangementPage;
use crate::{
    Cursor, Document, DocumentQuery, Freshness, Frontier, GraphQuery, LibraryError, NameQuery,
    Outline, OutlineNode, OutlineQuery, PackageKey, Query, QueryLimit, ReadManifest, Row, RowId,
    SymbolKey, ViewRecipeId, ViewRevision, ViewRoot, ViewSnapshot, ViewStateRoot,
    view_identity_bytes,
};
use std::collections::BTreeSet;

impl Library {
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
        self.work.record_seek();
        let row = self
            .arrangement
            .document(query.symbol)
            .then(|| self.view.row(RowId::Symbol(query.symbol)))
            .flatten()
            .ok_or(LibraryError::NotFound)?;
        let mut document = Document::new(query.symbol, basis, row.document.clone())
            .with_source_basis(self.revision_basis());
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
            self.arrangement
                .names_page(query.text(), offset, limit, |id| self.view.row(id))
        })?;
        let page = self
            .arrangement
            .names_page(query.text(), start, limit, |id| self.view.row(id));
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
        let mut symbols = self
            .arrangement
            .package_symbols_page(query.package, usize::from(QueryLimit::MAX))
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
        let root_index = symbols
            .iter()
            .position(|symbol| {
                self.row_for_id(RowId::Symbol(*symbol))
                    .is_some_and(|row| row.parent.is_none())
            })
            .unwrap_or(0);
        let symbol = symbols.remove(root_index);
        let mut budget = usize::from(QueryLimit::MAX).saturating_sub(1);
        let mut path = BTreeSet::new();
        path.insert(symbol);
        Ok(Outline::new(
            query.package,
            basis,
            OutlineNode {
                symbol,
                children: self.outline_children(query.package, symbol, &mut budget, 0, &mut path),
            },
        )
        .with_source_basis(self.revision_basis()))
    }

    fn outline_children(
        &self,
        package: PackageKey,
        parent: SymbolKey,
        budget: &mut usize,
        depth: usize,
        path: &mut BTreeSet<SymbolKey>,
    ) -> Box<[OutlineNode]> {
        // Outline replies have no independent continuation shape, so enforce
        // a hard node/depth budget at the arrangement boundary. A malformed
        // cyclic parent relation is cut at the path edge rather than causing
        // unbounded recursion or a whole-view scan.
        const MAX_DEPTH: usize = 64;
        if *budget == 0 || depth >= MAX_DEPTH {
            return Box::new([]);
        }
        let page = self
            .arrangement
            .children_page(package, Some(parent), *budget);
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
            let nested = self.outline_children(package, symbol, budget, depth + 1, path);
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
            self.arrangement
                .search_page(query.text(), offset, limit, |id| self.view.row(id))
        })?;
        let page = self
            .arrangement
            .search_page(query.text(), start, limit, |id| self.view.row(id));
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
    pub fn graph(&self, query: GraphQuery) -> Result<ViewSnapshot, LibraryError> {
        self.check_basis(query.basis())?;
        let symbol = query.symbol();
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
