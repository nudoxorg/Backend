use super::super::facts::{DependencyFact, DependencyManifest, DependencyManifestVersion};
use super::super::graph::RetainedDependencyGraph;
use super::ReaderBucket;
use super::interval::IntervalTree;
use super::{
    RegistrationId, RelationIndexes, SemanticReaderKey, SemanticReaderKeyAdapter,
    VersionedDependencyRelation,
};
use crate::canonical::canonical_scoped_read;
use crate::{AuthorityVersion, RecipeVersion, ScopedRead, ScopedReadObservation, SemanticError};
use std::collections::{BTreeMap, BTreeSet};

/// Retained reverse arrangements for versioned semantic readers.
#[derive(Clone, Debug)]
#[allow(clippy::struct_field_names)]
pub struct RetainedReaders<K: SemanticReaderKey = super::SemanticWorkKey> {
    pub(super) relation: VersionedDependencyRelation<K>,
}

impl<K: SemanticReaderKey> Default for RetainedReaders<K> {
    fn default() -> Self {
        Self {
            relation: VersionedDependencyRelation::default(),
        }
    }
}

impl<K: SemanticReaderKey> RetainedReaders<K> {
    /// Registers one witnessed dependency for a reader shard.
    ///
    /// The registration is inserted into exact and interval arrangements in
    /// one operation.  A negative read without complete coverage is rejected
    /// before any arrangement is mutated.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::IncompleteNegativeFact`] for an incomplete
    /// negative read, [`SemanticError::DuplicateRead`] for an existing
    /// selector, or [`SemanticError::Overflow`] when read support is
    /// exhausted.
    pub fn register(
        &mut self,
        reader: K,
        observation: ScopedReadObservation,
    ) -> Result<(), SemanticError> {
        self.register_observation(reader, observation, None)
    }

    fn register_observation(
        &mut self,
        reader: K,
        observation: ScopedReadObservation,
        recipe: Option<RecipeVersion>,
    ) -> Result<(), SemanticError> {
        if observation.read().is_negative() && !observation.authoritative_negative() {
            return Err(SemanticError::IncompleteNegativeFact);
        }
        let canonical = canonical_scoped_read(observation.read());
        if self
            .relation
            .indexes()
            .observations
            .get(&reader)
            .is_some_and(|reads| reads.get(canonical.as_slice()).is_some())
        {
            return Err(SemanticError::DuplicateRead);
        }
        let current_read_count = self
            .relation
            .indexes()
            .memberships
            .get(&reader)
            .map_or(0, |membership| membership.read_count);
        let next_read_count = current_read_count
            .checked_add(1)
            .ok_or(SemanticError::Overflow)?;
        self.relation
            .insert_observation(reader, canonical, observation, recipe)?;
        debug_assert_eq!(
            self.relation
                .indexes()
                .memberships
                .get(&reader)
                .map_or(0, |membership| membership.read_count),
            next_read_count
        );
        Ok(())
    }

    /// Registers an observation through a typed execution identity adapter.
    ///
    /// # Errors
    ///
    /// Returns the registration admission error from [`Self::register`].
    pub fn register_identity<I>(
        &mut self,
        identity: &I,
        observation: ScopedReadObservation,
    ) -> Result<(), SemanticError>
    where
        I: SemanticReaderKeyAdapter<ReaderKey = K>,
    {
        self.register(identity.reader_key(), observation)
    }

    /// Registers every read represented by a typed dependency fact.
    ///
    /// # Errors
    ///
    /// Returns the registration admission error for an invalid or duplicate
    /// read fact.
    pub fn register_fact(&mut self, reader: K, fact: &DependencyFact) -> Result<(), SemanticError> {
        self.register_fact_inner(reader, fact)
    }

    fn register_fact_inner(
        &mut self,
        reader: K,
        fact: &DependencyFact,
    ) -> Result<(), SemanticError> {
        match fact {
            DependencyFact::Read(value) => {
                self.register_observation(
                    reader,
                    value.observation().clone(),
                    Some(value.recipe()),
                )?;
            }
            DependencyFact::Authority(value) => {
                self.relation.insert_authority(
                    reader,
                    value.recipe(),
                    value.authority(),
                    value.scope(),
                    value.coverage(),
                )?;
            }
            DependencyFact::Recipe(value) => {
                // A direct recipe fact historically retained a tiny
                // manifest to keep the global dependency graph alive.
                // Admit that manifest and its reverse edge in one relation
                // transition, otherwise a persistent-tree failure between
                // two operations leaks manifest or recipe support.
                let edge_manifest = DependencyManifest::new(vec![DependencyFact::recipe(*value)])?;
                self.relation
                    .retain_manifest_with_facts(reader, &edge_manifest)?;
            }
        }
        Ok(())
    }

    /// Registers one complete dependency manifest transactionally.
    ///
    /// A preflight validates every potentially fallible local operation before
    /// the global recipe graph or reverse arrangements are changed.  Graph
    /// admission is itself transactional, so a failed manifest leaves both
    /// layers unchanged without cloning the full index.
    ///
    /// # Errors
    ///
    /// Returns a cycle, duplicate, coverage, or overflow error and restores
    /// the index to its pre-registration state.
    pub fn register_manifest(
        &mut self,
        reader: K,
        manifest: &DependencyManifest,
    ) -> Result<DependencyManifestVersion, SemanticError> {
        // Validate every operation that can fail before touching either the
        // graph or the reverse arrangements.  The previous implementation
        // cloned the complete index as a rollback journal; on a large shared
        // index that made a rejected candidate proportional to all retained
        // readers.  Manifest admission is immutable and already normalized,
        // so a small duplicate/overflow preflight is enough to preserve
        // transactionality without copying the arrangements.
        self.validate_manifest_registration(reader, manifest)?;
        // The relation owns the transaction boundary. It prepares the
        // manifest row and every derived fact in one checked persistent
        // transition, so a node-size or overflow rejection cannot publish a
        // partially registered manifest.
        self.relation.retain_manifest_with_facts(reader, manifest)
    }

    fn validate_manifest_registration(
        &self,
        reader: K,
        manifest: &DependencyManifest,
    ) -> Result<(), SemanticError> {
        let existing_reads = self.relation.indexes().observations.get(&reader);
        let mut reads = BTreeSet::new();
        let current_read_count = self
            .relation
            .indexes()
            .memberships
            .get(&reader)
            .map_or(0, |membership| membership.read_count);
        let manifest_read_count = manifest
            .facts()
            .iter()
            .filter(|fact| matches!(fact, DependencyFact::Read(_)))
            .count();
        current_read_count
            .checked_add(manifest_read_count)
            .ok_or(SemanticError::Overflow)?;
        for fact in manifest.facts() {
            if let DependencyFact::Read(value) = fact {
                let canonical = canonical_scoped_read(value.read());
                if !reads.insert(canonical.clone())
                    || existing_reads
                        .is_some_and(|current| current.get(canonical.as_slice()).is_some())
                {
                    return Err(SemanticError::DuplicateRead);
                }
            } else if let DependencyFact::Recipe(value) = fact {
                self.relation
                    .ensure_recipe_capacity(reader, value.recipe(), value.dependency())?;
            }
        }
        Ok(())
    }

    /// Removes one exact registration and returns whether it existed.
    pub fn unregister(&mut self, reader: K, read: &ScopedRead) -> bool {
        let canonical = canonical_scoped_read(read);
        if self
            .relation
            .indexes()
            .observations
            .get(&reader)
            .and_then(|reads| reads.get(canonical.as_slice()))
            .is_none()
        {
            return false;
        }
        self.relation.remove_observation(reader, &canonical)
    }

    /// Releases one manifest reference retained by a reader.
    #[must_use]
    pub fn unregister_manifest(&mut self, reader: K, manifest: DependencyManifestVersion) -> bool {
        let Some(membership) = self.relation.indexes().memberships.get(&reader) else {
            return false;
        };
        if !membership.manifests.contains_key(&manifest) {
            return false;
        }
        self.relation.release_manifest_reference(reader, manifest)
    }

    /// Removes all retained reads for a reader shard.
    pub fn unregister_reader(&mut self, reader: K) -> usize {
        self.relation.remove_reader(reader)
    }

    /// Returns the number of reader shards with retained dependencies.
    #[must_use]
    pub fn reader_count(&self) -> usize {
        self.relation.indexes().readers.len()
    }

    /// Returns the number of retained read observations.
    #[must_use]
    pub fn registration_count(&self) -> usize {
        self.relation
            .indexes()
            .observations
            .values()
            .map(BTreeMap::len)
            .sum()
    }

    /// Returns recipe-user fan-out for one recipe version.
    #[must_use]
    pub fn recipe_user_count(&self, recipe: RecipeVersion) -> usize {
        self.relation
            .indexes()
            .recipe_users(recipe)
            .map_or(0, BTreeSet::len)
    }

    /// Returns authority-user fan-out for one authority revision.
    #[must_use]
    pub fn authority_user_count(&self, authority: AuthorityVersion) -> usize {
        self.relation
            .indexes()
            .authority_users(authority)
            .map_or(0, BTreeSet::len)
    }

    /// Returns the retained global recipe dependency graph.
    #[must_use]
    pub const fn dependency_graph(&self) -> &RetainedDependencyGraph {
        self.relation.graph()
    }

    /// Returns the number of canonical relation records.
    #[must_use]
    pub fn relation_record_count(&self) -> usize {
        self.relation.record_count()
    }

    /// Rebuilds the materialized arrangements and compares them with the
    /// incrementally maintained projections.
    ///
    /// This deliberately performs a full canonical scan and is intended for
    /// snapshot hydration checks, diagnostics, and differential tests. The
    /// normal invalidation path remains bounded by the exact and interval
    /// projections.
    #[must_use]
    pub fn relation_index_parity(&self) -> bool {
        self.relation.graph_parity()
            && self.relation.indexes() == &RelationIndexes::from_relation(&self.relation)
    }

    /// Returns the exact persistent root of the canonical dependency relation.
    #[must_use]
    pub fn relation_root(&self) -> super::SemanticRelationRoot<K> {
        self.relation.state_root()
    }

    /// Returns the exact persistent root for the generational owner.
    #[must_use]
    pub(in crate::reuse) const fn relation_state_root(&self) -> super::SemanticRelationRoot<K> {
        self.relation.state_root()
    }

    /// Returns one observation through the canonical lookup projection.
    #[must_use]
    pub(in crate::reuse) fn observation(
        &self,
        registration: &RegistrationId<K>,
    ) -> Option<&ScopedReadObservation> {
        self.relation
            .indexes()
            .observations
            .get(&registration.reader)
            .and_then(|reads| reads.get(registration.selector.as_ref()))
            .map(AsRef::as_ref)
    }

    /// Returns exact-key candidates from the materialized projection.
    #[must_use]
    pub(in crate::reuse) fn exact_candidates(
        &self,
        bucket: &ReaderBucket,
        key: &[u8],
    ) -> Option<&BTreeSet<RegistrationId<K>>> {
        self.relation
            .indexes()
            .exact
            .get(bucket)
            .and_then(|keys| keys.get(key))
    }

    /// Returns the interval projection for one scope/facet bucket.
    #[must_use]
    pub(in crate::reuse) fn range_candidates(
        &self,
        bucket: &ReaderBucket,
    ) -> Option<&IntervalTree<K>> {
        self.relation.indexes().ranges.get(bucket)
    }

    /// Iterates read observations directly from the canonical relation.
    ///
    /// This is the independent differential oracle: it deliberately bypasses
    /// exact and interval projections so a stale or incomplete projection is
    /// detected rather than silently becoming the source of truth.
    pub(in crate::reuse) fn relation_observations(
        &self,
    ) -> impl Iterator<Item = (K, &ScopedReadObservation)> {
        self.relation
            .iter()
            .filter_map(|(key, value)| match (key, value) {
                (
                    super::relation::RelationKey::Observation(registration),
                    super::relation::RelationValue::Observation { observation, .. },
                ) => Some((registration.reader, observation)),
                _ => None,
            })
    }

    /// Returns one materialized recipe-user fanout set.
    #[must_use]
    pub(in crate::reuse) fn recipe_users(&self, recipe: RecipeVersion) -> Option<&BTreeSet<K>> {
        self.relation.indexes().recipe_users(recipe)
    }

    /// Returns one materialized authority-user fanout set.
    #[must_use]
    pub(in crate::reuse) fn authority_users(
        &self,
        authority: AuthorityVersion,
    ) -> Option<&BTreeSet<K>> {
        self.relation.indexes().authority_users(authority)
    }
}
