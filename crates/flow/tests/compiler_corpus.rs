//! Executes the deterministic source-to-output audit matrix.
//!
//! This test is deliberately source-first.  A row is either compiled from a
//! real native/semantic authority, or it records a typed local-unavailability
//! terminal.  No row is downgraded to a syntax-only answer when its authority
//! is absent.  The compact fragment, owned IR, durable reopen, and renderer
//! observations are joined by [`CaseId`], never by the order in which a pass
//! admitted its inputs.

#[path = "support/compiler_corpus/authority.rs"]
mod authority;
#[path = "support/compiler_corpus/comparison.rs"]
mod comparison;
#[path = "support/compiler_corpus/execution.rs"]
mod execution;
#[path = "support/compiler_corpus/grouped.rs"]
mod grouped;
#[path = "support/compiler_corpus/inventory.rs"]
mod inventory;
#[path = "support/multilingual_corpus.rs"]
mod multilingual_corpus;
#[path = "support/native_tooling.rs"]
mod native_tooling;
#[path = "support/compiler_corpus/observation.rs"]
mod observation;
#[path = "support/compiler_corpus/publication.rs"]
mod publication;
#[path = "support/compiler_corpus/real.rs"]
mod real;

use std::{
    fmt::Write as _,
    fs,
    hash::{Hash, Hasher},
    io::{self, Read},
    num::NonZeroUsize,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use backend_engine::driver::{
    CompileControl, CompileFailure, CompileOutput, CompileRequest, CompileScratch,
    CompiledSemantic, DeclarationScope, NativeTool, ResolvedToolchain, SemanticAuthorityInput,
    ToolchainSelection, compile_semantic,
};
use backend_engine::publication::manifest::SemanticImageRegion;
use backend_engine::publication::{
    OpenPublicationScratch, OpenSemanticPublicationScratch, OpenedFragmentError,
    PublicationScratch, PublishControl, SemanticPublicationScratch, open_published,
    open_published_semantic, publish_compiled, publish_semantic,
};
use backend_semantic::ir::{
    BuiltinType, ConcreteType, DeclarationFamilyId, DeclarationKeyFault, EntityAuthorityFacts,
    EntityId, EntityKind, FactAvailability, FragmentRangeManifest, FragmentView, ImageProvenance,
    Ir, ItemKind, PackageLineage, PackageLineageFault, ParentageAuthority, PrimitiveType,
    SemanticImageIdentity, SemanticReader, SourceIdentity, SourceSpan, TypeExpr, TypeId, TypeNode,
    VariantFingerprint, Visibility,
};
use backend_semantic::vocabulary::{
    CSharpVersion, CxxStandard, GoVersion, JavaRelease, Language, LanguageProfile, PythonVersion,
    RustEdition, Stage, TypeScriptSource,
};
use backend_store::journal::{DurablePublisher, PublicationLimits, PublicationPaths};
use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};
use multilingual_corpus::{
    CaseAvailability, CaseId, CorpusLanguage, CorpusPackage, CountExpectation, ExpectedFacts,
    ExpectedType, PACKAGE_COUNT, PackageShape, PlaneAvailability, RelationExpectation,
    RenderAvailability, SOURCE_BYTE_LIMIT, corpus_packages,
};
use native_tooling::{HostTool, NativeToolingError, NativeWork};
use thiserror::Error;

use authority::{
    AuthorityBuildError, AuthorityFactory, AuthorityUnavailableCause, CSharpHelperError, HostTools,
    NativeUnavailableCause, NativeUnavailableKind, ProviderSlot, ResolvedTools,
    SourceUnavailableCause, SourceUnavailableKind, csharp_row_is_unavailable,
    go_error_is_unavailable, go_fixture, java_row_is_unavailable, native_slot, native_tool,
    rust_error_is_unavailable, rust_fixture,
};
use comparison::{
    CorpusMismatch, Pass, PermutationField, Plane, compare_passes, expected_mismatches,
};
use execution::run_package;
use inventory::{
    CorpusCapacityVerdict, InventoryInvariant, REAL_PACKAGE_COUNT, RealInventorySummary,
    validate_source_inventory,
};
use observation::{
    CaseObservation, CaseOutputObservation, CompactObservation, CountObservation,
    EntityObservation, ObservedTypeShape, OwnedObservation, PlaneObservation, RenderVerdict,
    ReopenedObservation, StableHasher, VersionObservation, digest_bytes, observe_compact,
    observe_owned, range_manifest_digest, render_neutral,
};
use publication::PassPublisher;

const DIAGNOSTIC_BYTES: usize = 16 * 1024;
const FRAGMENT_BYTES: usize = 256 * 1024;
const SEMANTIC_IMAGE_BYTES: usize = 64 * 1024 * 1024;
const MANIFEST_BYTES: usize = 256 * 1024;
const LOCALITY_BYTES: usize = 256 * 1024;
const AUTHORITY_BYTES: usize = 32 * 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(120);
const CSHARP_DIAGNOSTIC_BYTES: usize = 4 * 1024;
const CSHARP_IMAGE_BYTES: usize = 32 * 1024 * 1024;

type Digest = [u8; 32];

/// A compact, copyable key used to join observations from differently ordered
/// matrix passes.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct CaseKey {
    case_id: CaseId,
    language: CorpusLanguage,
    shape: PackageShape,
}

const fn case_key(package: CorpusPackage) -> CaseKey {
    CaseKey {
        case_id: package.case_id,
        language: package.language,
        shape: package.shape,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LaneSummary {
    attempted: u16,
    output: u16,
    unavailable: u16,
    mismatches: u16,
}

impl LaneSummary {
    const ZERO: Self = Self {
        attempted: 0,
        output: 0,
        unavailable: 0,
        mismatches: 0,
    };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MatrixSummary {
    lanes: [LaneSummary; CorpusLanguage::ALL.len() * PackageShape::ALL.len()],
}

impl MatrixSummary {
    const fn new() -> Self {
        Self {
            lanes: [LaneSummary::ZERO; CorpusLanguage::ALL.len() * PackageShape::ALL.len()],
        }
    }

    fn lane_mut(&mut self, package: CorpusPackage) -> &mut LaneSummary {
        &mut self.lanes[lane_index(package)]
    }
}

fn lane_index(package: CorpusPackage) -> usize {
    language_index(package.language) * PackageShape::ALL.len() + shape_index(package.shape)
}

const fn language_index(language: CorpusLanguage) -> usize {
    match language {
        CorpusLanguage::Rust => 0,
        CorpusLanguage::TypeScript => 1,
        CorpusLanguage::Python => 2,
        CorpusLanguage::Go => 3,
        CorpusLanguage::Java => 4,
        CorpusLanguage::CSharp => 5,
        CorpusLanguage::Clang => 6,
    }
}

const fn shape_index(shape: PackageShape) -> usize {
    match shape {
        PackageShape::Constant => 0,
        PackageShape::Callable => 1,
        PackageShape::Aggregate => 2,
        PackageShape::Generic => 3,
        PackageShape::Documentation => 4,
        PackageShape::Reference => 5,
    }
}

#[derive(Debug, Error)]
enum CorpusAuditError {
    #[error(transparent)]
    Render(#[from] multilingual_corpus::CorpusRenderError),
    #[error("grouped corpus source assembly failed")]
    GroupedSource(#[from] grouped::GroupedSourceError),
    #[error("corpus declaration scope lineage was rejected")]
    ScopeLineage(PackageLineageFault),
    #[error("corpus declaration scope key was rejected")]
    ScopeKey(DeclarationKeyFault),
    #[error("corpus filesystem phase {phase:?} failed")]
    Io {
        phase: AuditIoPhase,
        #[source]
        source: io::Error,
    },
    #[error("corpus durable publisher limits were rejected")]
    Limits(#[from] backend_store::journal::PublicationLimitError),
    #[error("corpus durable publisher could not be created")]
    Publisher(#[from] backend_store::journal::PublicationOpenError),
    #[error("corpus durable publisher could not be shut down")]
    Shutdown(#[from] backend_store::journal::ShutdownError),
    #[error("corpus publication failed")]
    Publish(#[from] backend_engine::publication::PublishCompiledError),
    #[error("corpus semantic publication failed")]
    PublishSemantic(#[from] backend_engine::publication::PublishSemanticError),
    #[error("corpus publication reopen failed")]
    Open(#[from] backend_engine::publication::OpenPublishedError),
    #[error("corpus reopened fragment failed")]
    OpenedFragment(#[from] OpenedFragmentError),
    #[error("corpus reopened semantic artifact failed")]
    OpenedSemanticArtifact(#[from] backend_engine::publication::OpenedSemanticArtifactError),
    #[error("corpus fragment range manifest could not be reconstructed")]
    Ranges(#[from] backend_semantic::ir::FragmentRangeManifestError),
    #[error("corpus authority setup failed for {key:?}")]
    AuthoritySetup {
        key: CaseKey,
        #[source]
        cause: AuthorityBuildError,
    },
    #[error("corpus native work failed for {key:?}")]
    NativeWork {
        key: CaseKey,
        #[source]
        cause: NativeToolingError,
    },
    #[error("corpus invariant {cause:?} failed for {key:?}")]
    Invariant {
        key: Option<CaseKey>,
        cause: CorpusInvariant,
    },
    #[error("real package inventory invariant {cause:?} failed")]
    Inventory { cause: InventoryInvariant },
    #[error("source/output matrix retained {count} mismatches; first={first:?}")]
    Mismatches {
        count: usize,
        first: Option<CorpusMismatch>,
    },
}

/// Structural audit failures kept separate from source-vs-output mismatches.
/// These are keyed and typed so a malformed pass cannot be mistaken for a
/// semantic disagreement in one source row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CorpusInvariant {
    PackageCount {
        observed: usize,
        expected: usize,
    },
    CaseIdentity {
        ordinal: usize,
        observed: CaseId,
    },
    DuplicateCase,
    MissingCase,
    LaneCount {
        language: CorpusLanguage,
        shape: PackageShape,
        observed: usize,
        expected: usize,
    },
    MissingPublishedFragment,
    ExtraPublishedFragment,
    PublisherClosed,
    AuthorityBindingMismatch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AuditIoPhase {
    Fixture,
    RustManifest,
    RustSource,
    GoManifest,
    GoSource,
    CSharpPublish,
    CSharpSource,
    CSharpImage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CompileTerminalKind {
    SourceLength,
    UnsupportedStage,
    ToolchainSelectionMismatch,
    ToolchainMismatch,
    NativeWork,
    NativeWorkCleanup,
    ToolingUnavailable,
    ToolStart,
    MissingToolInput,
    MissingToolInputCleanup,
    MissingToolDiagnostic,
    MissingToolDiagnosticCleanup,
    ToolInput,
    ToolInputCleanup,
    ToolTerminate,
    ToolWait,
    ToolWaitCleanup,
    ToolDiagnosticRead,
    ToolDiagnosticReadCleanup,
    NativeWorkerPanic,
    Cancelled,
    DeadlineExceeded,
    DiagnosticLimit,
    NativeRejected,
    Authority,
    AuthorityInputRequired,
    AuthorityInputProfileMismatch,
    LoweringUnsupported,
    ExtensionAtomUnbound,
    ExtensionTypeParametersUnbound,
    ClangProjection,
    Build,
    Prepare,
    Write,
    Validate,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PassObservation {
    cases: Box<[CaseObservation; PACKAGE_COUNT]>,
    summary: MatrixSummary,
    mismatches: Box<[CorpusMismatch]>,
}

struct FixtureDir {
    path: PathBuf,
}

static FIXTURE_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

impl FixtureDir {
    fn new(label: &str) -> Result<Self, io::Error> {
        for _attempt in 0..64 {
            let serial = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "backend-flow-operation-corpus-{label}-{}-{serial}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(source) => return Err(source),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique corpus fixture directory",
        ))
    }

    fn child(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _removed = fs::remove_dir_all(&self.path);
    }
}

fn declaration_scope(
    package: CorpusPackage,
) -> Result<DeclarationScope<'static>, CorpusAuditError> {
    let scope = package.scope_spec();
    let lineage = PackageLineage::new(scope.ecosystem, scope.package)
        .map_err(CorpusAuditError::ScopeLineage)?;
    DeclarationScope::new(lineage, scope.path).map_err(CorpusAuditError::ScopeKey)
}

fn source_identity(source: &[u8]) -> SourceIdentity {
    SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source),
        byte_len: u32::try_from(source.len()).unwrap_or(u32::MAX),
    }
}

fn expected_recipe(
    profile: LanguageProfile,
    tool: NativeTool,
    source: SourceIdentity,
    toolchain: ResolvedToolchain<'_>,
) -> backend_semantic::vocabulary::CompileRecipeFact {
    backend_semantic::vocabulary::CompileRecipeFact::derive(
        profile,
        Stage::LowerIr,
        tool,
        source.identity,
        toolchain.identity,
    )
}

fn validate_matrix(packages: &[CorpusPackage]) -> Result<(), CorpusAuditError> {
    if packages.len() != PACKAGE_COUNT {
        return Err(CorpusAuditError::Invariant {
            key: None,
            cause: CorpusInvariant::PackageCount {
                observed: packages.len(),
                expected: PACKAGE_COUNT,
            },
        });
    }
    let canonical: Vec<CorpusPackage> = corpus_packages().collect();
    let mut seen = [false; PACKAGE_COUNT];
    let mut lanes = [0_usize; CorpusLanguage::ALL.len() * PackageShape::ALL.len()];
    for (position, package) in packages.iter().copied().enumerate() {
        let raw = usize::from(package.case_id.raw());
        if raw >= PACKAGE_COUNT || package.ordinal != raw {
            return Err(CorpusAuditError::Invariant {
                key: Some(case_key(package)),
                cause: CorpusInvariant::CaseIdentity {
                    ordinal: package.ordinal,
                    observed: package.case_id,
                },
            });
        }
        if seen[raw] {
            return Err(CorpusAuditError::Invariant {
                key: Some(case_key(package)),
                cause: CorpusInvariant::DuplicateCase,
            });
        }
        seen[raw] = true;
        lanes[lane_index(package)] += 1;
        if canonical.get(position).copied() != Some(package) {
            return Err(CorpusAuditError::Invariant {
                key: Some(case_key(package)),
                cause: CorpusInvariant::CaseIdentity {
                    ordinal: position,
                    observed: package.case_id,
                },
            });
        }
    }
    if seen.iter().any(|present| !present) {
        return Err(CorpusAuditError::Invariant {
            key: None,
            cause: CorpusInvariant::MissingCase,
        });
    }
    for language in CorpusLanguage::ALL {
        for shape in PackageShape::ALL {
            let package = CorpusPackage {
                ordinal: 0,
                language,
                shape,
                case_id: CaseId(0),
            };
            let observed = lanes[lane_index(package)];
            if observed != multilingual_corpus::CASES_PER_SHAPE {
                return Err(CorpusAuditError::Invariant {
                    key: None,
                    cause: CorpusInvariant::LaneCount {
                        language,
                        shape,
                        observed,
                        expected: multilingual_corpus::CASES_PER_SHAPE,
                    },
                });
            }
        }
    }
    Ok(())
}

fn ordered_packages(packages: &[CorpusPackage], pass: Pass) -> Vec<CorpusPackage> {
    let mut ordered = packages.to_vec();
    match pass {
        Pass::Original => {}
        Pass::Reverse => ordered.reverse(),
        Pass::FixedShuffle => {
            ordered.sort_by_key(|package| (shuffle_key(package.case_id.raw()), package.case_id))
        }
    }
    ordered
}

/// A bijective affine permutation over the complete 210-row domain. The
/// multiplier is coprime to 210, so this cannot silently collapse rows or
/// leave the input in ordinal order as an overflowing u16 arithmetic trick
/// would.
const fn shuffle_key(raw: u16) -> u16 {
    (((raw as u32) * 137 + 17) % PACKAGE_COUNT as u32) as u16
}

const _: () = {
    assert!(shuffle_key(0) == 17);
    assert!(shuffle_key(209) == 90);
    assert!(shuffle_key(0) != 0);
    assert!(shuffle_key(1) != 1);
    // `0` sorts before `1`, unlike reverse order; `2` sorts before `1`,
    // unlike canonical order. Together these witnesses prove the pass is a
    // distinct permutation rather than either control ordering.
    assert!(shuffle_key(0) < shuffle_key(1));
    assert!(shuffle_key(1) > shuffle_key(2));
};

#[test]
#[allow(
    clippy::result_large_err,
    reason = "the audit retains exact typed source, authority, publication, and mismatch facts"
)]
fn all_two_hundred_ten_cases_compare_source_to_ir_publish_reopen_and_render()
-> Result<(), CorpusAuditError> {
    let hosts = HostTools::resolve();
    let resolved = ResolvedTools::from_hosts(&hosts);
    let first_key = CaseKey {
        case_id: CaseId(0),
        language: CorpusLanguage::Rust,
        shape: PackageShape::Constant,
    };
    let authorities =
        AuthorityFactory::new(&hosts).map_err(|cause| CorpusAuditError::AuthoritySetup {
            key: first_key,
            cause,
        })?;
    grouped::run_grouped_audit(&hosts, &resolved, &authorities)
}

/// Audits the frozen real-package source inventory through the fused semantic
/// compiler/publication boundary.  Source absence, authority absence, and
/// deferred reader planes remain typed red outcomes; none can be converted to
/// a generated-case success.
#[test]
fn real_package_inventory_keeps_source_provenance_and_closed_terminals()
-> Result<(), CorpusAuditError> {
    let inventory: RealInventorySummary =
        validate_source_inventory().map_err(|cause| CorpusAuditError::Inventory { cause })?;
    if inventory.attempted.iter().sum::<usize>() != REAL_PACKAGE_COUNT {
        return Err(CorpusAuditError::Inventory {
            cause: InventoryInvariant::TotalCount {
                observed: inventory.attempted.iter().sum(),
                expected: REAL_PACKAGE_COUNT,
            },
        });
    }

    let hosts = HostTools::resolve();
    let resolved = ResolvedTools::from_hosts(&hosts);
    let packages: Vec<CorpusPackage> = corpus_packages().collect();
    let first_key = packages
        .first()
        .copied()
        .map(case_key)
        .ok_or(CorpusAuditError::Invariant {
            key: None,
            cause: CorpusInvariant::MissingCase,
        })?;
    let authorities =
        AuthorityFactory::new(&hosts).map_err(|cause| CorpusAuditError::AuthoritySetup {
            key: first_key,
            cause,
        })?;
    let audit = real::audit_real_inventory(&hosts, &resolved, &authorities)?;
    for (index, language) in CorpusLanguage::ALL.into_iter().enumerate() {
        let lane = audit.lanes[index];
        eprintln!(
            "real-corpus language={language:?} attempted={} source_bound={} output={} verified={} unavailable={} not_compared={} terminals={} mismatches={}",
            lane.attempted,
            lane.source_bound,
            lane.output,
            lane.verified,
            lane.unavailable,
            lane.not_compared,
            lane.terminals,
            lane.mismatches,
        );
    }
    eprintln!("real-corpus capacity={:?}", audit.capacity);
    eprintln!(
        "real-corpus manifest={} seed={:#x} unavailable_terminals={} parity_mismatches={}",
        inventory::fleet_manifest::FLEET_MANIFEST_VERSION,
        inventory::fleet_manifest::FLEET_SELECTION_SEED,
        audit.unavailable.len(),
        audit.mismatches.len(),
    );
    // Bounded audit telemetry: the first mismatch records, one line each, so
    // a lane-wide red verdict identifies *which* authority plane disagreed on
    // *which* row without re-running a filtered sweep. The dump is hard-capped
    // at 60 records — a diagnostic head, not a transcript — and prints only
    // the language, coordinate, and plane field; digests stay in the typed
    // verdict that follows.
    for entry in audit.mismatches.iter().take(60) {
        match entry {
            CorpusMismatch::Real { case, field, .. } => eprintln!(
                "real-row-mismatch lang={:?} coord={:?} field={}",
                case.language,
                case.coordinate.raw(),
                real::encode_real_field(*field),
            ),
            CorpusMismatch::RealUnavailable { case, field, cause } => eprintln!(
                "real-row-mismatch lang={:?} coord={:?} field={} cause={}",
                case.language,
                case.coordinate.raw(),
                real::encode_real_field(*field),
                real::encode_cause(cause),
            ),
            CorpusMismatch::RealTerminal { case, terminal, .. } => eprintln!(
                "real-row-mismatch lang={:?} coord={:?} field=terminal kind={}",
                case.language,
                case.coordinate.raw(),
                real::encode_terminal_kind(*terminal),
            ),
            _ => {}
        }
    }
    let accounted: usize = audit.lanes.iter().map(|lane| lane.attempted).sum();
    if accounted != audit.expected {
        return Err(CorpusAuditError::Inventory {
            cause: InventoryInvariant::TotalCount {
                observed: accounted,
                expected: audit.expected,
            },
        });
    }
    real::real_audit_verdict(&audit)?;
    Ok(())
}

/// Non-vacuity proof for the real-package accounting.
///
/// The audit's final decision must be driven by genuine parity mismatches:
/// counted observer/source/toolchain Unavailable terminals must pass, while a
/// real source-vs-output disagreement must still fail. The unavailable case is
/// the exact shape the former code mis-filed into the mismatch sink, and the
/// mismatch case proves that routing did not blunt real parity enforcement.
#[test]
fn real_audit_verdict_separates_unavailable_terminals_from_parity_mismatches() {
    let case = inventory::real_package_cases()
        .next()
        .expect("the committed fleet manifest always selects at least one case");

    let mut lanes = [real_lane(0); CorpusLanguage::ALL.len()];
    lanes[0] = real_lane(REAL_PACKAGE_COUNT);

    let unavailable_only = real::RealAuditSummary {
        lanes,
        expected: REAL_PACKAGE_COUNT,
        capacity: CorpusCapacityVerdict::MeetsMinimum,
        unavailable: vec![CorpusMismatch::RealUnavailable {
            case,
            field: comparison::RealAuditField::SourceSpans,
            cause: AuthorityUnavailableCause::ObserverUnavailable,
        }]
        .into_boxed_slice(),
        mismatches: Vec::new().into_boxed_slice(),
    };
    real::real_audit_verdict(&unavailable_only)
        .expect("counted Unavailable terminals must not fail the capacity verdict");

    let genuine_mismatch = real::RealAuditSummary {
        lanes,
        expected: REAL_PACKAGE_COUNT,
        capacity: CorpusCapacityVerdict::MeetsMinimum,
        unavailable: Vec::new().into_boxed_slice(),
        mismatches: vec![CorpusMismatch::Real {
            case,
            field: comparison::RealAuditField::SourceSpans,
            expected: [0_u8; 32],
            observed: [1_u8; 32],
        }]
        .into_boxed_slice(),
    };
    let error = real::real_audit_verdict(&genuine_mismatch)
        .expect_err("a genuine parity mismatch must remain fatal");
    assert!(
        matches!(
            error,
            CorpusAuditError::Mismatches {
                count: 1,
                first: Some(_)
            }
        ),
        "expected a mismatch verdict, observed {error:?}"
    );
}

/// Focused polyfill repro for the C# conditional-shell selection.
///
/// polyfill 2.0.0's raw-largest source, `DefaultInterpolatedStringHandler.cs`,
/// is wrapped in `#if HAS_SPAN && !NET6_0_OR_GREATER`. The oracle never
/// defines `HAS_SPAN` and always defines `NET6_0_OR_GREATER`, so the bound
/// tree emits zero declarations; because the authority image is built from
/// the bound tree alone, compiling the whole package alongside it changes
/// nothing. This proves the shell image is empty, then proves the resolver's
/// replacement selection (`is_csharp_conditional_shell`) produces a real
/// image that lowers through the exact request the audit uses.
#[test]
#[allow(
    clippy::result_large_err,
    reason = "the audit retains exact typed source, authority, publication, and mismatch facts"
)]
fn csharp_polyfill_package_source_lowers_real_declarations() -> Result<(), CorpusAuditError> {
    let hosts = HostTools::resolve();
    let resolved = ResolvedTools::from_hosts(&hosts);
    let key = CaseKey {
        case_id: CaseId(0),
        language: CorpusLanguage::CSharp,
        shape: PackageShape::Reference,
    };
    let authorities = AuthorityFactory::new(&hosts)
        .map_err(|cause| CorpusAuditError::AuthoritySetup { key, cause })?;
    let ProviderSlot::Ready(provider) = &authorities.csharp else {
        eprintln!("csharp provider unavailable; skipping focused polyfill repro");
        return Ok(());
    };
    let Ok((profile, toolchain)) = native_slot(CorpusLanguage::CSharp, &resolved) else {
        eprintln!("csharp native toolchain unavailable; skipping focused polyfill repro");
        return Ok(());
    };

    let mut polyfill = None;
    for input in inventory::real_package_inputs() {
        if let inventory::RealPackageInput::Source { case, source } = input {
            if case.coordinate == inventory::PackageCoordinate::Purl("nuget:polyfill@2.0.0") {
                polyfill = Some((case, source));
                break;
            }
        }
    }
    let Some((case, source)) = polyfill else {
        eprintln!("polyfill not provisioned; skipping focused repro");
        return Ok(());
    };

    // The raw-largest source is the conditional shell: its source-bound image
    // must carry no declarations, which is the exact terminal being fixed.
    let Some(shell) = raw_largest_csharp_source(&source.package_root) else {
        eprintln!("polyfill package has no .cs files; skipping focused repro");
        return Ok(());
    };
    // A distinct synthetic case id keeps the provider's per-case staging
    // directory from colliding with the real row's own extraction below.
    let shell_image = provider
        .image_for_package_source(u16::MAX, &source.package_root, &shell)
        .map_err(|cause| CorpusAuditError::AuthoritySetup { key, cause })?;
    let shell_declarations = backend_frontend_csharp::legacy::CSharpImage::open(&shell_image)
        .map_err(|error| CorpusAuditError::AuthoritySetup {
            key,
            cause: AuthorityBuildError::CSharpImage(error),
        })?
        .declarations()
        .len();

    let image = provider
        .image_for_package_source(case.case_id.raw(), &source.package_root, &source.path)
        .map_err(|cause| CorpusAuditError::AuthoritySetup { key, cause })?;
    let declarations = backend_frontend_csharp::legacy::CSharpImage::open(&image)
        .map_err(|error| CorpusAuditError::AuthoritySetup {
            key,
            cause: AuthorityBuildError::CSharpImage(error),
        })?
        .declarations()
        .len();
    eprintln!(
        "polyfill raw_largest={} shell_declarations={shell_declarations} selected={} declarations={declarations}",
        shell.display(),
        source.path.display(),
    );

    let (ecosystem, package) = case.coordinate.lineage();
    let lineage = PackageLineage::new(ecosystem.as_str(), package.as_str())
        .map_err(CorpusAuditError::ScopeLineage)?;
    let path = source
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("source");
    let scope = DeclarationScope::new(lineage, path).map_err(CorpusAuditError::ScopeKey)?;
    let work = NativeWork::create().map_err(|cause| CorpusAuditError::NativeWork { key, cause })?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0_u8; DIAGNOSTIC_BYTES];
    let mut fragment = vec![0xa5_u8; FRAGMENT_BYTES];
    let compiled = execution::compile_with_authority(
        profile,
        &source.bytes,
        scope,
        toolchain,
        SemanticAuthorityInput::CSharp { image: &image },
        &cancelled,
        &mut diagnostic,
        work.path(),
        &mut fragment,
    );

    assert_eq!(
        shell_declarations, 0,
        "the raw-largest polyfill source is expected to be a fully gated shell"
    );
    assert!(
        declarations > 0,
        "the resolver must select a polyfill source with real declarations"
    );
    assert!(
        compiled.is_ok(),
        "polyfill must lower real declarations, observed {:?}",
        compiled.err()
    );
    let entity_count = compiled
        .as_ref()
        .map(|compiled| compiled.artifact.fragment.entities().count())
        .unwrap_or(0);
    assert!(
        entity_count > 0,
        "the lowered polyfill fragment must carry real entities"
    );
    Ok(())
}

/// The raw-largest `.cs` beneath a package root, matching the resolver's
/// pre-fix selection shape (size descending, then path) closely enough to
/// identify polyfill's gated shell.
fn raw_largest_csharp_source(root: &Path) -> Option<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    let mut best: Option<(u64, PathBuf)> = None;
    while let Some(directory) = stack.pop() {
        let entries = fs::read_dir(&directory).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if matches!(
                    entry.file_name().to_str(),
                    Some("bin" | "obj" | ".git" | "node_modules")
                ) {
                    continue;
                }
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|extension| extension.to_str()) != Some("cs") {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            let length = metadata.len();
            if best.as_ref().is_none_or(|(known, _)| length > *known) {
                best = Some((length, path));
            }
        }
    }
    best.map(|(_, path)| path)
}

fn real_lane(attempted: usize) -> real::RealLaneSummary {
    real::RealLaneSummary {
        attempted,
        source_bound: 0,
        output: 0,
        verified: 0,
        unavailable: 0,
        not_compared: 0,
        terminals: 0,
        mismatches: 0,
    }
}

fn run_pass(
    packages: &[CorpusPackage],
    pass: Pass,
    hosts: &HostTools,
    resolved: &ResolvedTools<'_>,
    authorities: &AuthorityFactory,
) -> Result<PassObservation, CorpusAuditError> {
    let ordered = ordered_packages(packages, pass);
    let mut publisher = PassPublisher::new(pass, FRAGMENT_BYTES)?;
    let mut slots: Vec<Option<CaseObservation>> = vec![None; PACKAGE_COUNT];
    let mut summary = MatrixSummary::new();
    let mut mismatches = Vec::new();
    let mut failure = None;
    for package in ordered {
        let result = match run_package(package, hosts, resolved, authorities, &mut publisher) {
            Ok(result) => result,
            Err(error) => {
                failure = Some(error);
                break;
            }
        };
        let raw = usize::from(package.case_id.raw());
        if raw >= PACKAGE_COUNT {
            failure = Some(CorpusAuditError::Invariant {
                key: Some(case_key(package)),
                cause: CorpusInvariant::CaseIdentity {
                    ordinal: package.ordinal,
                    observed: package.case_id,
                },
            });
            break;
        }
        if slots[raw].is_some() {
            failure = Some(CorpusAuditError::Invariant {
                key: Some(case_key(package)),
                cause: CorpusInvariant::DuplicateCase,
            });
            break;
        }
        let lane = summary.lane_mut(package);
        lane.attempted = lane.attempted.saturating_add(1);
        match &result.observation {
            CaseObservation::Output(_) => lane.output = lane.output.saturating_add(1),
            CaseObservation::LocallyUnavailable { .. } => {
                lane.unavailable = lane.unavailable.saturating_add(1)
            }
            CaseObservation::Terminal { .. } => {}
        }
        lane.mismatches = lane
            .mismatches
            .saturating_add(u16::try_from(result.mismatches.len()).unwrap_or(u16::MAX));
        slots[raw] = Some(result.observation);
        mismatches.extend(result.mismatches.iter().cloned());
    }
    let shutdown = publisher.finish();
    if let Some(error) = failure {
        let _ = shutdown;
        return Err(error);
    }
    shutdown?;
    if slots.iter().any(Option::is_none) {
        return Err(CorpusAuditError::Invariant {
            key: None,
            cause: CorpusInvariant::MissingCase,
        });
    }
    let cases = match slots.into_iter().collect::<Option<Vec<_>>>() {
        Some(cases) => match cases.into_boxed_slice().try_into() {
            Ok(cases) => cases,
            Err(_) => {
                return Err(CorpusAuditError::Invariant {
                    key: None,
                    cause: CorpusInvariant::PackageCount {
                        observed: PACKAGE_COUNT,
                        expected: PACKAGE_COUNT,
                    },
                });
            }
        },
        None => {
            return Err(CorpusAuditError::Invariant {
                key: None,
                cause: CorpusInvariant::MissingCase,
            });
        }
    };
    Ok(PassObservation {
        cases,
        summary,
        mismatches: mismatches.into_boxed_slice(),
    })
}

/// Proves the fleet manifest selection is seeded and deterministic: the same
/// seed produces byte-identical selections and a different seed does not. Both
/// invocations run in-process against the committed manifest, so this is a
/// genuine reproducibility check rather than a recorded constant.
#[test]
fn fleet_selection_is_seeded_and_reproducible() {
    use inventory::fleet_manifest::{
        FLEET_MANIFEST_VERSION, FLEET_SELECTION_SEED, manifest_lane_counts, manifest_origin_counts,
        selection_bytes_for_seed,
    };

    let first = selection_bytes_for_seed(FLEET_SELECTION_SEED);
    let second = selection_bytes_for_seed(FLEET_SELECTION_SEED);
    assert_eq!(
        first, second,
        "the same seed must select the same rows in the same order"
    );
    assert_eq!(
        first.iter().filter(|byte| **byte == b'\n').count(),
        REAL_PACKAGE_COUNT,
        "the selection must contain exactly one row per newline"
    );

    let alternate_seed = FLEET_SELECTION_SEED ^ 0x00C0_FFEE_0000_0001;
    let alternate = selection_bytes_for_seed(alternate_seed);
    assert_ne!(
        first, alternate,
        "a different seed must not reproduce the default selection"
    );

    let fingerprint = first.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    });

    eprintln!(
        "fleet-selection version={} seed={:#x} alternate_seed={:#x} rows={} bytes={} fingerprint={fingerprint:#018x}",
        FLEET_MANIFEST_VERSION,
        FLEET_SELECTION_SEED,
        alternate_seed,
        REAL_PACKAGE_COUNT,
        first.len(),
    );
    eprintln!(
        "fleet-selection lane_candidates={:?} origin_candidates(locked,committed,host)={:?}",
        manifest_lane_counts(),
        manifest_origin_counts(),
    );
}
