//! Admitted workspace root summaries.

use super::{
    AdmittedAuthority, AuthorityClaim, ObjectSummary, RelationSummary, ReplicationError,
    RootSummary, SparseCoverage, TransportLimits, WireRootSummary, WorkspaceRoot,
    WorkspaceRootClaim,
};
use backend_version::{CheckedWorkspaceManifest, Schema};

impl<T: Schema> RootSummary<T> {
    #[cfg(test)]
    pub(crate) fn from_parts_for_test(
        schema: u32,
        workspace: WorkspaceRoot,
        relations: Vec<RelationSummary>,
        objects: Vec<ObjectSummary<T>>,
        authority: AuthorityClaim,
        coverage: SparseCoverage,
    ) -> Self {
        Self {
            schema,
            workspace,
            relations,
            objects,
            authority,
            coverage,
        }
    }

    /// Builds a checked summary from a version-owned admitted workspace
    /// manifest. The manifest supplies the root-of-roots and relation
    /// bindings; callers can only add object claims after that proof and must
    /// supply authority evidence that has crossed its owner expectation.
    ///
    /// # Errors
    ///
    /// Returns an identity, ordering, coverage, or bounds error when the
    /// supplied summaries do not describe the admitted manifest exactly.
    pub fn from_admitted_manifest(
        manifest: &CheckedWorkspaceManifest,
        authority: AdmittedAuthority,
        relations: Vec<RelationSummary>,
        objects: Vec<ObjectSummary<T>>,
        coverage: SparseCoverage,
        limits: TransportLimits,
    ) -> Result<Self, ReplicationError> {
        manifest
            .validate()
            .map_err(|_| ReplicationError::IdentityMismatch)?;
        let claim = authority.claim();
        if *manifest.authority() != claim.id.as_bytes()
            || manifest.relations().len() != relations.len()
            || manifest
                .relations()
                .iter()
                .zip(&relations)
                .any(|(binding, relation)| {
                    binding.schema().ty() != relation.relation
                        || binding.root() != relation.root.to_bytes()
                })
        {
            return Err(ReplicationError::IdentityMismatch);
        }
        let summary = Self {
            schema: manifest.schema(),
            workspace: manifest.root(),
            relations,
            objects,
            authority: claim,
            coverage,
        };
        summary.validate(limits)?;
        Ok(summary)
    }

    /// Returns the canonical workspace schema ABI.
    #[must_use]
    pub const fn schema(&self) -> u32 {
        self.schema
    }

    /// Returns the workspace root admitted by the owner expectation.
    #[must_use]
    pub const fn workspace(&self) -> WorkspaceRoot {
        self.workspace
    }

    /// Borrows the admitted relation summaries.
    #[must_use]
    pub fn relations(&self) -> &[RelationSummary] {
        &self.relations
    }

    /// Borrows the admitted immutable object summaries.
    #[must_use]
    pub fn objects(&self) -> &[ObjectSummary<T>] {
        &self.objects
    }

    /// Returns the authority claim retained after exact summary admission.
    #[must_use]
    pub const fn authority(&self) -> AuthorityClaim {
        self.authority
    }

    /// Borrows summary coverage.
    #[must_use]
    pub fn coverage(&self) -> &SparseCoverage {
        &self.coverage
    }

    /// Validates ordering and bounded sizes before the summary is used.
    ///
    /// # Errors
    ///
    /// Returns an error when objects, relations, coverage, or object sizes
    /// exceed the supplied limits or object keys are unsorted.
    pub fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate()?;
        if self.objects.len() > limits.max_objects || self.relations.len() > limits.max_objects {
            return Err(ReplicationError::MessageTooLarge);
        }
        if self
            .objects
            .windows(2)
            .any(|pair| pair[0].key >= pair[1].key)
        {
            return Err(ReplicationError::Unsorted);
        }
        if self
            .relations
            .windows(2)
            .any(|pair| pair[0].relation >= pair[1].relation)
        {
            return Err(ReplicationError::Unsorted);
        }
        if self.coverage.ranges().len() > limits.max_ranges {
            return Err(ReplicationError::CoverageLimit);
        }
        for object in &self.objects {
            if object.len == 0 || object.len > limits.max_object {
                return Err(ReplicationError::ObjectTooLarge);
            }
        }
        Ok(())
    }

    /// Converts an admitted summary into a wire claim envelope.
    ///
    /// # Errors
    ///
    /// Returns an identity encoding error if a fixed-width claim cannot be
    /// represented on the wire.
    pub fn to_wire(&self) -> Result<WireRootSummary<T>, ReplicationError> {
        Ok(WireRootSummary {
            schema: self.schema,
            workspace: WorkspaceRootClaim::from_bytes(*self.workspace.as_bytes()),
            relations: self
                .relations
                .iter()
                .map(RelationSummary::to_wire)
                .collect::<Result<Vec<_>, _>>()?,
            objects: self
                .objects
                .iter()
                .map(ObjectSummary::to_wire)
                .collect::<Result<Vec<_>, _>>()?,
            authority: self.authority,
            coverage: self.coverage.clone(),
        })
    }
}
