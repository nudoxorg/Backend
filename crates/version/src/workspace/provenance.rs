/// Authority and transaction provenance bound to a commit.
///
/// Both identities are canonical typed object versions.  The detail bytes are
/// an opaque producer record (for example an authority fence, request ID, or
/// journal transaction payload) and are included in the commit identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitProvenance {
    authority: ObjectClosure,
    transaction: ObjectClosure,
    detail: Vec<u8>,
}

/// A typed schema/version reference retained by untrusted provenance bytes.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct UntrustedClosureRef {
    schema: SchemaIdentity,
    version: [u8; ID_BYTES],
}

impl UntrustedClosureRef {
    /// Returns the schema identity claimed by the wire value.
    #[must_use]
    pub const fn schema(self) -> SchemaIdentity {
        self.schema
    }

    /// Returns the untrusted object-version bytes claimed by the wire value.
    #[must_use]
    pub const fn version(self) -> [u8; ID_BYTES] {
        self.version
    }
}

/// Structurally decoded commit provenance awaiting typed closure admission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UntrustedCommitProvenance {
    authority: UntrustedClosureRef,
    transaction: UntrustedClosureRef,
    detail: Vec<u8>,
}

impl UntrustedCommitProvenance {
    /// Returns the untrusted authority closure claim.
    #[must_use]
    pub const fn authority(&self) -> UntrustedClosureRef {
        self.authority
    }

    /// Returns the untrusted transaction closure claim.
    #[must_use]
    pub const fn transaction(&self) -> UntrustedClosureRef {
        self.transaction
    }

    /// Returns opaque producer detail bytes from the wire value.
    #[must_use]
    pub fn detail(&self) -> &[u8] {
        &self.detail
    }

    /// Re-encodes the untrusted value in canonical form.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        encode_provenance(self.authority, self.transaction, &self.detail)
    }

    /// Admits this provenance against exact typed authority and transaction
    /// closures.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError::ProvenanceMismatch`] if either claimed
    /// closure differs from the supplied typed object.
    pub fn admit(
        self,
        authority: ObjectClosure,
        transaction: ObjectClosure,
    ) -> Result<CommitProvenance, WorkspaceError> {
        if self.authority.schema != authority.schema
            || self.authority.version != authority.root
            || self.transaction.schema != transaction.schema
            || self.transaction.version != transaction.root
        {
            return Err(WorkspaceError::ProvenanceMismatch);
        }
        Ok(CommitProvenance::new(authority, transaction, self.detail))
    }
}

impl CommitProvenance {
    /// Creates provenance from checked authority and transaction closures.
    #[must_use]
    pub fn new(
        authority: ObjectClosure,
        transaction: ObjectClosure,
        detail: impl Into<Vec<u8>>,
    ) -> Self {
        Self {
            authority,
            transaction,
            detail: detail.into(),
        }
    }

    /// Creates provenance directly from typed object versions.
    #[must_use]
    pub fn from_versions<A: Schema, T: Schema>(
        authority: ObjectVersion<A>,
        transaction: ObjectVersion<T>,
        detail: impl Into<Vec<u8>>,
    ) -> Self {
        Self::new(
            ObjectClosure::from_version(authority),
            ObjectClosure::from_version(transaction),
            detail,
        )
    }

    /// Encodes checked provenance in canonical bounded wire form.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        encode_provenance(
            UntrustedClosureRef {
                schema: self.authority.schema,
                version: self.authority.root,
            },
            UntrustedClosureRef {
                schema: self.transaction.schema,
                version: self.transaction.root,
            },
            &self.detail,
        )
    }

    /// Decodes provenance without trusting either closure identity.
    ///
    /// # Errors
    ///
    /// Returns a bounded structural error for malformed fields, oversized
    /// detail, an unknown wire version, or trailing bytes.
    pub fn decode_untrusted(
        bytes: &[u8],
    ) -> Result<UntrustedCommitProvenance, WorkspaceDecodeError> {
        let mut reader = WireReader::new(bytes)?;
        reader.magic(PROVENANCE_WIRE_MAGIC)?;
        let authority = UntrustedClosureRef {
            schema: read_schema(&mut reader)?,
            version: reader.digest()?,
        };
        let transaction = UntrustedClosureRef {
            schema: read_schema(&mut reader)?,
            version: reader.digest()?,
        };
        let detail = reader.field()?.to_vec();
        if detail.len() > MAX_WIRE_DETAIL_BYTES {
            return Err(WorkspaceDecodeError::InvalidLength);
        }
        reader.finish()?;
        Ok(UntrustedCommitProvenance {
            authority,
            transaction,
            detail,
        })
    }

    /// Rebinds a decoded provenance value to exact typed closures.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError::ProvenanceMismatch`] when the wire claims do
    /// not match the supplied typed closures.
    pub fn admit_untrusted(
        untrusted: UntrustedCommitProvenance,
        authority: ObjectClosure,
        transaction: ObjectClosure,
    ) -> Result<Self, WorkspaceError> {
        untrusted.admit(authority, transaction)
    }

    /// Returns typed durable references for the authority and transaction
    /// objects carried by this checked provenance.
    #[must_use]
    pub fn closure_refs(&self) -> Vec<ClosureRef> {
        vec![
            ClosureRef::new(
                ClosureKind::Authority,
                self.authority.schema,
                self.authority.root,
            ),
            ClosureRef::new(
                ClosureKind::Transaction,
                self.transaction.schema,
                self.transaction.root,
            ),
        ]
    }

    /// Returns the authority closure.
    #[must_use]
    pub const fn authority(&self) -> ObjectClosure {
        self.authority
    }

    /// Returns the transaction closure.
    #[must_use]
    pub const fn transaction(&self) -> ObjectClosure {
        self.transaction
    }

    /// Returns the opaque producer detail bytes.
    #[must_use]
    pub fn detail(&self) -> &[u8] {
        &self.detail
    }
}

fn encode_provenance(
    authority: UntrustedClosureRef,
    transaction: UntrustedClosureRef,
    detail: &[u8],
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&PROVENANCE_WIRE_MAGIC);
    out.push(WORKSPACE_WIRE_VERSION);
    push_schema(&mut out, authority.schema);
    push_digest(&mut out, authority.version);
    push_schema(&mut out, transaction.schema);
    push_digest(&mut out, transaction.version);
    append_wire_field(&mut out, detail);
    out
}
