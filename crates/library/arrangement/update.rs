//! Checked path-copy maintenance for library materialized indexes.

use super::{
    ArrangementError, ChildRowKey, ChildrenRelation, ChildrenTree, DocumentIndexRelation,
    NameIndexRelation, NameKey, NamePostingKey, NamePostingRelation, NamePostingTree,
    PackageIndexRelation, PackageRowKey, PackageSymbolsRelation, PackageSymbolsTree,
    ProjectionArrangement, UnscopedIndexRelation, WorkCounters, trigrams,
};
use crate::{CommittedViewDelta, Row, RowChange, RowId, ViewDelta, ViewRoot};
use backend_flow::MaterializedIndex;
use backend_version::{CoverageWitness, Relation, TreeError};
use std::collections::BTreeMap;

impl ProjectionArrangement {
    /// Creates an empty arrangement without a fallible constructor or panic.
    pub(crate) fn empty() -> Self {
        Self {
            documents: None,
            packages: None,
            unscoped_symbols: None,
            names: None,
            name_postings: None,
            package_symbols: None,
            children: None,
        }
    }

    pub(crate) fn build(view: &ViewRoot, work: &WorkCounters) -> Result<Self, ArrangementError> {
        let arrangement = Self::build_inner(view)?;
        work.record_indexed(view.rows().len());
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
                                old.as_ref(),
                                Some(row.as_ref()),
                                view.flow_frontier().frontier(),
                                work,
                            )?
                        }
                        RowChange::Remove(id) => {
                            let old = base.row(*id);
                            arrangement.update_one(
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
        let rows = view.rows();
        let coverage = view.relation_coverage();
        let frontier = view.flow_frontier().frontier();
        let mut documents = Vec::new();
        let mut packages = Vec::new();
        let mut unscoped_symbols = Vec::new();
        let mut names = Vec::new();
        let mut name_postings = Vec::new();
        let mut package_symbols = Vec::new();
        let mut children = Vec::new();

        for row in rows {
            match row.id {
                RowId::Package(_) => packages.push(row.id),
                RowId::Symbol(_) => {
                    documents.push(row.id);
                    if row.package.is_none() {
                        unscoped_symbols.push(row.id);
                    }
                    let key = name_key(row);
                    names.push(key.clone());
                    // Search spans the complete retained row projection,
                    // rather than only the display name.  Keeping grams for
                    // the signature and documentation makes the selective
                    // posting seek a sound candidate filter for full-text
                    // queries; the query path still verifies the complete
                    // projection before returning a row.
                    for gram in trigrams(&super::searchable_text(row)) {
                        name_postings.push(NamePostingKey {
                            gram,
                            id: Some(key.id),
                        });
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
        next.names = Some(update_unit(
            names,
            old.filter(|row| matches!(row.id, RowId::Symbol(_)))
                .map(name_key),
            new.filter(|row| matches!(row.id, RowId::Symbol(_)))
                .map(name_key),
            frontier.clone(),
            work,
        )?);

        let old_name = old.filter(|row| matches!(row.id, RowId::Symbol(_)));
        let new_name = new.filter(|row| matches!(row.id, RowId::Symbol(_)));
        next.name_postings = Some(update_name_postings(
            name_postings,
            old_name,
            new_name,
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
pub(super) fn required<R: Relation>(
    tree: Option<&MaterializedIndex<R>>,
) -> Result<&MaterializedIndex<R>, ArrangementError> {
    tree.ok_or(ArrangementError(TreeError::InvalidRoot))
}

fn unit_tree<R>(
    mut keys: Vec<R::Key>,
    coverage: CoverageWitness,
    frontier: backend_flow::Frontier,
) -> Result<MaterializedIndex<R>, ArrangementError>
where
    R: Relation<Value = ()>,
{
    keys.sort_unstable();
    let items = keys.into_iter().map(|key| (key, ())).collect::<Vec<_>>();
    MaterializedIndex::from_entries(items, coverage, frontier)
        .map_err(|_| ArrangementError(TreeError::InvalidRoot))
}

fn update_unit<R>(
    tree: &MaterializedIndex<R>,
    old: Option<R::Key>,
    new: Option<R::Key>,
    frontier: backend_flow::Frontier,
    work: &WorkCounters,
) -> Result<MaterializedIndex<R>, ArrangementError>
where
    R: Relation<Value = ()>,
{
    if old == new {
        return tree
            .advance(frontier)
            .map_err(|_| ArrangementError(TreeError::InvalidRoot));
    }
    let mut changes = BTreeMap::new();
    if let Some(old) = old.filter(|key| tree.get(key).is_some()) {
        changes.insert(old, None);
    }
    if let Some(new) = new.filter(|key| tree.get(key).is_none()) {
        changes.insert(new, Some(()));
    }
    apply_changes(tree, changes, frontier, work)
}

fn update_name_postings(
    tree: &NamePostingTree,
    old: Option<&Row>,
    new: Option<&Row>,
    frontier: backend_flow::Frontier,
    work: &WorkCounters,
) -> Result<NamePostingTree, ArrangementError> {
    let mut changes = BTreeMap::new();
    if let Some(old) = old {
        for gram in trigrams(&super::searchable_text(old)) {
            changes.insert(
                NamePostingKey {
                    gram,
                    id: Some(old.id),
                },
                None,
            );
        }
    }
    if let Some(new) = new {
        for gram in trigrams(&super::searchable_text(new)) {
            changes.insert(
                NamePostingKey {
                    gram,
                    id: Some(new.id),
                },
                Some(()),
            );
        }
    }
    apply_changes(tree, changes, frontier, work)
}

fn update_package_membership(
    tree: &PackageSymbolsTree,
    old: Option<&Row>,
    new: Option<&Row>,
    frontier: backend_flow::Frontier,
    work: &WorkCounters,
) -> Result<PackageSymbolsTree, ArrangementError> {
    let mut changes = BTreeMap::new();
    if let Some(old_row) = old
        && let Some(package) = old_row.package
    {
        changes.insert(
            PackageRowKey {
                package,
                id: Some(old_row.id),
            },
            None,
        );
    }
    if let Some(new_row) = new
        && let Some(package) = new_row.package
    {
        changes.insert(
            PackageRowKey {
                package,
                id: Some(new_row.id),
            },
            Some(()),
        );
    }
    apply_changes(tree, changes, frontier, work)
}

fn update_children_membership(
    tree: &ChildrenTree,
    old: Option<&Row>,
    new: Option<&Row>,
    frontier: backend_flow::Frontier,
    work: &WorkCounters,
) -> Result<ChildrenTree, ArrangementError> {
    let mut changes = BTreeMap::new();
    if let Some(old_row) = old
        && let Some(package) = old_row.package
    {
        changes.insert(
            ChildRowKey {
                package,
                parent: old_row.parent,
                id: Some(old_row.id),
            },
            None,
        );
    }
    if let Some(new_row) = new
        && let Some(package) = new_row.package
    {
        changes.insert(
            ChildRowKey {
                package,
                parent: new_row.parent,
                id: Some(new_row.id),
            },
            Some(()),
        );
    }
    apply_changes(tree, changes, frontier, work)
}

fn apply_changes<R>(
    tree: &MaterializedIndex<R>,
    changes: BTreeMap<R::Key, Option<R::Value>>,
    frontier: backend_flow::Frontier,
    work: &WorkCounters,
) -> Result<MaterializedIndex<R>, ArrangementError>
where
    R: Relation,
{
    if changes.is_empty() {
        return tree
            .advance(frontier)
            .map_err(|_| ArrangementError(TreeError::InvalidRoot));
    }
    let changes = changes
        .into_iter()
        .map(|(key, after)| backend_version::MapChange {
            before: tree.get(&key).cloned(),
            key,
            after,
        })
        .collect::<Vec<_>>();
    let (prepared, index_work) = tree
        .prepare(changes, frontier)
        .map_err(|_| ArrangementError(TreeError::InvalidRoot))?;
    work.record_materialized(index_work);
    prepared
        .commit(tree)
        .map(|(index, _)| index)
        .map_err(|_| ArrangementError(TreeError::InvalidRoot))
}

fn name_key(row: &Row) -> NameKey {
    NameKey {
        normalized: row
            .label
            .rsplit("::")
            .next()
            .unwrap_or(&row.label)
            .to_lowercase(),
        id: row.id,
    }
}
