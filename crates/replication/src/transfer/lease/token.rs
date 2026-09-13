//! Domain separated derivation for transfer lease capabilities.

use crate::{AuthorityClaim, Fence, RevocationVersion, TransferId, WireIdentity};

/// Derives the token from every immutable lease field and its identity context.
pub(super) fn derive_lease_token(
    transfer: TransferId,
    owner: WireIdentity,
    authority: AuthorityClaim,
    revocation_version: RevocationVersion,
    fence: Fence,
    issued_at: u64,
    expires_at: u64,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.replication.transfer-lease.v1\0");
    hasher.update(&transfer.get().to_be_bytes());
    hasher.update(&owner.as_bytes());
    hasher.update(&[owner.context().class(), owner.context().domain()]);
    hasher.update(&owner.context().ty().to_be_bytes());
    hasher.update(&[owner.context().version()]);
    hasher.update(&authority.id.as_bytes());
    hasher.update(&authority.epoch.0.to_be_bytes());
    hasher.update(&revocation_version.0.to_be_bytes());
    hasher.update(&fence.as_bytes());
    hasher.update(&issued_at.to_be_bytes());
    hasher.update(&expires_at.to_be_bytes());
    *hasher.finalize().as_bytes()
}
