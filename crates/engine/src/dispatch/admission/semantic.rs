use super::{
    AttestationVerifier, CompleteSemanticCoverage, DispatchError, Dispatcher,
    OutputAdmissionValidator, Relation, SemanticCoverageAdmissionError, SemanticCoverageBinding,
    SemanticCoverageValidator, UntrustedSemanticCoverageClaim,
};

impl<V, A> Dispatcher<V, A>
where
    V: OutputAdmissionValidator + SemanticCoverageValidator,
    A: AttestationVerifier + Send + Sync + 'static,
{
    /// Admits one untrusted semantic coverage claim through the dispatcher's
    /// mandatory recipe/scope authority validator.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn admit_semantic<R: Relation>(
        &self,
        identity: &backend_execution::VersionedWorkIdentity<R>,
        claim: UntrustedSemanticCoverageClaim,
    ) -> Result<CompleteSemanticCoverage, DispatchError> {
        let semantic =
            CompleteSemanticCoverage::admit(identity, claim, &self.validator).map_err(|error| {
                match error {
                    SemanticCoverageAdmissionError::Incomplete
                    | SemanticCoverageAdmissionError::InvalidScope
                    | SemanticCoverageAdmissionError::BindingMismatch => {
                        DispatchError::IncompleteSemanticCoverage
                    }
                    SemanticCoverageAdmissionError::Rejected => DispatchError::AuthorityRejected,
                }
            })?;
        self.observe_semantic_freshness(identity, &semantic)?;
        Ok(semantic)
    }

    /// Compatibility spelling for semantic coverage admission.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn admit_semantic_coverage<R: Relation>(
        &self,
        identity: &backend_execution::VersionedWorkIdentity<R>,
        claim: UntrustedSemanticCoverageClaim,
    ) -> Result<CompleteSemanticCoverage, DispatchError> {
        self.admit_semantic(identity, claim)
    }

    /// Admits a complete dependency manifest together with its untrusted
    /// producer claim.  The manifest is retained by the opaque capability and
    /// registered transactionally when its output is published.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn admit_semantic_with_manifest<R: Relation>(
        &self,
        identity: &backend_execution::VersionedWorkIdentity<R>,
        claim: UntrustedSemanticCoverageClaim,
        manifest: backend_semantic::DependencyManifest,
    ) -> Result<CompleteSemanticCoverage, DispatchError> {
        let semantic = CompleteSemanticCoverage::admit_with_manifest(
            identity,
            claim,
            manifest,
            &self.validator,
        )
        .map_err(|error| match error {
            SemanticCoverageAdmissionError::Incomplete
            | SemanticCoverageAdmissionError::InvalidScope
            | SemanticCoverageAdmissionError::BindingMismatch => {
                DispatchError::IncompleteSemanticCoverage
            }
            SemanticCoverageAdmissionError::Rejected => DispatchError::AuthorityRejected,
        })?;
        self.observe_semantic_freshness(identity, &semantic)?;
        Ok(semantic)
    }

    pub(crate) fn validate_semantic_capability<R: Relation>(
        &self,
        identity: &backend_execution::VersionedWorkIdentity<R>,
        semantic: &CompleteSemanticCoverage,
    ) -> Result<(), DispatchError> {
        if !semantic.binds(identity) {
            return Err(DispatchError::IncompleteSemanticCoverage);
        }
        if self
            .authority_transitions
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?
            .get(&identity.authority)
            .is_some_and(|freshness| {
                semantic.authority_epoch().0 < freshness.authority_epoch
                    || semantic.revocation_version().0 < freshness.revocation_version
            })
        {
            return Err(DispatchError::AuthorityRejected);
        }
        let binding = SemanticCoverageBinding::from_identity(identity);
        // Recheck the base recipe/scope/freshness authority on every use. A
        // manifest hook is an additional dependency admission check; it must
        // never replace the base validator, since an authority can rotate or
        // revoke the producer claim after the opaque capability was minted.
        SemanticCoverageValidator::validate(&self.validator, &binding, semantic.claim())
            .and_then(|()| {
                semantic.dependency_manifest().map_or(Ok(()), |manifest| {
                    SemanticCoverageValidator::validate_manifest(
                        &self.validator,
                        &binding,
                        semantic.claim(),
                        manifest,
                    )
                })
            })
            .map_err(|error| match error {
                SemanticCoverageAdmissionError::Incomplete
                | SemanticCoverageAdmissionError::InvalidScope
                | SemanticCoverageAdmissionError::BindingMismatch => {
                    DispatchError::IncompleteSemanticCoverage
                }
                SemanticCoverageAdmissionError::Rejected => DispatchError::AuthorityRejected,
            })
    }
}
