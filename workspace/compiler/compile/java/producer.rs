//! Java [`Producer`](crate::compile::producer::Producer) — javac + javadoc doclet.

use std::path::Path;

use heart::Language;
use sandbox::{Captured, SealedInput};

use crate::compile::producer::{
	AuxOutputs, ExecPlan, ForgeContext, Producer, ProducerError, ProducerId, ProducerOutput,
	ThreatTier,
};

/// Java project producer (vendored javadoc doclet → IR).
///
/// Multi-step (materialize → javac → javadoc) is orchestrated inside the existing
/// oracle path via [`produce`](Producer::produce). `plan`/`decode` return explicit
/// errors (not hollow empty commands); ForgeRuntime (Phase 4) will stage sequential
/// sealed commands under one scratch.
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

	fn plan<C: ForgeContext>(
		&self,
		_ctx: &C,
		_input: &SealedInput,
	) -> Result<ExecPlan, ProducerError> {
		Err(ProducerError::adaptive("javadoc multi-step"))
	}

	fn decode(
		&self,
		_input: &SealedInput,
		_captured: Captured,
	) -> Result<ProducerOutput, ProducerError> {
		Err(ProducerError::decode(
			"javadoc multi-step: adaptive — call produce() (Phase 4 stages sealed commands)",
		))
	}

	fn produce<C: ForgeContext>(
		&self,
		ctx: &C,
		input: &SealedInput,
	) -> Result<ProducerOutput, ProducerError> {
		self.lower_in_process(ctx, &input.root)
	}

	fn lower_in_process<C: ForgeContext>(
		&self,
		ctx: &C,
		root: &Path,
	) -> Result<ProducerOutput, ProducerError> {
		let index = super::package::lower_package(ctx, root).map_err(ProducerError::lower)?;
		Ok(ProducerOutput {
			index,
			aux: AuxOutputs::default(),
		})
	}
}
