//! Measures `heart-root` benches capacity-planning runner orchestration work with production data paths.
//! Measurements separate setup from steady-state work and retain resource counters.
//! Results support capacity decisions without changing the measured implementation.
//! Thin, ordered public-API journey orchestration for measured and prepared stages.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use compiler_driver::{NativeTool, ResolvedToolchain};

use crate::{
    BenchmarkError,
    measure::{StageWork, stage},
    model::{
        BenchmarkResult, CacheMode, Configuration, Machine, Stage, StageSample,
        UnavailableCapability,
    },
    runner::{
        exact::exact_query,
        fixture::{Corpus, Fixture, FragmentSlots},
        machine::{cpu_model, operating_system_release, physical_cpu_count, physical_memory_bytes},
        native::compile_corpus,
        publication::{deterministic_build, durable_publish},
        semantic::{self, SemanticCorpus},
        support::{absolute_path, absolute_result_path, find_rustc},
        tantivy::{tantivy_build, tantivy_query},
        vector::{vector_ingress, vector_query},
    },
};

/// Resolved operating facts shared by all samples in one invocation.
pub(crate) struct Runtime {
    executable: PathBuf,
    machine: Machine,
}

impl Runtime {
    /// Resolves an explicit or PATH-discovered native Rust compiler once.
    pub(crate) fn discover(requested: Option<PathBuf>) -> Result<Self, BenchmarkError> {
        let executable = match requested {
            Some(path) => absolute_path(path)?,
            None => find_rustc()?,
        };
        let version = Command::new(&executable)
            .args(["--version", "--verbose"])
            .output()
            .map_err(BenchmarkError::RustcVersionStart)?;
        if !version.status.success() {
            return Err(BenchmarkError::RustcVersionRejected {
                status: version.status,
            });
        }
        let version =
            String::from_utf8(version.stdout).map_err(BenchmarkError::RustcVersionUtf8)?;
        let _ = ResolvedToolchain::from_version(NativeTool::Rustc, &executable, version.as_bytes())
            .map_err(|source| BenchmarkError::ToolchainResolution(Box::new(source)))?;
        Ok(Self {
            executable: executable.clone(),
            machine: Machine {
                operating_system: std::env::consts::OS,
                architecture: std::env::consts::ARCH,
                rustc_version: version,
                rustc_path: executable,
                profile: if cfg!(debug_assertions) {
                    "debug"
                } else {
                    "release"
                },
                operating_system_release: operating_system_release(),
                cpu_model: cpu_model(),
                physical_cpu_count: physical_cpu_count(),
                physical_memory_bytes: physical_memory_bytes(),
            },
        })
    }

    /// Produces an invocation result from actual public APIs without model-runtime fabrication.
    pub(crate) fn run(
        &self,
        configuration: Configuration,
        results_root: &Path,
    ) -> Result<BenchmarkResult, BenchmarkError> {
        let owned_root = absolute_result_path(results_root)?;
        for warmup in 0..configuration.warmups {
            let fixture = Fixture::create(&owned_root, "warmup", warmup + 1)?;
            let _samples = self.run_sample(&configuration, &fixture, 0, false)?;
        }
        let mut samples = Vec::with_capacity(configuration.samples * Stage::all().len());
        for sample in 1..=configuration.samples {
            let fixture = Fixture::create(&owned_root, "sample", sample)?;
            samples.extend(self.run_sample(&configuration, &fixture, sample, true)?);
        }
        Ok(BenchmarkResult {
            schema: "heart.capacity-planning.v1",
            machine: self.machine.clone(),
            configuration,
            samples,
            unavailable: [
                UnavailableCapability::QdrantVectorTransport,
                UnavailableCapability::LocalEmbeddingInference,
            ],
        })
    }

    fn run_sample(
        &self,
        configuration: &Configuration,
        fixture: &Fixture,
        sample: usize,
        record: bool,
    ) -> Result<Vec<StageSample>, BenchmarkError> {
        let sources = Corpus::new(configuration.corpus_size)?;
        let toolchain = ResolvedToolchain::from_version(
            NativeTool::Rustc,
            &self.executable,
            self.machine.rustc_version.as_bytes(),
        )
        .map_err(|source| BenchmarkError::ToolchainResolution(Box::new(source)))?;
        let cache_mode = configuration.cache_mode;
        let mut fragments = FragmentSlots::new();
        let semantic_source = SemanticCorpus::new(&sources);
        let mut semantic_ir = None;
        let mut samples = Vec::with_capacity(Stage::all().len());

        Self::run_or_prepare(
            Stage::NativeCompileLowerIr,
            configuration,
            record,
            &mut samples,
            cache_mode,
            sample,
            || {
                let bytes_read = sources.byte_len()?;
                let written = compile_corpus(&toolchain, &sources, fixture, &mut fragments)?;
                Ok(StageWork {
                    input_items: sources.len,
                    output_items: sources.len,
                    bytes_read,
                    bytes_written: written,
                    durable_bytes: 0,
                })
            },
        )?;

        Self::run_or_prepare(
            Stage::SemanticIrBuild,
            configuration,
            record,
            &mut samples,
            cache_mode,
            sample,
            || {
                let (ir, work) = semantic::build(&semantic_source)?;
                semantic_ir = Some(ir);
                Ok(work)
            },
        )?;

        Self::run_or_prepare(
            Stage::SemanticIrRender,
            configuration,
            record,
            &mut samples,
            cache_mode,
            sample,
            || {
                semantic::render(
                    semantic_ir
                        .as_ref()
                        .ok_or(BenchmarkError::MissingSemanticIr)?,
                )
            },
        )?;

        Self::run_or_prepare(
            Stage::IrVcsDiff,
            configuration,
            record,
            &mut samples,
            cache_mode,
            sample,
            || {
                semantic::vcs(
                    semantic_ir
                        .as_ref()
                        .ok_or(BenchmarkError::MissingSemanticIr)?,
                )
            },
        )?;

        Self::run_or_prepare(
            Stage::TrustfallIrQuery,
            configuration,
            record,
            &mut samples,
            cache_mode,
            sample,
            || {
                semantic::trustfall(
                    semantic_ir
                        .as_ref()
                        .ok_or(BenchmarkError::MissingSemanticIr)?,
                )
            },
        )?;
        let compiled = fragments.compiled(sources.len)?;

        Self::run_or_prepare(
            Stage::DurableCompilerPublication,
            configuration,
            record,
            &mut samples,
            cache_mode,
            sample,
            || durable_publish(&compiled, fixture),
        )?;

        Self::run_or_prepare(
            Stage::DeterministicIndexBuild,
            configuration,
            record,
            &mut samples,
            cache_mode,
            sample,
            || deterministic_build(fixture),
        )?;

        Self::run_or_prepare(
            Stage::ExactCoreQuery,
            configuration,
            record,
            &mut samples,
            cache_mode,
            sample,
            || exact_query(&sources, &fragments),
        )?;

        Self::run_or_prepare(
            Stage::TantivyLexicalBuild,
            configuration,
            record,
            &mut samples,
            cache_mode,
            sample,
            || tantivy_build(&sources, &fragments),
        )?;

        Self::run_or_prepare(
            Stage::TantivyLexicalQuery,
            configuration,
            record,
            &mut samples,
            cache_mode,
            sample,
            || tantivy_query(&sources, &fragments),
        )?;

        Self::run_or_prepare(
            Stage::VectorIngress,
            configuration,
            record,
            &mut samples,
            cache_mode,
            sample,
            || vector_ingress(sources.len),
        )?;

        Self::run_or_prepare(
            Stage::VectorExactQuery,
            configuration,
            record,
            &mut samples,
            cache_mode,
            sample,
            || vector_query(sources.len),
        )?;

        Self::run_or_prepare(
            Stage::IrVectorExactQuery,
            configuration,
            record,
            &mut samples,
            cache_mode,
            sample,
            || {
                semantic::vector(
                    semantic_ir
                        .as_ref()
                        .ok_or(BenchmarkError::MissingSemanticIr)?,
                )
            },
        )?;

        Ok(samples)
    }

    fn run_or_prepare(
        target: Stage,
        configuration: &Configuration,
        record: bool,
        samples: &mut Vec<StageSample>,
        cache_mode: CacheMode,
        sample: usize,
        operation: impl FnOnce() -> Result<StageWork, BenchmarkError>,
    ) -> Result<(), BenchmarkError> {
        if let Some(selected) = configuration.only_stage {
            if target.position() > selected.position() {
                return Ok(());
            }
            if selected != target {
                operation()?;
                return Ok(());
            }
        }
        if record {
            samples.push(stage(target, cache_mode, sample, operation)?);
        } else {
            operation()?;
        }
        Ok(())
    }
}
