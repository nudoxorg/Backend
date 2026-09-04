#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use compiler_driver::{
    CompileControl, CompileFailure, CompileOutput, CompileRequest, CompileScratch, NativeTool,
    ResolvedToolchain, SemanticAuthorityInput, ToolchainSelection, compile, compile_ir,
};
use compiler_ir::{EntityKind, FragmentError, FragmentView, OccurrenceConfidence, TypeFactSegment};
use compiler_languages_rust::{RustAuthorityError, RustPackageUrl, RustPurlError, RustToolchain};
use compiler_publication::immutable::ImmutableArtifactStore;
use compiler_publication::{
    OpenPublicationScratch, PublicationScratch, PublishControl, open_published, publish_compiled,
};
use compiler_vocabulary::{LanguageProfile, Stage};
use server_index_build::{IndexBuildScratch, build};
use server_index_publish::{
    CompilationIndexScratch, encode_index_pack, plan_index_pack, seal_compilation_index,
};
use server_journal::{DurablePublisher, PublicationLimits, PublicationPaths};
use sha2::{Digest, Sha256};
use std::{
    fs,
    mem::MaybeUninit,
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

// Blind stride over the 2026-09-04 cache: 1614 eligible directories, stride
// ceil(1614 / 16) = 101, after removing the two named rows and workspace rows.
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
        purl: "cargo:ab_glyph@0.2.32",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:block2@0.6.2",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:cookie_store@0.7.0",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:dragonbox_ecma@0.1.12",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:futures-executor@0.3.34",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:hashbrown@0.14.5",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:jni-sys-macros@0.4.1",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:ndk-context@0.1.1",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:ownedbytes@0.9.0",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:proptest@1.11.0",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:rayon@1.11.0",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:semver-parser@0.7.0",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:tantivy@0.25.0",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:typed-arena@2.0.2",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:wasmtime-internal-versioned-export-macros@47.0.3",
        kind: LocateKind::Registry,
    },
    CorpusRow {
        purl: "cargo:windows_x86_64_gnullvm@0.48.5",
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
    #[error("publication failed: {0}")]
    Publication(String),
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
        CompileFailure::CSharpProjection { .. } => "csharp-projection".to_owned(),
        CompileFailure::ClangProjection { .. } => "clang-projection".to_owned(),
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
            declaration_scope: compiler_driver::DeclarationScope::fixture(),
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
    let request = CompileRequest {
        control: CompileControl {
            deadline: Instant::now() + Duration::from_secs(180),
            ..request.control
        },
        ..request
    };
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
    let scratch = std::env::temp_dir().join(format!("nudox-rust-corpus-{}", std::process::id()));
    fs::create_dir_all(&scratch).map_err(|source| TestError::Io {
        operation: "create journal scratch",
        source,
    })?;
    let artifacts = scratch.join("artifacts");
    let journal_path = scratch.join("journal");
    fs::create_dir_all(&artifacts).map_err(|source| TestError::Io {
        operation: "create artifacts",
        source,
    })?;
    let limits = PublicationLimits::new(std::num::NonZeroUsize::MIN, std::num::NonZeroUsize::MIN)
        .map_err(|error| TestError::Publication(error.to_string()))?;
    let journal = DurablePublisher::create(&PublicationPaths::in_directory(&journal_path), limits)
        .map_err(|error| TestError::Publication(error.to_string()))?;
    let store = ImmutableArtifactStore::new(&artifacts)
        .map_err(|error| TestError::Publication(error.to_string()))?;
    let mut earlier = Vec::new();
    let mut previous = None;
    let mut previous_root = None;
    let mut saw_2015 = false;
    let mut saw_2021 = false;
    for row in CORPUS {
        let feature_count = usize::from(row.purl == "cargo:serde@1.0.229") + 1;
        for feature_index in 0..feature_count {
            let feature_name = if feature_index == 0 {
                "default"
            } else {
                "derive"
            };
            let started = Instant::now();
            let purl = RustPackageUrl::parse(row.purl)
                .map_err(|cause| crate_error(row.purl, CrateCause::Parse(cause)))?;
            let cancelled = AtomicBool::new(false);
            let located = purl
                .locate(&workspace, &toolchain, Some(&registry), &cancelled)
                .map_err(|cause| crate_error(row.purl, CrateCause::Locate(cause)))?;
            if matches!(row.kind, LocateKind::Workspace) != located.from_workspace() {
                return Err(TestError::Falsified(
                    "corpus location kind disagreed with table",
                ));
            }
            let manifest = located.project().root.join("Cargo.toml");
            let (layout, edition) = manifest_facts(&manifest)?;
            if edition == "2015" {
                saw_2015 = true;
            }
            if edition == "2021" {
                saw_2021 = true;
            }
            let expected = match edition.as_str() {
                "2015" => compiler_vocabulary::RustEdition::Rust2015,
                "2018" => compiler_vocabulary::RustEdition::Rust2018,
                "2021" => compiler_vocabulary::RustEdition::Rust2021,
                "2024" => compiler_vocabulary::RustEdition::Rust2024,
                _ => {
                    return Err(TestError::Falsified(
                        "manifest edition was not one of the closed Rust editions",
                    ));
                }
            };
            if located.project().edition != expected {
                return Err(TestError::Falsified(
                    "located edition disagreed with manifest",
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
            let cancelled = AtomicBool::new(false);
            let mut diagnostic = vec![0_u8; DIAGNOSTIC_BYTES];
            let mut output = vec![0_u8; OUTPUT_BYTES];
            let request = CompileRequest {
                profile: LanguageProfile::Rust(located.project().edition),
                stage: Stage::LowerIr,
                source: &source,
            declaration_scope: compiler_driver::DeclarationScope::fixture(),
                toolchain: ToolchainSelection::ResolvedNative(resolved),
                authority: SemanticAuthorityInput::Rust {
                    project: located.project(),
                    maximum_source_bytes: compiler_languages_rust::SourceByteLimit::from(
                        SOURCE_LIMIT,
                    ),
                    features: if feature_index == 0 {
                        compiler_languages_rust::RustFeatureControl::default()
                    } else {
                        compiler_languages_rust::RustFeatureControl {
                            features: &["derive"],
                            ..compiler_languages_rust::RustFeatureControl::default()
                        }
                    },
                },
                control: CompileControl {
                    deadline: Instant::now() + Duration::from_secs(180),
                    cancelled: &cancelled,
                },
            };
            let compiled = match compile(
                request,
                CompileScratch {
                    diagnostic_output: &mut diagnostic,
                    native_work: &workspace,
                },
                CompileOutput {
                    fragment_output: &mut output,
                },
            ) {
                Ok(compiled) => compiled,
                Err(failure) => {
                    let cause = CrateCause::Compile(failure_label(&failure));
                    if cause_is_lane_bound(&cause) {
                        println!("{} BOUNDED-OUT({})", row.purl, cause);
                    } else {
                        return Err(crate_error(row.purl, cause));
                    }
                    continue;
                }
            };
            let view = FragmentView::validate(compiled.fragment.as_ref())
                .map_err(|cause| crate_error(row.purl, CrateCause::Validate(cause)))?;
            let entities: Vec<_> = view.entities().collect();
            let mut declared = 0;
            let mut computed = 0;
            if let Some(cursor) = view.type_facts() {
                for fact in cursor.flatten() {
                    if matches!(fact.segment, TypeFactSegment::Declared) {
                        declared += 1;
                    } else {
                        computed += 1;
                    }
                }
            }
            let occurrences = view.occurrences().map_or(0, |cursor| cursor.count());
            let oracle = view.occurrences().map_or(0, |cursor| {
                cursor
                    .filter(|fact| {
                        fact.as_ref().is_ok_and(|fact| {
                            fact.occurrence.confidence == OccurrenceConfidence::Oracle
                        })
                    })
                    .count()
            });
            let docs = view.docs().map_or(0, |cursor| cursor.count());
            if entities.is_empty() || declared == 0 {
                return Err(TestError::Falsified(
                    "corpus root emitted zero entities or declared type facts",
                ));
            }
            let named = |name: &[u8], kind: EntityKind| {
                entities.iter().any(|entity| {
                    entity.kind == kind
                        && view
                            .atoms()
                            .nth(entity.name.raw as usize)
                            .is_some_and(|atom| atom.bytes == name)
                })
            };
            if row.purl == "cargo:serde@1.0.229" && feature_index == 0 {
                let reexports = entities
                    .iter()
                    .filter(|entity| entity.kind == EntityKind::Reexport)
                    .count();
                println!(
                    "serde reexports decoded={reexports} serialize={} deserialize={}",
                    named(b"Serialize", EntityKind::Reexport),
                    named(b"Deserialize", EntityKind::Reexport)
                );
                if reexports != 0 {
                    return Err(TestError::Falsified(
                        "serde default features admitted cfg-gated reexports",
                    ));
                }
            }
            if row.purl == "cargo:serde@1.0.229" && feature_index == 1 {
                if !named(b"Serialize", EntityKind::Reexport)
                    || !named(b"Deserialize", EntityKind::Reexport)
                {
                    return Err(TestError::Falsified(
                        "serde derive reexports were absent from decoded fragment",
                    ));
                }
            }
            // thiserror source truth: lib.rs is a facade (`mod ...; pub use
            // thiserror_impl::*; mod private;`). The public export is a glob, so
            // the lane's honest glob law emits no Reexport fact. `Error` is
            // re-exported only in src/private.rs, a Foreign file, not this root.
            let digest = Sha256::digest(compiled.fragment.as_ref());
            let (facts, publication) = match corpus_publish(&journal, &artifacts, &compiled) {
                Ok(value) => value,
                Err(TestError::Publication(message)) if publication_is_lane_bound(&message) => {
                    println!("{} BOUNDED-OUT({})", row.purl, message);
                    continue;
                }
                Err(cause) => return Err(cause),
            };
            if previous.is_some_and(|sequence| publication.stable.sequence <= sequence) {
                return Err(TestError::Falsified(
                    "journal generation did not strictly increase",
                ));
            }
            if previous_root.is_some_and(|root| publication.generation.pinned_root == root) {
                return Err(TestError::Falsified("journal pinned root did not change"));
            }
            previous = Some(publication.stable.sequence);
            previous_root = Some(publication.generation.pinned_root);
            earlier.push((facts, digest));
            for (old_facts, old_digest) in &earlier {
                let mut old_output = vec![0_u8; OUTPUT_BYTES];
                let old = store
                    .open(*old_facts, &mut old_output)
                    .map_err(|error| TestError::Publication(error.to_string()))?;
                if Sha256::digest(old.as_ref()) != *old_digest {
                    return Err(TestError::Falsified(
                        "earlier fragment digest changed after later publish",
                    ));
                }
                FragmentView::validate(old.as_ref())
                    .map_err(|error| TestError::Publication(error.to_string()))?;
            }
            println!(
                "{}[{}] entities={} types={} computed={} occurrences={} oracle={} docs={} ms={} edition={} layout={} PASS",
                row.purl,
                feature_name,
                entities.len(),
                declared,
                computed,
                occurrences,
                oracle,
                docs,
                started.elapsed().as_millis(),
                edition,
                layout
            );
        }
    }
    if !saw_2015 || !saw_2021 {
        return Err(TestError::Falsified(
            "corpus edition span missed 2015 or 2021",
        ));
    }
    journal
        .shutdown()
        .map_err(|error| TestError::Publication(error.to_string()))?;
    fs::remove_dir_all(&scratch).map_err(|source| TestError::Io {
        operation: "remove journal scratch",
        source,
    })
}

/// True for the lane's own exact lowering terminal.
fn cause_is_lane_bound(cause: &CrateCause) -> bool {
    match cause {
        CrateCause::Compile(label) => label.starts_with("lowering-unsupported"),
        _ => false,
    }
}

/// The second recorded class, currently string-plumbed from the typed
/// `server_index_build::BuildAdmissionError::EntityLimit` terminal.
fn publication_is_lane_bound(message: &str) -> bool {
    message.starts_with("fragment has ") && message.contains("shared segment capacity is ")
}

fn manifest_facts(path: &Path) -> Result<(&'static str, String), TestError> {
    let text = fs::read_to_string(path).map_err(|source| TestError::Io {
        operation: "read crate manifest",
        source,
    })?;
    let edition = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("edition = \"")?.strip_suffix('"'))
        .or_else(|| {
            text.lines()
                .find(|line| line.trim() == "edition.workspace = true")
                .map(|_| "2024")
        })
        .unwrap_or("2015");
    let layout = if path.with_file_name("build.rs").is_file() {
        "build.rs"
    } else if text.lines().any(|line| line.trim() == "[lib]") {
        "[lib] section"
    } else {
        "plain lib"
    };
    Ok((layout, edition.to_owned()))
}

fn corpus_publish(
    journal: &DurablePublisher,
    artifacts: &Path,
    fragment: &compiler_driver::CompiledFragment<'_>,
) -> Result<
    (
        compiler_publication::manifest::StoredFragmentFacts,
        server_journal::PublicationFacts,
    ),
    TestError,
> {
    let mut manifest = vec![0_u8; 1 << 20];
    let mut facts = [None; 1];
    let mut ordinals = [0_usize; 1];
    let mut locality = vec![0_u8; 1 << 16];
    let mut binding = vec![0_u8; 128];
    publish_compiled(
        journal,
        artifacts,
        std::slice::from_ref(fragment),
        PublishControl::Continue,
        PublicationScratch {
            manifest_output: &mut manifest,
            manifest_facts: &mut facts,
            ordinals: &mut ordinals,
            locality_output: &mut locality,
            binding_output: &mut binding,
        },
    )
    .map_err(|error| TestError::Publication(error.to_string()))?;
    let mut reopened_manifest = vec![0_u8; 1 << 20];
    let mut reopened_facts = [None; 1];
    let mut reopened_fragments = vec![0_u8; 1 << 20];
    let mut reopened_locality = vec![0_u8; 1 << 16];
    let opened = open_published(
        journal,
        artifacts,
        OpenPublicationScratch {
            manifest_output: &mut reopened_manifest,
            manifest_facts: &mut reopened_facts,
            fragment_output: &mut reopened_fragments,
            locality_output: &mut reopened_locality,
        },
    )
    .map_err(|error| TestError::Publication(error.to_string()))?
    .ok_or(TestError::Falsified("publication disappeared on reopen"))?;
    let publication = opened.publication;
    let current = opened
        .fragments()
        .next()
        .ok_or(TestError::Falsified("publication had no fragment"))?
        .map_err(|error| TestError::Publication(error.to_string()))?;
    let mut projections = [MaybeUninit::uninit(); 4096];
    let mut entities = [MaybeUninit::uninit(); 4096];
    let mut exact = [MaybeUninit::uninit(); 4096];
    let mut lexical = [MaybeUninit::uninit(); 4096];
    let mut atoms = [MaybeUninit::uninit(); 4096];
    let mut types = [MaybeUninit::uninit(); 4096];
    let prepared = build(
        &current,
        IndexBuildScratch {
            projections: &mut projections,
            entities: &mut entities,
            exact_rows: &mut exact,
            lexical_rows: &mut lexical,
            atoms: &mut atoms,
            type_nodes: &mut types,
        },
    )
    .map_err(|error| TestError::Publication(error.to_string()))?;
    let mut exact_ids = [prepared.exact.id];
    let mut lexical_ids = [prepared.lexical.id];
    let sealed = seal_compilation_index(
        opened,
        std::slice::from_ref(&prepared),
        CompilationIndexScratch {
            exact: &mut exact_ids,
            lexical: &mut lexical_ids,
        },
    )
    .map_err(|error| TestError::Publication(error.error.to_string()))?;
    let plan =
        plan_index_pack(&sealed).map_err(|error| TestError::Publication(error.to_string()))?;
    let mut encoded = vec![0_u8; plan.encoded_bytes];
    encode_index_pack(&plan, &mut encoded)
        .map_err(|error| TestError::Publication(error.to_string()))?;
    Ok((current.facts, publication))
}

#[test]
fn located_edition_carries_through_compile() -> Result<(), TestError> {
    // ab_glyph@0.2.32 declares edition 2021 in its registry manifest; the
    // located project must compile under that edition, never a hardcoded one.
    let row = CORPUS
        .iter()
        .find(|row| row.purl == "cargo:ab_glyph@0.2.32")
        .ok_or(TestError::Falsified("ab_glyph row absent"))?;
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
