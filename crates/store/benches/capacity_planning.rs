//! Measures `backend-store` benches capacity-planning work with production data paths.
//! Measurements separate setup from steady-state work and retain resource counters.
//! Results support capacity decisions without changing the measured implementation.
#![deny(unsafe_code)]
//! Real-public-API compiler, publication, index, Tantivy, and vector capacity benchmark.

#[path = "capacity_planning/measure.rs"]
mod measure;
#[path = "capacity_planning/model.rs"]
mod model;
#[path = "capacity_planning/runner.rs"]
mod runner;

use std::{
    env,
    fs::{self, File},
    io::{self, Write},
    num::TryFromIntError,
    path::{Path, PathBuf},
    process::ExitStatus,
    str::Utf8Error,
    time::SystemTimeError,
};

use backend_engine::driver::ToolchainResolutionError;
use backend_engine::publication::PublishCompiledError;
use backend_store::journal::{PublicationLimitError, PublicationOpenError, ShutdownError};
use thiserror::Error;

use crate::{
    model::{BenchmarkResult, CacheMode, Configuration, Stage},
    runner::Runtime,
};

/// Capacity planning runner failure with a closed stage or operating-system cause.
#[derive(Debug, Error)]
enum BenchmarkError {
    #[error("unrecognized argument {argument}")]
    UnknownArgument { argument: String },
    #[error("argument {argument} requires a value")]
    MissingValue { argument: Argument },
    #[error("argument {argument} rejected value {value}")]
    InvalidValue { argument: Argument, value: String },
    #[error("corpus size {observed} is outside 1..={maximum}")]
    CorpusSize { maximum: usize, observed: usize },
    #[error(
        "cold-process mode requires exactly one sample; invoke it again for another cold sample"
    )]
    ColdProcessSampleCount,
    #[error("corpus slot {index} is outside initialized length {len}")]
    CorpusSlot { index: usize, len: usize },
    #[error("compact source exceeded its fixed benchmark buffer")]
    SourceCapacity,
    #[error("generated source slot has invalid initialized length {len}")]
    SourceSlotLength { len: usize },
    #[error("generated lexical source name was not UTF-8")]
    SourceNameUtf8(#[source] Utf8Error),
    #[error("fragment slot {index} has invalid length {len}")]
    FragmentSlot { index: usize, len: usize },
    #[error("fragment slot {index} is outside fixed capacity {capacity}")]
    FragmentIndex { index: usize, capacity: usize },
    #[error("stage-derived slice length {observed} exceeds fixed capacity {capacity}")]
    FixedSliceLength { observed: usize, capacity: usize },
    #[error("the allocation-measurement closure did not execute")]
    MeasurementDidNotRun,
    #[error("byte count cannot fit the benchmark accounting width")]
    ByteCount(#[source] TryFromIntError),
    #[error("logical byte count overflowed")]
    ByteCountOverflow,
    #[error("logical item count overflowed")]
    ItemCountOverflow,
    #[error("could not create directory {path}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not resolve the process working directory")]
    CurrentDirectory(#[source] io::Error),
    #[error("could not inspect directory {path}")]
    ReadDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not read a directory entry")]
    ReadDirectoryEntry(#[source] io::Error),
    #[error("could not inspect file type for {path}")]
    FileType {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not inspect metadata for {path}")]
    Metadata {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("system clock predates the Unix epoch")]
    Clock(#[source] SystemTimeError),
    #[error("requested rustc path must be absolute: {path}")]
    RelativeRustc { path: PathBuf },
    #[error("could not locate an absolute rustc on PATH")]
    RustcNotFound,
    #[error("could not start rustc for version discovery")]
    RustcVersionStart(#[source] io::Error),
    #[error("rustc --version --verbose failed with {status}")]
    RustcVersionRejected { status: ExitStatus },
    #[error("rustc version bytes were not UTF-8")]
    RustcVersionUtf8(#[source] std::string::FromUtf8Error),
    #[error("could not bind selected rustc to its version provenance")]
    ToolchainResolution(#[source] Box<ToolchainResolutionError>),
    #[error("native compile/lower public API rejected generated source slot {slot}: {cause:?}")]
    Compile {
        slot: usize,
        cause: Box<runner::CompileFailureFact>,
    },
    #[error("fresh compact fragment validation failed")]
    FragmentValidate(#[source] Box<backend_semantic::ir::FragmentError>),
    #[error("canonical semantic IR construction failed")]
    SemanticIr(#[source] backend_semantic::ir::BuildError),
    #[error("canonical semantic IR prerequisite was not constructed")]
    MissingSemanticIr,
    #[error("canonical semantic rendering exceeded its accounting width")]
    SemanticRender,
    #[error("direct canonical-IR Trustfall query failed")]
    TrustfallIr(#[source] backend_extension_trustfall::server::TrustfallGraphError),
    #[error("deterministic public index build failed: {cause:?}")]
    IndexBuild {
        cause: Box<runner::BuildFailureFact>,
    },
    #[error("durable publisher limits rejected the benchmark configuration")]
    PublicationLimits(#[source] Box<PublicationLimitError>),
    #[error("could not create durable publisher")]
    PublicationOpen(#[source] Box<PublicationOpenError>),
    #[error("durable compiler publication failed")]
    Publish(#[source] Box<PublishCompiledError>),
    #[error("durable publisher shutdown failed")]
    PublicationShutdown(#[source] Box<ShutdownError>),
    #[error("compiler publication and subsequent durable shutdown both failed")]
    PublishAndShutdown {
        publish: Box<PublishCompiledError>,
        shutdown: Box<ShutdownError>,
    },
    #[error("could not reopen the selected durable compiler publication")]
    OpenPublished(#[source] Box<backend_engine::publication::OpenPublishedError>),
    #[error("durable compiler publication selected no package")]
    MissingPublishedCompilation,
    #[error("could not reconstruct a manifest-named durable compact fragment")]
    OpenedFragment(#[source] Box<backend_engine::publication::OpenedFragmentError>),
    #[error("valid generated exact rows were rejected: {cause:?}")]
    ExactSegment { cause: runner::ExactSegmentFault },
    #[error("generated exact snapshot was rejected")]
    IndexSnapshot(backend_semantic::index_core::IndexSnapshotError),
    #[error("generated exact manifest was rejected")]
    ExactManifest(backend_semantic::index_core::ExactManifestError),
    #[error("generated lexical segment was rejected: {cause:?}")]
    LexicalSegment {
        cause: Box<runner::LexicalSegmentFault>,
    },
    #[error("generated lexical manifest was rejected")]
    LexicalManifest(backend_semantic::index_core::LexicalManifestError),
    #[error("real Tantivy adapter operation failed")]
    Tantivy(#[source] backend_extension_tantivy::server::TantivyAdapterError),
    #[error("generated vector ingress facts were rejected: {cause:?}")]
    VectorIngress {
        cause: Box<backend_semantic::graph_vector::VectorSegmentError>,
    },
    #[error("generated validated vector segment was rejected: {cause:?}")]
    VectorSegment {
        cause: Box<backend_semantic::graph_vector::VectorSegmentError>,
    },
    #[error("real scalar vector query failed: {cause:?}")]
    VectorQuery {
        cause: Box<backend_semantic::graph_vector::VectorQueryError>,
    },
    #[error("could not create capacity result file {path}")]
    CreateResult {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not serialize capacity result")]
    Serialize(#[source] serde_json::Error),
    #[error("could not flush capacity result file")]
    FlushResult(#[source] io::Error),
}

/// Closed command argument identity used by typed parser errors.
#[derive(Clone, Copy, Debug, Error)]
enum Argument {
    #[error("--corpus")]
    Corpus,
    #[error("--samples")]
    Samples,
    #[error("--warmups")]
    Warmups,
    #[error("--mode")]
    Mode,
    #[error("--output")]
    Output,
    #[error("--rustc")]
    Rustc,
    #[error("--only-stage")]
    OnlyStage,
}

struct CommandLine {
    configuration: Configuration,
    output: PathBuf,
    rustc: Option<PathBuf>,
}

fn main() -> Result<(), BenchmarkError> {
    let Some(command) = parse_command_line()? else {
        print_help();
        return Ok(());
    };
    let runtime = Runtime::discover(command.rustc)?;
    let result = runtime.run(command.configuration, &command.output)?;
    write_result(&command.output, &result)?;
    print_summary(&result);
    Ok(())
}

fn parse_command_line() -> Result<Option<CommandLine>, BenchmarkError> {
    let mut corpus_size = 8_usize;
    let mut samples = 1_usize;
    let mut warmups = 0_usize;
    let mut cache_mode = CacheMode::ColdProcess;
    let mut output = PathBuf::from("results");
    let mut rustc = None;
    let mut only_stage = None;
    let mut arguments = env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--corpus" => {
                corpus_size = parse_usize(
                    Argument::Corpus,
                    next_value(&mut arguments, Argument::Corpus)?,
                )?;
            }
            "--samples" => {
                samples = parse_usize(
                    Argument::Samples,
                    next_value(&mut arguments, Argument::Samples)?,
                )?;
            }
            "--warmups" => {
                warmups = parse_usize(
                    Argument::Warmups,
                    next_value(&mut arguments, Argument::Warmups)?,
                )?;
            }
            "--mode" => cache_mode = parse_mode(next_value(&mut arguments, Argument::Mode)?)?,
            "--output" => output = PathBuf::from(next_value(&mut arguments, Argument::Output)?),
            "--rustc" => rustc = Some(PathBuf::from(next_value(&mut arguments, Argument::Rustc)?)),
            "--only-stage" => {
                only_stage = parse_stage(next_value(&mut arguments, Argument::OnlyStage)?)?;
            }
            // Cargo appends this compatibility flag when executing a custom benchmark target.
            "--bench" => {}
            "--help" => {
                return Ok(None);
            }
            _ => return Err(BenchmarkError::UnknownArgument { argument }),
        }
    }
    if corpus_size == 0 || corpus_size > model::MAX_CORPUS {
        return Err(BenchmarkError::CorpusSize {
            maximum: model::MAX_CORPUS,
            observed: corpus_size,
        });
    }
    if samples == 0 {
        return Err(BenchmarkError::InvalidValue {
            argument: Argument::Samples,
            value: "0".to_owned(),
        });
    }
    if matches!(cache_mode, CacheMode::ColdProcess) && samples != 1 {
        return Err(BenchmarkError::ColdProcessSampleCount);
    }
    Ok(Some(CommandLine {
        configuration: Configuration {
            corpus_size,
            samples,
            warmups,
            cache_mode,
            only_stage,
        },
        output,
        rustc,
    }))
}

fn print_help() {
    println!(
        "Usage: capacity-planning [--corpus 1..64] [--samples N] [--warmups N] [--mode cold|warm] [--output DIR] [--rustc ABSOLUTE_PATH] [--only-stage STAGE]"
    );
}

fn next_value(
    arguments: &mut impl Iterator<Item = String>,
    argument: Argument,
) -> Result<String, BenchmarkError> {
    arguments
        .next()
        .ok_or(BenchmarkError::MissingValue { argument })
}

fn parse_usize(argument: Argument, value: String) -> Result<usize, BenchmarkError> {
    value
        .parse::<usize>()
        .map_err(|_| BenchmarkError::InvalidValue { argument, value })
}

fn parse_mode(value: String) -> Result<CacheMode, BenchmarkError> {
    match value.as_str() {
        "cold" => Ok(CacheMode::ColdProcess),
        "warm" => Ok(CacheMode::WarmProcess),
        _ => Err(BenchmarkError::InvalidValue {
            argument: Argument::Mode,
            value,
        }),
    }
}

fn parse_stage(value: String) -> Result<Option<Stage>, BenchmarkError> {
    if value == "all" {
        return Ok(None);
    }
    Stage::parse(&value)
        .map(Some)
        .ok_or(BenchmarkError::InvalidValue {
            argument: Argument::OnlyStage,
            value,
        })
}

fn write_result(directory: &Path, result: &BenchmarkResult) -> Result<(), BenchmarkError> {
    fs::create_dir_all(directory).map_err(|source| BenchmarkError::CreateDirectory {
        path: directory.to_path_buf(),
        source,
    })?;
    let path = directory.join("result.json");
    let file = File::create(&path).map_err(|source| BenchmarkError::CreateResult {
        path: path.clone(),
        source,
    })?;
    serde_json::to_writer_pretty(file, result).map_err(BenchmarkError::Serialize)?;
    let mut newline = File::options().append(true).open(&path).map_err(|source| {
        BenchmarkError::CreateResult {
            path: path.clone(),
            source,
        }
    })?;
    newline
        .write_all(b"\n")
        .map_err(BenchmarkError::FlushResult)?;
    newline.flush().map_err(BenchmarkError::FlushResult)
}

fn print_summary(result: &BenchmarkResult) {
    println!(
        "COMPILER capacity sample: corpus={} samples={} mode={:?}",
        result.configuration.corpus_size,
        result.configuration.samples,
        result.configuration.cache_mode
    );
    for stage in Stage::all() {
        let mut count = 0_u128;
        let mut total = 0_u128;
        for sample in result.samples.iter().filter(|sample| sample.stage == stage) {
            count += 1;
            total += sample.wall_time_ns;
        }
        if let Some(mean_nanoseconds) = total.checked_div(count) {
            let milliseconds = mean_nanoseconds / 1_000_000;
            let microseconds = (mean_nanoseconds % 1_000_000) / 1_000;
            println!(
                "  {:<30} mean_wall_ms={milliseconds}.{microseconds:03}",
                stage.selector(),
            );
        }
    }
    println!("  machine-readable result.json written to the requested --output directory");
}
