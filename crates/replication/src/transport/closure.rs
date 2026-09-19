//! Correlated Merkle closure control messages.
//!
//! Merkle pages are deliberately carried as control messages instead of being
//! smuggled through the object frame stream.  The correlation token makes a
//! request/response pair unambiguous when page reads and object transfers are
//! interleaved on one duplex connection.

use crate::{
    MerklePage, MerklePageRequest, ReplicationError, TransportLimits, WireAuthority,
    WorkspaceRootClaim,
};

/// Constant-time offer of one authenticated closure root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClosureRootOffer {
    /// Connection-local request correlation.
    pub correlation: u64,
    /// Workspace closure identity.
    pub workspace: WorkspaceRootClaim,
    /// Merkle root commitment.
    pub root: crate::MerkleRoot,
    /// Authority which authenticated the offer.
    pub authority: WireAuthority,
    /// Canonical workspace-manifest bytes. The receiver decodes and admits
    /// these bytes against the persisted relation root before publishing any
    /// warm marker.
    pub workspace_manifest: Vec<u8>,
}

impl ClosureRootOffer {
    /// Validates the root offer envelope.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid correlation or oversized manifest bytes.
    pub fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate()?;
        if self.correlation == 0 {
            return Err(ReplicationError::InvalidIdentifier);
        }
        if self.workspace_manifest.is_empty() || self.workspace_manifest.len() > limits.max_frame {
            return Err(ReplicationError::MessageTooLarge);
        }
        Ok(())
    }
}

/// Worker response to a root offer. `warm` is true only after the worker has
/// a durable, authenticated closure and exact recipe input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClosureRootAck {
    /// Correlation copied from the offer.
    pub correlation: u64,
    /// Root being acknowledged.
    pub root: crate::MerkleRoot,
    /// Whether the worker can admit a recipe immediately.
    pub warm: bool,
    /// Exact immutable object versions absent from the worker CAS.
    pub missing: Vec<crate::WireIdentity>,
    /// Cursor for the next bounded need batch. `None` means this is the final
    /// batch and the closure can proceed to recipe admission after transfer.
    pub next: Option<u32>,
}

impl ClosureRootAck {
    /// Validates the acknowledgement envelope.
    ///
    /// # Errors
    ///
    /// Returns an error when the need batch or continuation is out of bounds.
    pub fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate()?;
        if self.correlation == 0 {
            return Err(ReplicationError::InvalidIdentifier);
        }
        if self.missing.len() > limits.max_objects {
            return Err(ReplicationError::MessageTooLarge);
        }
        if self.next.is_some_and(|cursor| cursor == u32::MAX) {
            return Err(ReplicationError::Range);
        }
        Ok(())
    }
}

/// Requests the next bounded object-need batch after the previous batch has
/// been transferred. Keeping this cursor explicit makes the object plane
/// resumable without retaining a closure-sized missing vector in either peer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClosureNeedRequest {
    /// Connection-local request correlation.
    pub correlation: u64,
    /// Root whose object need stream is being resumed.
    pub root: crate::MerkleRoot,
    /// Exclusive cursor into the bounded need stream.
    pub cursor: u32,
}

impl ClosureNeedRequest {
    /// Validates the request against negotiated transport limits.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid correlation or limits.
    pub fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate()?;
        if self.correlation == 0 {
            return Err(ReplicationError::InvalidIdentifier);
        }
        Ok(())
    }
}

/// One bounded request for a page of an authenticated closure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClosurePageRequest {
    /// Connection-local request correlation.
    pub correlation: u64,
    /// The authenticated root and node window to read.
    pub request: MerklePageRequest,
}

impl ClosurePageRequest {
    /// Validates the correlation and page allocation bound.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid correlation, item count, or limits.
    pub fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate()?;
        if self.correlation == 0 || self.request.max_items == 0 {
            return Err(ReplicationError::InvalidIdentifier);
        }
        if usize::from(self.request.max_items) > limits.max_objects {
            return Err(ReplicationError::MessageTooLarge);
        }
        Ok(())
    }
}

/// The response to one [`ClosurePageRequest`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClosurePageResponse {
    /// Correlation copied from the request.
    pub correlation: u64,
    /// Canonical node/manifest bytes covering the page commitment. The
    /// receiver authenticates these bytes before using the page body.
    pub proof: Vec<u8>,
    /// Untrusted page claim; the receiver must bind it to the requested root.
    pub page: MerklePage,
}

impl ClosurePageResponse {
    /// Validates page shape and bounds before a reconciler sees the page.
    ///
    /// # Errors
    ///
    /// Returns an error when proof or page shape exceeds negotiated bounds.
    pub fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate()?;
        if self.correlation == 0 {
            return Err(ReplicationError::InvalidIdentifier);
        }
        if self.proof.is_empty() || self.proof.len() > limits.max_frame {
            return Err(ReplicationError::MessageTooLarge);
        }
        self.page.validate(limits.max_objects, limits.max_key_bytes)
    }

    /// Binds an untrusted response to the exact outstanding request. A valid
    /// page for another root or node is still a protocol violation.
    ///
    /// # Errors
    ///
    /// Returns an error when the response does not match the outstanding
    /// request or violates negotiated page bounds.
    pub fn admit_against(
        &self,
        request: ClosurePageRequest,
        limits: TransportLimits,
    ) -> Result<(), ReplicationError> {
        request.validate(limits)?;
        self.validate(limits)?;
        if self.correlation != request.correlation
            || self.page.root != request.request.root
            || self.page.node != request.request.node
            || self.page.cursor != request.request.cursor
        {
            return Err(ReplicationError::IdentityMismatch);
        }
        let items = match &self.page.body {
            crate::MerklePageBody::Branch(children) => children.len(),
            crate::MerklePageBody::Leaf(entries) => entries.len(),
        };
        if items > usize::from(request.request.max_items) {
            return Err(ReplicationError::MessageTooLarge);
        }
        if let Some(next) = self.page.next {
            let expected = self
                .page
                .cursor
                .offset
                .checked_add(u32::try_from(items).map_err(|_| ReplicationError::Overflow)?)
                .ok_or(ReplicationError::Overflow)?;
            if next.offset != expected || items == 0 {
                return Err(ReplicationError::Range);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MerklePageBody, NodeDigest, PageCursor};

    fn limits() -> TransportLimits {
        TransportLimits::default()
    }

    #[test]
    fn page_proof_is_required_before_a_window_is_admitted() {
        let root = crate::MerkleRoot::new(1, NodeDigest([1; 32]));
        let request = ClosurePageRequest {
            correlation: 1,
            request: MerklePageRequest {
                root,
                node: root.digest(),
                cursor: PageCursor::origin(),
                max_items: 1,
            },
        };
        let response = ClosurePageResponse {
            correlation: 1,
            proof: Vec::new(),
            page: MerklePage {
                root,
                node: root.digest(),
                level: 0,
                cursor: PageCursor::origin(),
                next: None,
                body: MerklePageBody::Leaf(Vec::new()),
            },
        };
        assert!(response.admit_against(request, limits()).is_err());
    }
}
