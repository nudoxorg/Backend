//! `ProjectionArrangement` construction and incremental update entry points.
use super::*;

impl ProjectionArrangement {
    /// Creates an empty arrangement without a fallible constructor or panic.
    pub(crate) fn empty() -> Self {
        Self {
            documents: None,
            packages: None,
            unscoped_symbols: None,
            names: None,
            name_postings: None,
            search_postings: None,
            package_symbols: None,
            children: None,
        }
    }

    pub(crate) fn build(view: &ViewRoot, work: &WorkCounters) -> Result<Self, ArrangementError> {
        let arrangement = Self::build_inner(view)?;
        work.record_indexed(usize::try_from(view.row_count()).unwrap_or(usize::MAX));
        Ok(arrangement)
    }

    /// Applies one committed row change by path-copying only affected
    /// relation keys. Untouched canonical nodes and posting values remain
    /// shared through the persistent tree's retained arcs.
    pub(crate) fn update(
        previous: &ProjectionArrangement,
        view: &ViewRoot,
        delta: &CommittedViewDelta,
        work: &WorkCounters,
    ) -> Result<Self, ArrangementError> {
        match &delta.delta {
            ViewDelta::Coverage { .. } => previous.advance_all(view.flow_frontier().frontier()),
            ViewDelta::Upsert { row } => {
                let base = delta.base_for_wire();
                let old = base.row(row.id);
                let arrangement = previous.update_one(
                    base,
                    view,
                    old.as_ref(),
                    Some(row),
                    view.flow_frontier().frontier(),
                    work,
                )?;
                work.record_indexed(1);
                Ok(arrangement)
            }
            ViewDelta::Remove { id } => {
                let base = delta.base_for_wire();
                let old = base.row(*id);
                let arrangement = previous.update_one(
                    base,
                    view,
                    old.as_ref(),
                    None,
                    view.flow_frontier().frontier(),
                    work,
                )?;
                work.record_indexed(1);
                Ok(arrangement)
            }
            ViewDelta::Patch { changes } => {
                let base = delta.base_for_wire();
                let mut arrangement = previous.clone();
                for change in changes.iter() {
                    arrangement = match change {
                        RowChange::Upsert(row) => {
                            let old = base.row(row.id);
                            arrangement.update_one(
                                base,
                                view,
                                old.as_ref(),
                                Some(row.as_ref()),
                                view.flow_frontier().frontier(),
                                work,
                            )?
                        }
                        RowChange::Remove(id) => {
                            let old = base.row(*id);
                            arrangement.update_one(
                                base,
                                view,
                                old.as_ref(),
                                None,
                                view.flow_frontier().frontier(),
                                work,
                            )?
                        }
                    };
                }
                work.record_indexed(changes.len());
                Ok(arrangement)
            }
            ViewDelta::Reset { .. } => {
                let arrangement = Self::build_inner(view)?;
                work.record_indexed(delta.changed_row_count());
                Ok(arrangement)
            }
        }
    }

    fn build_inner(view: &ViewRoot) -> Result<Self, ArrangementError> {
        let coverage = view.relation_coverage();
        let frontier = view.flow_frontier().frontier();
        let mut documents = Vec::new();
        let mut packages = Vec::new();
        let mut unscoped_symbols = Vec::new();
        let mut names = Vec::new();
        let mut name_postings = Vec::new();
        let mut search_postings = Vec::new();
        let mut package_symbols = Vec::new();
        let mut children = Vec::new();

        for row in view.iter_rows() {
            match row.id {
                RowId::Package(_) => packages.push(row.id),
                RowId::Symbol(_) => {
                    documents.push(row.id);
                    if row.package.is_none() {
                        unscoped_symbols.push(row.id);
                    }
                    let key = name_key(row);
                    if !is_synthetic_result_slot(row, |id| view.row(id)) {
                        names.push(key.clone());
                        for gram in search_grams(&key.normalized) {
                            name_postings.push(NamePostingKey {
                                gram,
                                id: Some(key.id),
                            });
                        }
                        for gram in search_grams(&searchable_text(row)) {
                            search_postings.push(SearchPostingKey {
                                gram,
                                id: Some(row.id),
                            });
                        }
                    }
                    if let Some(package) = row.package {
                        package_symbols.push(PackageRowKey {
                            package,
                            id: Some(row.id),
                        });
                        children.push(ChildRowKey {
                            package,
                            parent: row.parent,
                            id: Some(row.id),
                        });
                    }
                }
                RowId::Object(_) => {}
            }
        }

        documents.sort_unstable();
        packages.sort_unstable();
        names.sort_unstable();
        name_postings.sort_unstable();
        name_postings.dedup();
        search_postings.sort_unstable();
        search_postings.dedup();
        package_symbols.sort_unstable();
        package_symbols.dedup();
        children.sort_unstable();
        children.dedup();

        Ok(Self {
            documents: Some(unit_tree::<DocumentIndexRelation>(
                documents,
                coverage,
                frontier.clone(),
            )?),
            packages: Some(unit_tree::<PackageIndexRelation>(
                packages,
                coverage,
                frontier.clone(),
            )?),
            unscoped_symbols: Some(unit_tree::<UnscopedIndexRelation>(
                unscoped_symbols,
                coverage,
                frontier.clone(),
            )?),
            names: Some(unit_tree::<NameIndexRelation>(
                names,
                coverage,
                frontier.clone(),
            )?),
            name_postings: Some(unit_tree::<NamePostingRelation>(
                name_postings,
                coverage,
                frontier.clone(),
            )?),
            search_postings: Some(unit_tree::<SearchPostingRelation>(
                search_postings,
                coverage,
                frontier.clone(),
            )?),
            package_symbols: Some(unit_tree::<PackageSymbolsRelation>(
                package_symbols,
                coverage,
                frontier.clone(),
            )?),
            children: Some(unit_tree::<ChildrenRelation>(children, coverage, frontier)?),
        })
    }

    fn advance_all(&self, frontier: backend_flow::Frontier) -> Result<Self, ArrangementError> {
        let mut next = self.clone();
        next.documents = Some(
            required(self.documents.as_ref())?
                .advance(frontier.clone())
                .map_err(|_| ArrangementError(TreeError::InvalidRoot))?,
        );
        next.packages = Some(
            required(self.packages.as_ref())?
                .advance(frontier.clone())
                .map_err(|_| ArrangementError(TreeError::InvalidRoot))?,
        );
        next.unscoped_symbols = Some(
            required(self.unscoped_symbols.as_ref())?
                .advance(frontier.clone())
                .map_err(|_| ArrangementError(TreeError::InvalidRoot))?,
        );
        next.names = Some(
            required(self.names.as_ref())?
                .advance(frontier.clone())
                .map_err(|_| ArrangementError(TreeError::InvalidRoot))?,
        );
        next.name_postings = Some(
            required(self.name_postings.as_ref())?
                .advance(frontier.clone())
                .map_err(|_| ArrangementError(TreeError::InvalidRoot))?,
        );
        next.search_postings = Some(
            required(self.search_postings.as_ref())?
                .advance(frontier.clone())
                .map_err(|_| ArrangementError(TreeError::InvalidRoot))?,
        );
        next.package_symbols = Some(
            required(self.package_symbols.as_ref())?
                .advance(frontier.clone())
                .map_err(|_| ArrangementError(TreeError::InvalidRoot))?,
        );
        next.children = Some(
            required(self.children.as_ref())?
                .advance(frontier)
                .map_err(|_| ArrangementError(TreeError::InvalidRoot))?,
        );
        Ok(next)
    }

    fn update_one(
        &self,
        base: &ViewRoot,
        view: &ViewRoot,
        old: Option<&Row>,
        new: Option<&Row>,
        frontier: backend_flow::Frontier,
        work: &WorkCounters,
    ) -> Result<Self, ArrangementError> {
        let mut next = self.clone();
        let documents = required(self.documents.as_ref())?;
        let packages = required(self.packages.as_ref())?;
        let names = required(self.names.as_ref())?;
        let name_postings = required(self.name_postings.as_ref())?;
        let search_postings = required(self.search_postings.as_ref())?;
        let package_symbols = required(self.package_symbols.as_ref())?;
        let children = required(self.children.as_ref())?;

        next.documents = Some(update_unit(
            documents,
            old.filter(|row| matches!(row.id, RowId::Symbol(_)))
                .map(|row| row.id),
            new.filter(|row| matches!(row.id, RowId::Symbol(_)))
                .map(|row| row.id),
            frontier.clone(),
            work,
        )?);
        next.packages = Some(update_unit(
            packages,
            old.filter(|row| matches!(row.id, RowId::Package(_)))
                .map(|row| row.id),
            new.filter(|row| matches!(row.id, RowId::Package(_)))
                .map(|row| row.id),
            frontier.clone(),
            work,
        )?);
        next.unscoped_symbols = Some(update_unit(
            required(self.unscoped_symbols.as_ref())?,
            old.filter(|row| matches!(row.id, RowId::Symbol(_)) && row.package.is_none())
                .map(|row| row.id),
            new.filter(|row| matches!(row.id, RowId::Symbol(_)) && row.package.is_none())
                .map(|row| row.id),
            frontier.clone(),
            work,
        )?);
        // `old_searchable`/`new_searchable` additionally drop a synthetic
        // result-slot row from name/search admission, matching
        // `build_inner`, so an incrementally-updated arrangement and a
        // freshly rebuilt one agree on what a name search returns. The
        // parent lookup reads whichever view the row itself came from: the
        // pre-delta `base` for an old row, the current `view` for a new one.
        // `package_symbols`/`children` keep using the unfiltered `old_name`/
        // `new_name`: the row is still a real, addressable member of its
        // package and parentage tree, it just isn't independently
        // name-searchable.
        let old_name = old.filter(|row| matches!(row.id, RowId::Symbol(_)));
        let new_name = new.filter(|row| matches!(row.id, RowId::Symbol(_)));
        let old_searchable =
            old_name.filter(|row| !is_synthetic_result_slot(row, |id| base.row(id)));
        let new_searchable =
            new_name.filter(|row| !is_synthetic_result_slot(row, |id| view.row(id)));
        next.names = Some(update_unit(
            names,
            old_searchable.map(name_key),
            new_searchable.map(name_key),
            frontier.clone(),
            work,
        )?);

        next.name_postings = Some(update_name_postings(
            name_postings,
            old_searchable,
            new_searchable,
            frontier.clone(),
            work,
        )?);
        next.search_postings = Some(update_search_postings(
            search_postings,
            old_searchable,
            new_searchable,
            frontier.clone(),
            work,
        )?);
        next.package_symbols = Some(update_package_membership(
            package_symbols,
            old_name,
            new_name,
            frontier.clone(),
            work,
        )?);
        next.children = Some(update_children_membership(
            children, old_name, new_name, frontier, work,
        )?);
        Ok(next)
    }
}
