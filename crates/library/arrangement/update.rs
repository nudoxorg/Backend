//! Checked path-copy maintenance for library materialized indexes.

use super::{
    ArrangementError, ChildRowKey, ChildrenRelation, ChildrenTree, DocumentIndexRelation,
    NameIndexRelation, NameKey, NamePostingKey, NamePostingRelation, NamePostingTree,
    PackageIndexRelation, PackageRowKey, PackageSymbolsRelation, PackageSymbolsTree,
    ProjectionArrangement, SearchPostingKey, SearchPostingRelation, SearchPostingTree,
    UnscopedIndexRelation, WorkCounters, search_grams, searchable_text,
};
use crate::{CommittedViewDelta, DeclarationKind, Row, RowChange, RowId, ViewDelta, ViewRoot};
use backend_flow::MaterializedIndex;
use backend_version::{CoverageWitness, Relation, TreeError};
use std::collections::BTreeMap;

mod apply;

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
        let normalized = old.label.to_lowercase();
        for gram in search_grams(&normalized) {
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
        let normalized = new.label.to_lowercase();
        for gram in search_grams(&normalized) {
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

fn update_search_postings(
    tree: &SearchPostingTree,
    old: Option<&Row>,
    new: Option<&Row>,
    frontier: backend_flow::Frontier,
    work: &WorkCounters,
) -> Result<SearchPostingTree, ArrangementError> {
    let mut changes = BTreeMap::new();
    if let Some(old) = old {
        for gram in search_grams(&searchable_text(old)) {
            changes.insert(
                SearchPostingKey {
                    gram,
                    id: Some(old.id),
                },
                None,
            );
        }
    }
    if let Some(new) = new {
        for gram in search_grams(&searchable_text(new)) {
            changes.insert(
                SearchPostingKey {
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

/// Returns whether `row` is a producer-minted synthetic child fact that
/// exists only to give a function's return-type "result slot" a
/// content-addressed identity (see
/// `crates/engine/src/driver/lower/{python,rust}.rs`), rather than a real,
/// independently searchable declaration.
///
/// A real declaration can coincidentally share its exact spelling with its
/// parent -- a constructor (`class Foo { Foo() {} }`), a Rust
/// `mod foo { pub fn foo() }`, a method named like its class -- so name
/// sharing alone is not a safe signal; those must stay searchable. What is
/// unique to the synthetic slot is its *kind*: it is the only
/// `DeclarationKind::Variable` row whose immediate parent is itself callable
/// (a function or method). A real field, constructor, or nested function
/// never presents as `Variable`, so requiring that kind and a callable
/// parent, on top of the name match, narrows this to exactly the synthetic
/// result slot.
///
/// `row_at` looks the parent up in whichever view `row` itself was read
/// from (the pre-delta base for an old row, the current view for a new
/// one), so an incremental update and a full rebuild agree.
fn is_synthetic_result_slot(row: &Row, row_at: impl Fn(RowId) -> Option<Row>) -> bool {
    row.kind == Some(DeclarationKind::Variable)
        && row
            .parent
            .and_then(|parent| row_at(RowId::Symbol(parent)))
            .is_some_and(|parent_row| {
                name_key(&parent_row).normalized == name_key(row).normalized
                    && matches!(
                        parent_row.kind,
                        Some(DeclarationKind::Function | DeclarationKind::Method)
                    )
            })
}
