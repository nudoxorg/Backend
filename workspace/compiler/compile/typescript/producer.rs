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
		ctx: &C,
		root: &Path,
	) -> Result<ProducerOutput, ProducerError> {
		// Tier C (opt-in via NUDOX_TYPESCRIPT_ORACLE): try the tsgo checker
		// oracle first; on ANY failure fall back to the syntactic OXC pass.
		// Unset env → the default path below, byte-identical to Tier A/B.
		if super::oracle::tsgo::enabled() {
			match super::oracle::tsgo::normalize(ctx, root, &self.name) {
				Ok(index) => {
					let mut out = ProducerOutput::from_index(index);
					out.aux.extraction_tier = "tsgo-emit".to_string();
					return Ok(out);
				}
				Err(reason) => {
					tracing::warn!("tsgo oracle fell back to syntactic: {reason}");
					let index =
						super::oxc::generate_ir(root, &self.name).map_err(ProducerError::lower)?;
					let mut out = ProducerOutput::from_index(index);
					out.aux.extraction_tier = "syntactic".to_string();
					out.aux.extraction_failure = Some(reason);
					return Ok(out);
				}
			}
		}

		let index = super::oxc::generate_ir(root, &self.name).map_err(ProducerError::lower)?;
		let mut out = ProducerOutput::from_index(index);
		out.aux.extraction_tier = "syntactic".to_string();
		Ok(out)
	}
}
