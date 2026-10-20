use crate::caches::dependency_index::{self, DependencyIndex, DependencyIndexState};
use crate::caches::shared_instantiation::collect_eval_entry_def_dependencies;
use crate::def::{DefId, DefinitionStore};
use crate::evaluation::request::EvaluationCacheKey;
use crate::intern::TypeInterner;
use crate::types::TypeId;
use rustc_hash::FxHashMap;
use std::cell::RefCell;

pub(super) type EvalDependencyIndexState = DependencyIndexState<EvaluationCacheKey>;
pub(super) type EvalDependencyIndex = DependencyIndex<EvaluationCacheKey>;

pub(super) fn record_dependencies(
    interner: &TypeInterner,
    definition_store: Option<&DefinitionStore>,
    index: &EvalDependencyIndex,
    key: EvaluationCacheKey,
    old_result: Option<TypeId>,
    result: TypeId,
) {
    // Without a DefinitionStore, immutable key/result types determine every
    // dependency. A store-backed write must still refresh its edges: the same
    // TypeIds can now reach a different body-dependency graph.
    if definition_store.is_none() && old_result == Some(result) {
        return;
    }
    let deps = collect_eval_entry_def_dependencies(interner, definition_store, key, result);
    dependency_index::record_dependencies(index, key, old_result.is_some(), deps);
}

pub(super) fn invalidate_for_def(
    index: &EvalDependencyIndex,
    cache: &RefCell<FxHashMap<EvaluationCacheKey, TypeId>>,
    def_id: DefId,
) {
    dependency_index::invalidate_for_def(index, cache, def_id);
}

/// Drop `key`'s dependency edges after the caller has removed it from the cache
/// directly (single-entry eviction, not `DefId`-scoped invalidation).
pub(super) fn forget_key(index: &EvalDependencyIndex, key: &EvaluationCacheKey) {
    dependency_index::forget_key(index, key);
}
