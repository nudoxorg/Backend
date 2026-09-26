//! Root and immutable-object summary claims.

use std::fmt;

use backend_version::{
    ObjectKey as VersionObjectKey, ObjectVersion as VersionObjectVersion, Schema,
};

use crate::{
    AdmittedAuthority, AuthorityClaim, AuthorityExpectation, ReplicationError, RevocationVersion,
    SchemaWireObjectKey, SchemaWireObjectVersion, SparseCoverage, StateRoot, TransportLimits,
    WireAuthority, WireStateRoot, WorkspaceRoot, WorkspaceRootClaim, claim_state_root,
};

mod admitted;

/// A schema-marked immutable object summary.
pub struct ObjectSummary<T: Schema = crate::ImmutableObjectSchema> {
    /// Logical object key.
    pub key: VersionObjectKey<T>,
    /// Complete object version.
    pub version: VersionObjectVersion<T>,
    /// Canonical encoded object length.
    pub len: u64,
}
impl<T: Schema> Copy for ObjectSummary<T> {}
impl<T: Schema> Clone for ObjectSummary<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T: Schema> fmt::Debug for ObjectSummary<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ObjectSummary")
            .field("key", &self.key)
            .field("version", &self.version)
            .field("len", &self.len)
            .finish()
    }
}
impl<T: Schema> PartialEq for ObjectSummary<T> {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && self.version == other.version && self.len == other.len
    }
}
impl<T: Schema> Eq for ObjectSummary<T> {}

/// A relation root and its sparse availability coverage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelationSummary {
    /// Relation type tag chosen by the workspace schema.
    pub relation: u16,
    /// Canonical relation root.
    pub root: StateRoot,
    /// Ranges of the relation/node stream available at this peer.
    pub coverage: SparseCoverage,
}

/// A checked root-of-roots summary used to plan immutable object requests.
///
/// The representation is intentionally opaque at the crate boundary. A wire
/// claim is not a checked summary and cannot be assembled by filling public
/// fields:
///
/// ```compile_fail
/// use backend_replication::RootSummary;
/// fn forge(summary: &RootSummary) {
///     let _ = summary.workspace;
/// }
/// ```
pub struct RootSummary<T: Schema = crate::ImmutableObjectSchema> {
    /// Canonical schema ABI number for this workspace manifest.
    schema: u32,
    /// Exact workspace root represented by this summary.
    workspace: WorkspaceRoot,
    /// Relation-root summaries selected by the workspace schema.
    relations: Vec<RelationSummary>,
    /// Immutable objects available or requested under this root.
    objects: Vec<ObjectSummary<T>>,
    /// Untrusted authority claim carried with the summary. The workspace
    /// owner admits it against its own authority policy.
    authority: AuthorityClaim,
    /// Sparse coverage for the summary stream.
    coverage: SparseCoverage,
}
impl<T: Schema> Clone for RootSummary<T> {
    fn clone(&self) -> Self {
        Self {
            schema: self.schema,
            workspace: self.workspace,
            relations: self.relations.clone(),
            objects: self.objects.clone(),
            authority: self.authority,
            coverage: self.coverage.clone(),
        }
    }
}
impl<T: Schema> fmt::Debug for RootSummary<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RootSummary")
            .field("schema", &self.schema)
            .field("workspace", &self.workspace)
            .field("relations", &self.relations)
            .field("objects", &self.objects)
            .field("authority", &self.authority)
            .field("coverage", &self.coverage)
            .finish()
    }
}
impl<T: Schema> PartialEq for RootSummary<T> {
    fn eq(&self, other: &Self) -> bool {
        self.schema == other.schema
            && self.workspace == other.workspace
            && self.relations == other.relations
            && self.objects == other.objects
            && self.authority == other.authority
            && self.coverage == other.coverage
    }
}
impl<T: Schema> Eq for RootSummary<T> {}

/// One typed relation root expected in a workspace summary closure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RelationRootExpectation {
    /// Relation type tag in the workspace schema.
    pub relation: u16,
    /// Exact relation root expected for this tag.
    pub root: StateRoot,
}

/// One typed immutable object expected in a root-summary closure.
pub struct ObjectSummaryExpectation<T: Schema = crate::ImmutableObjectSchema> {
    /// Exact logical object key expected by the caller.
    pub key: VersionObjectKey<T>,
    /// Exact complete object version expected by the caller.
    pub version: VersionObjectVersion<T>,
    /// Exact canonical object length expected by the caller.
    pub len: u64,
}
impl<T: Schema> Copy for ObjectSummaryExpectation<T> {}
impl<T: Schema> Clone for ObjectSummaryExpectation<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T: Schema> fmt::Debug for ObjectSummaryExpectation<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ObjectSummaryExpectation")
            .field("key", &self.key)
            .field("version", &self.version)
            .field("len", &self.len)
            .finish()
    }
}
impl<T: Schema> PartialEq for ObjectSummaryExpectation<T> {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && self.version == other.version && self.len == other.len
    }
}
impl<T: Schema> Eq for ObjectSummaryExpectation<T> {}

/// Caller-owned schema and relation closure used to admit a wire root.
pub struct RootSummaryExpectation<T: Schema = crate::ImmutableObjectSchema> {
    /// Exact workspace schema ABI expected by the caller.
    pub schema: u32,
    /// Exact workspace root expected by the caller.
    pub workspace: WorkspaceRoot,
    /// Exact ordered relation-root closure expected by the caller.
    pub relations: Vec<RelationRootExpectation>,
    /// Exact ordered immutable-object closure expected by the caller.
    pub objects: Vec<ObjectSummaryExpectation<T>>,
}
impl<T: Schema> Clone for RootSummaryExpectation<T> {
    fn clone(&self) -> Self {
        Self {
            schema: self.schema,
            workspace: self.workspace,
            relations: self.relations.clone(),
            objects: self.objects.clone(),
        }
    }
}
impl<T: Schema> fmt::Debug for RootSummaryExpectation<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RootSummaryExpectation")
            .field("schema", &self.schema)
            .field("workspace", &self.workspace)
            .field("relations", &self.relations)
            .field("objects", &self.objects)
            .finish()
    }
}
impl<T: Schema> PartialEq for RootSummaryExpectation<T> {
    fn eq(&self, other: &Self) -> bool {
        self.schema == other.schema
            && self.workspace == other.workspace
            && self.relations == other.relations
            && self.objects == other.objects
    }
}
impl<T: Schema> Eq for RootSummaryExpectation<T> {}
impl<T: Schema> RootSummaryExpectation<T> {
    /// Validates the expected closure before comparing wire claims.
    ///
    /// # Errors
    ///
    /// Returns a size or ordering error when the caller's closure is not
    /// canonical under the negotiated limits.
    pub fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate()?;
        if self.relations.len() > limits.max_objects {
            return Err(ReplicationError::MessageTooLarge);
        }
        if self
            .relations
            .windows(2)
            .any(|pair| pair[0].relation >= pair[1].relation)
        {
            return Err(ReplicationError::Unsorted);
        }
        if self.objects.len() > limits.max_objects
            || self
                .objects
                .windows(2)
                .any(|pair| pair[0].key >= pair[1].key)
        {
            return Err(ReplicationError::Unsorted);
        }
        if self
            .objects
            .iter()
            .any(|object| object.len == 0 || object.len > limits.max_object)
        {
            return Err(ReplicationError::ObjectTooLarge);
        }
        Ok(())
    }
}
impl RelationSummary {
    /// Converts an admitted relation summary into a wire claim envelope.
    ///
    /// # Errors
    ///
    /// Returns an identity encoding error if the relation root cannot be
    /// represented on the wire.
    pub fn to_wire(&self) -> Result<WireRelationSummary, ReplicationError> {
        Ok(WireRelationSummary {
            relation: self.relation,
            root: claim_state_root(self.root).map_err(ReplicationError::from)?,
            coverage: self.coverage.clone(),
        })
    }
}

impl<T: Schema> ObjectSummary<T> {
    /// Converts an admitted object summary into a wire claim envelope.
    ///
    /// # Errors
    ///
    /// Returns an identity encoding error if a fixed-width claim cannot be
    /// represented on the wire.
    pub fn to_wire(&self) -> Result<WireObjectSummary<T>, ReplicationError> {
        Ok(WireObjectSummary {
            key: crate::claim_schema_object_key(self.key).map_err(ReplicationError::from)?,
            version: crate::claim_schema_object_version(self.version)
                .map_err(ReplicationError::from)?,
            len: self.len,
        })
    }
}

/// A root summary claim as received from the wire.
pub struct WireRootSummary<T: Schema = crate::ImmutableObjectSchema> {
    /// Canonical schema ABI number.
    pub schema: u32,
    /// Untrusted workspace-root bytes.
    pub workspace: WorkspaceRootClaim,
    /// Untrusted relation roots.
    pub relations: Vec<WireRelationSummary>,
    /// Untrusted immutable object summaries.
    pub objects: Vec<WireObjectSummary<T>>,
    /// Untrusted authority claim.
    pub authority: WireAuthority,
    /// Sparse summary coverage.
    pub coverage: SparseCoverage,
}
impl<T: Schema> Clone for WireRootSummary<T> {
    fn clone(&self) -> Self {
        Self {
            schema: self.schema,
            workspace: self.workspace,
            relations: self.relations.clone(),
            objects: self.objects.clone(),
            authority: self.authority,
            coverage: self.coverage.clone(),
        }
    }
}
impl<T: Schema> fmt::Debug for WireRootSummary<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WireRootSummary")
            .field("schema", &self.schema)
            .field("workspace", &self.workspace)
            .field("relations", &self.relations)
            .field("objects", &self.objects)
            .field("authority", &self.authority)
            .field("coverage", &self.coverage)
            .finish()
    }
}
impl<T: Schema> PartialEq for WireRootSummary<T> {
    fn eq(&self, other: &Self) -> bool {
        self.schema == other.schema
            && self.workspace == other.workspace
            && self.relations == other.relations
            && self.objects == other.objects
            && self.authority == other.authority
            && self.coverage == other.coverage
    }
}
impl<T: Schema> Eq for WireRootSummary<T> {}
impl<T: Schema> WireRootSummary<T> {
    /// Checks bounded wire structure before a requested workspace root is
    /// available for identity admission.
    ///
    /// # Errors
    ///
    /// Returns a size or coverage error when the untrusted summary exceeds
    /// negotiated limits.
    pub fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate()?;
        if self.objects.len() > limits.max_objects || self.relations.len() > limits.max_objects {
            return Err(ReplicationError::MessageTooLarge);
        }
        for relation in &self.relations {
            relation.validate(limits)?;
        }
        for object in &self.objects {
            object.validate(limits)?;
        }
        if self
            .relations
            .windows(2)
            .any(|pair| pair[0].relation >= pair[1].relation)
            || self
                .objects
                .windows(2)
                .any(|pair| pair[0].key.as_bytes() >= pair[1].key.as_bytes())
        {
            return Err(ReplicationError::Unsorted);
        }
        if self.coverage.ranges().len() > limits.max_ranges
            || self
                .relations
                .iter()
                .any(|relation| relation.coverage.ranges().len() > limits.max_ranges)
        {
            return Err(ReplicationError::CoverageLimit);
        }
        if self
            .objects
            .iter()
            .any(|object| object.len == 0 || object.len > limits.max_object)
        {
            return Err(ReplicationError::ObjectTooLarge);
        }
        for range in self.coverage.ranges() {
            range.end()?;
        }
        Ok(())
    }

    /// Admits all identity claims against the replication schemas and the
    /// caller-owned schema/workspace/relation closure.
    ///
    /// # Errors
    ///
    /// Returns an admission, size, coverage, or identity error when any claim
    /// fails the supplied schema and transport checks.
    pub fn admit(
        self,
        expected: &RootSummaryExpectation<T>,
        limits: TransportLimits,
    ) -> Result<RootSummary<T>, ReplicationError> {
        expected.validate(limits)?;
        self.validate(limits)?;
        let workspace = self.workspace.admit(expected.workspace)?;
        if self.relations.len() != expected.relations.len()
            || self.objects.len() != expected.objects.len()
        {
            return Err(ReplicationError::IdentityMismatch);
        }
        let relations = self
            .relations
            .into_iter()
            .zip(&expected.relations)
            .map(|(summary, expected)| summary.admit_against(*expected, limits))
            .collect::<Result<Vec<_>, _>>()?;
        let objects = self
            .objects
            .into_iter()
            .zip(&expected.objects)
            .map(|(summary, expected)| summary.admit_against(*expected, limits))
            .collect::<Result<Vec<_>, _>>()?;
        let summary = RootSummary {
            schema: self.schema,
            workspace,
            relations,
            objects,
            authority: self.authority,
            coverage: self.coverage,
        };
        summary.validate(limits)?;
        if summary.schema != expected.schema
            || summary.relations.len() != expected.relations.len()
            || summary
                .relations
                .iter()
                .zip(&expected.relations)
                .any(|(actual, expected)| {
                    actual.relation != expected.relation || actual.root != expected.root
                })
            || summary
                .objects
                .iter()
                .zip(&expected.objects)
                .any(|(actual, expected)| {
                    actual.key != expected.key
                        || actual.version != expected.version
                        || actual.len != expected.len
                })
        {
            return Err(ReplicationError::IdentityMismatch);
        }
        Ok(summary)
    }

    /// Admits a wire summary against an exact schema/workspace/relation
    /// closure supplied by the workspace owner.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::IdentityMismatch`] when the claimed schema
    /// or relation closure differs from the caller's expected material.
    pub fn admit_against(
        self,
        expected: &RootSummaryExpectation<T>,
        limits: TransportLimits,
    ) -> Result<RootSummary<T>, ReplicationError> {
        self.admit(expected, limits)
    }

    /// Admits a root summary and checks its opaque authority claim against a
    /// caller-owned authority policy.
    ///
    /// The returned summary still carries the wire authority claim. The
    /// execution or workspace owner retains the typed authority material and
    /// decides how that claim is used after this exact policy check.
    ///
    /// # Errors
    ///
    /// Returns the same root, identity, or authority errors as [`Self::admit`]
    /// and [`AuthorityExpectation::admit`].
    pub fn admit_with_authority(
        self,
        expected: &RootSummaryExpectation<T>,
        expected_authority: AuthorityExpectation,
        observed_revocation: RevocationVersion,
        limits: TransportLimits,
    ) -> Result<RootSummary<T>, ReplicationError> {
        let summary = self.admit(expected, limits)?;
        expected_authority.admit(summary.authority, observed_revocation)?;
        Ok(summary)
    }
}

/// An untrusted relation-root summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WireRelationSummary {
    /// Relation type tag.
    pub relation: u16,
    /// Untrusted relation root claim.
    pub root: WireStateRoot,
    /// Sparse availability coverage.
    pub coverage: SparseCoverage,
}
impl WireRelationSummary {
    /// Checks the relation-root claim and coverage bounds without granting a
    /// typed state root.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::IdentityContext`] for a wrong relation
    /// schema or [`ReplicationError::CoverageLimit`] for too many ranges.
    pub fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate()?;
        if self.coverage.ranges().len() > limits.max_ranges {
            return Err(ReplicationError::CoverageLimit);
        }
        for range in self.coverage.ranges() {
            range.end()?;
        }
        self.root
            .admit_context(backend_version::IdContext::relation::<
                crate::ReplicationRelation,
            >())?;
        Ok(())
    }

    /// Admits the bounded wire envelope while retaining its root as an
    /// untrusted claim. Use [`Self::admit_against`] to obtain a typed summary.
    ///
    /// # Errors
    ///
    /// Returns a size, range, coverage, or context error when the claim is
    /// malformed or exceeds `limits`.
    pub fn admit(self, limits: TransportLimits) -> Result<Self, ReplicationError> {
        self.validate(limits)?;
        Ok(self)
    }

    /// Admits this relation claim by exact comparison with the caller-owned
    /// typed relation root.
    ///
    /// # Errors
    ///
    /// Returns an identity, coverage, range, or context error when this claim
    /// does not match the expected typed relation or exceeds `limits`.
    pub fn admit_against(
        self,
        expected: RelationRootExpectation,
        limits: TransportLimits,
    ) -> Result<RelationSummary, ReplicationError> {
        self.validate(limits)?;
        if self.relation != expected.relation {
            return Err(ReplicationError::IdentityMismatch);
        }
        let expected_claim = claim_state_root(expected.root).map_err(ReplicationError::from)?;
        if self.root != expected_claim {
            return Err(ReplicationError::IdentityMismatch);
        }
        Ok(RelationSummary {
            relation: self.relation,
            root: expected.root,
            coverage: self.coverage,
        })
    }
}

/// An untrusted immutable object summary.
pub struct WireObjectSummary<T: Schema = crate::ImmutableObjectSchema> {
    /// Untrusted logical object key claim.
    pub key: SchemaWireObjectKey<T>,
    /// Untrusted object-version claim.
    pub version: SchemaWireObjectVersion<T>,
    /// Claimed canonical object length.
    pub len: u64,
}
impl<T: Schema> Copy for WireObjectSummary<T> {}
impl<T: Schema> Clone for WireObjectSummary<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T: Schema> fmt::Debug for WireObjectSummary<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WireObjectSummary")
            .field("key", &self.key)
            .field("version", &self.version)
            .field("len", &self.len)
            .finish()
    }
}
impl<T: Schema> PartialEq for WireObjectSummary<T> {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && self.version == other.version && self.len == other.len
    }
}
impl<T: Schema> Eq for WireObjectSummary<T> {}
impl<T: Schema> WireObjectSummary<T> {
    /// Checks identity contexts and object-size limits without granting typed
    /// object identities.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::ObjectTooLarge`] or
    /// [`ReplicationError::IdentityContext`] when the claim is invalid.
    pub fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate()?;
        if self.len == 0 || self.len > limits.max_object {
            return Err(ReplicationError::ObjectTooLarge);
        }
        self.key
            .admit_context(backend_version::IdContext::object_key::<T>())?;
        self.version
            .admit_context(backend_version::IdContext::schema::<T>())?;
        Ok(())
    }

    /// Admits the bounded wire envelope while retaining its object identities
    /// as untrusted claims. Use [`Self::admit_against`] to obtain a typed
    /// summary.
    ///
    /// # Errors
    ///
    /// Returns a size, range, coverage, or context error when the claim is
    /// malformed or exceeds `limits`.
    pub fn admit(self, limits: TransportLimits) -> Result<Self, ReplicationError> {
        self.validate(limits)?;
        Ok(self)
    }

    /// Admits this object claim by exact comparison with caller-owned typed
    /// key, version, and canonical length.
    ///
    /// # Errors
    ///
    /// Returns an identity, size, range, or context error when this claim does
    /// not match the expected typed object or exceeds `limits`.
    pub fn admit_against(
        self,
        expected: ObjectSummaryExpectation<T>,
        limits: TransportLimits,
    ) -> Result<ObjectSummary<T>, ReplicationError> {
        self.validate(limits)?;
        if self.len != expected.len {
            return Err(ReplicationError::IdentityMismatch);
        }
        let expected_key =
            crate::claim_schema_object_key(expected.key).map_err(ReplicationError::from)?;
        let expected_version =
            crate::claim_schema_object_version(expected.version).map_err(ReplicationError::from)?;
        if self.key != expected_key || self.version != expected_version {
            return Err(ReplicationError::IdentityMismatch);
        }
        Ok(ObjectSummary {
            key: expected.key,
            version: expected.version,
            len: expected.len,
        })
    }
}
