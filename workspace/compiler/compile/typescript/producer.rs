//! TypeScript [`Producer`](crate::compile::producer::Producer) — library form (OXC).

use std::path::Path;

use heart::Language;
use sandbox::{Captured, SealedInput, WorkerLang};

use crate::compile::producer::{
	AuxOutputs, ExecPlan, ForgeContext, Producer, ProducerError, ProducerId, ProducerOutput,
	ThreatTier, decode_index_json,
};

/// TypeScript package producer (OXC via worker pool / in-process).
#[derive(Debug, Clone)]
pub struct TypescriptProducer {
	/// Package name (from package.json / coordinates).
	pub name: String,
}

impl Producer for TypescriptProducer {
	const ID: ProducerId = ProducerId("oxc/1");

	fn language(&self) -> Language {
		Language::Typescript
	}

	fn tier(&self) -> ThreatTier {
		ThreatTier::Hostile
	}

	fn plan<C: ForgeContext>(
		&self,
		_ctx: &C,
		_input: &SealedInput,
	) -> Result<ExecPlan, ProducerError> {
		Ok(ExecPlan::Library(WorkerLang::Typescript))
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

	fn lower_in_process<C: ForgeContext>(
		&self,
		_ctx: &C,
		root: &Path,
	) -> Result<ProducerOutput, ProducerError> {
		let index = super::oxc::generate_ir(root, &self.name).map_err(ProducerError::lower)?;
		Ok(ProducerOutput::from_index(index))
	}
}
