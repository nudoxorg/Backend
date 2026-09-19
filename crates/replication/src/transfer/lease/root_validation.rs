//! GC-root admission kept at the lease/object boundary.

use super::{GcRoot, TransferLease};
use crate::{AuthorityClaim, ReplicationError, TransferId, TransportLimits};
use backend_version::{ObjectKey, ObjectVersion, Schema};

pub(crate) fn validate_roots<T: Schema, K>(
    checkpoint: &crate::transfer::checkpoint::TransferCheckpoint<T>,
    lease: &TransferLease<K>,
    roots: &[GcRoot<T, K>],
    limits: TransportLimits,
) -> Result<(), ReplicationError> {
    validate_root_binding(
        checkpoint.transfer,
        checkpoint.key,
        checkpoint.version,
        checkpoint.authority,
        lease,
        roots,
        limits,
    )
}

/// Validates a root set without manufacturing a byte-retaining checkpoint.
pub(crate) fn validate_root_binding<T: Schema, K>(
    transfer: TransferId,
    key: ObjectKey<T>,
    version: ObjectVersion<T>,
    authority: AuthorityClaim,
    lease: &TransferLease<K>,
    roots: &[GcRoot<T, K>],
    limits: TransportLimits,
) -> Result<(), ReplicationError> {
    if authority != lease.authority || transfer != lease.transfer || lease.fence.is_zero() {
        return Err(ReplicationError::StaleFence);
    }
    if roots.is_empty() || roots.len() > limits.max_objects {
        return Err(ReplicationError::CoverageLimit);
    }
    if roots
        .iter()
        .any(|root| root.transfer != transfer || root.lease != lease.token)
    {
        return Err(ReplicationError::StaleFence);
    }
    if !roots
        .iter()
        .any(|root| root.key == key && root.version == version)
    {
        return Err(ReplicationError::IdentityMismatch);
    }
    Ok(())
}
