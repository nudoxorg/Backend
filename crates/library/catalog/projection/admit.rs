//! Query cursor admission and snapshot construction for catalog projections.

use super::super::Library;
use crate::arrangement::ArrangementPage;
use crate::{
    Cursor, Freshness, Frontier, LibraryError, ReadManifest, Row, RowId, ViewRecipeId,
    ViewRevision, ViewRoot, ViewSnapshot, ViewStateRoot, view_identity_bytes,
};

impl Library {
    pub(super) fn check_basis(&self, basis: ViewRevision) -> Result<ViewStateRoot, LibraryError> {
        let expected = self.revision_root();
        if !basis.matches(expected) {
            return Err(LibraryError::WrongBasis {
                expected,
                observed: basis,
            });
        }
        Ok(expected)
    }

    pub(in crate::catalog) fn row_for_id(&self, id: RowId) -> Option<Row> {
        self.view.row(id)
    }

    pub(in crate::catalog) fn snapshot_for(
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
        let next_offset = next_offset
            .map(u64::try_from)
            .transpose()
            .map_err(|_| LibraryError::CursorMismatch)?;
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
            next_offset.map_or(next, |offset| next.with_query_offset(offset))
        });
        Ok(ViewSnapshot {
            root,
            freshness: Freshness::Current,
            next,
            graph_relations: None,
            rich_graph: None,
        })
    }

    pub(super) fn query_recipe(
        &self,
        recipe: &[u8],
        read_manifest: Option<&ReadManifest>,
    ) -> ViewRecipeId {
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

    pub(super) fn check_query_cursor(
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

    pub(super) fn revision_basis(&self) -> crate::Basis {
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
