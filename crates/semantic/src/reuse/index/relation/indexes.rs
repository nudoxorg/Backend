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
    /// Applies the effective changes of a committed canonical relation delta.
    /// The projection consumes the same before/after rows used by the delta,
    /// so rejected mutations never reach this method.
    pub(super) fn apply_delta(&mut self, delta: &Delta<SemanticDependencyRelation<K>>) {
        for change in delta.changes() {
            self.apply_change(change);
        }
    }

    fn apply_change(&mut self, change: &backend_version::MapChange<SemanticDependencyRelation<K>>) {
        match &change.key {
            RelationKey::Observation(registration) => {
                let before = observation_value(change.before.as_ref());
                let after = observation_value(change.after.as_ref());
                if let Some((observation, recipe)) = before {
                    self.remove_observation(registration, observation, recipe);
                }
                if let Some((observation, recipe)) = after {
                    self.insert_observation(registration, observation, recipe);
                }
            }
            RelationKey::Authority {
                reader,
                recipe,
                authority,
                scope,
                producer,
            } => {
                if matches!(
                    change.before.as_ref(),
                    Some(RelationValue::Authority { .. })
                ) {
                    self.remove_authority(*reader, *recipe, *authority, *scope, *producer);
                }
                if let Some(RelationValue::Authority { coverage }) = change.after.as_ref() {
                    self.insert_authority(*reader, *recipe, *authority, *scope, *coverage);
                }
            }
            RelationKey::Recipe {
                reader,
                recipe,
                dependency,
            } => {
                let before = recipe_count(change.before.as_ref());
                let after = recipe_count(change.after.as_ref());
                if before == 0 && after != 0 {
                    self.insert_recipe(*reader, *recipe, *dependency);
                } else if before != 0 && after == 0 {
                    self.remove_recipe(*reader, *recipe, *dependency);
                }
            }
            RelationKey::Manifest { reader, version } => {
                let before = manifest_count(change.before.as_ref());
                let after = manifest_count(change.after.as_ref());
                if before == 0 && after != 0 {
                    self.retain_manifest(*reader, *version, after);
                } else if before != 0 && after == 0 {
                    self.release_manifest(*reader, *version, before);
                } else if before != after {
                    self.set_manifest(*reader, *version, after);
                }
            }
        }
    }

    pub(super) fn insert_observation(
        &mut self,
        registration: &RegistrationId<K>,
        observation: &ScopedReadObservation,
        recipe: Option<RecipeVersion>,
    ) {
        let reader = registration.reader;
        let bucket = ReaderBucket {
            facet: observation.read().facet(),
            scope: observation.read().scope_root(),
        };
        if let ReadSelector::Exact(key) = observation.read().selector() {
            self.exact
                .entry(bucket.clone())
                .or_default()
                .entry(key.clone())
                .or_default()
                .insert(registration.clone());
        }
        self.ranges.entry(bucket).or_default().insert(
            super::super::interval::interval_key_with_selector(
                observation.read(),
                reader,
                registration.selector.clone(),
            ),
        );
        self.observations
            .entry(reader)
            .or_default()
            .insert(registration.selector.clone(), Arc::new(observation.clone()));
        let next_read_count = self
            .memberships
            .get(&reader)
            .map_or(0, |membership| membership.read_count)
            .saturating_add(1);
        self.memberships.entry(reader).or_default().read_count = next_read_count;
        if let Some(recipe) = recipe {
            self.add_recipe(reader, recipe);
        }
        self.readers.insert(reader);
    }

    pub(super) fn remove_observation(
        &mut self,
        registration: &RegistrationId<K>,
        observation: &ScopedReadObservation,
        recipe: Option<RecipeVersion>,
    ) {
        let reader = registration.reader;
        let bucket = ReaderBucket {
            facet: observation.read().facet(),
            scope: observation.read().scope_root(),
        };
        if let ReadSelector::Exact(key) = observation.read().selector()
            && let Some(keys) = self.exact.get_mut(&bucket)
        {
            if let Some(readers) = keys.get_mut(key) {
                readers.remove(registration);
                if readers.is_empty() {
                    keys.remove(key);
                }
            }
            if keys.is_empty() {
                self.exact.remove(&bucket);
            }
        }
        let interval = super::super::interval::interval_key(observation.read(), reader);
        if let Some(tree) = self.ranges.get_mut(&bucket) {
            tree.remove(&interval);
            if tree.len == 0 {
                self.ranges.remove(&bucket);
            }
        }
        if let Some(reads) = self.observations.get_mut(&reader) {
            reads.remove(&registration.selector);
            if reads.is_empty() {
                self.observations.remove(&reader);
            }
        }
        let had_membership = if let Some(membership) = self.memberships.get_mut(&reader) {
            membership.read_count = membership.read_count.saturating_sub(1);
            true
        } else {
            false
        };
        if had_membership && let Some(recipe) = recipe {
            self.remove_recipe_use(reader, recipe);
        }
        self.remove_empty_reader(reader);
    }

    pub(super) fn insert_authority(
        &mut self,
        reader: K,
        recipe: RecipeVersion,
        authority: AuthorityVersion,
        scope: AuthorityScopeVersion,
        coverage: backend_version::AuthorizedCompleteCoverage,
    ) {
        self.readers.insert(reader);
        self.add_recipe(reader, recipe);
        self.add_authority(reader, authority);
        let membership = self.memberships.entry(reader).or_default();
        membership
            .authority_facts
            .insert((recipe, authority, scope, coverage.producer_identity()));
    }

    pub(super) fn insert_recipe(
        &mut self,
        reader: K,
        recipe: RecipeVersion,
        dependency: RecipeVersion,
    ) {
        self.readers.insert(reader);
        self.add_recipe(reader, dependency);
        let membership = self.memberships.entry(reader).or_default();
        membership.recipe_edges.insert((recipe, dependency));
    }

    pub(super) fn remove_authority(
        &mut self,
        reader: K,
        recipe: RecipeVersion,
        authority: AuthorityVersion,
        scope: AuthorityScopeVersion,
        producer: [u8; 32],
    ) {
        if let Some(membership) = self.memberships.get_mut(&reader) {
            membership
                .authority_facts
                .remove(&(recipe, authority, scope, producer));
        }
        self.remove_authority_use(reader, authority);
        self.remove_recipe_use(reader, recipe);
        self.remove_empty_reader(reader);
    }

    pub(super) fn remove_recipe(
        &mut self,
        reader: K,
        recipe: RecipeVersion,
        dependency: RecipeVersion,
    ) {
        if let Some(membership) = self.memberships.get_mut(&reader) {
            membership.recipe_edges.remove(&(recipe, dependency));
        }
        self.remove_recipe_use(reader, dependency);
        self.remove_empty_reader(reader);
    }

    pub(super) fn retain_manifest(
        &mut self,
        reader: K,
        version: DependencyManifestVersion,
        count: u64,
    ) {
        self.readers.insert(reader);
        self.memberships
            .entry(reader)
            .or_default()
            .manifests
            .insert(version, count.try_into().map_or(usize::MAX, |value| value));
    }

    pub(super) fn set_manifest(
        &mut self,
        reader: K,
        version: DependencyManifestVersion,
        count: u64,
    ) {
        if let Some(membership) = self.memberships.get_mut(&reader) {
            if count == 0 {
                membership.manifests.remove(&version);
            } else {
                membership
                    .manifests
                    .insert(version, count.try_into().map_or(usize::MAX, |value| value));
            }
        }
        self.remove_empty_reader(reader);
    }

    pub(super) fn release_manifest(
        &mut self,
        reader: K,
        version: DependencyManifestVersion,
        before: u64,
    ) {
        if let Some(membership) = self.memberships.get_mut(&reader) {
            if before > 1 {
                if let Some(references) = membership.manifests.get_mut(&version) {
                    *references = references.saturating_sub(1);
                }
            } else {
                membership.manifests.remove(&version);
            }
        }
        self.remove_empty_reader(reader);
    }

    fn remove_empty_reader(&mut self, reader: K) {
        let remove = self.memberships.get(&reader).is_some_and(|membership| {
            membership.read_count == 0
                && membership.recipes.is_empty()
                && membership.authorities.is_empty()
                && membership.manifests.is_empty()
                && membership.recipe_edges.is_empty()
                && membership.authority_facts.is_empty()
        });
        if remove {
            self.memberships.remove(&reader);
            self.readers.remove(&reader);
        }
    }

    fn add_recipe(&mut self, reader: K, recipe: RecipeVersion) {
        let membership = self.memberships.entry(reader).or_default();
        let uses = membership.recipe_uses.entry(recipe).or_default();
        *uses = uses.saturating_add(1);
        membership.recipes.insert(recipe);
        self.users
            .entry(ReaderUser::Recipe(recipe))
            .or_default()
            .insert(reader);
    }

    fn remove_recipe_use(&mut self, reader: K, recipe: RecipeVersion) {
        let remove = if let Some(membership) = self.memberships.get_mut(&reader) {
            if let Some(uses) = membership.recipe_uses.get_mut(&recipe) {
                *uses = uses.saturating_sub(1);
                *uses == 0
            } else {
                false
            }
        } else {
            false
        };
        if remove {
            if let Some(membership) = self.memberships.get_mut(&reader) {
                membership.recipe_uses.remove(&recipe);
                membership.recipes.remove(&recipe);
            }
            Self::remove_user_from(&mut self.users, ReaderUser::Recipe(recipe), reader);
        }
    }

    fn add_authority(&mut self, reader: K, authority: AuthorityVersion) {
        let membership = self.memberships.entry(reader).or_default();
        let uses = membership.authority_uses.entry(authority).or_default();
        *uses = uses.saturating_add(1);
        membership.authorities.insert(authority);
        self.users
            .entry(ReaderUser::Authority(authority))
            .or_default()
            .insert(reader);
    }

    fn remove_authority_use(&mut self, reader: K, authority: AuthorityVersion) {
        let remove = if let Some(membership) = self.memberships.get_mut(&reader) {
            if let Some(uses) = membership.authority_uses.get_mut(&authority) {
                *uses = uses.saturating_sub(1);
                *uses == 0
            } else {
                false
            }
        } else {
            false
        };
        if remove {
            if let Some(membership) = self.memberships.get_mut(&reader) {
                membership.authority_uses.remove(&authority);
                membership.authorities.remove(&authority);
            }
            Self::remove_user_from(&mut self.users, ReaderUser::Authority(authority), reader);
        }
    }

    fn remove_user_from(
        users: &mut BTreeMap<ReaderUser, BTreeSet<K>>,
        user: ReaderUser,
        reader: K,
    ) {
        let empty = if let Some(readers) = users.get_mut(&user) {
            readers.remove(&reader);
            readers.is_empty()
        } else {
            false
        };
        if empty {
            users.remove(&user);
        }
    }

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

fn observation_value(
    value: Option<&RelationValue>,
) -> Option<(&ScopedReadObservation, Option<RecipeVersion>)> {
    match value {
        Some(RelationValue::Observation {
            observation,
            recipe,
        }) => Some((observation, *recipe)),
        Some(
            RelationValue::Count(_)
            | RelationValue::Manifest { .. }
            | RelationValue::Authority { .. },
        )
        | None => None,
    }
}

fn recipe_count(value: Option<&RelationValue>) -> u64 {
    match value {
        Some(RelationValue::Count(count)) => *count,
        Some(
            RelationValue::Observation { .. }
            | RelationValue::Manifest { .. }
            | RelationValue::Authority { .. },
        )
        | None => 0,
    }
}

fn manifest_count(value: Option<&RelationValue>) -> u64 {
    match value {
        Some(RelationValue::Manifest { references, .. }) => *references,
        Some(
            RelationValue::Observation { .. }
            | RelationValue::Count(_)
            | RelationValue::Authority { .. },
        )
        | None => 0,
    }
}
