//! Process-unique subscription lease identities.

use crate::protocol::ProtocolError;
use backend_engine::LocalSubscriptionId;
#[cfg(unix)]
use std::io::Read;

/// One process-local lease namespace. It is minted from the OS before the
/// first Open, never serialized, and discarded when this owner stops. A peer
/// reconnecting to the same endpoint path receives a new random namespace;
/// reusing an old 128-bit lease ID would require a cryptographic collision.
pub(crate) struct OwnerBootNonce(pub(crate) [u8; 32]);

impl OwnerBootNonce {
    fn fresh() -> std::io::Result<Self> {
        let mut bytes = [0_u8; 32];
        #[cfg(unix)]
        std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        #[cfg(windows)]
        backend_platform::win32::random::fill(&mut bytes)?;
        #[cfg(not(any(unix, windows)))]
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "owner lease entropy is unavailable on this platform",
        ));
        Ok(Self(bytes))
    }

    fn lease_id(&self, nonce: u64, request_id: u64, cursor: &[u8]) -> Option<LocalSubscriptionId> {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend-locald-subscription-lease.v2\0");
        hasher.update(&self.0);
        hasher.update(&nonce.to_be_bytes());
        hasher.update(&request_id.to_be_bytes());
        hasher.update(cursor);
        let digest = hasher.finalize();
        let mut bytes = [0_u8; 16];
        bytes.copy_from_slice(&digest.as_bytes()[..16]);
        (bytes != [0_u8; 16]).then_some(LocalSubscriptionId::from_bytes(bytes))
    }
}

impl Drop for OwnerBootNonce {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

/// Mints lease identities: an OS-random boot namespace plus a checked ordinal,
/// so an identity can neither recur across owner boots nor wrap within one.
#[derive(Default)]
pub(crate) struct OwnerLeaseIdentity {
    pub(crate) boot_nonce: Option<OwnerBootNonce>,
    pub(crate) next_nonce: u64,
}

impl OwnerLeaseIdentity {
    /// Allocates an identity that `is_active` does not already hold.
    pub(crate) fn allocate(
        &mut self,
        request_id: u64,
        cursor: &[u8],
        is_active: impl Fn(&LocalSubscriptionId) -> bool,
    ) -> Result<LocalSubscriptionId, ProtocolError> {
        if self.boot_nonce.is_none() {
            self.boot_nonce =
                Some(OwnerBootNonce::fresh().map_err(|_| ProtocolError::LeaseEntropyUnavailable)?);
        }
        let boot_nonce = self
            .boot_nonce
            .as_ref()
            .ok_or(ProtocolError::LeaseEntropyUnavailable)?;
        loop {
            self.next_nonce = self
                .next_nonce
                .checked_add(1)
                .ok_or(ProtocolError::LeaseIdsExhausted)?;
            if let Some(lease) = boot_nonce.lease_id(self.next_nonce, request_id, cursor)
                && !is_active(&lease)
            {
                return Ok(lease);
            }
        }
    }
}
