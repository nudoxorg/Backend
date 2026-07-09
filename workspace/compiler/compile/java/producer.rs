//! Java [`Producer`](crate::compile::producer::Producer) — javac + javadoc doclet.

use std::path::Path;

use heart::Language;
use sandbox::{Captured, SealedInput};

use crate::compile::producer::{
	AuxOutputs, ExecPlan, Producer, ProducerError, ProducerId, ProducerOutput, ThreatTier,
};

/// Java project producer (vendored javadoc doclet → IR).
///
/// Multi-step (materialize → javac → javadoc) is orchestrated inside the existing
/// oracle path; [`produce`](Producer::produce) calls that path so intermediate
/// classpaths stay consistent. `plan`/`decode` document the final doclet step
/// for ForgeRuntime (Phase 4) which will own staged scratch.
#[derive(Debug, Default, Clone, Copy)]
pub struct JavaProducer;

impl Producer for JavaProducer {
	const ID: ProducerId = ProducerId("javadoc/1");

	fn language(&self) -> Language {
		Language::Java
	}

	fn tier(&self) -> ThreatTier {
		ThreatTier::Untrusted
	}

	fn plan(&self, input: &SealedInput) -> Result<ExecPlan, ProducerError> {
		// Adaptive: source-root discovery + oracle compile are host prep.
		// Phase 4 will split these into sequential SealedCommands under one
		// scratch; for now produce() owns the full pipeline.
		let _ = input;
		Ok(ExecPlan::Commands(Vec::new()))
	}

	fn decode(
		&self,
		_input: &SealedInput,
		_captured: Captured,
	) -> Result<ProducerOutput, ProducerError> {
		Err(ProducerError::decode(
			"java decode goes through produce() until multi-command staging lands",
		))
	}

	fn produce(&self, input: &SealedInput) -> Result<ProducerOutput, ProducerError> {
		self.lower_in_process(&input.root)
	}

	fn lower_in_process(&self, root: &Path) -> Result<ProducerOutput, ProducerError> {
		let index = super::package::lower_package(root).map_err(ProducerError::lower)?;
		Ok(ProducerOutput {
			index,
			aux: AuxOutputs::default(),
		})
	}
}
