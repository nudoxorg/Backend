//! Supplied Python checker authority boundary proofs on the fragment path.
//!
//! Every falsifier hand-constructs a `CheckerReport` (no pyrefly/uvx) and
//! pins the extractor's exact spans: the `target` call in `caller` owns
//! span 109..115, the `osp` binding call in `foreign` owns span 152..155,
//! and the unannotated parameter `p` of `site` owns name span 9..10.

use std::{
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use compiler_driver::{
    CompileControl, CompileFailure, CompileOutput, CompileRequest, CompileScratch, NativeTool,
    ResolvedToolchain, SemanticAuthorityInput, ToolchainSelection, compile, compile_ir,
};
use compiler_ir::{
    Confidence, DecodedOccurrence, EntityKind, FragmentView, LanguageExtensionWireFact,
    OccurrenceConfidence, OccurrenceTarget, PythonFacts,
};
use compiler_languages_python::{
    CheckerReport, Inference, InferenceSite, InferredType, Span, SymbolOutcome, SymbolResolution,
};
use compiler_vocabulary::{LanguageProfile, PythonVersion, Stage, TypeScriptSource};
use thiserror::Error;

/// The card fixture. The import law fires on a call whose target is the
/// import binding itself (`osp(...)`): the extractor records attribute
/// calls only when the attribute spelling is a module name, so
/// `osp.join("x")` records no occurrence and no report can upgrade it.
const SOURCE: &[u8] = br#"import typing
from os import path as osp

def target() -> int:
    return 1

def caller() -> int:
    return target()

def foreign() -> int:
    return osp("x")
"#;

const PARAM_SOURCE: &[u8] = b"def site(p): return p\n";

/// Exact extractor spans pinned for this fixture before writing the laws.
const TARGET_CALL_SPAN: Span = Span {
    start: 109,
    end: 115,
};
const OSP_CALL_SPAN: Span = Span {
    start: 152,
    end: 155,
};
const PARAMETER_NAME_SPAN: Span = Span { start: 9, end: 10 };

/// The debug-build Python lowering runs within a few kilobytes of the
/// libtest thread default, so every falsifier executes its compile on an
/// explicitly sized worker instead of depending on ambient stack budgets.
const WORKER_STACK_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Error)]
enum TestError {
    #[error("toolchain resolution failed")]
    Resolve,
    #[error("compile failed: {0}")]
    Compile(&'static str),
    #[error("fragment validation failed")]
    Validate,
    #[error("decode failed: {0}")]
    Decode(&'static str),
    #[error("entity {0:?} was not found")]
    MissingEntity(String),
    #[error("lane falsifier: {0}")]
    Falsified(&'static str),
    #[error("deep-stack worker failed")]
    Worker,
}

fn failure_label(failure: &CompileFailure<'_>) -> &'static str {
    match failure {
        CompileFailure::Authority { .. } => "authority",
        CompileFailure::AuthorityInputRequired { .. } => "authority-input-required",
        CompileFailure::AuthorityInputProfileMismatch { .. } => "authority-profile-mismatch",
        CompileFailure::LoweringUnsupported { .. } => "lowering-unsupported",
        CompileFailure::ExtensionAtomUnbound { .. } => "extension-atom-unbound",
        CompileFailure::FactRejected { .. } => "fact-rejected",
        CompileFailure::Build { .. } => "build",
        CompileFailure::Prepare { .. } => "prepare",
        CompileFailure::Write { .. } => "write",
        CompileFailure::Validate { .. } => "validate",
        CompileFailure::SourceLength { .. } => "source-length",
        CompileFailure::UnsupportedStage { .. } => "unsupported-stage",
        CompileFailure::ToolchainSelectionMismatch { .. } => "toolchain-selection-mismatch",
        CompileFailure::ToolchainMismatch { .. } => "toolchain-mismatch",
        CompileFailure::NativeWork { .. } => "native-work",
        CompileFailure::NativeWorkCleanup { .. } => "native-work-cleanup",
        CompileFailure::ToolingUnavailable { .. } => "tooling-unavailable",
        CompileFailure::ToolStart { .. } => "tool-start",
        CompileFailure::MissingToolInput { .. } => "missing-tool-input",
        CompileFailure::MissingToolInputCleanup { .. } => "missing-tool-input-cleanup",
        CompileFailure::MissingToolDiagnostic { .. } => "missing-tool-diagnostic",
        CompileFailure::MissingToolDiagnosticCleanup { .. } => "missing-tool-diagnostic-cleanup",
        CompileFailure::ToolInput { .. } => "tool-input",
        CompileFailure::ToolInputCleanup { .. } => "tool-input-cleanup",
        CompileFailure::ToolTerminate { .. } => "tool-terminate",
        CompileFailure::ToolWait { .. } => "tool-wait",
        CompileFailure::ToolWaitCleanup { .. } => "tool-wait-cleanup",
        CompileFailure::ToolDiagnosticRead { .. } => "tool-diagnostic-read",
        CompileFailure::ToolDiagnosticReadCleanup { .. } => "tool-diagnostic-read-cleanup",
        CompileFailure::NativeWorkerPanic { .. } => "native-worker-panic",
        CompileFailure::Cancelled { .. } => "cancelled",
        CompileFailure::DeadlineExceeded { .. } => "deadline-exceeded",
        CompileFailure::DiagnosticLimit { .. } => "diagnostic-limit",
        CompileFailure::NativeRejected { .. } => "native-rejected",
    }
}

fn with_deep_stack<Decoded>(
    work: impl FnOnce() -> Result<Decoded, TestError> + Send,
) -> Result<Decoded, TestError>
where
    Decoded: Send,
{
    std::thread::scope(|scope| {
        let worker = std::thread::Builder::new()
            .stack_size(WORKER_STACK_BYTES)
            .spawn_scoped(scope, work)
            .map_err(|_| TestError::Worker)?;
        worker.join().map_err(|_| TestError::Worker)?
    })
}

fn python_toolchain() -> Result<ResolvedToolchain<'static>, TestError> {
    ResolvedToolchain::from_version(
        NativeTool::Python,
        Path::new("/bin/true"),
        b"python-authority-test",
    )
    .map_err(|_| TestError::Resolve)
}

fn typescript_toolchain() -> Result<ResolvedToolchain<'static>, TestError> {
    ResolvedToolchain::from_version(
        NativeTool::TypeScriptCompiler,
        Path::new("/bin/true"),
        b"python-authority-test",
    )
    .map_err(|_| TestError::Resolve)
}

fn compile_fragment<'source>(
    source: &'source [u8],
    authority: SemanticAuthorityInput<'source>,
) -> Result<Vec<u8>, TestError> {
    let toolchain = python_toolchain()?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0_u8; 4096];
    let mut output = vec![0_u8; 8 * 1024 * 1024];
    let compiled = compile(
        CompileRequest {
            profile: LanguageProfile::Python(PythonVersion::Python314),
            stage: Stage::LowerIr,
            source,
            toolchain: ToolchainSelection::ResolvedNative(toolchain),
            authority,
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(30),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: Path::new("/tmp"),
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    )
    .map_err(|failure| TestError::Compile(failure_label(&failure)))?;
    Ok(compiled.fragment.as_ref().to_vec())
}

fn empty_report() -> CheckerReport {
    CheckerReport {
        inferences: Box::from([]),
        imports: Box::from([]),
        symbols: Box::from([]),
    }
}

fn report_with_symbols(symbols: Box<[SymbolResolution]>) -> CheckerReport {
    CheckerReport {
        inferences: Box::from([]),
        imports: Box::from([]),
        symbols,
    }
}

fn decoded_occurrences(
    bytes: &[u8],
) -> Result<(FragmentView<'_>, Vec<DecodedOccurrence<'_>>), TestError> {
    let fragment = FragmentView::validate(bytes).map_err(|_| TestError::Validate)?;
    let mut occurrences = Vec::new();
    if let Some(mut cursor) = fragment.occurrences() {
        for row in cursor.by_ref() {
            occurrences.push(row.map_err(|_| TestError::Decode("occurrence"))?);
        }
    }
    Ok((fragment, occurrences))
}

fn entity_position(
    fragment: &FragmentView<'_>,
    name: &[u8],
    kind: EntityKind,
) -> Result<usize, TestError> {
    let atoms: Vec<&[u8]> = fragment.atoms().map(|atom| atom.bytes).collect();
    fragment
        .entities()
        .enumerate()
        .find(|(_, row)| {
            let named = usize::try_from(row.name.raw)
                .ok()
                .and_then(|ordinal| atoms.get(ordinal).copied())
                == Some(name);
            named && row.kind == kind
        })
        .map(|(position, _)| position)
        .ok_or_else(|| TestError::MissingEntity(String::from_utf8_lossy(name).into_owned()))
}

/// Falsifier 1: the supplied report binds the `target` call at its exact
/// span to a local declaration, and the decoded occurrence carries the
/// oracle tier the index lane alone can never produce. The fragment bytes
/// for one (source, report) pair are pinned deterministic.
#[test]
fn supplied_local_resolution_upgrades_the_call_to_oracle() -> Result<(), TestError> {
    with_deep_stack(|| {
        let report = report_with_symbols(Box::from([SymbolResolution {
            target: "target".to_owned(),
            span: TARGET_CALL_SPAN,
            outcome: SymbolOutcome::Local,
        }]));
        let bytes = compile_fragment(SOURCE, SemanticAuthorityInput::Python { report: &report })?;
        let again = compile_fragment(SOURCE, SemanticAuthorityInput::Python { report: &report })?;
        if bytes != again {
            return Err(TestError::Falsified("fragment bytes are not deterministic"));
        }
        let (fragment, occurrences) = decoded_occurrences(&bytes)?;
        let target_row = entity_position(&fragment, b"target", EntityKind::Function)?;
        let upgraded = occurrences.iter().any(|fact| {
            fact.occurrence.confidence == OccurrenceConfidence::Oracle
                && matches!(fact.occurrence.target,
                    OccurrenceTarget::Local(local) if local.raw as usize == target_row)
        });
        if upgraded {
            Ok(())
        } else {
            Err(TestError::Falsified(
                "the exact-span local resolution did not decode as an Oracle occurrence",
            ))
        }
    })
}

/// Falsifier 2: the supplied report binds the `osp` binding call at its
/// exact span to a resolved import, and the decoded occurrence carries the
/// import tier over a foreign target.
#[test]
fn supplied_foreign_resolution_upgrades_the_call_to_import() -> Result<(), TestError> {
    with_deep_stack(|| {
        let report = report_with_symbols(Box::from([SymbolResolution {
            target: "osp".to_owned(),
            span: OSP_CALL_SPAN,
            outcome: SymbolOutcome::Foreign {
                module: "os.path".to_owned(),
            },
        }]));
        let bytes = compile_fragment(SOURCE, SemanticAuthorityInput::Python { report: &report })?;
        let (_, occurrences) = decoded_occurrences(&bytes)?;
        let upgraded = occurrences.iter().any(|fact| {
            fact.occurrence.confidence == OccurrenceConfidence::Import
                && matches!(fact.occurrence.target, OccurrenceTarget::Foreign(_))
        });
        if upgraded {
            Ok(())
        } else {
            Err(TestError::Falsified(
                "the exact-span foreign resolution did not decode as an Import occurrence",
            ))
        }
    })
}

/// Falsifier 3: `Unresolved` outcomes at the same exact spans upgrade
/// nothing — every occurrence keeps its index tier and every proven syntax
/// target keeps its shape.
#[test]
fn unresolved_outcomes_keep_every_proven_syntax_tier() -> Result<(), TestError> {
    with_deep_stack(|| {
        let report = report_with_symbols(Box::from([
            SymbolResolution {
                target: "target".to_owned(),
                span: TARGET_CALL_SPAN,
                outcome: SymbolOutcome::Unresolved,
            },
            SymbolResolution {
                target: "osp".to_owned(),
                span: OSP_CALL_SPAN,
                outcome: SymbolOutcome::Unresolved,
            },
        ]));
        let bytes = compile_fragment(SOURCE, SemanticAuthorityInput::Python { report: &report })?;
        let (fragment, occurrences) = decoded_occurrences(&bytes)?;
        let target_row = entity_position(&fragment, b"target", EntityKind::Function)?;
        if occurrences.is_empty() {
            return Err(TestError::Falsified("no occurrences decoded"));
        }
        for fact in &occurrences {
            if fact.occurrence.confidence != OccurrenceConfidence::Index {
                return Err(TestError::Falsified(
                    "an unresolved outcome weakened or lifted a proven syntax tier",
                ));
            }
        }
        let local_target_intact = occurrences.iter().any(|fact| {
            matches!(fact.occurrence.target,
                OccurrenceTarget::Local(local) if local.raw as usize == target_row)
        });
        let foreign_target_intact = occurrences
            .iter()
            .any(|fact| matches!(fact.occurrence.target, OccurrenceTarget::Foreign(_)));
        if local_target_intact && foreign_target_intact {
            Ok(())
        } else {
            Err(TestError::Falsified(
                "unresolved outcomes changed the resolved syntax targets",
            ))
        }
    })
}

/// Falsifier 4: a report whose symbol spans belong to a different module
/// upgrades nothing — authority applies only at exact spans.
#[test]
fn foreign_module_spans_upgrade_nothing() -> Result<(), TestError> {
    with_deep_stack(|| {
        const SHIFT: u32 = 1_000;
        let shift = |span: Span| Span {
            start: span.start + SHIFT,
            end: span.end + SHIFT,
        };
        let report = report_with_symbols(Box::from([
            SymbolResolution {
                target: "target".to_owned(),
                span: shift(TARGET_CALL_SPAN),
                outcome: SymbolOutcome::Local,
            },
            SymbolResolution {
                target: "osp".to_owned(),
                span: shift(OSP_CALL_SPAN),
                outcome: SymbolOutcome::Foreign {
                    module: "os.path".to_owned(),
                },
            },
        ]));
        let bytes = compile_fragment(SOURCE, SemanticAuthorityInput::Python { report: &report })?;
        let (_, occurrences) = decoded_occurrences(&bytes)?;
        if occurrences.is_empty() {
            return Err(TestError::Falsified("no occurrences decoded"));
        }
        for fact in &occurrences {
            if fact.occurrence.confidence != OccurrenceConfidence::Index {
                return Err(TestError::Falsified(
                    "an off-span authority cell upgraded an occurrence",
                ));
            }
        }
        Ok(())
    })
}

/// Falsifier 5: the supplied inference at the unannotated parameter's
/// exact name span lifts the parameter's Python extension row to compiler
/// confidence (the index-only lane keeps it syntactic). The cell is the
/// fragment's Python plane: a 16-byte section header, one 20-byte directory
/// entry per plane (Python is the fifth), then the row table and the dense
/// 12-byte fact pool.
#[test]
fn supplied_inference_lifts_the_parameter_to_compiler_confidence() -> Result<(), TestError> {
    with_deep_stack(|| {
        let report = CheckerReport {
            inferences: Box::from([Inference {
                site: PARAMETER_NAME_SPAN,
                kind: InferenceSite::Parameter,
                observed: InferredType::Integer,
            }]),
            imports: Box::from([]),
            symbols: Box::from([]),
        };
        let bytes = compile_fragment(
            PARAM_SOURCE,
            SemanticAuthorityInput::Python { report: &report },
        )?;
        let (fragment, _) = decoded_occurrences(&bytes)?;
        let parameter_row = entity_position(&fragment, b"p", EntityKind::Parameter)?;
        let payload = fragment
            .language_extension_payload()
            .ok_or(TestError::Decode("extension section"))?;
        const PYTHON_DIRECTORY: usize = 16 + 4 * 20;
        let word = |payload: &[u8], offset: usize| -> Result<u32, TestError> {
            let cell = payload
                .get(offset..offset + 4)
                .ok_or(TestError::Decode("extension word"))?;
            let raw: [u8; 4] = cell
                .try_into()
                .map_err(|_| TestError::Decode("extension word"))?;
            Ok(u32::from_le_bytes(raw))
        };
        let absent_row = |payload: &[u8], row: usize| -> Result<Option<PythonFacts>, TestError> {
            let rows = usize::try_from(word(payload, PYTHON_DIRECTORY + 4)?)
                .map_err(|_| TestError::Decode("extension rows"))?;
            let pool = word(payload, PYTHON_DIRECTORY + 8)?;
            let offset = usize::try_from(word(payload, PYTHON_DIRECTORY + 12)?)
                .map_err(|_| TestError::Decode("extension offset"))?;
            if pool == 0 {
                return Err(TestError::Decode("empty python plane"));
            }
            let ordinal = word(payload, offset + row * 4)?;
            if ordinal == compiler_ir::SECTION_NONE {
                return Ok(None);
            }
            let fact = usize::try_from(ordinal)
                .map_err(|_| TestError::Decode("extension ordinal"))?
                .checked_mul(PythonFacts::WIDTH)
                .and_then(|width| (offset + rows * 4).checked_add(width))
                .ok_or(TestError::Decode("extension offset"))?;
            PythonFacts::decode(payload, fact)
                .map(Some)
                .ok_or(TestError::Decode("python extension fact"))
        };
        let facts = absent_row(payload, parameter_row)?.ok_or(TestError::Falsified(
            "the parameter carries no Python extension row",
        ))?;
        if facts.dynamic_confidence == Confidence::Compiler {
            Ok(())
        } else {
            Err(TestError::Falsified(
                "the supplied inference did not lift the parameter to compiler confidence",
            ))
        }
    })
}

/// Falsifier 6: a Python authority bound to a TypeScript profile is
/// rejected before lowering with the exact profile-mismatch terminal.
#[test]
fn python_authority_rejects_a_non_python_profile() -> Result<(), TestError> {
    with_deep_stack(|| {
        let report = empty_report();
        let toolchain = typescript_toolchain()?;
        let cancelled = AtomicBool::new(false);
        let mut diagnostic = [0_u8; 4096];
        let outcome = compile_ir(
            CompileRequest {
                profile: LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
                stage: Stage::LowerIr,
                source: SOURCE,
                toolchain: ToolchainSelection::ResolvedNative(toolchain),
                authority: SemanticAuthorityInput::Python { report: &report },
                control: CompileControl {
                    deadline: Instant::now() + Duration::from_secs(30),
                    cancelled: &cancelled,
                },
            },
            CompileScratch {
                diagnostic_output: &mut diagnostic,
                native_work: Path::new("/tmp"),
            },
        );
        let failure = match outcome {
            Ok(_) => {
                return Err(TestError::Falsified(
                    "a Python authority must not bind to a TypeScript profile",
                ));
            }
            Err(failure) => failure,
        };
        if failure_label(&failure) == "authority-profile-mismatch" {
            Ok(())
        } else {
            Err(TestError::Falsified(
                "the cross-profile rejection lost its exact terminal label",
            ))
        }
    })
}

/// Falsifier 7: `None` authority still compiles the fixture — the
/// env-checker arm stays the untouched backwards-compatible path.
#[test]
fn none_authority_still_compiles_the_fragment() -> Result<(), TestError> {
    with_deep_stack(|| {
        let bytes = compile_fragment(SOURCE, SemanticAuthorityInput::None)?;
        let (fragment, occurrences) = decoded_occurrences(&bytes)?;
        entity_position(&fragment, b"target", EntityKind::Function)?;
        entity_position(&fragment, b"foreign", EntityKind::Function)?;
        if occurrences.is_empty() {
            return Err(TestError::Falsified(
                "the none-authority fragment lost its occurrence plane",
            ));
        }
        Ok(())
    })
}
