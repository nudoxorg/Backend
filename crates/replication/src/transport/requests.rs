//! Resumable object request claims.

use std::fmt;

use backend_version::{
    ObjectKey as VersionObjectKey, ObjectVersion as VersionObjectVersion, Schema,
};

use crate::{
    ImmutableObjectSchema, ReplicationError, SchemaWireObjectKey, SchemaWireObjectVersion,
    SparseCoverage, TransferId, TransportLimits, claim_schema_object_key,
    claim_schema_object_version,
};

/// A resumable request carrying sparse coverage after reconnect.
pub struct ResumeRequest<T: Schema = ImmutableObjectSchema> {
    /// Transfer identifier being resumed.
    pub transfer: TransferId,
    /// Exact object key.
    pub key: VersionObjectKey<T>,
    /// Exact object version.
    pub version: VersionObjectVersion<T>,
    /// Complete canonical object length used to bound retained ranges.
    pub len: u64,
    /// Current retained coverage.
    pub coverage: SparseCoverage,
}
impl<T: Schema> Clone for ResumeRequest<T> {
    fn clone(&self) -> Self {
        Self {
            transfer: self.transfer,
            key: self.key,
            version: self.version,
            len: self.len,
            coverage: self.coverage.clone(),
        }
    }
}
impl<T: Schema> fmt::Debug for ResumeRequest<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResumeRequest")
            .field("transfer", &self.transfer)
            .field("key", &self.key)
            .field("version", &self.version)
            .field("len", &self.len)
            .field("coverage", &self.coverage)
            .finish()
    }
}
impl<T: Schema> PartialEq for ResumeRequest<T> {
    fn eq(&self, other: &Self) -> bool {
        self.transfer == other.transfer
            && self.key == other.key
            && self.version == other.version
            && self.len == other.len
            && self.coverage == other.coverage
    }
}
impl<T: Schema> Eq for ResumeRequest<T> {}
impl<T: Schema> ResumeRequest<T> {
    /// Validates resumable coverage against negotiated limits.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::InvalidIdentifier`] for a zero transfer or
    /// too many retained sparse ranges.
    pub fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate()?;
        if self.transfer.get() == 0 {
            return Err(ReplicationError::InvalidIdentifier);
        }
        if self.len == 0 || self.len > limits.max_object {
            return Err(ReplicationError::ObjectTooLarge);
        }
        if self.coverage.ranges().len() > limits.max_ranges {
            return Err(ReplicationError::CoverageLimit);
        }
        for range in self.coverage.ranges() {
            if range.end().map_or(true, |end| end > self.len) {
                return Err(ReplicationError::Range);
            }
        }
        Ok(())
    }

    /// Converts an admitted resume request into a wire claim envelope.
    ///
    /// # Errors
    ///
    /// Returns an identity encoding error if a fixed-width claim cannot be
    /// represented on the wire.
    pub fn to_wire(&self) -> Result<WireResumeRequest<T>, ReplicationError> {
        Ok(WireResumeRequest {
            transfer: self.transfer,
            key: claim_schema_object_key(self.key).map_err(ReplicationError::from)?,
            version: claim_schema_object_version(self.version).map_err(ReplicationError::from)?,
            len: self.len,
            coverage: self.coverage.clone(),
        })
    }
}

/// A resumable request whose object identities are still untrusted.
pub struct WireResumeRequest<T: Schema = ImmutableObjectSchema> {
    /// Transfer identifier.
    pub transfer: TransferId,
    /// Untrusted object key.
    pub key: SchemaWireObjectKey<T>,
    /// Untrusted object version.
    pub version: SchemaWireObjectVersion<T>,
    /// Complete canonical object length.
    pub len: u64,
    /// Sparse coverage retained by the receiver.
    pub coverage: SparseCoverage,
}
impl<T: Schema> Clone for WireResumeRequest<T> {
    fn clone(&self) -> Self {
        Self {
            transfer: self.transfer,
            key: self.key,
            version: self.version,
            len: self.len,
            coverage: self.coverage.clone(),
        }
    }
}
impl<T: Schema> fmt::Debug for WireResumeRequest<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WireResumeRequest")
            .field("transfer", &self.transfer)
            .field("key", &self.key)
            .field("version", &self.version)
            .field("len", &self.len)
            .field("coverage", &self.coverage)
            .finish()
    }
}
impl<T: Schema> PartialEq for WireResumeRequest<T> {
    fn eq(&self, other: &Self) -> bool {
        self.transfer == other.transfer
            && self.key == other.key
            && self.version == other.version
            && self.len == other.len
            && self.coverage == other.coverage
    }
}
impl<T: Schema> Eq for WireResumeRequest<T> {}
impl<T: Schema> WireResumeRequest<T> {
    /// Checks object identity contexts and sparse coverage bounds without
    /// granting typed object identities.
    ///
    /// # Errors
    ///
    /// Returns an identity, identifier, or coverage error when the wire
    /// claim is malformed.
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
        if self.coverage.ranges().len() > limits.max_ranges {
            return Err(ReplicationError::CoverageLimit);
        }
        for range in self.coverage.ranges() {
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
    /// Returns an identity, object-size, range, coverage, or context error
    /// when the claim is malformed or exceeds `limits`.
    pub fn admit(self, limits: TransportLimits) -> Result<Self, ReplicationError> {
        self.validate(limits)?;
        Ok(self)
    }

    /// Admits this request by exact comparison with the caller-owned typed
    /// request retained across a reconnect.
    ///
    /// # Errors
    ///
    /// Returns an identity, fence, coverage, or context error when the claim
    /// differs from `expected` or exceeds `limits`.
    pub fn admit_against(
        self,
        expected: &ResumeRequest<T>,
        limits: TransportLimits,
    ) -> Result<ResumeRequest<T>, ReplicationError> {
        self.validate(limits)?;
        expected.validate(limits)?;
        if self.transfer != expected.transfer {
            return Err(ReplicationError::StaleFence);
        }
        if self.len != expected.len || self.coverage != expected.coverage {
            return Err(ReplicationError::IdentityMismatch);
        }
        let expected_key = claim_schema_object_key(expected.key).map_err(ReplicationError::from)?;
        let expected_version =
            claim_schema_object_version(expected.version).map_err(ReplicationError::from)?;
        if self.key.context() != expected_key.context()
            || self.version.context() != expected_version.context()
        {
            return Err(ReplicationError::IdentityContext);
        }
        if self.key != expected_key || self.version != expected_version {
            return Err(ReplicationError::StaleFence);
        }
        Ok(expected.clone())
    }
}
