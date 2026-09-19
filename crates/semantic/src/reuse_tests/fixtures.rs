//! Shared semantic reuse fixtures.

use super::*;
pub(super) use backend_flow::{
    AuthorityIdentity, EquivalenceIdentity, InputIdentity, ReadIdentity, RecipeIdentity,
};
pub(super) use backend_version::{
    AuthorityScopeClaim, AuthorizedCompleteCoverage, ObjectVersion, ProducerObservationClaims,
    ProducerObservationVerifier, Relation, ScopeRoot, UntrustedProducerObservation,
    admit_complete_scope, admit_producer_observation, canonical_empty,
};
pub(super) use std::collections::BTreeSet;

pub(super) struct InputRelation;

#[derive(Debug)]
pub(super) struct FixtureProducerError;

pub(super) struct FixtureProducerVerifier {
    pub(super) expected_identity: [u8; 32],
}

impl ProducerObservationVerifier for FixtureProducerVerifier {
    type Error = FixtureProducerError;

    fn verify(
        &self,
        observation: &UntrustedProducerObservation,
    ) -> Result<ProducerObservationClaims, Self::Error> {
        if observation.producer_identity() != self.expected_identity {
            return Err(FixtureProducerError);
        }
        Ok(ProducerObservationClaims::new(
            self.expected_identity,
            observation.scope_root(),
            observation.context(),
            *blake3::hash(observation.evidence()).as_bytes(),
        ))
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(super) struct ExecutionWorkKey(pub(super) [u8; 32]);

impl SemanticReaderKey for ExecutionWorkKey {
    fn encode_canonical(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"backend.semantic.test.execution-work-key.v1\0");
        out.extend_from_slice(&self.0);
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct HashCollisionReaderKey(pub(super) u8);

impl std::hash::Hash for HashCollisionReaderKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // Deliberately collide: persisted identity must come from the
        // explicit canonical encoding below, never from this map hash.
        std::hash::Hash::hash(&0_u8, state);
    }
}

impl SemanticReaderKey for HashCollisionReaderKey {
    fn encode_canonical(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"backend.semantic.test.colliding-reader.v1\0");
        out.push(self.0);
    }
}

pub(super) struct ExecutionIdentity(pub(super) ExecutionWorkKey);

impl SemanticReaderKeyAdapter for ExecutionIdentity {
    type ReaderKey = ExecutionWorkKey;

    fn reader_key(&self) -> Self::ReaderKey {
        self.0
    }
}

impl Relation for InputRelation {
    const DOMAIN: u8 = 0x75;
    const TYPE: u16 = 1;
    type Key = u64;
    type Value = u64;

    fn encode_key(key: &Self::Key, out: &mut Vec<u8>) {
        out.extend_from_slice(&key.to_be_bytes());
    }

    fn encode_value(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.to_be_bytes());
    }
}

pub(super) fn scope(seed: u8) -> ScopeRoot {
    let value = FacetValue::new(FacetKind::Facet, vec![seed]);
    ScopeRoot::from_bytes(ObjectVersion::<FacetValueSchema>::from_value(&value).to_bytes())
}

pub(super) fn complete(seed: u8) -> AuthorizedCompleteCoverage {
    authorized_with_identity(seed, seed)
}

pub(super) fn authorized_with_identity(
    scope_seed: u8,
    producer_seed: u8,
) -> AuthorizedCompleteCoverage {
    let scope = scope(scope_seed);
    let claim =
        AuthorityScopeClaim::from_object_version(ObjectVersion::<FacetValueSchema>::from_value(
            &FacetValue::new(FacetKind::Facet, vec![scope_seed]),
        ));
    let admitted = admit_producer_observation(
        UntrustedProducerObservation::new(
            [producer_seed; 32],
            scope,
            [scope_seed; 32],
            vec![scope_seed, producer_seed],
        ),
        &FixtureProducerVerifier {
            expected_identity: [producer_seed; 32],
        },
    )
    .expect("producer admission");
    admit_complete_scope(claim, admitted).expect("scope binding")
}

pub(super) fn complete_for_scope(authority: &AuthorityScope) -> AuthorizedCompleteCoverage {
    authorized_for_scope(authority, authority.scope_root().as_bytes()[0])
}

pub(super) fn authorized_for_scope(
    authority: &AuthorityScope,
    producer_seed: u8,
) -> AuthorizedCompleteCoverage {
    let version = authority.version();
    let claim = AuthorityScopeClaim::from_object_version(version);
    let admitted = admit_producer_observation(
        UntrustedProducerObservation::new(
            [producer_seed; 32],
            claim.scope_root(),
            *claim.scope_root().as_bytes(),
            authority.canonical_bytes(),
        ),
        &FixtureProducerVerifier {
            expected_identity: [producer_seed; 32],
        },
    )
    .expect("producer admission");
    admit_complete_scope(claim, admitted).expect("admitted authority scope")
}

pub(super) fn recipe(seed: u8) -> RecipeVersion {
    ObjectVersion::<RecipeSchema>::from_value(&Recipe::Names.typed(
        1,
        vec![seed],
        ReadManifest::new(Vec::new()).expect("empty reads"),
    ))
}

pub(super) fn wide_recipe(seed: u32) -> RecipeVersion {
    ObjectVersion::<RecipeSchema>::from_value(&Recipe::Names.typed(
        1,
        seed.to_be_bytes().to_vec(),
        ReadManifest::new(Vec::new()).expect("empty reads"),
    ))
}

pub(super) fn work(seed: u8) -> backend_flow::WorkKey {
    let recipe = RecipeIdentity::from_value(&[seed; 32]);
    let input = InputIdentity::from_value(&[seed; 32]);
    let read = ReadIdentity::from_value(&[seed; 32]);
    let authority = AuthorityIdentity::from_value(&[seed; 32]);
    let equivalence = EquivalenceIdentity::from_value(&[seed; 32]);
    backend_flow::WorkKey::new(recipe, input, read, authority, equivalence)
}

pub(super) fn authority_scope(seed: u8) -> AuthorityScope {
    let authority = Authority::new("test", "semantic").expect("authority");
    let source = Source::new(1, format!("file-{seed}")).expect("source");
    let source_value = SourceValue::new(vec![seed], SourceEncoding::Binary, u64::from(seed))
        .expect("source value");
    let authority_value = AuthorityValue::new(authority.clone(), u64::from(seed), vec![seed]);
    let provenance =
        Provenance::new(authority, authority_value, source, source_value).expect("provenance");
    AuthorityScope::new(
        provenance,
        vec![seed],
        Some((vec![0], vec![u8::MAX])),
        vec![FacetKind::Name, FacetKind::Type],
    )
    .expect("scope")
}
