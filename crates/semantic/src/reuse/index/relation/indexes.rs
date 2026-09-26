use super::super::super::facts::DependencyManifestVersion;
use super::super::interval::IntervalTree;
use super::super::{ReaderBucket, RegistrationId, SemanticReaderKey};
use super::schema::SemanticDependencyRelation;
use super::{RelationKey, RelationValue, VersionedDependencyRelation};
use crate::{
    AuthorityScopeVersion, AuthorityVersion, ReadSelector, RecipeVersion, ScopedReadObservation,
};
use backend_version::Delta;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

mod delta;

/// Materialized lookup projections for [`VersionedDependencyRelation`].
///
/// These maps are indexes, never sources of truth. Their ownership in one
/// value makes snapshot replacement and delta publication a single operation,
/// while relation records provide the differential oracle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::reuse::index) struct RelationIndexes<K: SemanticReaderKey> {
    pub(in crate::reuse::index) exact:
        BTreeMap<ReaderBucket, BTreeMap<Vec<u8>, BTreeSet<RegistrationId<K>>>>,
    pub(in crate::reuse::index) ranges: BTreeMap<ReaderBucket, IntervalTree<K>>,
    pub(in crate::reuse::index) observations:
        BTreeMap<K, BTreeMap<Arc<[u8]>, Arc<ScopedReadObservation>>>,
    pub(in crate::reuse::index) memberships: BTreeMap<K, ReaderMembership>,
    pub(in crate::reuse::index) readers: BTreeSet<K>,
    pub(super) users: BTreeMap<ReaderUser, BTreeSet<K>>,
}

impl<K: SemanticReaderKey> Default for RelationIndexes<K> {
    fn default() -> Self {
        Self {
            exact: BTreeMap::new(),
            ranges: BTreeMap::new(),
            observations: BTreeMap::new(),
            memberships: BTreeMap::new(),
            readers: BTreeSet::new(),
            users: BTreeMap::new(),
        }
    }
}

/// Reader-local multiplicities for the user projections and cleanup roots.
///
/// The sets are convenient canonical membership views; the maps preserve the
/// number of independent relation records that keep each user alive. This
/// lets an observation be retracted without scanning every other row owned by
/// the reader.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(in crate::reuse::index) struct ReaderMembership {
    pub(in crate::reuse::index) read_count: usize,
    pub(in crate::reuse::index) recipes: BTreeSet<RecipeVersion>,
    pub(in crate::reuse::index) authorities: BTreeSet<AuthorityVersion>,
    pub(in crate::reuse::index) recipe_uses: BTreeMap<RecipeVersion, usize>,
    pub(in crate::reuse::index) authority_uses: BTreeMap<AuthorityVersion, usize>,
    pub(in crate::reuse::index) recipe_edges: BTreeSet<(RecipeVersion, RecipeVersion)>,
    pub(in crate::reuse::index) authority_facts: BTreeSet<(
        RecipeVersion,
        AuthorityVersion,
        AuthorityScopeVersion,
        [u8; 32],
    )>,
    pub(in crate::reuse::index) manifests: BTreeMap<DependencyManifestVersion, usize>,
}

/// User-index key for reverse invalidation joins.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(super) enum ReaderUser {
    /// Readers depending on a recipe version.
    Recipe(RecipeVersion),
    /// Readers depending on an authority revision.
    Authority(AuthorityVersion),
}

impl<K: SemanticReaderKey> RelationIndexes<K> {
    /// Rebuilds every materialized arrangement from the canonical relation.
    ///
    /// This path is intentionally a differential and recovery oracle. Normal
    /// mutations apply the same key transition to the relation and the small
    /// affected projections; rebuilding exists to prove that those deltas
    /// remain equivalent after snapshot hydration or implementation changes.
    pub(in crate::reuse::index) fn from_relation(
        relation: &VersionedDependencyRelation<K>,
    ) -> Self {
        let mut indexes = Self::default();
        for (key, value) in relation.iter() {
            match (key, value) {
                (
                    RelationKey::Observation(registration),
                    RelationValue::Observation {
                        observation,
                        recipe,
                    },
                ) => {
                    let reader = registration.reader;
                    let canonical = registration.selector.clone();
                    let bucket = ReaderBucket {
                        facet: observation.read().facet(),
                        scope: observation.read().scope_root(),
                    };
                    if let ReadSelector::Exact(key) = observation.read().selector() {
                        indexes
                            .exact
                            .entry(bucket.clone())
                            .or_default()
                            .entry(key.clone())
                            .or_default()
                            .insert(registration.clone());
                    }
                    indexes.ranges.entry(bucket).or_default().insert(
                        super::super::interval::interval_key_with_selector(
                            observation.read(),
                            reader,
                            registration.selector.clone(),
                        ),
                    );
                    indexes
                        .observations
                        .entry(reader)
                        .or_default()
                        .insert(canonical, Arc::new(observation.clone()));
                    let next_read_count = indexes
                        .memberships
                        .get(&reader)
                        .map_or(0, |membership| membership.read_count)
                        .saturating_add(1);
                    indexes.memberships.entry(reader).or_default().read_count = next_read_count;
                    if let Some(recipe) = recipe {
                        indexes.add_recipe(reader, *recipe);
                    }
                    indexes.readers.insert(reader);
                }
                (
                    RelationKey::Authority {
                        reader,
                        recipe,
                        authority,
                        scope,
                        producer,
                    },
                    RelationValue::Authority {
                        coverage: _coverage,
                    },
                ) => {
                    indexes.readers.insert(*reader);
                    indexes.add_recipe(*reader, *recipe);
                    indexes.add_authority(*reader, *authority);
                    let membership = indexes.memberships.entry(*reader).or_default();
                    membership
                        .authority_facts
                        .insert((*recipe, *authority, *scope, *producer));
                }
                (
                    RelationKey::Recipe {
                        reader,
                        recipe,
                        dependency,
                    },
                    RelationValue::Count(count),
                ) if *count != 0 => {
                    indexes.readers.insert(*reader);
                    indexes.add_recipe(*reader, *dependency);
                    let membership = indexes.memberships.entry(*reader).or_default();
                    membership.recipe_edges.insert((*recipe, *dependency));
                }
                (
                    RelationKey::Manifest { reader, version },
                    RelationValue::Manifest { references, .. },
                ) if *references != 0 => {
                    indexes.readers.insert(*reader);
                    indexes
                        .memberships
                        .entry(*reader)
                        .or_default()
                        .manifests
                        .insert(
                            *version,
                            (*references).try_into().map_or(usize::MAX, |value| value),
                        );
                }
                _ => {}
            }
        }
        indexes
    }

    pub(in crate::reuse::index) fn recipe_users(
        &self,
        recipe: RecipeVersion,
    ) -> Option<&BTreeSet<K>> {
        self.users.get(&ReaderUser::Recipe(recipe))
    }

    pub(in crate::reuse::index) fn authority_users(
        &self,
        authority: AuthorityVersion,
    ) -> Option<&BTreeSet<K>> {
        self.users.get(&ReaderUser::Authority(authority))
    }
}
