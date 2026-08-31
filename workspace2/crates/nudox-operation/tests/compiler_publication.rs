//! Chief-owned public compiler and publication falsifiers.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use nudox_compile_driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, CompiledFragment, NativeTool,
    ResolvedToolchain, ToolchainResolutionError, ToolchainSelection, compile,
};
use nudox_compile_vocab::{Language, Stage};
use nudox_ir_format::{EntityKind, PrimitiveType, TypeNode};
use thiserror::Error;

static WORK_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Error)]
enum TestFailure {
    #[error("PATH is unavailable while resolving the Rust compiler fixture")]
    MissingPath,
    #[error("the Rust compiler is unavailable to the public compiler journey")]
    MissingRustc,
    #[error("could not canonicalize the Rust compiler fixture")]
    Canonicalize(#[source] std::io::Error),
    #[error("could not probe the exact Rust compiler version")]
    ProbeVersion(#[source] std::io::Error),
    #[error("the Rust compiler version probe was rejected with {status}")]
    VersionRejected { status: ExitStatus },
    #[error(transparent)]
    Resolve(#[from] ToolchainResolutionError),
    #[error("could not create caller-owned native work")]
    CreateWork(#[source] std::io::Error),
    #[error("could not inspect caller-owned native work")]
    InspectWork(#[source] std::io::Error),
    #[error("native work retained an unexpected artifact after compilation")]
    WorkNotEmpty,
    #[error("the public Rust compilation unexpectedly failed")]
    Compile,
}

struct HostRustc {
    executable: PathBuf,
}

impl HostRustc {
    fn resolve() -> Result<Self, TestFailure> {
        let paths = env::var_os("PATH").ok_or(TestFailure::MissingPath)?;
        for directory in env::split_paths(&paths) {
            let candidate = directory.join("rustc");
            if candidate.is_file() {
                return Ok(Self {
                    executable: candidate
                        .canonicalize()
                        .map_err(TestFailure::Canonicalize)?,
                });
            }
        }
        Err(TestFailure::MissingRustc)
    }

    fn toolchain(&self) -> Result<ResolvedToolchain<'_>, TestFailure> {
        let version = Command::new(&self.executable)
            .arg("--version")
            .output()
            .map_err(TestFailure::ProbeVersion)?;
        if !version.status.success() {
            return Err(TestFailure::VersionRejected {
                status: version.status,
            });
        }
        Ok(ResolvedToolchain::from_version(
            NativeTool::Rustc,
            &self.executable,
            &version.stdout,
        )?)
    }
}

struct NativeWork {
    path: PathBuf,
}

impl NativeWork {
    fn create() -> Result<Self, TestFailure> {
        let sequence = WORK_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = env::temp_dir().join(format!(
            "nudox-operation-compiler-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).map_err(TestFailure::CreateWork)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn assert_empty(&self) -> Result<(), TestFailure> {
        let mut entries = fs::read_dir(&self.path).map_err(TestFailure::InspectWork)?;
        match entries.next() {
            Some(Ok(_entry)) => Err(TestFailure::WorkNotEmpty),
            Some(Err(source)) => Err(TestFailure::InspectWork(source)),
            None => Ok(()),
        }
    }
}

impl Drop for NativeWork {
    fn drop(&mut self) {
        let _removed = fs::remove_dir(&self.path);
    }
}

#[test]
fn distinct_equal_shape_declarations_produce_distinct_semantic_ir() -> Result<(), TestFailure> {
    let alpha_source = b"pub const alpha: bool = true;";
    let bravo_source = b"pub const bravo: i32 = 1    ;";
    assert_eq!(alpha_source.len(), bravo_source.len());

    let host = HostRustc::resolve()?;
    let toolchain = host.toolchain()?;
    let work = NativeWork::create()?;
    let cancelled = AtomicBool::new(false);
    let mut alpha_diagnostic = [0; 4_096];
    let mut bravo_diagnostic = [0; 4_096];
    let mut alpha_output = [0; 512];
    let mut bravo_output = [0; 512];
    let alpha = compile(
        request(alpha_source, toolchain, &cancelled),
        CompileScratch {
            diagnostic_output: &mut alpha_diagnostic,
            native_work: work.path(),
        },
        CompileOutput {
            fragment_output: &mut alpha_output,
        },
    )
    .map_err(|_source| TestFailure::Compile)?;
    work.assert_empty()?;
    let bravo = compile(
        request(bravo_source, toolchain, &cancelled),
        CompileScratch {
            diagnostic_output: &mut bravo_diagnostic,
            native_work: work.path(),
        },
        CompileOutput {
            fragment_output: &mut bravo_output,
        },
    )
    .map_err(|_source| TestFailure::Compile)?;
    work.assert_empty()?;

    assert_ne!(alpha.source.identity, bravo.source.identity);
    assert_ne!(alpha.fragment.as_ref(), bravo.fragment.as_ref());
    assert_fragment(&alpha, b"alpha", PrimitiveType::Bool);
    assert_fragment(&bravo, b"bravo", PrimitiveType::I32);
    Ok(())
}

fn assert_fragment(
    compiled: &CompiledFragment<'_>,
    expected_name: &[u8],
    expected_type: PrimitiveType,
) {
    assert!(
        compiled
            .fragment
            .entities()
            .map(|entity| (entity.kind, entity.name.raw))
            .eq([(EntityKind::Constant, 0)])
    );
    assert!(
        compiled
            .fragment
            .atoms()
            .map(|atom| atom.bytes)
            .eq([expected_name])
    );
    assert!(
        compiled
            .fragment
            .type_nodes()
            .eq([TypeNode::Primitive(expected_type)])
    );
}

fn request<'source, 'toolchain, 'cancel>(
    source: &'source [u8],
    toolchain: ResolvedToolchain<'toolchain>,
    cancelled: &'cancel AtomicBool,
) -> CompileRequest<'source, 'toolchain, 'cancel> {
    CompileRequest {
        language: Language::Rust,
        stage: Stage::LowerIr,
        source,
        toolchain: ToolchainSelection::ResolvedNative(toolchain),
        control: CompileControl {
            deadline: Instant::now() + Duration::from_secs(5),
            cancelled,
        },
    }
}
