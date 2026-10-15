//! Immutable per-program facts shared by every checker context of one project image.
//!
//! A project check or query session builds one checker per file, and each checker
//! spawns short-lived child contexts for cross-file delegation. Anything those
//! contexts need to know about the *whole program* (does any file augment a
//! module, which names are ambient modules, what every request to one module
//! specifier resolves to) is the same for all of them. Recomputing it per context,
//! or per call, makes one lookup cost work proportional to the program's file
//! count; a program with thousands of declaration files then spends its time
//! re-deriving answers that cannot change. The facts below are computed once,
//! when the sealed program indexes are built, and shared by `Arc`.

use std::sync::{Arc, RwLock, Weak};

use rustc_hash::{FxHashMap, FxHashSet};
use tsz_binder::BinderState;

use super::{ProjectModuleConsensus, ResolvedModuleRequestOutcomeMap};

/// Program-wide answers that depend only on the sealed binders and the exact
/// compiler-owned module-resolution outcome map of one program.
pub struct SharedProgramFacts {
    has_module_augmentations: bool,
    has_global_augmentations: bool,
    /// Names that any binder declares as an ambient module or augments. A re-export
    /// owner spelled like one of these cannot be told apart from a file by its key.
    ambient_module_names: FxHashSet<String>,
    /// Retains the exact outcome allocation's identity without retaining the
    /// program itself. The weak reference prevents address reuse from making
    /// another outcome map look like the one that governed these answers.
    consensus_outcomes: RwLock<Option<Weak<ResolvedModuleRequestOutcomeMap>>>,
    /// `(source file, specifier)` -> agreed outcome of every explicit request.
    consensus: RwLock<FxHashMap<usize, FxHashMap<String, Option<ProjectModuleConsensus>>>>,
}

impl SharedProgramFacts {
    /// Derives the program-wide facts from every binder, once.
    #[must_use]
    pub fn from_binders(binders: &[Arc<BinderState>]) -> Arc<Self> {
        let mut has_module_augmentations = false;
        let mut has_global_augmentations = false;
        let mut ambient_module_names: FxHashSet<String> = FxHashSet::default();
        // Binders usually share one program-level set by `Arc`; walk each distinct
        // set once instead of once per binder.
        let mut seen_declared: FxHashSet<usize> = FxHashSet::default();
        let mut seen_augmentations: FxHashSet<usize> = FxHashSet::default();
        for binder in binders {
            if !binder.module_augmentations.is_empty()
                || !binder.augmentation_target_modules.is_empty()
            {
                has_module_augmentations = true;
            }
            if !binder.global_augmentations.is_empty() {
                has_global_augmentations = true;
            }
            if seen_declared.insert(Arc::as_ptr(&binder.declared_modules) as usize) {
                ambient_module_names.extend(binder.declared_modules.iter().cloned());
            }
            if seen_augmentations.insert(Arc::as_ptr(&binder.module_augmentations) as usize) {
                ambient_module_names.extend(binder.module_augmentations.keys().cloned());
            }
        }
        Arc::new(Self {
            has_module_augmentations,
            has_global_augmentations,
            ambient_module_names,
            consensus_outcomes: RwLock::new(None),
            consensus: RwLock::new(FxHashMap::default()),
        })
    }

    /// Whether any binder of the program registers a module augmentation or an
    /// augmentation target.
    #[must_use]
    pub fn has_module_augmentations(&self) -> bool {
        self.has_module_augmentations
    }

    /// Whether any binder of the program registers a `declare global` augmentation.
    #[must_use]
    pub fn has_global_augmentations(&self) -> bool {
        self.has_global_augmentations
    }

    /// Whether some binder declares or augments an ambient module with this exact name.
    #[must_use]
    pub fn is_ambient_module_name(&self, name: &str) -> bool {
        self.ambient_module_names.contains(name)
    }

    /// Binds the memo to `outcomes` (by identity) and reports whether this caller
    /// may use it. The first caller wins; a context holding another map is refused.
    fn accepts_outcomes(&self, outcomes: &Arc<ResolvedModuleRequestOutcomeMap>) -> bool {
        let bound = self.consensus_outcomes.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(bound) = bound.as_ref() {
            return std::ptr::eq(bound.as_ptr(), Arc::as_ptr(outcomes));
        }
        drop(bound);
        let mut bound = self.consensus_outcomes.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        let bound = bound.get_or_insert_with(|| Arc::downgrade(outcomes));
        std::ptr::eq(bound.as_ptr(), Arc::as_ptr(outcomes))
    }

    /// The memoized consensus for one pair: `None` when this pair has not been
    /// computed (or the memo belongs to another outcome map), `Some(answer)` when it
    /// has.
    #[must_use]
    pub fn consensus_get(
        &self,
        outcomes: &Arc<ResolvedModuleRequestOutcomeMap>,
        source_file_idx: usize,
        specifier: &str,
    ) -> Option<Option<ProjectModuleConsensus>> {
        if !self.accepts_outcomes(outcomes) {
            return None;
        }
        let memo = self
            .consensus
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        memo.get(&source_file_idx)?.get(specifier).copied()
    }

    /// Records the consensus of one pair. Ignored when the memo belongs to another
    /// outcome map. A caller must not record an answer computed while execution was
    /// stopped: such an answer is an artifact of the stop.
    pub fn consensus_insert(
        &self,
        outcomes: &Arc<ResolvedModuleRequestOutcomeMap>,
        source_file_idx: usize,
        specifier: &str,
        answer: Option<ProjectModuleConsensus>,
    ) {
        if !self.accepts_outcomes(outcomes) {
            return;
        }
        self.consensus
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(source_file_idx)
            .or_default()
            .insert(specifier.to_owned(), answer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binder_declaring(modules: &[&str]) -> Arc<BinderState> {
        let mut binder = BinderState::new();
        binder.declared_modules = Arc::new(modules.iter().map(|name| (*name).to_owned()).collect());
        Arc::new(binder)
    }

    #[test]
    fn facts_union_every_binder_once() {
        let plain = Arc::new(BinderState::new());
        let facts = SharedProgramFacts::from_binders(&[
            Arc::clone(&plain),
            binder_declaring(&["ambient-a"]),
            binder_declaring(&["ambient-b", "ambient-a"]),
        ]);
        assert!(facts.is_ambient_module_name("ambient-a"));
        assert!(facts.is_ambient_module_name("ambient-b"));
        assert!(!facts.is_ambient_module_name("./a.ts"));
        assert!(!facts.has_module_augmentations());
        assert!(!facts.has_global_augmentations());

        let mut augmenting = BinderState::new();
        let mut augmentations = rustc_hash::FxHashMap::default();
        augmentations.insert("vue".to_owned(), Vec::new());
        augmenting.module_augmentations = Arc::new(augmentations);
        let facts = SharedProgramFacts::from_binders(&[plain, Arc::new(augmenting)]);
        assert!(facts.has_module_augmentations());
        assert!(facts.is_ambient_module_name("vue"), "an augmented module name is ambient");
        assert!(!facts.has_global_augmentations());
    }

    #[test]
    fn consensus_memo_is_shared_and_bound_to_one_outcome_map() {
        let facts = SharedProgramFacts::from_binders(&[]);
        let original = Arc::new(ResolvedModuleRequestOutcomeMap::default());
        let foreign = Arc::new(ResolvedModuleRequestOutcomeMap::default());
        assert_eq!(facts.consensus_get(&original, 0, "./a"), None, "nothing computed yet");
        facts.consensus_insert(&original, 0, "./a", Some(ProjectModuleConsensus::File(3)));
        facts.consensus_insert(&original, 0, "./b", None);
        assert_eq!(facts.consensus_get(&original, 0, "./a"), Some(Some(ProjectModuleConsensus::File(3))));
        assert_eq!(facts.consensus_get(&original, 0, "./b"), Some(None), "a computed non-answer is remembered");
        assert_eq!(facts.consensus_get(&original, 1, "./a"), None, "pairs are keyed by importer");
        // A context holding another outcome map must never read or write this memo.
        assert_eq!(facts.consensus_get(&foreign, 0, "./a"), None);
        facts.consensus_insert(&foreign, 0, "./c", Some(ProjectModuleConsensus::External));
        assert_eq!(facts.consensus_get(&original, 0, "./c"), None);
        assert_eq!(facts.consensus_get(&foreign, 0, "./c"), None);
        drop(original);
        assert!(facts.consensus_outcomes.read().unwrap().as_ref().unwrap().upgrade().is_none());
        let replacement = Arc::new(ResolvedModuleRequestOutcomeMap::default());
        assert_eq!(facts.consensus_get(&replacement, 0, "./a"), None,
            "a dead outcome allocation cannot lend its cache to a replacement");
    }
}
