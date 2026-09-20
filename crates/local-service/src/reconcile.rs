//! Product closure prelude and bounded CAS adapters.
//!
//! The product profile exposes the checked version tree retained by the
//! workspace owner as an authenticated Merkle closure. Branch and leaf
//! preimages are executable, content-addressed node objects; the receiver does
//! not persist their rows a second time. Consequently an unchanged root
//! performs no transfer and a one-row edit transfers only its changed Merkle
//! frontier plus the fixed-size Product input. The owner binds execution only from the
//! [`BoundClosureRoot`] returned after [`ClosureSync`] has exhausted that
//! frontier.

use backend_engine::{
    AuthorityClaim, CanonicalRelation, ChunkParts, Frame, ImmutableObjectSchema, ObjectKey,
    ObjectVersion, ReplicationError, TransferId, TransportLimits,
};
#[cfg(test)]
use backend_engine::{
    MerklePageBody, MerklePageRequest, MerklePageSource, RelationState, WorkspaceRoot,
};
#[cfg(test)]
use std::collections::BTreeMap;

#[path = "reconcile/page_source.rs"]
mod page_source;
pub(crate) use page_source::ProductPageSource;

fn encode_key<R: CanonicalRelation>(key: &R::Key) -> Vec<u8> {
    let mut bytes = Vec::new();
    R::encode_key(key, &mut bytes);
    bytes
}

fn encode_row<R: CanonicalRelation>(key: &R::Key, value: &R::Value) -> Vec<u8> {
    let key_bytes = encode_key::<R>(key);
    let mut value_bytes = Vec::new();
    R::encode_value(value, &mut value_bytes);
    backend_engine::canonical_relation_row(&key_bytes, &value_bytes)
}

/// Produces the bounded object frames for a previously authenticated closure.
///
/// The Merkle walk decides *which* immutable objects are absent; this helper
/// is the byte-plane of that decision.  Each object gets one stable transfer
/// identity and is split at the negotiated chunk boundary.  The chain links
/// are calculated over the emitted sequence, so a receiver can persist every
/// extent and resume without retaining the whole object in memory.
///
/// Callers should send the returned frames only after sending the associated
/// root prelude.  A warm receiver can skip every frame by checking
/// [`ProductReceivingCas::contains`] against the object version.
#[cfg(test)]
pub(crate) fn product_closure_frames<R: CanonicalRelation>(
    workspace: WorkspaceRoot,
    relation: &RelationState<R>,
    authority: AuthorityClaim,
    limits: TransportLimits,
    first_transfer: u64,
) -> Result<Vec<Frame>, ReplicationError> {
    product_closure_frames_for_versions(
        workspace,
        relation,
        authority,
        limits,
        first_transfer,
        None,
    )
}

/// Builds only frames whose immutable versions were requested by the worker.
/// The filter is applied before frame allocation, keeping a one-leaf change
/// proportional to the changed object rather than the complete relation.
#[cfg(test)]
pub(crate) fn product_closure_frames_for_versions<R: CanonicalRelation>(
    workspace: WorkspaceRoot,
    relation: &RelationState<R>,
    authority: AuthorityClaim,
    limits: TransportLimits,
    first_transfer: u64,
    wanted: Option<&std::collections::BTreeSet<[u8; 32]>>,
) -> Result<Vec<Frame>, ReplicationError> {
    limits.validate()?;
    let source = ProductPageSource::from_relation(workspace, relation)?;
    product_closure_frames_from_source(&source, authority, limits, first_transfer, wanted)
}

#[cfg(test)]
pub(crate) fn product_closure_frames_from_source<R: CanonicalRelation>(
    source: &ProductPageSource<R>,
    authority: AuthorityClaim,
    limits: TransportLimits,
    first_transfer: u64,
    wanted: Option<&std::collections::BTreeSet<[u8; 32]>>,
) -> Result<Vec<Frame>, ReplicationError> {
    limits.validate()?;
    let chunk_size = limits.max_chunk.max(1);
    let mut transfer = first_transfer;
    if transfer == 0 {
        return Err(ReplicationError::InvalidIdentifier);
    }
    let mut frames = Vec::new();
    for bytes in source.all_object_bytes_for_test() {
        let key = ObjectKey::<ImmutableObjectSchema>::from_value(bytes.as_ref());
        let version = ObjectVersion::<ImmutableObjectSchema>::from_value(bytes.as_ref());
        if wanted.is_some_and(|versions| !versions.contains(&version.to_bytes())) {
            continue;
        }
        let transfer_id = TransferId::new(transfer)?;
        transfer = transfer.checked_add(1).ok_or(ReplicationError::Overflow)?;
        let object_len = u64::try_from(bytes.len()).map_err(|_| ReplicationError::Overflow)?;
        let mut previous = backend_engine::ChunkChain([0; 32]);
        for (sequence, payload) in bytes.chunks(chunk_size).enumerate() {
            let sequence = u64::try_from(sequence).map_err(|_| ReplicationError::Overflow)?;
            let offset = sequence
                .checked_mul(u64::try_from(chunk_size).map_err(|_| ReplicationError::Overflow)?)
                .ok_or(ReplicationError::Overflow)?;
            let frame = Frame::new(
                transfer_id,
                key,
                version,
                ChunkParts {
                    object_len,
                    offset,
                    sequence,
                    previous_chain: previous,
                    payload: payload.to_vec(),
                },
                authority,
            )?;
            previous = frame.chain;
            frame.validate(limits)?;
            frames.push(frame);
        }
    }
    Ok(frames)
}

/// Builds the chunk sequence for exactly one page-derived object claim. The
/// caller supplies the next transfer number and retains only this bounded
/// object's frames while it is in flight.
pub(crate) fn product_frames_for_version_from_source<R: CanonicalRelation>(
    source: &mut ProductPageSource<R>,
    version: [u8; 32],
    authority: AuthorityClaim,
    limits: TransportLimits,
    first_transfer: u64,
) -> Result<Vec<Frame>, ReplicationError> {
    limits.validate()?;
    let bytes = source
        .object_bytes(version)
        .ok_or(ReplicationError::CorruptFrame)?;
    let key = ObjectKey::<ImmutableObjectSchema>::from_value(bytes.as_ref());
    let object_version = ObjectVersion::<ImmutableObjectSchema>::from_value(bytes.as_ref());
    if object_version.to_bytes() != version {
        return Err(ReplicationError::IdentityMismatch);
    }
    let transfer = TransferId::new(first_transfer)?;
    let chunk_size = limits.max_chunk.max(1);
    let object_len = u64::try_from(bytes.len()).map_err(|_| ReplicationError::Overflow)?;
    let mut previous = backend_engine::ChunkChain([0; 32]);
    let mut frames = Vec::new();
    for (sequence, payload) in bytes.chunks(chunk_size).enumerate() {
        let sequence = u64::try_from(sequence).map_err(|_| ReplicationError::Overflow)?;
        let offset = sequence
            .checked_mul(u64::try_from(chunk_size).map_err(|_| ReplicationError::Overflow)?)
            .ok_or(ReplicationError::Overflow)?;
        let frame = Frame::new(
            transfer,
            key,
            object_version,
            ChunkParts {
                object_len,
                offset,
                sequence,
                previous_chain: previous,
                payload: payload.to_vec(),
            },
            authority,
        )?;
        previous = frame.chain;
        frame.validate(limits)?;
        frames.push(frame);
    }
    Ok(frames)
}

/// Builds the recipe input object directly from its admitted relation root.
/// This is the pending-ticket path: the ticket retains the checked root
/// identity, so it does not need to clone or keep a full relation state while
/// closure pages are in flight.
pub(crate) fn product_input_frame(
    input: backend_engine::ProductInput,
    authority: AuthorityClaim,
    limits: TransportLimits,
    transfer: u64,
) -> Result<Frame, ReplicationError> {
    limits.validate()?;
    let transfer = TransferId::new(transfer)?;
    let bytes = backend_engine::product_input_bytes(&input);
    let key = ObjectKey::<ImmutableObjectSchema>::from_value(&bytes);
    let version = backend_engine::product_input_version(&input);
    let frame = Frame::new(
        transfer,
        key,
        version,
        ChunkParts {
            object_len: bytes.len() as u64,
            offset: 0,
            sequence: 0,
            previous_chain: backend_engine::ChunkChain([0; 32]),
            payload: bytes,
        },
        authority,
    )?;
    frame.validate(limits)?;
    Ok(frame)
}

/// Checks an output through the canonical CAS digest path before the engine's
/// scheduler publication. The product validator still owns semantic and
/// authority admission; this helper only proves the retained bytes are the
/// exact canonical object represented by `output`.
pub(crate) fn validate_canonical_output(
    output: backend_engine::OutputVersion,
    bytes: &[u8],
) -> Result<(), ReplicationError> {
    let digest = backend_engine::canonical_object_digest::<backend_engine::OutputSchema>(
        bytes.len() as u64,
        [bytes],
    );
    if digest != output.to_bytes() {
        return Err(ReplicationError::IdentityMismatch);
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    struct FixtureCoverageProducer {
        scope: backend_engine::ScopeRoot,
    }

    impl backend_engine::ProducerObservationVerifier for FixtureCoverageProducer {
        type Error = &'static str;

        fn verify(
            &self,
            observation: &backend_engine::UntrustedProducerObservation,
        ) -> Result<(), Self::Error> {
            if observation.producer_identity() == [0x72; 32]
                && observation.scope_root() == self.scope
                && observation.context() == [0x73; 32]
                && observation.evidence() == [0x74, 0x75]
            {
                Ok(())
            } else {
                Err("fixture producer rejected")
            }
        }
    }

    fn fixture_coverage(
        authority: backend_engine::AuthorityVersion,
    ) -> backend_engine::CoverageWitness {
        let declaration = backend_engine::AuthorityScopeClaim::from_object_version(authority);
        let producer = FixtureCoverageProducer {
            scope: declaration.scope_root(),
        };
        let observation = backend_engine::UntrustedProducerObservation::new(
            [0x72; 32],
            declaration.scope_root(),
            [0x73; 32],
            vec![0x74, 0x75],
        );
        let admitted = backend_engine::admit_producer_observation(observation, &producer)
            .expect("producer observation");
        backend_engine::CoverageWitness::Complete(
            backend_engine::admit_complete_scope(declaration, admitted).expect("coverage"),
        )
    }

    #[derive(Debug)]
    struct Relation;
    impl backend_engine::Relation for Relation {
        const DOMAIN: u8 = 0x41;
        const TYPE: u16 = 1;
        type Key = u64;
        type Value = u64;
        fn encode_key(value: &Self::Key, out: &mut Vec<u8>) {
            out.extend_from_slice(&value.to_be_bytes());
        }
        fn encode_value(value: &Self::Value, out: &mut Vec<u8>) {
            out.extend_from_slice(&value.to_be_bytes());
        }
    }
    impl CanonicalRelation for Relation {
        fn decode_key(bytes: &[u8]) -> Result<Self::Key, backend_engine::RelationDecodeError> {
            bytes
                .try_into()
                .map(u64::from_be_bytes)
                .map_err(|_| backend_engine::RelationDecodeError::Malformed)
        }
        fn decode_value(bytes: &[u8]) -> Result<Self::Value, backend_engine::RelationDecodeError> {
            bytes
                .try_into()
                .map(u64::from_be_bytes)
                .map_err(|_| backend_engine::RelationDecodeError::Malformed)
        }
    }

    fn state(value: u64) -> RelationState<Relation> {
        let authority = backend_engine::AuthorityVersion::from_value(b"fixture-authority");
        let coverage = fixture_coverage(authority);
        RelationState::from_entries([(1_u64, value)], coverage).expect("relation")
    }

    fn workspace_root(relation: &RelationState<Relation>) -> WorkspaceRoot {
        let authority = backend_engine::AuthorityVersion::from_value(b"fixture-authority");
        let coverage = fixture_coverage(authority);
        backend_engine::WorkspaceManifest::new_checked(
            1,
            vec![backend_engine::RelationBinding::from_state(relation)],
            Vec::new(),
            backend_engine::ObjectClosure::from_version(authority),
            coverage,
        )
        .expect("workspace manifest")
        .root()
    }

    #[test]
    fn transfer_frames_are_bounded_and_chain_ordered() {
        let limits = TransportLimits {
            max_chunk: 4,
            max_frame: 4096,
            ..TransportLimits::default()
        };
        let authority = AuthorityClaim::from_typed(
            &backend_engine::AuthorityVersion::from_value(b"test-authority"),
            backend_engine::AuthorityEpoch(1),
        );
        let frames =
            product_closure_frames(workspace_root(&state(9)), &state(9), authority, limits, 1)
                .expect("frames");
        assert!(frames.len() > 1);
        let mut by_transfer = BTreeMap::<TransferId, Vec<&Frame>>::new();
        for frame in &frames {
            by_transfer.entry(frame.transfer).or_default().push(frame);
            assert!(frame.payload.len() <= limits.max_chunk);
            frame.validate(limits).expect("frame validation");
        }
        for frames in by_transfer.values() {
            let mut previous = backend_engine::ChunkChain([0; 32]);
            for (sequence, frame) in frames.iter().enumerate() {
                assert_eq!(frame.sequence, sequence as u64);
                assert_eq!(frame.previous_chain, previous);
                previous = frame.chain;
            }
        }
    }

    #[test]
    fn page_source_retains_only_root_handles_until_requested() {
        let relation = state(9);
        let workspace = workspace_root(&relation);
        let mut source = ProductPageSource::from_relation(workspace, &relation).expect("source");
        // Construction stores one synthetic manifest root and the dedicated
        // input object; no relation rows are encoded before a page cursor asks
        // for them.
        assert_eq!(source.closure_counts(), (1, 1));
        let (handle_bytes, object_bytes) = source.cache_bytes_for_test();
        assert!(handle_bytes <= page_source::NODE_CACHE_BYTE_CAPACITY);
        assert!(object_bytes <= page_source::OBJECT_CACHE_BYTE_CAPACITY);
        let root = source.root();
        let page = source
            .page(MerklePageRequest {
                root,
                node: root.digest(),
                cursor: backend_engine::PageCursor::origin(),
                max_items: 1,
            })
            .expect("root page");
        assert!(matches!(page.body, MerklePageBody::Branch(_)));
        assert_eq!(source.closure_counts(), (1, 1));
    }

    #[test]
    fn product_input_transfer_preserves_the_typed_input_identity() {
        let authority = backend_engine::profile_ids(&backend_engine::Profile::Product)
            .expect("product profile")
            .authority;
        let relation = backend_engine::product_source_fixture_with_authority(false, authority)
            .expect("product relation");
        let input = backend_engine::ProductInput::from_checked_relation(&relation);
        let claim = AuthorityClaim::from_typed(&authority, backend_engine::AuthorityEpoch(1));
        let frame = product_input_frame(input, claim, TransportLimits::default(), 1)
            .expect("product input frame");
        assert_eq!(
            frame.version.as_bytes(),
            backend_engine::product_input_version(&input).as_bytes()
        );
        assert_eq!(
            frame.version.context(),
            backend_engine::IdContext::schema::<ImmutableObjectSchema>()
        );
        assert_eq!(frame.payload, backend_engine::product_input_bytes(&input));
    }
}
