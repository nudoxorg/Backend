//! Nix [`Producer`](crate::compile::producer::Producer) — library form (snix).

use std::path::Path;

use heart::Language;
use sandbox::{Captured, SealedInput, WorkerLang};

use crate::compile::producer::{
	AuxOutputs, ExecPlan, Producer, ProducerError, ProducerId, ProducerOutput, ThreatTier,
	decode_index_json,
};

/// Nix flake producer (snix hermetic eval via worker pool / in-process).
///
/// Hermeticity is part of the budget: pure builtins + sealed [`DocsIO`](super::eval::DocsIO).
/// Worker children start with `env_clear` (seal-time projection); in-process
/// eval never reads host secrets through EvalIO.
#[derive(Debug, Default, Clone, Copy)]
pub struct NixProducer;

impl Producer for NixProducer {
	const ID: ProducerId = ProducerId("snix/1");

	fn language(&self) -> Language {
		Language::Nix
	}

	fn tier(&self) -> ThreatTier {
		ThreatTier::Hostile
	}

	fn plan(&self, _input: &SealedInput) -> Result<ExecPlan, ProducerError> {
		Ok(ExecPlan::Library(WorkerLang::Nix))
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

	fn lower_in_process(&self, root: &Path) -> Result<ProducerOutput, ProducerError> {
		let index = super::lower_package(root).map_err(ProducerError::lower)?;
		Ok(ProducerOutput::from_index(index))
	}
}
