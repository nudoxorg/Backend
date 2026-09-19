//! The canonical retained dependency relation and its checked projections.
//!
//! `schema` owns the domain-separated key/value encoding, `indexes` owns
//! bounded lookup arrangements, and this façade owns exact-base transitions.
//! A projection is changed only after its corresponding path-copy update has
//! committed, so the relation root remains the sole source of truth.

mod indexes;
mod schema;

pub(in crate::reuse::index) use self::indexes::RelationIndexes;
pub use self::schema::SemanticRelationRoot;
pub(in crate::reuse::index) use self::schema::{RelationKey, RelationValue};
use self::schema::{SemanticDependencyRelation, internal_storage_coverage};
use super::super::facts::{DependencyManifest, DependencyManifestVersion};
use super::super::graph::RetainedDependencyGraph;
use super::SemanticReaderKey;
use crate::{RecipeVersion, ScopedReadObservation, SemanticError};
use backend_version::AuthorizedCompleteCoverage;
use backend_version::{MapChange, RelationState, prepare_delta_with_state};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Persistent logical relation plus its checked recipe graph and indexes.
///
/// Each mutation prepares a checked `MapChange` against the exact current
/// `RelationState` and publishes the retained persistent tree from that
/// bounded path-copy update. The graph and materialized indexes are updated
/// only after that commit succeeds.
#[derive(Clone, Debug)]
pub(in crate::reuse::index) struct VersionedDependencyRelation<K: SemanticReaderKey> {
    state: RelationState<SemanticDependencyRelation<K>>,
    graph: RetainedDependencyGraph,
    indexes: RelationIndexes<K>,
}

impl<K: SemanticReaderKey> Default for VersionedDependencyRelation<K> {
    fn default() -> Self {
        Self {
            state: RelationState::empty(internal_storage_coverage()),
            graph: RetainedDependencyGraph::default(),
            indexes: RelationIndexes::default(),
        }
    }
}

impl<K: SemanticReaderKey> VersionedDependencyRelation<K> {
    /// Returns the exact canonical persistent relation root.
    #[must_use]
    pub(super) const fn state_root(&self) -> SemanticRelationRoot<K> {
        SemanticRelationRoot::from_state_root(self.state.root())
    }

    /// Returns the checked recipe graph projection.
    #[must_use]
    pub(super) const fn graph(&self) -> &RetainedDependencyGraph {
        &self.graph
    }

    /// Returns the maintained materialized arrangements.
    pub(super) const fn indexes(&self) -> &RelationIndexes<K> {
        &self.indexes
    }

    /// Looks up one relation record by its canonical key.
    #[must_use]
    pub(super) fn get(&self, key: &RelationKey<K>) -> Option<&RelationValue> {
        self.state.get(key)
    }

    /// Iterates canonical relation records for parity checks and rebuilding.
    pub(super) fn iter(&self) -> impl Iterator<Item = (&RelationKey<K>, &RelationValue)> {
        self.state.iter()
    }

    /// Rebuilds the graph projection from complete manifest rows in the
    /// canonical relation. This is intentionally a diagnostic/hydration path;
    /// steady-state mutations advance the graph from the committed row delta.
    pub(super) fn graph_parity(&self) -> bool {
        let mut rebuilt = RetainedDependencyGraph::default();
        for (key, value) in self.iter() {
            if let (
                RelationKey::Manifest { .. },
                RelationValue::Manifest {
                    manifest,
                    references,
                },
            ) = (key, value)
                && rebuilt
                    .retain_manifest_references(manifest, *references)
                    .is_err()
            {
                return false;
            }
        }
        self.graph.same_state(&rebuilt)
    }

    /// Inserts one witnessed read into the canonical relation.
    pub(super) fn insert_observation(
        &mut self,
        reader: K,
        selector: Vec<u8>,
        observation: ScopedReadObservation,
        recipe: Option<RecipeVersion>,
    ) -> Result<(), SemanticError> {
        let registration = super::RegistrationId {
            reader,
            selector: Arc::from(selector),
        };
        self.apply_change(
            RelationKey::Observation(registration.clone()),
            Some(RelationValue::Observation {
                observation,
                recipe,
            }),
        )?;
        Ok(())
    }

    /// Removes one witnessed read from the canonical relation.
    pub(super) fn remove_observation(&mut self, reader: K, selector: &[u8]) -> bool {
        let registration = super::RegistrationId {
            reader,
            selector: Arc::from(selector.to_vec()),
        };
        let key = RelationKey::Observation(registration.clone());
        let Some(RelationValue::Observation { .. }) = self.get(&key) else {
            return false;
        };
        if self.apply_change(key, None).is_err() {
            return false;
        }
        true
    }

    /// Inserts an authority fact; duplicate authority facts coalesce.
    pub(super) fn insert_authority(
        &mut self,
        reader: K,
        recipe: RecipeVersion,
        authority: crate::AuthorityVersion,
        scope: crate::AuthorityScopeVersion,
        coverage: AuthorizedCompleteCoverage,
    ) -> Result<(), SemanticError> {
        let key = RelationKey::Authority {
            reader,
            recipe,
            authority,
            scope,
            producer: coverage.producer_identity(),
        };
        if self.get(&key).is_some() {
            return Ok(());
        }
        self.apply_change(key, Some(RelationValue::Authority { coverage }))?;
        Ok(())
    }

    /// Removes one retained authority fact for a reader.
    pub(super) fn remove_authority(
        &mut self,
        reader: K,
        recipe: RecipeVersion,
        authority: crate::AuthorityVersion,
        scope: crate::AuthorityScopeVersion,
        producer: [u8; 32],
    ) -> bool {
        let key = RelationKey::Authority {
            reader,
            recipe,
            authority,
            scope,
            producer,
        };
        if self.get(&key).is_none() || self.apply_change(key, None).is_err() {
            return false;
        }
        true
    }

    /// Checks that one recipe-edge support count can be incremented.
    pub(super) fn ensure_recipe_capacity(
        &self,
        reader: K,
        recipe: RecipeVersion,
        dependency: RecipeVersion,
    ) -> Result<(), SemanticError> {
        let key = RelationKey::Recipe {
            reader,
            recipe,
            dependency,
        };
        match self.get(&key) {
            None => Ok(()),
            Some(RelationValue::Count(count)) => count
                .checked_add(1)
                .map(|_| ())
                .ok_or(SemanticError::Overflow),
            Some(
                RelationValue::Observation { .. }
                | RelationValue::Manifest { .. }
                | RelationValue::Authority { .. },
            ) => Err(SemanticError::Overflow),
        }
    }

    /// Removes all support for one reader-owned recipe edge.
    pub(super) fn remove_recipe(
        &mut self,
        reader: K,
        recipe: RecipeVersion,
        dependency: RecipeVersion,
    ) -> bool {
        let key = RelationKey::Recipe {
            reader,
            recipe,
            dependency,
        };
        if self.get(&key).is_none() || self.apply_change(key, None).is_err() {
            return false;
        }
        true
    }

    /// Retains a manifest and all of its reverse-index facts as one checked
    /// relation transition.
    ///
    /// A manifest is a transaction boundary.  Building one `MapChange` set
    /// means a canonical-node admission failure cannot leave the manifest,
    /// a prefix/range arrangement, and the recipe/authority arrangements at
    /// different states.  It also gives the persistent tree one path-copy
    /// operation for the complete batch instead of one operation per fact.
    pub(super) fn retain_manifest_with_facts(
        &mut self,
        reader: K,
        manifest: &DependencyManifest,
    ) -> Result<DependencyManifestVersion, SemanticError> {
        let version = manifest.version();
        let manifest_key = RelationKey::Manifest { reader, version };
        let mut after = BTreeMap::<RelationKey<K>, Option<RelationValue>>::new();
        let references = match self.get(&manifest_key) {
            None => 1,
            Some(RelationValue::Manifest { references, .. }) => {
                references.checked_add(1).ok_or(SemanticError::Overflow)?
            }
            Some(
                RelationValue::Observation { .. }
                | RelationValue::Count(_)
                | RelationValue::Authority { .. },
            ) => {
                return Err(SemanticError::Overflow);
            }
        };
        after.insert(
            manifest_key.clone(),
            Some(RelationValue::Manifest {
                manifest: manifest.clone(),
                references,
            }),
        );

        for fact in manifest.facts() {
            match fact {
                super::super::facts::DependencyFact::Read(value) => {
                    let selector = crate::canonical::canonical_scoped_read(value.read());
                    let key = RelationKey::Observation(super::RegistrationId {
                        reader,
                        selector: Arc::from(selector),
                    });
                    if self.get(&key).is_some() || after.contains_key(&key) {
                        return Err(SemanticError::DuplicateRead);
                    }
                    after.insert(
                        key,
                        Some(RelationValue::Observation {
                            observation: value.observation().clone(),
                            recipe: Some(value.recipe()),
                        }),
                    );
                }
                super::super::facts::DependencyFact::Authority(value) => {
                    let key = RelationKey::Authority {
                        reader,
                        recipe: value.recipe(),
                        authority: value.authority(),
                        scope: value.scope(),
                        producer: value.coverage().producer_identity(),
                    };
                    // Authority facts with the same immutable identity
                    // coalesce, matching direct registration semantics.
                    after.entry(key).or_insert_with(|| {
                        Some(RelationValue::Authority {
                            coverage: value.coverage(),
                        })
                    });
                }
                super::super::facts::DependencyFact::Recipe(value) => {
                    let key = RelationKey::Recipe {
                        reader,
                        recipe: value.recipe(),
                        dependency: value.dependency(),
                    };
                    let count = match after.get(&key).and_then(Option::as_ref) {
                        Some(RelationValue::Count(count)) => {
                            count.checked_add(1).ok_or(SemanticError::Overflow)?
                        }
                        Some(_) => return Err(SemanticError::Overflow),
                        None => match self.get(&key) {
                            Some(RelationValue::Count(count)) => {
                                count.checked_add(1).ok_or(SemanticError::Overflow)?
                            }
                            Some(_) => return Err(SemanticError::Overflow),
                            None => 1,
                        },
                    };
                    after.insert(key, Some(RelationValue::Count(count)));
                }
            }
        }

        let changes = after
            .into_iter()
            .map(|(key, after)| {
                let before = self.state.get(&key).cloned();
                MapChange { key, before, after }
            })
            .collect::<Vec<_>>();
        self.graph.validate_retain_manifest(manifest)?;
        let (prepared, _) =
            prepare_delta_with_state(&self.state, changes).map_err(|_| SemanticError::Overflow)?;
        self.commit_change(prepared)?;
        let graph_result = self.graph.retain_manifest(manifest);
        debug_assert!(graph_result.is_ok());
        graph_result.map(|_| version)
    }

    /// Releases one reader reference and its graph root atomically.
    pub(super) fn release_manifest_reference(
        &mut self,
        reader: K,
        version: DependencyManifestVersion,
    ) -> bool {
        let key = RelationKey::Manifest { reader, version };
        let Some(RelationValue::Manifest {
            manifest,
            references,
        }) = self.get(&key)
        else {
            return false;
        };
        let before = *references;
        let after = (before > 1).then_some(RelationValue::Manifest {
            manifest: manifest.clone(),
            references: before - 1,
        });
        if !self.graph.can_release_manifest(version) {
            return false;
        }
        let Ok(prepared) = self.prepare_change(key, after) else {
            return false;
        };
        if self.commit_change(prepared).is_err() {
            return false;
        }
        // The graph release was preflighted against the same relation row;
        // it therefore cannot fail unless an internal invariant was already
        // broken. The canonical relation and its projections are updated
        // before this derived graph projection is advanced.
        let released = self.graph.release_manifest(version);
        debug_assert!(released);
        released
    }

    /// Returns the number of canonical relation records.
    #[must_use]
    pub(super) fn record_count(&self) -> usize {
        self.state.iter().count()
    }

    /// Removes every relation record and projection owned by one reader.
    pub(super) fn remove_reader(&mut self, reader: K) -> usize {
        let Some(membership) = self.indexes.memberships.get(&reader).cloned() else {
            return 0;
        };
        let reads = self
            .indexes
            .observations
            .get(&reader)
            .map(|observations| observations.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let read_count = reads.len();
        for selector in reads {
            let _ = self.remove_observation(reader, &selector);
        }
        for (&manifest, &references) in &membership.manifests {
            for _ in 0..references {
                let _ = self.release_manifest_reference(reader, manifest);
            }
        }
        for &(recipe, dependency) in &membership.recipe_edges {
            let _ = self.remove_recipe(reader, recipe, dependency);
        }
        for &(recipe, authority, scope, producer) in &membership.authority_facts {
            let _ = self.remove_authority(reader, recipe, authority, scope, producer);
        }
        debug_assert!(!self.indexes.memberships.contains_key(&reader));
        read_count
    }

    fn apply_change(
        &mut self,
        key: RelationKey<K>,
        after: Option<RelationValue>,
    ) -> Result<(), SemanticError> {
        let prepared = self.prepare_change(key, after)?;
        self.commit_change(prepared)
    }

    fn prepare_change(
        &self,
        key: RelationKey<K>,
        after: Option<RelationValue>,
    ) -> Result<backend_version::PreparedDelta<SemanticDependencyRelation<K>>, SemanticError> {
        let before = self.state.get(&key).cloned();
        let (prepared, _) =
            prepare_delta_with_state(&self.state, vec![MapChange { key, before, after }])
                .map_err(|_| SemanticError::Overflow)?;
        Ok(prepared)
    }

    fn commit_change(
        &mut self,
        prepared: backend_version::PreparedDelta<SemanticDependencyRelation<K>>,
    ) -> Result<(), SemanticError> {
        let delta = prepared.delta().clone();
        self.state = prepared
            .commit(&self.state)
            .map_err(|_| SemanticError::Overflow)?;
        self.indexes.apply_delta(&delta);
        Ok(())
    }
}
