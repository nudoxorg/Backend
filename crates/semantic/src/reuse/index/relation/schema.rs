use super::super::super::facts::{DependencyManifest, DependencyManifestVersion};
use super::super::{RegistrationId, SemanticReaderKey};
use crate::canonical::canonical_scoped_read;
use crate::support::object_version;
use crate::{AuthorityScopeVersion, AuthorityVersion, RecipeVersion, ScopedReadObservation};
use backend_version::{
    AuthorizedCompleteCoverage, ClosedRelationScope, CoverageWitness, Relation, ScopeRoot,
    StateRoot,
};
use std::marker::PhantomData;

/// One canonical key in the retained dependency/observation relation.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(in crate::reuse::index) enum RelationKey<K: SemanticReaderKey> {
    /// A witnessed read observation keyed by its reader and canonical selector.
    Observation(RegistrationId<K>),
    /// An authority fact keyed by its complete immutable identity.
    Authority {
        reader: K,
        recipe: RecipeVersion,
        authority: AuthorityVersion,
        scope: AuthorityScopeVersion,
        producer: [u8; 32],
    },
    /// A recipe-to-recipe prerequisite edge retained by a reader.
    Recipe {
        reader: K,
        recipe: RecipeVersion,
        dependency: RecipeVersion,
    },
    /// One reader's reference to a complete dependency manifest.
    Manifest {
        reader: K,
        version: DependencyManifestVersion,
    },
}

/// Payload retained by one dependency relation record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::reuse::index) enum RelationValue {
    /// The complete witnessed observation for an [`RelationKey::Observation`].
    Observation {
        observation: ScopedReadObservation,
        recipe: Option<RecipeVersion>,
    },
    /// A counted relation fact, used for repeated recipe edges.
    Count(u64),
    /// A complete manifest payload and its reader-local reference count.
    ///
    /// Retaining the immutable payload in the canonical relation means a
    /// hydrated relation can reconstruct graph projections without a second
    /// manifest registry. The manifest owns shared canonical bytes internally,
    /// so repeated references do not copy the fact set.
    Manifest {
        manifest: DependencyManifest,
        references: u64,
    },
    /// A complete authority fact with its admitted producer retained for
    /// restart and canonical identity checks.
    Authority {
        coverage: AuthorizedCompleteCoverage,
    },
}

/// Canonical relation schema for retained semantic dependency records.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct SemanticDependencyRelation<K: SemanticReaderKey>(PhantomData<fn() -> K>);

impl<K: SemanticReaderKey> Relation for SemanticDependencyRelation<K> {
    const DOMAIN: u8 = 0x73;
    const TYPE: u16 = 0x0032;
    type Key = RelationKey<K>;
    type Value = RelationValue;

    fn encode_key(key: &Self::Key, out: &mut Vec<u8>) {
        match key {
            RelationKey::Observation(registration) => {
                out.push(1);
                encode_reader(registration.reader, out);
                super::super::super::frame(out, registration.selector.as_ref());
            }
            RelationKey::Authority {
                reader,
                recipe,
                authority,
                scope,
                producer,
            } => {
                out.push(2);
                encode_reader(*reader, out);
                out.extend_from_slice(&recipe.to_bytes());
                out.extend_from_slice(&authority.to_bytes());
                out.extend_from_slice(&scope.to_bytes());
                out.extend_from_slice(producer);
            }
            RelationKey::Recipe {
                reader,
                recipe,
                dependency,
            } => {
                out.push(3);
                encode_reader(*reader, out);
                out.extend_from_slice(&recipe.to_bytes());
                out.extend_from_slice(&dependency.to_bytes());
            }
            RelationKey::Manifest { reader, version } => {
                out.push(4);
                encode_reader(*reader, out);
                out.extend_from_slice(&version.to_bytes());
            }
        }
    }

    fn encode_value(value: &Self::Value, out: &mut Vec<u8>) {
        match value {
            RelationValue::Observation {
                observation,
                recipe,
            } => {
                out.push(1);
                // Keep the complete read in the value as well as the key:
                // relation snapshots must be self describing after restart,
                // and the projection hydrator needs the typed selector.
                super::super::super::frame(out, &canonical_scoped_read(observation.read()));
                match observation.version() {
                    Some(version) => {
                        out.push(1);
                        object_version(out, &version);
                    }
                    None => out.push(0),
                }
                super::super::super::coverage(out, observation.coverage());
                match recipe {
                    Some(recipe) => {
                        out.push(1);
                        out.extend_from_slice(&recipe.to_bytes());
                    }
                    None => out.push(0),
                }
            }
            RelationValue::Count(count) => {
                out.push(2);
                out.extend_from_slice(&count.to_be_bytes());
            }
            RelationValue::Manifest {
                manifest,
                references,
            } => {
                out.push(3);
                out.extend_from_slice(&manifest.version().to_bytes());
                out.extend_from_slice(&references.to_be_bytes());
                super::super::super::frame(out, &manifest.canonical_bytes());
            }
            RelationValue::Authority { coverage } => {
                out.push(4);
                super::super::super::coverage(out, CoverageWitness::Complete(*coverage));
            }
        }
    }
}

/// Frames the complete execution identity before appending relation-specific
/// fields.  Reader keys are intentionally supplied by the execution layer,
/// so their bytes may have a variable length; an unframed concatenation would
/// make two valid encodings ambiguous at the relation boundary.
fn encode_reader<K: SemanticReaderKey>(reader: K, out: &mut Vec<u8>) {
    let encoded = reader.canonical_bytes();
    super::super::super::frame(out, &encoded);
}

/// An opaque semantic relation root bound to one reader-key domain.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SemanticRelationRoot<K: SemanticReaderKey> {
    root: StateRoot<SemanticDependencyRelation<K>>,
}

impl<K: SemanticReaderKey> SemanticRelationRoot<K> {
    pub(super) const fn from_state_root(root: StateRoot<SemanticDependencyRelation<K>>) -> Self {
        Self { root }
    }

    /// Returns the canonical root bytes for persistence and diagnostics.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; 32] {
        self.root.to_bytes()
    }

    /// Returns the canonical root bytes by reference.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        self.root.as_bytes()
    }
}

pub(super) fn internal_storage_coverage() -> CoverageWitness {
    // The relation's state witness describes the physical storage projection;
    // it is deliberately closed because storage cannot attest product
    // authority coverage or mint a producer capability.
    CoverageWitness::closed_relation(ClosedRelationScope::from_scope_root(ScopeRoot::from_bytes(
        [0; 32],
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_storage_coverage_cannot_authorize_product_scope() {
        assert!(matches!(
            internal_storage_coverage(),
            CoverageWitness::Closed(_)
        ));
    }
}
