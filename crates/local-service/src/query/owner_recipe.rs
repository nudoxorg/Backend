//! Immutable local query contract captured before runtime generation retirement.

use super::{
    AdmittedLocalQueryRecipe, LocalQueryRecipeDescriptor, LocalQueryRecipeError, LocalSemanticError,
};
use backend_compile::{EmbeddingActivationControl, EmbeddingExecutable, EmbeddingRuntimeSpecV1};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// No model/process authority: only the complete descriptor and its original selection path.
pub(crate) struct PreparedLocalQueryRecipe {
    path: PathBuf,
    spec: EmbeddingRuntimeSpecV1,
    descriptor: LocalQueryRecipeDescriptor,
}

impl PreparedLocalQueryRecipe {
    pub(crate) fn capture_until(
        path: &Path,
        spec: EmbeddingRuntimeSpecV1,
        control: &EmbeddingActivationControl<'_>,
    ) -> Result<Self, LocalSemanticError> {
        let descriptor =
            super::local_semantic::capture_descriptor(path, spec.recipe_identity(), Some(control))?;
        descriptor
            .validate_runtime_spec(spec)
            .map_err(LocalSemanticError::Recipe)?;
        control
            .checkpoint()
            .map_err(LocalSemanticError::Inference)?;
        Ok(Self {
            path: path.to_owned(),
            spec,
            descriptor,
        })
    }

    /// Re-read the same recipe-addressed selection. Identical replacement is lawful;
    /// any different full contract refuses before compiler/query installation.
    pub(crate) fn verify_current_until(
        &self,
        control: &EmbeddingActivationControl<'_>,
    ) -> Result<(), LocalSemanticError> {
        let current = super::local_semantic::capture_descriptor(
            &self.path,
            self.spec.recipe_identity(),
            Some(control),
        )?;
        current
            .validate_runtime_spec(self.spec)
            .map_err(LocalSemanticError::Recipe)?;
        if current != self.descriptor {
            return Err(LocalSemanticError::Recipe(
                LocalQueryRecipeError::RuntimeContractMismatch,
            ));
        }
        control
            .checkpoint()
            .map_err(LocalSemanticError::Inference)?;
        Ok(())
    }

    /// Retains the SAME activated Arc for the new compiler and query generation.
    pub(crate) fn admit_until(
        &self,
        runtime: Arc<EmbeddingExecutable>,
        control: &EmbeddingActivationControl<'_>,
    ) -> Result<Arc<AdmittedLocalQueryRecipe>, LocalSemanticError> {
        self.verify_current_until(control)?;
        let admitted = self
            .descriptor
            .admit(runtime)
            .map_err(LocalSemanticError::Recipe)?;
        control
            .checkpoint()
            .map_err(LocalSemanticError::Inference)?;
        Ok(Arc::new(admitted))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use backend_engine::application::PreparedEmbeddingRuntime;
    use std::{
        sync::atomic::{AtomicBool, Ordering},
        time::{Duration, Instant},
    };
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn owner_recipe_capture_revalidation_and_admission_preserve_exact_runtime_arc() -> TestResult {
        let fixture = super::super::local_semantic::tests::fixture_with_mode(4096, "plain")?;
        let flag = AtomicBool::new(false);
        let control =
            EmbeddingActivationControl::new(&flag, Instant::now() + Duration::from_secs(5));
        let config = fixture
            .descriptor
            .parent()
            .ok_or("compiler parent")?
            .join("embedding.config");
        let prepared = PreparedEmbeddingRuntime::capture_until(&config, &control)?
            .ok_or("configured fixture")?;
        let query = PreparedLocalQueryRecipe::capture_until(
            &fixture.descriptor,
            prepared.spec(),
            &control,
        )?;
        let log = std::fs::read_to_string(&fixture.log)?;
        let original = backend_platform::durable::open_private_read(&fixture.descriptor)?;
        let bytes = std::fs::read(&fixture.descriptor)?;
        backend_platform::durable::write_private_atomic(&fixture.descriptor, &bytes)?;
        query.verify_current_until(&control)?; // held old inode cannot veto identical content
        let runtime = fixture.provision.runtime().ok_or("real fixture runtime")?;
        let admitted = query.admit_until(Arc::clone(&runtime), &control)?;
        assert!(Arc::ptr_eq(admitted.runtime(), &runtime));
        assert_eq!(admitted.execution_identity(), runtime.execution_identity());
        assert_eq!(
            std::fs::read_to_string(&fixture.log)?,
            log,
            "capture/admission never reinfers"
        );
        drop(original);
        Ok(())
    }

    #[test]
    fn owner_recipe_capture_refuses_changed_contract_and_original_stop_cause() -> TestResult {
        let fixture = super::super::local_semantic::tests::fixture_with_mode(4096, "plain")?;
        let flag = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_secs(5);
        let control = EmbeddingActivationControl::new(&flag, deadline);
        let config = fixture
            .descriptor
            .parent()
            .ok_or("compiler parent")?
            .join("embedding.config");
        let prepared = PreparedEmbeddingRuntime::capture_until(&config, &control)?
            .ok_or("configured fixture")?;
        let query = PreparedLocalQueryRecipe::capture_until(
            &fixture.descriptor,
            prepared.spec(),
            &control,
        )?;
        let spec = prepared.spec();
        let different = EmbeddingRuntimeSpecV1::new(
            spec.model(),
            spec.model_version(),
            spec.tokenizer(),
            spec.executable(),
            spec.dimension(),
            spec.normalization(),
            std::num::NonZeroU32::new(4095).ok_or("text limit")?,
            spec.options_digest(),
        );
        assert!(matches!(
            PreparedLocalQueryRecipe::capture_until(&fixture.descriptor, different, &control),
            Err(LocalSemanticError::Recipe(
                LocalQueryRecipeError::RuntimeContractMismatch
            ))
        ));
        let mut bytes = std::fs::read(&fixture.descriptor)?;
        bytes[0] ^= 1;
        backend_platform::durable::write_private_atomic(&fixture.descriptor, &bytes)?;
        assert!(matches!(
            query.verify_current_until(&control),
            Err(LocalSemanticError::Recipe(
                LocalQueryRecipeError::DescriptorFormat
            ))
        ));
        flag.store(true, Ordering::Release);
        let expired = EmbeddingActivationControl::new(&flag, Instant::now());
        assert!(matches!(
            PreparedLocalQueryRecipe::capture_until(&fixture.descriptor, spec, &expired),
            Err(LocalSemanticError::Inference(
                backend_compile::EmbeddingExecutableError::Process(
                    backend_compile::ProcessError::Cancelled
                )
            ))
        ));
        flag.store(false, Ordering::Release);
        assert!(matches!(
            PreparedLocalQueryRecipe::capture_until(&fixture.descriptor, spec, &expired),
            Err(LocalSemanticError::Inference(
                backend_compile::EmbeddingExecutableError::Process(
                    backend_compile::ProcessError::Deadline
                )
            ))
        ));
        Ok(())
    }
}
