//! Compiled worker profile identities, recipe, and executor.

use super::{
    DependencyManifest, PureRecipeExecutor, RecipeId, SemanticCoverageAdmissionError,
    SemanticCoverageBinding, SemanticCoverageValidator, SparseCoverage,
    UntrustedSemanticCoverageClaim, WorkerError,
};
use crate::input_cas::BoundedFileImage;
use backend_engine::{
    IdContext, PRODUCT_EXECUTION_MEMORY_BYTES, SemanticPublicationProjectionBuilder,
    TreeNodeLoader, UntrustedId, semantic_publication_output_bytes,
};
use std::fmt::Write as _;
use std::path::PathBuf;

pub(super) use backend_engine::builtin::{
    BuiltinInputSchema, ProductSemanticPublicationKey, ProductSemanticPublicationRecord,
    Profile as BuiltinProfile, ProfileIds, execution_manifest, execution_resources,
    product_dependency_manifest, profile_descriptor, profile_ids,
};

/// Read-only adapter over canonical relation proofs retained by the worker's
/// durable input CAS. A request reaches this loader only after closure and
/// input admission have established the selected relation root.
#[derive(Clone, Debug)]
pub(super) struct ProductInputLoader {
    root: PathBuf,
}

impl ProductInputLoader {
    pub(super) fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn proof_path(&self, digest: [u8; 32]) -> PathBuf {
        let mut name = String::with_capacity(71);
        name.push_str(".proof-");
        for byte in digest {
            let _ = write!(name, "{byte:02x}");
        }
        self.root.join(name)
    }
}

impl TreeNodeLoader<super::ProductRelation> for ProductInputLoader {
    type Error = &'static str;

    fn load(
        &self,
        claim: UntrustedId<super::ProductRelation>,
    ) -> Result<backend_engine::CheckedCanonicalRoot<super::ProductRelation>, Self::Error> {
        if claim.context() != IdContext::relation::<super::ProductRelation>() {
            return Err("product relation proof has the wrong identity context");
        }
        let digest = *claim.as_bytes();
        let proof = BoundedFileImage::read_optional(&self.proof_path(digest), 64 * 1024)
            .map_err(|_| "product relation proof exceeds its canonical bound")?
            .ok_or("product relation proof is absent from the warm CAS")?;
        if proof.as_slice().is_empty() {
            return Err("product relation proof exceeds its canonical bound");
        }
        backend_engine::admit_canonical_root_claim(claim, proof.as_slice())
            .map_err(|_| "product relation proof failed canonical admission")
    }
}

#[derive(Clone, Debug)]
pub(super) struct BuiltinExecutor {
    pub(super) ids: ProfileIds,
    pub(super) product_inputs: Option<ProductInputLoader>,
}

impl PureRecipeExecutor for BuiltinExecutor {
    fn recipe_id(&self) -> RecipeId {
        self.ids.recipe
    }

    fn execute(
        &self,
        invocation: &backend_engine::AdmittedInvocation,
        context: &mut backend_engine::PureWorkContext<'_>,
    ) -> Result<backend_engine::PreparedOutput, WorkerError> {
        let output = if self.ids.include_basis {
            self.execute_product(invocation, context)?
        } else {
            context.charge_cpu(1)?;
            context.charge_memory(
                self.ids
                    .output_prefix
                    .len()
                    .try_into()
                    .map_err(|_| WorkerError::ResourceOverrun)?,
            )?;
            if context.is_cancelled() {
                return Err(WorkerError::Cancelled);
            }
            self.ids.output_prefix.to_vec()
        };
        let bytes = output.into_boxed_slice();
        let coverage =
            SparseCoverage::complete(bytes.len() as u64).map_err(WorkerError::Replication)?;
        Ok(backend_engine::PreparedOutput { bytes, coverage })
    }
}

impl BuiltinExecutor {
    fn execute_product(
        &self,
        invocation: &backend_engine::AdmittedInvocation,
        context: &mut backend_engine::PureWorkContext<'_>,
    ) -> Result<Vec<u8>, WorkerError> {
        let loader = self
            .product_inputs
            .as_ref()
            .ok_or(WorkerError::InputProof("product input loader"))?;
        let input = invocation
            .inputs
            .first()
            .ok_or(WorkerError::InputMismatch)?;
        if invocation.inputs.len() != 1 || input.bytes.len() != 32 {
            return Err(WorkerError::InputMismatch);
        }
        let claim = UntrustedId::<super::ProductRelation>::from_wire(
            &input.bytes,
            IdContext::relation::<super::ProductRelation>(),
        )
        .map_err(|_| WorkerError::InputProof("product relation identity"))?;
        let root = loader.load(claim).map_err(WorkerError::InputProof)?;
        let source = root.root();
        let expected_rows = root.row_count();
        context.charge_memory(PRODUCT_EXECUTION_MEMORY_BYTES)?;

        // Keep only typed child claims in the depth-first frontier. Canonical
        // node bytes are loaded and admitted one at a time from the warm CAS,
        // and rows feed the shared fixed-state accumulator immediately.
        let mut projection = SemanticPublicationProjectionBuilder::new();
        let mut node = Some(root);
        let mut frontier = Vec::new();
        loop {
            let current = if let Some(root) = node.take() {
                root
            } else if let Some(claim) = frontier.pop() {
                loader.load(claim).map_err(WorkerError::InputProof)?
            } else {
                break;
            };
            context.charge_cpu(1)?;
            if context.is_cancelled() {
                return Err(WorkerError::Cancelled);
            }
            if current.node().level() == 0 {
                let entries = current
                    .leaf_entries()
                    .map_err(|_| WorkerError::InputProof("product relation leaf"))?;
                for (key, value) in &entries {
                    projection
                        .push(key, value)
                        .map_err(|_| WorkerError::InputProof("semantic publication row"))?;
                }
                continue;
            }
            let children = current
                .child_summaries()
                .map_err(|_| WorkerError::InputProof("product relation branch"))?;
            for child in children.into_iter().rev() {
                frontier.push(
                    UntrustedId::from_wire(
                        child.commitment.as_bytes(),
                        IdContext::relation::<super::ProductRelation>(),
                    )
                    .map_err(|_| WorkerError::InputProof("product child identity"))?,
                );
            }
        }
        let projection = projection.finish(source);
        if projection
            .published()
            .saturating_add(projection.unavailable())
            != expected_rows
        {
            return Err(WorkerError::InputProof(
                "semantic publication relation row count",
            ));
        }
        Ok(semantic_publication_output_bytes(
            self.ids,
            invocation.input_basis,
            projection,
        ))
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct BuiltinSemanticAuthority {
    pub(super) ids: ProfileIds,
}

impl SemanticCoverageValidator for BuiltinSemanticAuthority {
    fn validate(
        &self,
        binding: &SemanticCoverageBinding,
        claim: &UntrustedSemanticCoverageClaim,
    ) -> Result<(), SemanticCoverageAdmissionError> {
        backend_engine::builtin::validate_semantic_claim(self.ids, binding, claim)
    }

    fn validate_manifest(
        &self,
        binding: &SemanticCoverageBinding,
        claim: &UntrustedSemanticCoverageClaim,
        manifest: &DependencyManifest,
    ) -> Result<(), SemanticCoverageAdmissionError> {
        backend_engine::builtin::validate_semantic_manifest(self.ids, binding, claim, manifest)
    }
}
