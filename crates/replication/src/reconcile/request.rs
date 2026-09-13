//! Bounded object and relation range request envelopes.

use std::fmt;

use backend_version::Schema;

use crate::{
    ReplicationError, SchemaWireObjectKey, SchemaWireObjectVersion, SparseCoverage, StateRoot,
    TransferId, TransportLimits, WireStateRoot, claim_state_root,
};

/// A requested immutable object or a bounded part of its byte stream.
pub struct ObjectRequest<T: Schema = crate::ImmutableObjectSchema> {
    /// Transfer identifier used to fence replayed chunks.
    pub transfer: TransferId,
    /// Logical object key.
    pub key: backend_version::ObjectKey<T>,
    /// Complete object version requested.
    pub version: backend_version::ObjectVersion<T>,
    /// Complete canonical object length.
    pub len: u64,
    /// Ranges already retained by the receiver.
    pub ranges: SparseCoverage,
}
impl<T: Schema> Clone for ObjectRequest<T> {
    fn clone(&self) -> Self {
        Self {
            transfer: self.transfer,
            key: self.key,
            version: self.version,
            len: self.len,
            ranges: self.ranges.clone(),
        }
    }
}
impl<T: Schema> fmt::Debug for ObjectRequest<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ObjectRequest")
            .field("transfer", &self.transfer)
            .field("key", &self.key)
            .field("version", &self.version)
            .field("len", &self.len)
            .field("ranges", &self.ranges)
            .finish()
    }
}
impl<T: Schema> PartialEq for ObjectRequest<T> {
    fn eq(&self, other: &Self) -> bool {
        self.transfer == other.transfer
            && self.key == other.key
            && self.version == other.version
            && self.len == other.len
            && self.ranges == other.ranges
    }
}
impl<T: Schema> Eq for ObjectRequest<T> {}
impl<T: Schema> ObjectRequest<T> {
    /// Creates a request for one whole object.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::InvalidIdentifier`] for a zero transfer or
    /// object length, or [`ReplicationError::CoverageLimit`] for a zero
    /// interval budget.
    pub fn whole(
        transfer: TransferId,
        key: backend_version::ObjectKey<T>,
        version: backend_version::ObjectVersion<T>,
        len: u64,
        max_ranges: usize,
    ) -> Result<Self, ReplicationError> {
        if transfer.get() == 0 || len == 0 {
            return Err(ReplicationError::InvalidIdentifier);
        }
        Ok(Self {
            transfer,
            key,
            version,
            len,
            ranges: SparseCoverage::new(max_ranges)?,
        })
    }
    /// Creates a request that begins with sparse ranges selected.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::InvalidIdentifier`] for a zero transfer or
    /// object length, or [`ReplicationError::Range`] when a range exceeds the
    /// object.
    pub fn for_ranges(
        transfer: TransferId,
        key: backend_version::ObjectKey<T>,
        version: backend_version::ObjectVersion<T>,
        len: u64,
        ranges: SparseCoverage,
    ) -> Result<Self, ReplicationError> {
        if transfer.get() == 0 || len == 0 {
            return Err(ReplicationError::InvalidIdentifier);
        }
        if ranges
            .ranges()
            .iter()
            .any(|range| range.end().map_or(true, |end| end > len))
        {
            return Err(ReplicationError::Range);
        }
        Ok(Self {
            transfer,
            key,
            version,
            len,
            ranges,
        })
    }

    /// Validates this request against negotiated object and range bounds.
    ///
    /// # Errors
    ///
    /// Returns an identifier, object-size, or range error when the request is
    /// malformed or exceeds the negotiated limits.
    pub fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate()?;
        if self.transfer.get() == 0 || self.len == 0 {
            return Err(ReplicationError::InvalidIdentifier);
        }
        if self.len > limits.max_object {
            return Err(ReplicationError::ObjectTooLarge);
        }
        if self.ranges.ranges().len() > limits.max_ranges
            || self
                .ranges
                .ranges()
                .iter()
                .any(|range| range.end().map_or(true, |end| end > self.len))
        {
            return Err(ReplicationError::Range);
        }
        Ok(())
    }
}

/// A bounded key/range request over a relation root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RangeRequest {
    /// Relation type tag.
    pub relation: u16,
    /// Exact relation root to read.
    pub root: StateRoot,
    /// Inclusive lower key bytes.
    pub start: Vec<u8>,
    /// Exclusive upper key bytes, if any.
    pub end: Option<Vec<u8>>,
    /// Maximum rows or nodes returned.
    pub limit: u32,
    /// Sparse byte coverage already retained by the receiver.
    pub resume: SparseCoverage,
}
impl RangeRequest {
    /// Validates range key and coverage bounds.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::Range`] when keys, limits, or coverage are
    /// malformed.
    pub fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate()?;
        if self.start.len() > limits.max_key_bytes
            || self
                .end
                .as_ref()
                .is_some_and(|end| end.len() > limits.max_key_bytes)
            || self.end.as_ref().is_some_and(|end| end <= &self.start)
            || self.limit == 0
            || self.resume.ranges().len() > limits.max_ranges
        {
            return Err(ReplicationError::Range);
        }
        for range in self.resume.ranges() {
            range.end()?;
        }
        Ok(())
    }
}

/// A relation range request whose root claim is still untrusted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WireRangeRequest {
    /// Relation type tag.
    pub relation: u16,
    /// Untrusted exact relation root.
    pub root: WireStateRoot,
    /// Inclusive lower key bytes.
    pub start: Vec<u8>,
    /// Exclusive upper key bytes, if any.
    pub end: Option<Vec<u8>>,
    /// Maximum rows or nodes returned.
    pub limit: u32,
    /// Sparse byte coverage already retained by the receiver.
    pub resume: SparseCoverage,
}
impl WireRangeRequest {
    /// Validates the bounded range envelope without granting a typed relation
    /// root.
    ///
    /// # Errors
    ///
    /// Returns an identity, range, or limit error when the wire claim is
    /// malformed.
    pub fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate()?;
        self.root
            .admit_context(backend_version::IdContext::relation::<
                crate::ReplicationRelation,
            >())?;
        if self.start.len() > limits.max_key_bytes
            || self
                .end
                .as_ref()
                .is_some_and(|end| end.len() > limits.max_key_bytes)
            || self.end.as_ref().is_some_and(|end| end <= &self.start)
            || self.limit == 0
            || self.resume.ranges().len() > limits.max_ranges
        {
            return Err(ReplicationError::Range);
        }
        for range in self.resume.ranges() {
            range.end()?;
        }
        Ok(())
    }

    /// Admits the bounded wire envelope while retaining its relation root as
    /// an untrusted claim. Use [`Self::admit_against`] to obtain a typed
    /// request.
    ///
    /// # Errors
    ///
    /// Returns an identity, range, coverage, or context error when the claim
    /// is malformed or exceeds `limits`.
    pub fn admit(self, limits: TransportLimits) -> Result<Self, ReplicationError> {
        self.validate(limits)?;
        Ok(self)
    }

    /// Admits the relation root by exact comparison with a caller-owned typed
    /// state root.
    ///
    /// # Errors
    ///
    /// Returns an identity, range, coverage, or context error when the claim
    /// differs from the expected relation request or exceeds `limits`.
    pub fn admit_against(
        self,
        expected_relation: u16,
        expected_root: StateRoot,
        limits: TransportLimits,
    ) -> Result<RangeRequest, ReplicationError> {
        self.validate(limits)?;
        if self.relation != expected_relation {
            return Err(ReplicationError::IdentityMismatch);
        }
        let expected_claim = claim_state_root(expected_root).map_err(ReplicationError::from)?;
        if self.root != expected_claim {
            return Err(ReplicationError::IdentityMismatch);
        }
        Ok(RangeRequest {
            relation: self.relation,
            root: expected_root,
            start: self.start,
            end: self.end,
            limit: self.limit,
            resume: self.resume,
        })
    }
}

impl RangeRequest {
    /// Converts an admitted relation range request into a wire claim
    /// envelope.
    ///
    /// # Errors
    ///
    /// Returns an identity encoding error if the relation root cannot be
    /// represented on the wire.
    pub fn to_wire(&self) -> Result<WireRangeRequest, ReplicationError> {
        Ok(WireRangeRequest {
            relation: self.relation,
            root: claim_state_root(self.root).map_err(ReplicationError::from)?,
            start: self.start.clone(),
            end: self.end.clone(),
            limit: self.limit,
            resume: self.resume.clone(),
        })
    }
}

/// A request for an immutable node/object extent.
pub struct NodeRequest<T: Schema = crate::ImmutableObjectSchema> {
    /// Transfer identifier used to fence chunks and reconnects.
    pub transfer: TransferId,
    /// Logical node/object key.
    pub key: backend_version::ObjectKey<T>,
    /// Complete immutable node version.
    pub version: backend_version::ObjectVersion<T>,
    /// Complete canonical node length.
    pub len: u64,
    /// Ranges already retained by the receiver.
    pub resume: SparseCoverage,
}
impl<T: Schema> Clone for NodeRequest<T> {
    fn clone(&self) -> Self {
        Self {
            transfer: self.transfer,
            key: self.key,
            version: self.version,
            len: self.len,
            resume: self.resume.clone(),
        }
    }
}
impl<T: Schema> fmt::Debug for NodeRequest<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NodeRequest")
            .field("transfer", &self.transfer)
            .field("key", &self.key)
            .field("version", &self.version)
            .field("len", &self.len)
            .field("resume", &self.resume)
            .finish()
    }
}
impl<T: Schema> PartialEq for NodeRequest<T> {
    fn eq(&self, other: &Self) -> bool {
        self.transfer == other.transfer
            && self.key == other.key
            && self.version == other.version
            && self.len == other.len
            && self.resume == other.resume
    }
}
impl<T: Schema> Eq for NodeRequest<T> {}
impl<T: Schema> NodeRequest<T> {
    /// Validates the node request against transport limits.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::ObjectTooLarge`] when the node or coverage
    /// exceeds the negotiated limits.
    pub fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate()?;
        if self.transfer.get() == 0 || self.len == 0 {
            return Err(ReplicationError::InvalidIdentifier);
        }
        if self.len > limits.max_object {
            return Err(ReplicationError::ObjectTooLarge);
        }
        if self.resume.ranges().len() > limits.max_ranges {
            return Err(ReplicationError::CoverageLimit);
        }
        for range in self.resume.ranges() {
            if range.end()? > self.len {
                return Err(ReplicationError::Range);
            }
        }
        Ok(())
    }

    /// Converts an admitted node request into a wire claim envelope.
    ///
    /// # Errors
    ///
    /// Returns an identity encoding error if a fixed-width claim cannot be
    /// represented on the wire.
    pub fn to_wire(&self) -> Result<WireNodeRequest<T>, ReplicationError> {
        Ok(WireNodeRequest {
            transfer: self.transfer,
            key: crate::claim_schema_object_key(self.key).map_err(ReplicationError::from)?,
            version: crate::claim_schema_object_version(self.version)
                .map_err(ReplicationError::from)?,
            len: self.len,
            resume: self.resume.clone(),
        })
    }
}

/// A wire node request whose identities are still untrusted.
pub struct WireNodeRequest<T: Schema = crate::ImmutableObjectSchema> {
    /// Transfer identifier claim.
    pub transfer: TransferId,
    /// Untrusted object key.
    pub key: SchemaWireObjectKey<T>,
    /// Untrusted object version.
    pub version: SchemaWireObjectVersion<T>,
    /// Claimed complete object length.
    pub len: u64,
    /// Existing sparse coverage.
    pub resume: SparseCoverage,
}
impl<T: Schema> Clone for WireNodeRequest<T> {
    fn clone(&self) -> Self {
        Self {
            transfer: self.transfer,
            key: self.key,
            version: self.version,
            len: self.len,
            resume: self.resume.clone(),
        }
    }
}
impl<T: Schema> fmt::Debug for WireNodeRequest<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WireNodeRequest")
            .field("transfer", &self.transfer)
            .field("key", &self.key)
            .field("version", &self.version)
            .field("len", &self.len)
            .field("resume", &self.resume)
            .finish()
    }
}
impl<T: Schema> PartialEq for WireNodeRequest<T> {
    fn eq(&self, other: &Self) -> bool {
        self.transfer == other.transfer
            && self.key == other.key
            && self.version == other.version
            && self.len == other.len
            && self.resume == other.resume
    }
}
impl<T: Schema> Eq for WireNodeRequest<T> {}
impl<T: Schema> WireNodeRequest<T> {
    /// Checks the node request identities' contexts and bounds without
    /// granting typed object identities.
    ///
    /// # Errors
    ///
    /// Returns an identity or size error when the wire claims are invalid.
    pub fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate()?;
        self.key
            .admit_context(backend_version::IdContext::object_key::<T>())?;
        self.version
            .admit_context(backend_version::IdContext::schema::<T>())?;
        if self.transfer.get() == 0 {
            return Err(ReplicationError::InvalidIdentifier);
        }
        if self.len == 0 || self.len > limits.max_object {
            return Err(ReplicationError::ObjectTooLarge);
        }
        if self.resume.ranges().len() > limits.max_ranges {
            return Err(ReplicationError::CoverageLimit);
        }
        for range in self.resume.ranges() {
            if range.end()? > self.len {
                return Err(ReplicationError::Range);
            }
        }
        Ok(())
    }

    /// Admits the bounded wire envelope while retaining object identities as
    /// untrusted claims. Use [`Self::admit_against`] to obtain a typed request.
    ///
    /// # Errors
    ///
    /// Returns an identity, size, range, coverage, or context error when the
    /// claim is malformed or exceeds `limits`.
    pub fn admit(self, limits: TransportLimits) -> Result<Self, ReplicationError> {
        self.validate(limits)?;
        Ok(self)
    }

    /// Admits this node request by exact comparison with caller-owned typed
    /// request material.
    ///
    /// # Errors
    ///
    /// Returns an identity, fence, range, coverage, or context error when the
    /// claim differs from `expected` or exceeds `limits`.
    pub fn admit_against(
        self,
        expected: &NodeRequest<T>,
        limits: TransportLimits,
    ) -> Result<NodeRequest<T>, ReplicationError> {
        self.validate(limits)?;
        expected.validate(limits)?;
        if self.transfer != expected.transfer {
            return Err(ReplicationError::StaleFence);
        }
        if self.len != expected.len || self.resume != expected.resume {
            return Err(ReplicationError::IdentityMismatch);
        }
        let expected_key =
            crate::claim_schema_object_key(expected.key).map_err(ReplicationError::from)?;
        let expected_version =
            crate::claim_schema_object_version(expected.version).map_err(ReplicationError::from)?;
        if self.key.context() != expected_key.context()
            || self.version.context() != expected_version.context()
        {
            return Err(ReplicationError::IdentityContext);
        }
        if self.key != expected_key || self.version != expected_version {
            return Err(ReplicationError::IdentityMismatch);
        }
        Ok(expected.clone())
    }
}
