//! Lease capability tests.

use super::*;
use crate::{
    AdmittedAuthority, AuthorityEpoch, AuthorityExpectation, RevocationVersion, SparseCoverage,
};

struct Bytes;
impl Schema for Bytes {
    const DOMAIN: u8 = 0x73;
    const TYPE: u16 = 9;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

fn limits() -> TransportLimits {
    TransportLimits {
        max_frame: 1024,
        max_chunk: 16,
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
    AuthorityClaim::from_typed(&id, AuthorityEpoch(4))
}

fn admitted_authority() -> AdmittedAuthority {
    let id = ObjectKey::<Bytes>::from_value(b"authority".as_slice());
    let claim = AuthorityClaim::from_typed(&id, AuthorityEpoch(4));
    AuthorityExpectation::from_typed(&id, AuthorityEpoch(4), RevocationVersion(2))
        .admit_capability(claim, RevocationVersion(2))
        .expect("authority")
}

fn request() -> ObjectRequest<Bytes> {
    let key = ObjectKey::<Bytes>::from_value(b"key".as_slice());
    let value = b"value".as_slice();
    ObjectRequest::whole(
        TransferId::new(9).expect("transfer"),
        key,
        ObjectVersion::<Bytes>::from_value(value),
        value.len() as u64,
        limits().max_ranges,
    )
    .expect("request")
}

#[test]
fn lease_binds_owner_fence_and_expiry() {
    let request = request();
    let owner_key = ObjectKey::<Bytes>::from_value(b"owner".as_slice());
    let owner = OwnerId::<Bytes>::from_typed(&owner_key);
    let fence = Fence::new(11u64).expect("fence");
    let lease = TransferLease::new(
        request.transfer,
        owner,
        admitted_authority(),
        RevocationVersion(2),
        fence,
        10,
        20,
    )
    .expect("lease");
    assert_eq!(
        lease.validate_at(
            owner,
            request.transfer,
            admitted_authority(),
            fence,
            RevocationVersion(2),
            10
        ),
        Ok(())
    );
    assert_eq!(
        lease.validate_at(
            owner,
            request.transfer,
            admitted_authority(),
            fence,
            RevocationVersion(2),
            20
        ),
        Err(ReplicationError::StaleFence)
    );

    let checkpoint = TransferCheckpoint {
        transfer: request.transfer,
        key: request.key,
        version: request.version,
        len: request.len,
        authority: authority(),
        chunks: Vec::new(),
        coverage: SparseCoverage::new(limits().max_ranges).expect("coverage"),
    };
    // A checkpoint with no retained bytes is structurally valid; the GC
    // root still pins the exact target object and lease token.
    let root = GcRoot::new(&lease, request.key, request.version);
    let leased = LeasedTransferCheckpoint::new(checkpoint, lease, vec![root], limits());
    assert!(leased.is_ok());
}
