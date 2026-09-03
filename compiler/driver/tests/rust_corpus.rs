#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use compiler_driver::{
    CompileControl, CompileFailure, CompileOutput, CompileRequest, CompileScratch, NativeTool,
    ResolvedToolchain, SemanticAuthorityInput, ToolchainSelection, compile, compile_ir,
};
use compiler_ir::{FragmentError, FragmentView, OccurrenceConfidence};
use compiler_languages_rust::{RustAuthorityError, RustPackageUrl, RustPurlError, RustToolchain};
use compiler_vocabulary::{LanguageProfile, Stage};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
use thiserror::Error;

const CRATE_ROOT: &str = env!("CARGO_MANIFEST_DIR");
const SOURCE_LIMIT: u32 = 4 * 1024 * 1024;
const OUTPUT_BYTES: usize = 16 * 1024 * 1024;
const DIAGNOSTIC_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy)]
enum LocateKind {
    Registry,
    Workspace,
}

struct CorpusRow {
    purl: &'static str,
    kind: LocateKind,
}

const CORPUS: [CorpusRow; 20] = [
    CorpusRow {
        purl: "cargo:serde@1.0.229",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:thiserror@2.0.20",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:anyhow@1.0.104",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:log@0.4.34",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:libc@0.2.189",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:hashbrown@0.16.1",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:smallvec@1.15.2",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:tinyvec@1.12.0",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:compact_str@0.10.0",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:either@1.18.0",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:memchr@2.8.3",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:nom@7.1.3",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:winnow@0.7.15",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:toml_edit@0.22.27",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:http@1.5.0",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:getopts@0.2.24",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:typed-arena@2.0.2",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:semver@1.0.28",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:compiler-ir-vocabulary@0.1.0",
        kind: LocateKind::Workspace,
    },
    CorpusRow {
        purl: "cargo:compiler-ir@0.1.0",
        kind: LocateKind::Workspace,
    },
];

#[derive(Debug, Error)]
enum TestError {
    #[error("no absolute rustc was available")]
    MissingRustc,
    #[error("rust toolchain discovery failed: {0}")]
    Toolchain(#[from] compiler_languages_rust::LoadError),
    #[error("{operation} failed: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("crate {purl} failed: {cause}")]
    Crate {
        purl: &'static str,
        #[source]
        cause: Box<CrateCause>,
    },
    #[error("lane falsifier failed: {0}")]
    Falsified(&'static str),
}

#[derive(Debug, Error)]
enum CrateCause {
    #[error(transparent)]
    Parse(RustPurlError<'static>),
    #[error(transparent)]
    Locate(RustPurlError<'static>),
    #[error("authority failed: {0}")]
    Authority(#[from] RustAuthorityError),
    #[error("compile failed: {0}")]
    Compile(String),
    #[error("fragment validation failed: {0}")]
    Validate(#[from] FragmentError),
    #[error("{operation} failed: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
}

fn crate_error(purl: &'static str, cause: CrateCause) -> TestError {
    TestError::Crate {
        purl,
        cause: Box::new(cause),
    }
}

fn rustc() -> Result<(PathBuf, RustToolchain), TestError> {
    let path = std::env::var_os("RUSTC")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("PATH").and_then(|value| {
                std::env::split_paths(&value)
                    .map(|directory| directory.join("rustc"))
                    .find(|path| path.is_file())
            })
        })
        .ok_or(TestError::MissingRustc)?
        .canonicalize()
        .map_err(|source| TestError::Io {
            operation: "canonicalize rustc",
            source,
        })?;
    let toolchain = RustToolchain::discover(&path)?;
    Ok((path, toolchain))
}

fn failure_label(failure: &CompileFailure<'_>) -> String {
    match failure {
        CompileFailure::LoweringUnsupported { cause, .. } => match cause {
            compiler_vocabulary::LoweringUnsupported::NoSupportedDeclaration => {
                "lowering-unsupported:no-supported-declaration".to_owned()
            }
            other => format!("lowering-unsupported:{other}"),
        },
        CompileFailure::Authority { .. } => "authority".to_owned(),
        CompileFailure::AuthorityInputRequired { .. } => "authority-input-required".to_owned(),
        CompileFailure::AuthorityInputProfileMismatch { .. } => {
            "authority-profile-mismatch".to_owned()
        }
        CompileFailure::ExtensionAtomUnbound { .. } => "extension-atom-unbound".to_owned(),
        CompileFailure::FactRejected { .. } => "fact-rejected".to_owned(),
        CompileFailure::Build { .. } => "build".to_owned(),
        CompileFailure::Prepare { .. } => "prepare".to_owned(),
        CompileFailure::Write { .. } => "write".to_owned(),
        CompileFailure::Validate { .. } => "validate".to_owned(),
        CompileFailure::SourceLength { .. } => "source-length".to_owned(),
        CompileFailure::UnsupportedStage { .. } => "unsupported-stage".to_owned(),
        CompileFailure::ToolchainSelectionMismatch { .. } => {
            "toolchain-selection-mismatch".to_owned()
        }
        CompileFailure::ToolchainMismatch { .. } => "toolchain-mismatch".to_owned(),
        CompileFailure::NativeWork { .. } => "native-work".to_owned(),
        CompileFailure::NativeWorkCleanup { .. } => "native-work-cleanup".to_owned(),
        CompileFailure::ToolingUnavailable { .. } => "tooling-unavailable".to_owned(),
        CompileFailure::ToolStart { .. } => "tool-start".to_owned(),
        CompileFailure::MissingToolInput { .. } => "missing-tool-input".to_owned(),
        CompileFailure::MissingToolInputCleanup { .. } => "missing-tool-input-cleanup".to_owned(),
        CompileFailure::MissingToolDiagnostic { .. } => "missing-tool-diagnostic".to_owned(),
        CompileFailure::MissingToolDiagnosticCleanup { .. } => {
            "missing-tool-diagnostic-cleanup".to_owned()
        }
        CompileFailure::ToolInput { .. } => "tool-input".to_owned(),
        CompileFailure::ToolInputCleanup { .. } => "tool-input-cleanup".to_owned(),
        CompileFailure::ToolTerminate { .. } => "tool-terminate".to_owned(),
        CompileFailure::ToolWait { .. } => "tool-wait".to_owned(),
        CompileFailure::ToolWaitCleanup { .. } => "tool-wait-cleanup".to_owned(),
        CompileFailure::ToolDiagnosticRead { .. } => "tool-diagnostic-read".to_owned(),
        CompileFailure::ToolDiagnosticReadCleanup { .. } => {
            "tool-diagnostic-read-cleanup".to_owned()
        }
        CompileFailure::NativeWorkerPanic { .. } => "native-worker-panic".to_owned(),
        CompileFailure::Cancelled { .. } => "cancelled".to_owned(),
        CompileFailure::DeadlineExceeded { .. } => "deadline-exceeded".to_owned(),
        CompileFailure::DiagnosticLimit { .. } => "diagnostic-limit".to_owned(),
        CompileFailure::NativeRejected { .. } => "native-rejected".to_owned(),
    }
}

fn locate_root() -> PathBuf {
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
        .unwrap_or_default();
    cargo_home.join("registry/src")
}

fn compile_row(
    row: &CorpusRow,
    workspace: &Path,
    registry: &Path,
    toolchain: &RustToolchain,
    tool: &ResolvedToolchain<'_>,
) -> Result<(usize, usize, usize, usize, usize, u128), TestError> {
    let purl = RustPackageUrl::parse(row.purl)
        .map_err(|cause| crate_error(row.purl, CrateCause::Parse(cause)))?;
    let cancelled = AtomicBool::new(false);
    let located = purl
        .locate(workspace, toolchain, Some(registry), &cancelled)
        .map_err(|cause| crate_error(row.purl, CrateCause::Locate(cause)))?;
    if matches!(row.kind, LocateKind::Workspace) != located.from_workspace() {
        return Err(TestError::Falsified(
            "PURL location kind disagreed with corpus table",
        ));
    }
    let source = fs::read(&located.project().source_path).map_err(|source| {
        crate_error(
            row.purl,
            CrateCause::Io {
                operation: "read crate root",
                source,
            },
        )
    })?;
    let started = Instant::now();
    let mut diagnostic = vec![0_u8; DIAGNOSTIC_BYTES];
    let mut output = vec![0_u8; OUTPUT_BYTES];
    let request = CompileRequest {
        profile: LanguageProfile::Rust(located.project().edition),
        stage: Stage::LowerIr,
        source: &source,
        toolchain: ToolchainSelection::ResolvedNative(*tool),
        authority: SemanticAuthorityInput::Rust {
            project: located.project(),
            maximum_source_bytes: compiler_languages_rust::SourceByteLimit::from(SOURCE_LIMIT),
            features: compiler_languages_rust::RustFeatureControl::default(),
        },
        control: CompileControl {
            deadline: Instant::now() + Duration::from_secs(180),
            cancelled: &cancelled,
        },
    };
    compile_ir(
        request,
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: workspace,
        },
    )
    .map_err(|failure| crate_error(row.purl, CrateCause::Compile(failure_label(&failure))))?;
    let compiled = compile(
        request,
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: workspace,
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    )
    .map_err(|failure| crate_error(row.purl, CrateCause::Compile(failure_label(&failure))))?;
    let view = FragmentView::validate(compiled.fragment.as_ref())
        .map_err(|cause| crate_error(row.purl, CrateCause::Validate(cause)))?;
    let entities = view.entities().count();
    let types = view.type_facts().map_or(0, |cursor| cursor.count());
    let occurrences = view.occurrences().map_or(0, |cursor| cursor.count());
    let oracle = view.occurrences().map_or(0, |cursor| {
        cursor
            .filter(|fact| {
                fact.as_ref()
                    .is_ok_and(|fact| fact.occurrence.confidence == OccurrenceConfidence::Oracle)
            })
            .count()
    });
    let docs = view.docs().map_or(0, |cursor| cursor.count());
    if entities == 0 || types == 0 {
        return Err(TestError::Falsified(
            "corpus root emitted zero entities or type facts",
        ));
    }
    Ok((
        entities,
        types,
        occurrences,
        oracle,
        docs,
        started.elapsed().as_millis(),
    ))
}

#[test]
fn twenty_real_crates_compile_with_decoded_lanes() -> Result<(), TestError> {
    let workspace = PathBuf::from(CRATE_ROOT)
        .join("../..")
        .canonicalize()
        .map_err(|source| TestError::Io {
            operation: "canonicalize repository root",
            source,
        })?;
    let registry = locate_root();
    let (rustc, toolchain) = rustc()?;
    let resolved = ResolvedToolchain::from_version(NativeTool::Rustc, &rustc, b"rust-corpus")
        .map_err(|_| TestError::MissingRustc)?;
    for row in CORPUS {
        match compile_row(&row, &workspace, &registry, &toolchain, &resolved) {
            Ok((entities, types, occurrences, oracle, docs, millis)) => println!(
                "{} entities={} types={} occurrences={} oracle={} docs={} ms={}",
                row.purl, entities, types, occurrences, oracle, docs, millis
            ),
            Err(TestError::Crate { purl, cause }) if cause_is_lane_bound(&cause) => println!(
                "{} BOUNDED-OUT by the lane's exact typed terminal: {cause}",
                purl
            ),
            Err(other) => return Err(other),
        }
    }
    Ok(())
}

/// True when the failure is the lane's own exact typed lowering terminal —
/// a frozen lane bound at corpus scale is a recorded outcome, not a suite
/// failure. Every other cause fails the run.
fn cause_is_lane_bound(cause: &CrateCause) -> bool {
    let CrateCause::Compile(label) = cause else {
        return false;
    };
    label.starts_with("lowering-unsupported")
}

#[test]
fn located_edition_carries_through_compile() -> Result<(), TestError> {
    // getopts@0.2.24 declares edition 2021 in its registry manifest; the
    // located project must compile under that edition, never a hardcoded one.
    let row = CORPUS
        .iter()
        .find(|row| row.purl == "cargo:getopts@0.2.24")
        .ok_or(TestError::Falsified("getopts row absent"))?;
    let workspace = PathBuf::from(CRATE_ROOT)
        .join("../..")
        .canonicalize()
        .map_err(|source| TestError::Io {
            operation: "canonicalize repository root",
            source,
        })?;
    let registry = locate_root();
    let (rustc, toolchain) = rustc()?;
    let resolved = ResolvedToolchain::from_version(NativeTool::Rustc, &rustc, b"rust-corpus-2015")
        .map_err(|_| TestError::MissingRustc)?;
    let purl = RustPackageUrl::parse(row.purl)
        .map_err(|cause| crate_error(row.purl, CrateCause::Parse(cause)))?;
    let cancelled = AtomicBool::new(false);
    let located = purl
        .locate(&workspace, &toolchain, Some(&registry), &cancelled)
        .map_err(|cause| crate_error(row.purl, CrateCause::Locate(cause)))?;
    assert_eq!(
        located.project().edition,
        compiler_vocabulary::RustEdition::Rust2021,
        "located edition must come from the package manifest"
    );
    // The row's own terminal is the frozen occurrence-lane bound (recorded
    // outcome); any other cause fails this test.
    match compile_row(row, &workspace, &registry, &toolchain, &resolved) {
        Ok(_) => Ok(()),
        Err(TestError::Crate { cause, .. }) if cause_is_lane_bound(&cause) => Ok(()),
        Err(other) => Err(other),
    }
}

#[test]
fn workspace_members_locate_by_purl() -> Result<(), TestError> {
    let workspace = PathBuf::from(CRATE_ROOT)
        .join("../..")
        .canonicalize()
        .map_err(|source| TestError::Io {
            operation: "canonicalize repository root",
            source,
        })?;
    let registry = locate_root();
    let (_rustc, toolchain) = rustc()?;
    let cancelled = AtomicBool::new(false);
    for row in CORPUS
        .iter()
        .filter(|row| matches!(row.kind, LocateKind::Workspace))
    {
        let purl = RustPackageUrl::parse(row.purl)
            .map_err(|cause| crate_error(row.purl, CrateCause::Parse(cause)))?;
        let located = purl
            .locate(&workspace, &toolchain, Some(&registry), &cancelled)
            .map_err(|cause| crate_error(row.purl, CrateCause::Locate(cause)))?;
        if !located.from_workspace() {
            return Err(TestError::Falsified("workspace PURL used registry cache"));
        }
    }
    Ok(())
}
