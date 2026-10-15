use crate::caches::dependency_index::{self, DependencyIndex, DependencyIndexState};
use crate::caches::shared_instantiation::collect_application_eval_entry_def_dependencies;
use crate::caches::shared_query_cache::ApplicationEvalCacheKey;
use crate::def::{DefId, DefinitionStore};
use crate::intern::TypeInterner;
use crate::types::TypeId;
use rustc_hash::FxHashMap;
use std::cell::RefCell;

pub(super) type ApplicationEvalDependencyIndexState = DependencyIndexState<ApplicationEvalCacheKey>;
pub(super) type ApplicationEvalDependencyIndex = DependencyIndex<ApplicationEvalCacheKey>;

pub(super) fn record_dependencies(
    interner: &TypeInterner,
    definition_store: Option<&DefinitionStore>,
    index: &ApplicationEvalDependencyIndex,
    key: &ApplicationEvalCacheKey,
    old_result: Option<TypeId>,
    result: TypeId,
) {
    // Writing the value the entry already holds changes none of its dependency
    // edges: they were derived from this same key and result when it was first
    // stored, and a definition whose body changed since would have dropped the entry
    // (making `old_result` `None`). A fresh evaluator re-commits its whole cache, so
    // almost every write of a long-lived project is such a rewrite, and re-deriving
    // the transitive definition closure of each one dominated JSX-heavy files.
    if old_result == Some(result) {
        return;
    }
    let deps =
        collect_application_eval_entry_def_dependencies(interner, definition_store, key, result);
    dependency_index::record_dependencies(index, key.clone(), old_result.is_some(), deps);
}

pub(super) fn invalidate_for_def(
    index: &ApplicationEvalDependencyIndex,
    cache: &RefCell<FxHashMap<ApplicationEvalCacheKey, TypeId>>,
    def_id: DefId,
) {
    dependency_index::invalidate_for_def(index, cache, def_id);
}

#[cfg(test)]
pub(super) fn key_count(index: &ApplicationEvalDependencyIndex, def_id: DefId) -> usize {
    index
        .borrow()
        .reverse
        .get(&def_id)
        .map_or(0, rustc_hash::FxHashSet::len)
}
