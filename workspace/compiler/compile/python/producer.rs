//! Python [`Producer`](crate::compile::producer::Producer) — library form (pyrefly).

use std::path::Path;

use heart::Language;
use sandbox::{Captured, SealedInput, WorkerLang};

use crate::compile::producer::{
	AuxOutputs, ExecPlan, ForgeContext, Producer, ProducerError, ProducerId, ProducerOutput,
	ThreatTier, decode_index_json,
};

/// Python package producer (pyrefly via worker pool / in-process).
#[derive(Debug, Default, Clone, Copy)]
pub struct PythonProducer;

impl Producer for PythonProducer {
	const ID: ProducerId = ProducerId("pyrefly/1");

	fn language(&self) -> Language {
		Language::Python
	}

	fn tier(&self) -> ThreatTier {
		ThreatTier::Hostile
	}

	fn plan(
		&self,
		_ctx: &dyn ForgeContext,
		_input: &SealedInput,
	) -> Result<ExecPlan, ProducerError> {
		Ok(ExecPlan::Library(WorkerLang::Python))
	}

	fn decode(
		&self,
		_input: &SealedInput,
		captured: Captured,
	) -> Result<ProducerOutput, ProducerError> {
		let index = decode_index_json(&captured.stdout)?;
		Ok(ProducerOutput {
			index,
			aux: AuxOutputs::default(),
		})
	}

	fn lower_in_process(
		&self,
		_ctx: &dyn ForgeContext,
		root: &Path,
	) -> Result<ProducerOutput, ProducerError> {
		let ctx = super::context::PythonContext::new();
		Ok(ProducerOutput::from_index(ctx.lower_package(root)))
	}
}
