//! Go method-set projection regressions through the owned IR.
//!
//! Each test drives one real `stretchr`-class width (the measured `pflag`
//! `FlagSet` 220 and `testify` `Assertions` 146 method sets) plus the exact
//! `MAX_REF_LIST_ELEMENTS` boundary (256 admitted; the old `u8` width of 255
//! is no longer the ceiling) from synthesized source through the Go oracle
//! image and `compile_semantic`, and then asserts the projected method set
//! carries the exact declared method names and count. An unprovisioned host
//! (no `NUDOX_GO_CORPUS_DIR`) records a typed, printed skip; it never fails
//! on missing tooling and never passes silently.

#![forbid(unsafe_code)]
#![allow(
    clippy::result_large_err,
    reason = "the typed error keeps the canonical build and projection terminals whole; boxing would discard their operands"
)]
#![allow(
    clippy::duration_suboptimal_units,
    reason = "Duration::from_minutes is not stable in this toolchain; the product spelling names the five-minute bound"
)]
#![allow(
    clippy::format_collect,
    reason = "the generated method block is one formatted row per declared method; map+collect keeps each row's formatting local"
)]

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant},
};

use backend_engine::driver::{
    CompileControl, CompileFailure, CompileOutput, CompileRequest, CompileScratch,
    CompiledSemantic, DeclarationScope, LoweringUnsupported, NativeTool, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainResolutionError, ToolchainSelection, compile_semantic,
};
use backend_frontend_go::legacy::{GoOracle, OracleError, StagingError, stage_module};
use backend_semantic::ir::{BuildError, SemanticReader};
use backend_semantic::vocabulary::{GoProjectionFault, GoVersion, LanguageProfile, Stage};
use thiserror::Error;

#[derive(Debug, Error)]
enum Error {
    #[error("fixture io: {0}")]
    Io(#[from] io::Error),
    #[error("oracle: {0}")]
    Oracle(#[from] OracleError),
    #[error("staging: {0}")]
    Staging(#[from] StagingError),
    #[error("toolchain: {0}")]
    Toolchain(#[from] ToolchainResolutionError),
    #[error("toolchain environment: {0}")]
    Environment(String),
    #[error("compile_semantic: {0}")]
    Compile(String),
    #[error("go projection terminal: {0:?}")]
    Projection(GoProjectionFault),
    #[error("canonical build terminal: {0}")]
    Build(#[source] BuildError),
    #[error("declaration {0:?} was absent from the semantic image")]
    Missing(&'static [u8]),
    #[error("Go extension facts were absent for Wide")]
    Extension,
    #[error("method_set entity list was absent")]
    List,
    #[error("method_set referenced a missing entity row")]
    Row,
    #[error("method_set held {observed} of {expected} declared Wide methods")]
    Incomplete { observed: usize, expected: usize },
    #[error(
        "Wide method_set diverged from the declared source truth at entry {index:?}: observed {observed:?}, expected {expected:?}"
    )]
    Diverged {
        index: Option<usize>,
        observed: String,
        expected: String,
    },
}

/// Typed fleet-provisioning outcome. A missing `NUDOX_GO_CORPUS_DIR` means
/// the host lacks the fleet Go provisioning; the regressions then record a
/// printed skip instead of failing on absent tooling or passing silently.
enum Provisioning {
    Ready,
    Unavailable(&'static str),
}

fn provisioning() -> Provisioning {
    if std::env::var_os("NUDOX_GO_CORPUS_DIR").is_none() {
        return Provisioning::Unavailable(
            "NUDOX_GO_CORPUS_DIR is unset: no fleet Go provisioning, projection regressions skipped",
        );
    }
    Provisioning::Ready
}

/// Runs `test` only on a provisioned host; otherwise prints the typed skip.
fn provisioned(test: impl FnOnce() -> Result<(), Error>) -> Result<(), Error> {
    match provisioning() {
        Provisioning::Ready => test(),
        Provisioning::Unavailable(reason) => {
            println!("skip: {reason}");
            Ok(())
        }
    }
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Result<Self, Error> {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "nudox-go-projection-{label}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn toolchain() -> Result<ResolvedToolchain<'static>, Error> {
    let path = std::env::var_os("COMPILER_GO_COMPILER")
        .map(PathBuf::from)
        .ok_or_else(|| Error::Environment("COMPILER_GO_COMPILER is not set".to_owned()))?;
    let version = {
        let output = std::process::Command::new(&path)
            .arg("version")
            .output()
            .map_err(|error| Error::Environment(error.to_string()))?;
        if output.stdout.is_empty() {
            output.stderr
        } else {
            output.stdout
        }
    };
    let version: &'static [u8] = Box::leak(version.into_boxed_slice());
    let path: &'static Path = Box::leak(
        path.canonicalize()
            .map_err(|error| Error::Environment(error.to_string()))?
            .into_boxed_path(),
    );
    Ok(ResolvedToolchain::from_version(
        NativeTool::GoCompiler,
        path,
        version,
    )?)
}

fn method_name(index: usize) -> Vec<u8> {
    format!("WideMethod{index:03}").into_bytes()
}

fn wide_source(methods: usize) -> Vec<u8> {
    let mut source = String::from("package wide\n\ntype Wide struct{ n int }\n\n");
    let methods_block: String = (0..methods)
        .map(|index| {
            format!("func (w *Wide) WideMethod{index:03}() int {{ return w.n + {index} }}\n\n")
        })
        .collect();
    source.push_str(&methods_block);
    source.into_bytes()
}

fn wide_method_set(methods: usize) -> Result<(), Error> {
    let source = wide_source(methods);
    let root = TempDir::new("wide")?;
    let staged = stage_module(&root.0, &root.0.join("wide.go"), &source)?;
    let oracle = GoOracle {
        output_limit: 32 * 1024 * 1024,
        timeout: Duration::from_secs(5 * 60),
    };
    let image = oracle.authority_image_for_package(&staged.source, &staged.root)?;
    let work = root.0.join("work");
    fs::create_dir_all(&work)?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = vec![0_u8; 64 * 1024];
    let mut output = vec![0xa5_u8; 16 * 1024 * 1024];
    let selection = toolchain()?;
    let request = CompileRequest {
        profile: LanguageProfile::Go(GoVersion::Go125),
        stage: Stage::LowerIr,
        source: &source,
        declaration_scope: DeclarationScope::fixture(),
        toolchain: ToolchainSelection::ResolvedNative(selection),
        authority: SemanticAuthorityInput::Go { image: &image },
        control: CompileControl {
            deadline: Instant::now() + Duration::from_secs(5 * 60),
            cancelled: &cancelled,
        },
    };
    let compiled = match compile_semantic(
        request,
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: &work,
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    ) {
        Ok(compiled) => compiled,
        Err(CompileFailure::LoweringUnsupported {
            cause: LoweringUnsupported::GoProjection { fault },
            ..
        }) => return Err(Error::Projection(fault)),
        Err(CompileFailure::Build { cause, .. }) => return Err(Error::Build(cause)),
        Err(failure) => return Err(Error::Compile(format!("{failure:?}"))),
    };
    expect_exact_method_set(&compiled, methods)
}

/// Asserts the owned IR's projected `Wide` method set carries the exact
/// declared method names and count of the synthesized source.
fn expect_exact_method_set(compiled: &CompiledSemantic<'_>, methods: usize) -> Result<(), Error> {
    let wide = compiled
        .ir
        .items_named(b"Wide")
        .next()
        .ok_or(Error::Missing(b"Wide"))?;
    let facts = compiled
        .ir
        .go_extension(wide.id())
        .ok_or(Error::Extension)?;
    let list = compiled
        .ir
        .entity_list(facts.method_set)
        .ok_or(Error::List)?;
    let mut observed: Vec<Vec<u8>> = list
        .map(|id| {
            compiled
                .ir
                .item(id)
                .map(|item| item.name().to_vec())
                .ok_or(Error::Row)
        })
        .collect::<Result<Vec<_>, Error>>()?;
    observed.sort();
    let mut expected: Vec<Vec<u8>> = (0..methods).map(method_name).collect();
    expected.sort();
    if observed.len() != methods {
        return Err(Error::Incomplete {
            observed: observed.len(),
            expected: methods,
        });
    }
    if observed != expected {
        let index = observed
            .iter()
            .zip(expected.iter())
            .position(|(observed, expected)| observed != expected);
        let name = |names: &[Vec<u8>]| {
            index.and_then(|index| names.get(index)).map_or_else(
                || "<absent>".to_owned(),
                |name| String::from_utf8_lossy(name).into_owned(),
            )
        };
        return Err(Error::Diverged {
            index,
            observed: name(&observed),
            expected: name(&expected),
        });
    }
    Ok(())
}

#[test]
fn wide_struct_with_128_pointer_methods_keeps_the_complete_method_set() -> Result<(), Error> {
    provisioned(|| wide_method_set(128))
}

#[test]
fn wide_struct_with_146_pointer_methods_keeps_the_complete_method_set() -> Result<(), Error> {
    provisioned(|| wide_method_set(146))
}

#[test]
fn wide_struct_with_220_pointer_methods_keeps_the_complete_method_set() -> Result<(), Error> {
    provisioned(|| wide_method_set(220))
}

#[test]
fn wide_struct_with_255_pointer_methods_keeps_the_complete_method_set() -> Result<(), Error> {
    provisioned(|| wide_method_set(255))
}

#[test]
fn wide_struct_with_256_pointer_methods_keeps_the_complete_method_set() -> Result<(), Error> {
    provisioned(|| wide_method_set(256))
}
