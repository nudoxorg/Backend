//! Rust [`Producer`](crate::compile::producer::Producer) — in-process
//! rust-analyzer HIR walk → IR.

use std::path::Path;

use heart::Language;
use sandbox::{Captured, SealedInput};
use semver::Version;

use crate::compile::producer::{
	AuxOutputs, ExecPlan, ForgeContext, Producer, ProducerError, ProducerId, ProducerOutput,
	ThreatTier,
};

/// Rust crate/workspace producer (in-process rust-analyzer HIR walk → IR).
///
/// Planning is adaptive: the workspace is loaded once and lowered in-process,
/// so [`produce`](Producer::produce) runs the in-process path directly.
/// `plan`/`decode` return explicit errors (not hollow empty commands).
#[derive(Debug, Clone)]
pub struct RustProducer {
	/// Root package name as cargo metadata reports it.
	pub name: String,
	/// Cargo version from package coordinates.
	pub version: Version,
	/// Direct-repo mode (document private items + workspace members).
	pub direct_repo: bool,
}

impl Producer for RustProducer {
	const ID: ProducerId = ProducerId("rustdoc/3");

	fn language(&self) -> Language {
		Language::Rust
	}

	fn tier(&self) -> ThreatTier {
		ThreatTier::Untrusted
	}

	fn plan(
		&self,
		_ctx: &dyn ForgeContext,
		_input: &SealedInput,
	) -> Result<ExecPlan, ProducerError> {
		Err(ProducerError::adaptive("rustdoc multi-crate"))
	}

	fn decode(
		&self,
		_input: &SealedInput,
		_captured: Captured,
	) -> Result<ProducerOutput, ProducerError> {
		Err(ProducerError::decode(
			"rustdoc multi-crate: adaptive — call produce() (Phase 4 stages sealed commands)",
		))
	}

	fn produce(
		&self,
		ctx: &dyn ForgeContext,
		input: &SealedInput,
	) -> Result<ProducerOutput, ProducerError> {
		self.lower_in_process(ctx, &input.root)
	}

	fn lower_in_process(
		&self,
		_ctx: &dyn ForgeContext,
		root: &Path,
	) -> Result<ProducerOutput, ProducerError> {
		// Route through the module entry point so the default rust-analyzer
		// producer is used (rustdoc remains a fallback via NUDOX_RUST_PRODUCER).
		let (index, source_map) =
			super::generate_ir(root, &self.name, &self.version, self.direct_repo)
				.map_err(ProducerError::lower)?;
		Ok(ProducerOutput {
			index,
			aux: AuxOutputs {
				source_map: Some(source_map.into_iter().collect()),
			},
		})
	}
}
