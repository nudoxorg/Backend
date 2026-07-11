//! Go [`Producer`](crate::compile::producer::Producer) — oracle form.

use std::path::Path;

use heart::Language;
use sandbox::{Captured, ProducerProfile, SealedInput};

use crate::compile::isolate::{self, IsolatedCommand};
use crate::compile::producer::{
	AuxOutputs, ExecPlan, ForgeContext, Producer, ProducerError, ProducerId, ProducerOutput,
	ThreatTier,
};

use super::context::GoContext;
use super::oracle;
use super::package;

/// Go module producer (`go run` oracle → IR).
#[derive(Debug, Default, Clone, Copy)]
pub struct GoProducer;

impl Producer for GoProducer {
	const ID: ProducerId = ProducerId("go-oracle/1");

	fn language(&self) -> Language {
		Language::Go
	}

	fn tier(&self) -> ThreatTier {
		ThreatTier::Untrusted
	}

	fn plan<C: ForgeContext>(
		&self,
		ctx: &C,
		input: &SealedInput,
	) -> Result<ExecPlan, ProducerError> {
		let module = package::discover_module(&input.root).map_err(ProducerError::plan)?;
		let oracle_dir = package::materialize_oracle().map_err(ProducerError::plan)?;
		let target = module.root.canonicalize().map_err(ProducerError::plan)?;

		let cmd = IsolatedCommand::new("go", ProducerProfile::Go)
			.arg("run")
			.arg(".")
			.arg(&target)
			.cwd(&oracle_dir)
			.env("GOWORK", "off")
			.env("GOFLAGS", "-mod=mod")
			.env("GOPROXY", "off")
			.ro(&oracle_dir)
			.ro(&target)
			.rw(&oracle_dir)
			.rw(input.budget.fs.scratch_path())
			.rw(std::env::temp_dir());

		Ok(ExecPlan::Commands(vec![isolate::seal(ctx, cmd)]))
	}

	fn decode(
		&self,
		input: &SealedInput,
		captured: Captured,
	) -> Result<ProducerOutput, ProducerError> {
		let output: oracle::Output =
			serde_json::from_slice(&captured.stdout).map_err(ProducerError::decode)?;
		for error in &output.errors {
			tracing::warn!("go oracle diagnostic: {error}");
		}
		let module = package::discover_module(&input.root).map_err(ProducerError::lower)?;
		let ctx = GoContext { module, output };
		Ok(ProducerOutput {
			index: ctx.lower_package(),
			aux: AuxOutputs::default(),
		})
	}

	/// Keep the historical one-shot path available for call sites / tests that
	/// don't go through plan→execute (still real lowering, not a stub).
	fn lower_in_process<C: ForgeContext>(
		&self,
		ctx: &C,
		root: &Path,
	) -> Result<ProducerOutput, ProducerError> {
		let index = super::lower_package(ctx, root).map_err(ProducerError::lower)?;
		Ok(ProducerOutput::from_index(index))
	}
}
