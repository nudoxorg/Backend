//! Rust [`Producer`](crate::compile::producer::Producer) — cargo rustdoc JSON.

use std::path::Path;

use heart::Language;
use sandbox::{Captured, SealedInput};
use semver::Version;

use crate::compile::producer::{
	AuxOutputs, ExecPlan, Producer, ProducerError, ProducerId, ProducerOutput, ThreatTier,
};

use super::RustPackage;

/// Rust crate/workspace producer (`cargo rustdoc --output-format json` → IR).
///
/// Planning is adaptive: `cargo metadata` decides which local packages to
/// document, so [`produce`](Producer::produce) orchestrates metadata + N rustdoc
/// runs. ForgeRuntime (Phase 4) will stage that as sequential sealed commands.
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

	fn plan(&self, input: &SealedInput) -> Result<ExecPlan, ProducerError> {
		// Adaptive multi-crate plan is owned by produce(); empty Commands would
		// fail execute(), so produce is the entry point.
		let _ = input;
		Ok(ExecPlan::Commands(Vec::new()))
	}

	fn decode(
		&self,
		_input: &SealedInput,
		_captured: Captured,
	) -> Result<ProducerOutput, ProducerError> {
		Err(ProducerError::decode(
			"rust decode goes through produce() until multi-command staging lands",
		))
	}

	fn produce(&self, input: &SealedInput) -> Result<ProducerOutput, ProducerError> {
		self.lower_in_process(&input.root)
	}

	fn lower_in_process(&self, root: &Path) -> Result<ProducerOutput, ProducerError> {
		let package = RustPackage {
			name: self.name.clone(),
			direct_repo: self.direct_repo,
		};
		let (collected, source_map) = package
			.generate_ir_with_sources(root, &self.version)
			.map_err(ProducerError::lower)?;
		Ok(ProducerOutput {
			index: collected.index().into_index(),
			aux: AuxOutputs {
				source_map: Some(source_map.into_iter().collect()),
			},
		})
	}
}
