//! C# [`Producer`](crate::compile::producer::Producer) — Roslyn oracle
//! (`dotnet oracle.dll`) → IR.
//!
//! The oracle publish dir is a Buck2-built resource; source collection +
//! `dotnet oracle.dll` invocation happen inside the oracle path via
//! [`produce`](Producer::produce). `plan`/`decode` return explicit errors
//! (adaptive multi-step, like Java) until ForgeRuntime stages sealed commands.

use std::path::Path;

use heart::Language;
use sandbox::{Captured, SealedInput};

use crate::compile::producer::{
	AuxOutputs, ExecPlan, ForgeContext, Producer, ProducerError, ProducerId, ProducerOutput,
	ThreatTier,
};

/// C# project producer (Roslyn oracle → IR).
#[derive(Debug, Default, Clone, Copy)]
pub struct CSharpProducer;

impl Producer for CSharpProducer {
	const ID: ProducerId = ProducerId("roslyn-oracle/1");

	fn language(&self) -> Language {
		Language::CSharp
	}

	fn tier(&self) -> ThreatTier {
		// Source mode runs no user code (Roslyn only parses), but package
		// analyzers/source-generators are a code-exec vector — treat like Java.
		ThreatTier::Untrusted
	}

	fn plan<C: ForgeContext>(
		&self,
		_ctx: &C,
		_input: &SealedInput,
	) -> Result<ExecPlan, ProducerError> {
		Err(ProducerError::adaptive("roslyn oracle multi-step"))
	}

	fn decode(
		&self,
		_input: &SealedInput,
		_captured: Captured,
	) -> Result<ProducerOutput, ProducerError> {
		Err(ProducerError::decode(
			"roslyn oracle multi-step: adaptive — call produce()",
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
