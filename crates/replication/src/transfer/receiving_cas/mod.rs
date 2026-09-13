//! Incremental sparse transfer admission backed by an unpublished CAS.
//!
//! [`super::Transfer`] is intentionally simple and keeps its chunk handles so
//! callers can ask for a checkpoint or assemble a value in memory. Large
//! objects need a different ownership path: each admitted payload should move
//! into a sparse unpublished extent store immediately, while the protocol
//! retains only the bounded placement and chain metadata needed for replay,
//! resume, and final identity verification. This module provides that path.

mod types;
mod validation;
pub use types::{ExtentId, StagedExtent, WireStagedExtent};

mod checkpoint;
pub use checkpoint::{
    LeasedReceivingCheckpoint, ReceivingCheckpoint, WireLeasedReceivingCheckpoint,
    WireReceivingCheckpoint,
};

mod sink;
pub use sink::ReceivingCasSink;

mod session;
pub use session::ReceivingCas;
#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use super::super::protocol::ChunkChain;
    use super::*;
    use crate::transfer::lease::{GcRoot, OwnerId, TransferLease};
    use crate::{
        AdmittedChunk, AuthorityClaim, AuthorityExpectation, ObjectRequest, ReplicationError,
        TransferId, TransportLimits,
    };
    use backend_version::{ObjectKey, ObjectVersion, Schema};

    struct Bytes;
    impl Schema for Bytes {
        const DOMAIN: u8 = 0x7a;
        const TYPE: u16 = 23;
        type Value = [u8];
        fn encode(value: &Self::Value, out: &mut Vec<u8>) {
            out.extend_from_slice(value);
        }
    }

    #[derive(Default)]
    struct Sink {
        objects: BTreeMap<ExtentId, Arc<[u8]>>,
        aborted: bool,
        committed: bool,
        fail_write: bool,
    }
    impl ReceivingCasSink<Bytes> for Sink {
        type Session = (TransferId, BTreeMap<ExtentId, Arc<[u8]>>);
        type Receipt = [u8; 32];

        fn begin(
            &mut self,
            transfer: TransferId,
            _key: ObjectKey<Bytes>,
            _version: ObjectVersion<Bytes>,
            _len: u64,
        ) -> Result<Self::Session, ReplicationError> {
            Ok((transfer, BTreeMap::new()))
        }

        fn resume(
            &mut self,
            transfer: TransferId,
            key: ObjectKey<Bytes>,
            version: ObjectVersion<Bytes>,
            len: u64,
            checkpoint: &ReceivingCheckpoint<Bytes>,
        ) -> Result<Self::Session, ReplicationError> {
            self.begin(transfer, key, version, len).map(|mut session| {
                for extent in &checkpoint.extents {
                    if let Some(bytes) = self.objects.get(&extent.id) {
                        session.1.insert(extent.id, Arc::clone(bytes));
                    }
                }
                session
            })
        }

        fn write(
            &mut self,
            session: &mut Self::Session,
            extent: StagedExtent,
            bytes: Arc<[u8]>,
        ) -> Result<(), ReplicationError> {
            if self.fail_write {
                return Err(ReplicationError::Disconnected);
            }
            session.1.insert(extent.id, Arc::clone(&bytes));
            self.objects.insert(extent.id, bytes);
            Ok(())
        }

        fn read_extent(
            &mut self,
            session: &mut Self::Session,
            extent: StagedExtent,
            visitor: &mut dyn FnMut(&[u8]) -> Result<(), ReplicationError>,
        ) -> Result<(), ReplicationError> {
            let bytes = session
                .1
                .get(&extent.id)
                .ok_or(ReplicationError::CorruptFrame)?;
            visitor(bytes)
        }

        fn commit(
            &mut self,
            _session: Self::Session,
            _key: ObjectKey<Bytes>,
            _version: ObjectVersion<Bytes>,
            _len: u64,
            digest: [u8; 32],
        ) -> Result<Self::Receipt, ReplicationError> {
            self.committed = true;
            Ok(digest)
        }

        fn abort(&mut self, _session: Self::Session) {
            self.aborted = true;
        }
    }

    fn limits() -> TransportLimits {
        TransportLimits {
            max_frame: 1024,
            max_chunk: 3,
            max_object: 64,
            max_objects: 8,
            max_ranges: 8,
            max_capabilities: 8,
            max_key_bytes: 32,
            max_inputs: 8,
        }
    }

    fn authority() -> AuthorityClaim {
        let id = ObjectKey::<Bytes>::from_value(b"authority".as_slice());
        AuthorityClaim::from_typed(&id, crate::AuthorityEpoch(1))
    }

    fn admitted_authority() -> crate::AdmittedAuthority {
        let id = ObjectKey::<Bytes>::from_value(b"authority".as_slice());
        AuthorityExpectation::from_typed(&id, crate::AuthorityEpoch(1), crate::RevocationVersion(1))
            .admit_capability(authority(), crate::RevocationVersion(1))
            .expect("authority")
    }

    fn request() -> ObjectRequest<Bytes> {
        let key = ObjectKey::<Bytes>::from_value(b"key".as_slice());
        ObjectRequest::whole(
            TransferId::new(1).expect("transfer"),
            key,
            ObjectVersion::<Bytes>::from_value(b"abcdef".as_slice()),
            6,
            limits().max_ranges,
        )
        .expect("request")
    }

    fn frame(
        request: &ObjectRequest<Bytes>,
        sequence: u64,
        offset: u64,
        previous: ChunkChain,
        payload: &[u8],
    ) -> AdmittedChunk<Bytes> {
        super::super::protocol::Frame::new(
            request.transfer,
            request.key,
            request.version,
            super::super::protocol::ChunkParts {
                object_len: request.len,
                offset,
                sequence,
                previous_chain: previous,
                payload: payload.to_vec(),
            },
            authority(),
        )
        .expect("frame")
        .admit(limits())
        .expect("admitted")
    }

    #[test]
    fn staging_moves_arcs_and_resumes_from_metadata_only_checkpoint() {
        let request = request();
        let authority = authority();
        let admitted_authority = admitted_authority();
        let mut sink = Sink::default();
        let mut transfer = ReceivingCas::new(request.clone(), authority, limits(), 8, &mut sink)
            .expect("receiving cas");
        let empty = transfer.checkpoint().expect("empty checkpoint");
        assert!(empty.extents.is_empty());
        let first = frame(&request, 0, 3, ChunkChain([0; 32]), b"def");
        let second = frame(&request, 1, 0, first.chain(), b"abc");
        transfer.stage(&mut sink, first).expect("first");
        transfer.stage(&mut sink, second).expect("second");
        let checkpoint = transfer.checkpoint().expect("checkpoint");
        assert!(checkpoint.extents.iter().all(|extent| extent.len <= 3));
        assert_eq!(checkpoint.extents.len(), 2);
        assert_eq!(checkpoint.to_wire().expect("wire").extents.len(), 2);
        let owner_key = ObjectKey::<Bytes>::from_value(b"owner".as_slice());
        let owner = OwnerId::<Bytes>::from_typed(&owner_key);
        let lease = TransferLease::new(
            request.transfer,
            owner,
            admitted_authority,
            crate::RevocationVersion(1),
            crate::Fence::new(7).expect("fence"),
            0,
            10,
        )
        .expect("lease");
        let root = GcRoot::new(&lease, request.key, request.version);
        let leased =
            LeasedReceivingCheckpoint::new(checkpoint.clone(), lease, vec![root], limits(), 8)
                .expect("leased checkpoint");
        let wire = leased.to_wire().expect("leased wire");
        let restored = wire
            .admit_against(
                &request,
                owner,
                &[root],
                admitted_authority,
                crate::RevocationVersion(1),
                crate::Fence::new(7).expect("fence"),
                1,
                limits(),
                8,
            )
            .expect("admitted lease");
        assert_eq!(restored.checkpoint().extents.len(), 2);
        let digest = transfer.finish(&mut sink).expect("finish");
        assert_eq!(digest, *request.version.as_bytes());
        assert!(sink.committed);

        let mut resumed_sink = Sink {
            objects: sink.objects,
            ..Sink::default()
        };
        let resumed = ReceivingCas::resume(
            request.clone(),
            authority,
            limits(),
            8,
            checkpoint,
            &mut resumed_sink,
        );
        assert!(resumed.is_ok());
    }

    #[test]
    fn corrupt_staged_extent_aborts_without_publishing() {
        let request = request();
        let mut sink = Sink::default();
        let mut transfer = ReceivingCas::new(request.clone(), authority(), limits(), 8, &mut sink)
            .expect("receiving cas");
        let first = frame(&request, 0, 0, ChunkChain([0; 32]), b"abc");
        let second = frame(&request, 1, 3, first.chain(), b"def");
        transfer.stage(&mut sink, first).expect("first");
        transfer.stage(&mut sink, second).expect("second");
        let key = 1u64;
        transfer.extents.get_mut(&key).expect("extent").chain = ChunkChain([9; 32]);
        assert_eq!(
            transfer.finish(&mut sink),
            Err(ReplicationError::CorruptFrame)
        );
        assert!(sink.aborted);
        assert!(!sink.committed);
    }

    #[test]
    fn sink_write_failure_aborts_the_linear_session() {
        let request = request();
        let mut sink = Sink {
            fail_write: true,
            ..Sink::default()
        };
        let mut transfer = ReceivingCas::new(request.clone(), authority(), limits(), 8, &mut sink)
            .expect("receiving cas");
        let chunk = frame(&request, 0, 0, ChunkChain([0; 32]), b"abc");
        assert_eq!(
            transfer.stage(&mut sink, chunk),
            Err(ReplicationError::Disconnected)
        );
        transfer.abort(&mut sink);
        assert!(sink.aborted);
        assert!(!sink.committed);
    }
}
