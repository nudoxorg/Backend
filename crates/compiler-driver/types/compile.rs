//! Defines types compile behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the types compile invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use backend_semantic::ir::{FragmentView, PrepareError};
use backend_semantic::registry::{AdapterRoute, FullRegistry};
use backend_semantic::vocabulary::{Language, LanguageProfile, Stage};

use crate::lower::{self, AdmissionFault, typescript::TypeScriptCollectError};

use super::{
    AuthorityDiagnostic, AuthorityDiagnosticFault, AuthorityFailure, CompileFailure, CompileOutput,
    CompileRecipeFact, CompileRequest, CompileScratch, CompiledFragment, CompiledSemantic,
    FactRejection, NativeDiagnostic, ResolvedToolchain, SemanticAuthorityInput, SourceIdentity,
    SourceLease, ToolchainSelection, ToolchainSelectionFact, WorkPermit, WorkStopped,
};

/// Projects the driver's complete admission snapshot into the one portable
/// lowering terminal.  The collector arena never escapes this boundary, but
/// every source operand does.
fn rejected_lowering(rejected: FactRejection) -> backend_semantic::vocabulary::LoweringUnsupported {
    backend_semantic::vocabulary::LoweringUnsupported::FactRejected {
        fact: lower::portable_count(rejected.fact),
        name_len: lower::portable_count(rejected.name_len),
        cause: lower::portable_admission(rejected.cause),
    }
}

/// Compiles the compact fragment compatibility projection.
///
/// This is deliberately a compact-only view: a later [`compile_ir`] call
/// repeats authority traversal, so two independent compatibility calls do
/// not prove that their fragment and owned image were fused from one fact
/// transaction. Use [`compile_semantic`] when that coherence is required.
pub fn compile<'source, 'toolchain, 'cancel, 'diagnostic, 'work, 'output>(
    request: CompileRequest<'source, 'toolchain, 'cancel>,
    scratch: CompileScratch<'diagnostic, 'work>,
    output: CompileOutput<'output>,
) -> Result<CompiledFragment<'output>, CompileFailure<'diagnostic>> {
    let transaction = collect_transaction(request, scratch)?;
    write_fragment(
        transaction.source,
        transaction.recipe,
        &transaction.facts,
        transaction.permit,
        output.fragment_output,
    )
}

/// Compiles the one coherent public semantic result.
///
/// Authority entry and lowering run exactly once. The owned [`backend_semantic::ir::Ir`]
/// and its IR-owned authority facts are built from that one fact lane before
/// the compact artifact is written and validated, so a failure returns no
/// partial result.
/// The returned value borrows only `output.fragment_output`; source bytes,
/// authority input, scratch storage, cancellation control, and deadline do
/// not escape. A retained-permit checkpoint runs after owned `Ir`
/// materialization but before fragment admission mutates caller output; the
/// writer repeats that gate at its admission linearization point. Cancellation
/// or deadline observed there returns the exact borrowed diagnostic terminal
/// and no fragment or IR; once synchronous admission has begun, a later
/// observation is not retroactive.
pub fn compile_semantic<'source, 'toolchain, 'cancel, 'diagnostic, 'work, 'output>(
    request: CompileRequest<'source, 'toolchain, 'cancel>,
    scratch: CompileScratch<'diagnostic, 'work>,
    output: CompileOutput<'output>,
) -> Result<CompiledSemantic<'output>, CompileFailure<'diagnostic>> {
    let transaction = collect_transaction(request, scratch)?;
    let ir = transaction
        .facts
        .build_ir(
            request.profile,
            transaction.source,
            transaction.recipe,
            request.declaration_scope,
        )
        .map_err(|cause| CompileFailure::Build {
            source_identity: transaction.source,
            recipe: transaction.recipe,
            cause,
        })?;
    // This observes the retained permit immediately after owned truth exists
    // and before the fragment writer can mutate the caller's lease. The
    // writer repeats the gate as its admission linearization point.
    checkpoint(transaction.permit, transaction.recipe)?;
    let artifact = write_fragment(
        transaction.source,
        transaction.recipe,
        &transaction.facts,
        transaction.permit,
        output.fragment_output,
    )?;
    Ok(CompiledSemantic { artifact, ir })
}

/// Materializes a queryable semantic image compatibility projection.
///
/// This owned-only view does not validate a compact artifact. A separate
/// [`compile`] call repeats authority traversal and therefore cannot prove
/// cross-call coherence; use [`compile_semantic`] for the fused parity result.
/// Its retained permit is checked after owned materialization and before this
/// compatibility result becomes visible.
pub fn compile_ir<'source, 'toolchain, 'cancel, 'diagnostic, 'work>(
    request: CompileRequest<'source, 'toolchain, 'cancel>,
    scratch: CompileScratch<'diagnostic, 'work>,
) -> Result<super::CompiledIr, CompileFailure<'diagnostic>> {
    let transaction = collect_transaction(request, scratch)?;
    let ir = transaction
        .facts
        .build_ir(
            request.profile,
            transaction.source,
            transaction.recipe,
            request.declaration_scope,
        )
        .map_err(|cause| CompileFailure::Build {
            source_identity: transaction.source,
            recipe: transaction.recipe,
            cause,
        })?;
    checkpoint(transaction.permit, transaction.recipe)?;
    Ok(super::CompiledIr { ir })
}

/// One private authority-to-facts transaction shared by every public result
/// projection. Its fact lane may borrow entered source and authority input,
/// but successful callers immediately materialize an owned IR and/or the
/// caller output fragment, so no such staging borrow crosses the public
/// boundary.
struct CollectedFacts<'source, 'cancel> {
    source: SourceIdentity,
    recipe: CompileRecipeFact,
    /// Retained through materialization to the final pre-write checkpoint.
    permit: WorkPermit<'cancel>,
    facts: lower::FactSet<'source>,
}

fn collect_transaction<'source, 'toolchain, 'cancel, 'diagnostic, 'work>(
    request: CompileRequest<'source, 'toolchain, 'cancel>,
    scratch: CompileScratch<'diagnostic, 'work>,
) -> Result<CollectedFacts<'source, 'cancel>, CompileFailure<'diagnostic>> {
    let prepared = prepare(request)?;
    let mut facts = lower::FactSet::with_primary_source(
        lower::ResourcePlan::for_source(request.profile, request.source.len()),
        u32::try_from(request.source.len()).unwrap_or(u32::MAX),
    );
    emit_facts(&prepared, Some(scratch.diagnostic_output), &mut facts)?;
    Ok(CollectedFacts {
        source: prepared.source,
        recipe: prepared.recipe,
        permit: prepared.permit,
        facts,
    })
}

/// Shared output linearization point. A stopped permit observed before
/// `lower::admit` begins leaves caller output unchanged. Admission is
/// synchronous, so a later cancellation or deadline cannot retract it.
fn write_fragment<'source, 'cancel, 'diagnostic, 'output>(
    source: SourceIdentity,
    recipe: CompileRecipeFact,
    facts: &lower::FactSet<'source>,
    permit: WorkPermit<'cancel>,
    output: &'output mut [u8],
) -> Result<CompiledFragment<'output>, CompileFailure<'diagnostic>> {
    checkpoint(permit, recipe)?;
    let bytes =
        lower::admit(facts, source, recipe, recipe.profile, output).map_err(
            |fault| match fault {
                AdmissionFault::Canonical(cause) => CompileFailure::Prepare {
                    source_identity: source,
                    recipe,
                    cause: PrepareError::SemanticData { cause },
                },
                AdmissionFault::Prepare(cause) => CompileFailure::Prepare {
                    source_identity: source,
                    recipe,
                    cause,
                },
                AdmissionFault::Write(cause) => CompileFailure::Write {
                    source_identity: source,
                    recipe,
                    cause,
                },
                AdmissionFault::ExtensionAtom {
                    row,
                    provisional,
                    atom_count,
                } => CompileFailure::ExtensionAtomUnbound {
                    source_identity: source,
                    recipe,
                    row,
                    provisional,
                    atom_count,
                },
                AdmissionFault::ExtensionTypeParameters {
                    row,
                    start,
                    length,
                    element_count,
                } => CompileFailure::ExtensionTypeParametersUnbound {
                    source_identity: source,
                    recipe,
                    row,
                    start,
                    length,
                    element_count,
                },
            },
        )?;
    let fragment = FragmentView::validate(bytes).map_err(|cause| CompileFailure::Validate {
        source_identity: source,
        recipe,
        cause,
    })?;
    Ok(CompiledFragment {
        source,
        recipe,
        fragment,
    })
}

struct PreparedCompile<'source, 'cancel> {
    source: SourceIdentity,
    recipe: CompileRecipeFact,
    lease: SourceLease<'source>,
    permit: WorkPermit<'cancel>,
    authority: EnteredAuthority<'source>,
}

/// Closed authority after source entry.  Profile/input mismatches are
/// rejected before this value exists, so lowerers receive only their one
/// meaningful native or checked authority shape.
enum EnteredAuthority<'source> {
    Clang {
        profile: LanguageProfile,
    },
    TypeScript {
        profile: backend_semantic::vocabulary::TypeScriptSource,
        report: Option<&'source backend_frontend_typescript::legacy::Report>,
    },
    Python {
        profile: backend_semantic::vocabulary::PythonVersion,
        report: Option<&'source backend_frontend_python::legacy::CheckerReport>,
    },
    Rust {
        profile: backend_semantic::vocabulary::RustEdition,
        project: &'source backend_frontend_rust::legacy::RustProject,
        maximum_source_bytes: backend_frontend_rust::legacy::SourceByteLimit,
        features: backend_frontend_rust::legacy::RustFeatureControl<'source>,
    },
    Go {
        profile: backend_semantic::vocabulary::GoVersion,
        image: &'source [u8],
    },
    Java {
        profile: backend_semantic::vocabulary::JavaRelease,
        image: &'source [u8],
    },
    CSharp {
        profile: backend_semantic::vocabulary::CSharpVersion,
        image: &'source [u8],
    },
}

/// The one post-entry language contract.  `EnteredAuthority` is the only
/// closed dynamic dispatch; each arm below immediately selects one static
/// implementation with its real authority shape and extension fact type.
/// Lowerers therefore cannot receive a profile/input mismatch or inspect a
/// foreign language's authority bytes.
mod language_spec_seal {
    pub trait Sealed {}
}

trait LanguageSpec: language_spec_seal::Sealed {
    type Authority<'source>
    where
        Self: 'source;
    type Extension: Copy;

    fn collect<'source, 'cancel, 'diagnostic>(
        authority: &Self::Authority<'source>,
        prepared: &PreparedCompile<'source, 'cancel>,
        diagnostic_output: Option<&'diagnostic mut [u8]>,
        facts: &mut lower::FactSet<'source>,
    ) -> Result<(), CompileFailure<'diagnostic>>;
}

struct ClangSpec;
struct TypeScriptSpec;
struct PythonSpec;
struct RustSpec;
struct GoSpec;
struct JavaSpec;
struct CSharpSpec;

macro_rules! seal_specs {
    ($($spec:ty),+ $(,)?) => { $(impl language_spec_seal::Sealed for $spec {})+ };
}
seal_specs!(
    ClangSpec,
    TypeScriptSpec,
    PythonSpec,
    RustSpec,
    GoSpec,
    JavaSpec,
    CSharpSpec
);

impl LanguageSpec for ClangSpec {
    type Authority<'source> = LanguageProfile;
    type Extension = backend_semantic::ir::ClangFacts;

    fn collect<'source, 'cancel, 'diagnostic>(
        profile: &Self::Authority<'source>,
        prepared: &PreparedCompile<'source, 'cancel>,
        _: Option<&'diagnostic mut [u8]>,
        facts: &mut lower::FactSet<'source>,
    ) -> Result<(), CompileFailure<'diagnostic>> {
        lower::clang::collect(
            *profile,
            prepared.lease.bytes(),
            prepared.permit.cancelled(),
            facts,
        )
        .map_err(|cause| clang_terminal(prepared.source, prepared.recipe, cause))
    }
}

impl LanguageSpec for TypeScriptSpec {
    type Authority<'source> = (
        backend_semantic::vocabulary::TypeScriptSource,
        Option<&'source backend_frontend_typescript::legacy::Report>,
    );
    type Extension = backend_semantic::ir::TypeScriptFacts;

    fn collect<'source, 'cancel, 'diagnostic>(
        authority: &Self::Authority<'source>,
        prepared: &PreparedCompile<'source, 'cancel>,
        diagnostic_output: Option<&'diagnostic mut [u8]>,
        facts: &mut lower::FactSet<'source>,
    ) -> Result<(), CompileFailure<'diagnostic>> {
        let source = prepared.lease.bytes();
        let owned = if authority.1.is_none() {
            Some(
                backend_frontend_typescript::legacy::Checker::default()
                    .run(authority.0, source)
                    .map_err(|cause| {
                        typescript_terminal(
                            None,
                            source,
                            prepared.source,
                            prepared.recipe,
                            TypeScriptCollectError::Authority(
                                backend_frontend_typescript::legacy::AuthorityError::Checker { cause },
                            ),
                        )
                    })?,
            )
        } else {
            None
        };
        lower::typescript::collect_with_checker(
            authority.0,
            source,
            authority.1.or(owned.as_ref()),
            facts,
        )
        .map_err(|cause| {
            typescript_terminal(
                diagnostic_output,
                source,
                prepared.source,
                prepared.recipe,
                cause,
            )
        })
    }
}

impl LanguageSpec for PythonSpec {
    type Authority<'source> = (
        backend_semantic::vocabulary::PythonVersion,
        Option<&'source backend_frontend_python::legacy::CheckerReport>,
    );
    type Extension = backend_semantic::ir::PythonFacts;

    fn collect<'source, 'cancel, 'diagnostic>(
        authority: &Self::Authority<'source>,
        prepared: &PreparedCompile<'source, 'cancel>,
        _: Option<&'diagnostic mut [u8]>,
        facts: &mut lower::FactSet<'source>,
    ) -> Result<(), CompileFailure<'diagnostic>> {
        let source = prepared.lease.bytes();
        match authority.1 {
            None => lower::python::collect(authority.0, source, facts)
                .map_err(|cause| python_terminal(prepared.source, prepared.recipe, cause)),
            Some(report) => {
                let module =
                    backend_frontend_python::legacy::extract(source, authority.0).map_err(|cause| {
                        python_terminal(
                            prepared.source,
                            prepared.recipe,
                            lower::python::PythonCollectError::Authority(cause),
                        )
                    })?;
                lower::python::collect_with_checker(&module, source, facts, Some(report))
                    .map_err(|cause| python_terminal(prepared.source, prepared.recipe, cause))
            }
        }
    }
}

impl LanguageSpec for RustSpec {
    type Authority<'source> = (
        &'source backend_frontend_rust::legacy::RustProject,
        backend_frontend_rust::legacy::SourceByteLimit,
        backend_frontend_rust::legacy::RustFeatureControl<'source>,
    );
    type Extension = backend_semantic::ir::RustFacts;

    fn collect<'source, 'cancel, 'diagnostic>(
        authority: &Self::Authority<'source>,
        prepared: &PreparedCompile<'source, 'cancel>,
        _: Option<&'diagnostic mut [u8]>,
        facts: &mut lower::FactSet<'source>,
    ) -> Result<(), CompileFailure<'diagnostic>> {
        lower::rust::collect(
            authority.0,
            authority.1,
            authority.2,
            prepared.permit.control(),
            prepared.lease.bytes(),
            facts,
        )
        .map_err(|cause| rust_terminal(prepared.source, prepared.recipe, cause))
    }
}

impl LanguageSpec for GoSpec {
    type Authority<'source> = &'source [u8];
    type Extension = backend_semantic::ir::GoFacts;

    fn collect<'source, 'cancel, 'diagnostic>(
        image: &Self::Authority<'source>,
        prepared: &PreparedCompile<'source, 'cancel>,
        _: Option<&'diagnostic mut [u8]>,
        facts: &mut lower::FactSet<'source>,
    ) -> Result<(), CompileFailure<'diagnostic>> {
        lower::go::collect(prepared.lease.bytes(), image, facts)
            .map_err(|cause| go_terminal(prepared.source, prepared.recipe, cause))
    }
}

impl LanguageSpec for JavaSpec {
    type Authority<'source> = (backend_semantic::vocabulary::JavaRelease, &'source [u8]);
    type Extension = backend_semantic::ir::JavaFacts;

    fn collect<'source, 'cancel, 'diagnostic>(
        authority: &Self::Authority<'source>,
        prepared: &PreparedCompile<'source, 'cancel>,
        _: Option<&'diagnostic mut [u8]>,
        facts: &mut lower::FactSet<'source>,
    ) -> Result<(), CompileFailure<'diagnostic>> {
        lower::java::collect(authority.0, prepared.lease.bytes(), authority.1, facts)
            .map_err(|cause| java_terminal(prepared.source, prepared.recipe, cause))
    }
}

impl LanguageSpec for CSharpSpec {
    type Authority<'source> = &'source [u8];
    type Extension = backend_semantic::ir::CSharpFacts;

    fn collect<'source, 'cancel, 'diagnostic>(
        image: &Self::Authority<'source>,
        prepared: &PreparedCompile<'source, 'cancel>,
        _: Option<&'diagnostic mut [u8]>,
        facts: &mut lower::FactSet<'source>,
    ) -> Result<(), CompileFailure<'diagnostic>> {
        lower::csharp::collect(prepared.lease.bytes(), image, facts)
            .map_err(|cause| csharp_terminal(prepared.source, prepared.recipe, cause))
    }
}

fn prepare<'source, 'toolchain, 'cancel, 'diagnostic>(
    request: CompileRequest<'source, 'toolchain, 'cancel>,
) -> Result<PreparedCompile<'source, 'cancel>, CompileFailure<'diagnostic>> {
    let lease =
        SourceLease::enter(request.source).map_err(|source| CompileFailure::SourceLength {
            actual: request.source.len(),
            source,
        })?;
    let source = lease.identity();
    let language = Language::from(request.profile);
    let route = FullRegistry
        .route(language, request.stage)
        .map_err(|cause| CompileFailure::UnsupportedStage {
            source_identity: source,
            language,
            stage: request.stage,
            cause,
        })?;
    let selected = match route {
        AdapterRoute::Native { tool } => tool,
        AdapterRoute::ToolingUnavailable { tool } => {
            if request.toolchain.fact() != (ToolchainSelectionFact::ExplicitlyUnavailable { tool })
            {
                return Err(CompileFailure::ToolchainSelectionMismatch {
                    source_identity: source,
                    language,
                    stage: request.stage,
                    selected: tool,
                    provided: request.toolchain.fact(),
                });
            }
            return Err(CompileFailure::ToolingUnavailable {
                source_identity: source,
                language,
                stage: request.stage,
                tool,
            });
        }
    };
    let resolved = match request.toolchain {
        ToolchainSelection::ResolvedNative(resolved) => resolved,
        ToolchainSelection::ExplicitlyUnavailable { .. } => {
            return Err(CompileFailure::ToolchainSelectionMismatch {
                source_identity: source,
                language,
                stage: request.stage,
                selected,
                provided: request.toolchain.fact(),
            });
        }
    };
    if selected != resolved.tool {
        return Err(CompileFailure::ToolchainMismatch {
            source_identity: source,
            language,
            stage: request.stage,
            selected,
            resolved: resolved.tool,
        });
    }
    let recipe = recipe_fact(request.profile, request.stage, resolved, source);
    let authority = enter_authority(request.profile, request.authority, source, recipe)?;
    let permit = WorkPermit::new(source, request.control);
    checkpoint(permit, recipe)?;
    Ok(PreparedCompile {
        source,
        recipe,
        lease,
        permit,
        authority,
    })
}

fn checkpoint<'diagnostic>(
    permit: WorkPermit<'_>,
    recipe: CompileRecipeFact,
) -> Result<(), CompileFailure<'diagnostic>> {
    match permit.checkpoint() {
        Ok(()) => Ok(()),
        Err(WorkStopped::Cancelled) => Err(CompileFailure::Cancelled {
            source_identity: permit.source(),
            recipe,
            diagnostic: NativeDiagnostic {
                bytes: &[],
                observed: 0,
                truncated: false,
            },
        }),
        Err(WorkStopped::Deadline) => Err(CompileFailure::DeadlineExceeded {
            source_identity: permit.source(),
            recipe,
            diagnostic: NativeDiagnostic {
                bytes: &[],
                observed: 0,
                truncated: false,
            },
        }),
    }
}

fn enter_authority<'source, 'diagnostic>(
    profile: LanguageProfile,
    input: SemanticAuthorityInput<'source>,
    source: SourceIdentity,
    recipe: CompileRecipeFact,
) -> Result<EnteredAuthority<'source>, CompileFailure<'diagnostic>> {
    let mismatch = || CompileFailure::AuthorityInputProfileMismatch {
        source_identity: source,
        recipe,
        profile,
    };
    let required = || CompileFailure::AuthorityInputRequired {
        source_identity: source,
        recipe,
        profile,
    };
    match (profile, input) {
        (LanguageProfile::C(profile), SemanticAuthorityInput::None) => {
            Ok(EnteredAuthority::Clang {
                profile: LanguageProfile::C(profile),
            })
        }
        (LanguageProfile::Cxx(profile), SemanticAuthorityInput::None) => {
            Ok(EnteredAuthority::Clang {
                profile: LanguageProfile::Cxx(profile),
            })
        }
        (LanguageProfile::TypeScript(profile), SemanticAuthorityInput::None) => {
            Ok(EnteredAuthority::TypeScript {
                profile,
                report: None,
            })
        }
        (LanguageProfile::TypeScript(profile), SemanticAuthorityInput::TypeScript { report }) => {
            Ok(EnteredAuthority::TypeScript {
                profile,
                report: Some(report),
            })
        }
        (LanguageProfile::Python(profile), SemanticAuthorityInput::None) => {
            Ok(EnteredAuthority::Python {
                profile,
                report: None,
            })
        }
        (LanguageProfile::Python(profile), SemanticAuthorityInput::Python { report }) => {
            Ok(EnteredAuthority::Python {
                profile,
                report: Some(report),
            })
        }
        (
            LanguageProfile::Rust(profile),
            SemanticAuthorityInput::Rust {
                project,
                maximum_source_bytes,
                features,
            },
        ) => Ok(EnteredAuthority::Rust {
            profile,
            project,
            maximum_source_bytes,
            features,
        }),
        (LanguageProfile::Rust(_), SemanticAuthorityInput::None) => Err(required()),
        (LanguageProfile::Go(profile), SemanticAuthorityInput::Go { image }) => {
            Ok(EnteredAuthority::Go { profile, image })
        }
        (LanguageProfile::Go(_), SemanticAuthorityInput::None) => Err(required()),
        (LanguageProfile::Java(profile), SemanticAuthorityInput::Java { image }) => {
            Ok(EnteredAuthority::Java { profile, image })
        }
        (LanguageProfile::Java(_), SemanticAuthorityInput::None) => Err(required()),
        (LanguageProfile::CSharp(profile), SemanticAuthorityInput::CSharp { image }) => {
            Ok(EnteredAuthority::CSharp { profile, image })
        }
        (LanguageProfile::CSharp(_), SemanticAuthorityInput::None) => Err(required()),
        (_, _) => Err(mismatch()),
    }
}

fn recipe_fact(
    profile: LanguageProfile,
    stage: Stage,
    toolchain: ResolvedToolchain<'_>,
    source: SourceIdentity,
) -> CompileRecipeFact {
    CompileRecipeFact::derive(
        profile,
        stage,
        toolchain.tool,
        source.identity,
        toolchain.identity,
    )
}

fn emit_facts<'source, 'cancel, 'diagnostic>(
    prepared: &PreparedCompile<'source, 'cancel>,
    diagnostic_output: Option<&'diagnostic mut [u8]>,
    facts: &mut lower::FactSet<'source>,
) -> Result<(), CompileFailure<'diagnostic>> {
    checkpoint(prepared.permit, prepared.recipe)?;
    match &prepared.authority {
        EnteredAuthority::Clang { profile } => ClangSpec::collect(profile, prepared, None, facts)?,
        EnteredAuthority::TypeScript { profile, report } => {
            TypeScriptSpec::collect(&(*profile, *report), prepared, diagnostic_output, facts)?;
        }
        EnteredAuthority::Python { profile, report } => {
            PythonSpec::collect(&(*profile, *report), prepared, None, facts)?;
        }
        EnteredAuthority::Rust {
            project,
            maximum_source_bytes,
            features,
            ..
        } => RustSpec::collect(
            &(*project, *maximum_source_bytes, *features),
            prepared,
            None,
            facts,
        )?,
        EnteredAuthority::Go { image, .. } => GoSpec::collect(image, prepared, None, facts)?,
        EnteredAuthority::Java { profile, image } => {
            JavaSpec::collect(&(*profile, *image), prepared, None, facts)?;
        }
        EnteredAuthority::CSharp { image, .. } => {
            CSharpSpec::collect(image, prepared, None, facts)?
        }
    }
    checkpoint(prepared.permit, prepared.recipe)?;
    require_facts(prepared.source, prepared.recipe, facts)
}

fn require_facts<'source, 'diagnostic>(
    source: SourceIdentity,
    recipe: CompileRecipeFact,
    facts: &lower::FactSet<'source>,
) -> Result<(), CompileFailure<'diagnostic>> {
    if facts.len() == 0 {
        return Err(CompileFailure::LoweringUnsupported {
            source_identity: source,
            recipe,
            cause: backend_semantic::vocabulary::LoweringUnsupported::NoSupportedDeclaration,
        });
    }
    Ok(())
}

fn python_terminal<'diagnostic>(
    source_identity: SourceIdentity,
    recipe: CompileRecipeFact,
    cause: lower::python::PythonCollectError,
) -> CompileFailure<'diagnostic> {
    match cause {
        lower::python::PythonCollectError::Authority(cause) => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::Python {
                diagnostic: AuthorityDiagnostic::absent(),
                cause,
            },
        },
        lower::python::PythonCollectError::Checker(cause) => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::PythonChecker {
                diagnostic: AuthorityDiagnostic::absent(),
                cause,
            },
        },
        lower::python::PythonCollectError::Rejected(rejected) => {
            CompileFailure::LoweringUnsupported {
                source_identity,
                recipe,
                cause: rejected_lowering(rejected),
            }
        }
        lower::python::PythonCollectError::Projection(fault) => {
            CompileFailure::LoweringUnsupported {
                source_identity,
                recipe,
                cause: backend_semantic::vocabulary::LoweringUnsupported::PythonProjection { fault },
            }
        }
        lower::python::PythonCollectError::Lowering(cause) => CompileFailure::LoweringUnsupported {
            source_identity,
            recipe,
            cause,
        },
        lower::python::PythonCollectError::Span { start, end } => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::PythonSpan {
                diagnostic: AuthorityDiagnostic::absent(),
                start,
                end,
            },
        },
    }
}

fn rust_terminal<'diagnostic>(
    source_identity: SourceIdentity,
    recipe: CompileRecipeFact,
    cause: lower::rust::RustCollectError,
) -> CompileFailure<'diagnostic> {
    match cause {
        lower::rust::RustCollectError::Authority(cause) => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::Rust {
                diagnostic: AuthorityDiagnostic::absent(),
                cause,
            },
        },
        lower::rust::RustCollectError::Lowering(cause) => CompileFailure::LoweringUnsupported {
            source_identity,
            recipe,
            cause,
        },
    }
}

fn go_terminal<'diagnostic>(
    source_identity: SourceIdentity,
    recipe: CompileRecipeFact,
    cause: lower::go::GoCollectError,
) -> CompileFailure<'diagnostic> {
    match cause {
        lower::go::GoCollectError::Image(cause) => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::GoImage {
                diagnostic: AuthorityDiagnostic::absent(),
                cause,
            },
        },
        lower::go::GoCollectError::SourceBinding { expected, observed } => {
            CompileFailure::Authority {
                source_identity,
                recipe,
                failure: AuthorityFailure::GoSourceBinding {
                    diagnostic: AuthorityDiagnostic::absent(),
                    expected,
                    observed,
                },
            }
        }
        lower::go::GoCollectError::Rejected(rejected) => CompileFailure::LoweringUnsupported {
            source_identity,
            recipe,
            cause: rejected_lowering(rejected),
        },
        lower::go::GoCollectError::Lowering(cause) => CompileFailure::LoweringUnsupported {
            source_identity,
            recipe,
            cause,
        },
    }
}

fn csharp_terminal<'diagnostic>(
    source_identity: SourceIdentity,
    recipe: CompileRecipeFact,
    cause: lower::csharp::CSharpCollectError,
) -> CompileFailure<'diagnostic> {
    match cause {
        lower::csharp::CSharpCollectError::Image(cause) => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::CSharpImage {
                diagnostic: AuthorityDiagnostic::absent(),
                cause,
            },
        },
        lower::csharp::CSharpCollectError::SourceBinding { expected, observed } => {
            CompileFailure::Authority {
                source_identity,
                recipe,
                failure: AuthorityFailure::CSharpSourceBinding {
                    diagnostic: AuthorityDiagnostic::absent(),
                    expected,
                    observed,
                },
            }
        }
        lower::csharp::CSharpCollectError::Span { start, end } => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::CSharpSpan {
                diagnostic: AuthorityDiagnostic::absent(),
                start,
                end,
            },
        },
        lower::csharp::CSharpCollectError::Rejected(rejected) => {
            CompileFailure::LoweringUnsupported {
                source_identity,
                recipe,
                cause: rejected_lowering(rejected),
            }
        }
        lower::csharp::CSharpCollectError::Lowering(cause) => CompileFailure::LoweringUnsupported {
            source_identity,
            recipe,
            cause,
        },
    }
}

fn java_terminal<'diagnostic>(
    source_identity: SourceIdentity,
    recipe: CompileRecipeFact,
    cause: lower::java::JavaCollectError,
) -> CompileFailure<'diagnostic> {
    match cause {
        lower::java::JavaCollectError::Image(cause) => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::JavaBoundImage {
                diagnostic: AuthorityDiagnostic::absent(),
                cause,
            },
        },
        lower::java::JavaCollectError::Release {
            requested,
            observed,
        } => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::JavaRelease {
                diagnostic: AuthorityDiagnostic::absent(),
                requested,
                observed,
            },
        },
        lower::java::JavaCollectError::SourceBinding { expected, observed } => {
            CompileFailure::Authority {
                source_identity,
                recipe,
                failure: AuthorityFailure::JavaSourceBinding {
                    diagnostic: AuthorityDiagnostic::absent(),
                    expected,
                    observed,
                },
            }
        }
        lower::java::JavaCollectError::Rejected(rejected) => CompileFailure::LoweringUnsupported {
            source_identity,
            recipe,
            cause: rejected_lowering(rejected),
        },
        lower::java::JavaCollectError::Lowering(cause) => CompileFailure::LoweringUnsupported {
            source_identity,
            recipe,
            cause,
        },
    }
}

fn clang_terminal<'diagnostic>(
    source_identity: SourceIdentity,
    recipe: CompileRecipeFact,
    cause: lower::clang::ClangCollectError,
) -> CompileFailure<'diagnostic> {
    match cause {
        lower::clang::ClangCollectError::Authority(cause) => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::Clang {
                diagnostic: AuthorityDiagnostic::absent(),
                cause,
            },
        },
        lower::clang::ClangCollectError::Rejected(rejected) => {
            CompileFailure::LoweringUnsupported {
                source_identity,
                recipe,
                cause: rejected_lowering(rejected),
            }
        }
        lower::clang::ClangCollectError::Projection(fault) => CompileFailure::ClangProjection {
            source_identity,
            recipe,
            fault,
        },
        lower::clang::ClangCollectError::Lowering(cause) => CompileFailure::LoweringUnsupported {
            source_identity,
            recipe,
            cause,
        },
        lower::clang::ClangCollectError::Admission(cause) => match cause {
            AdmissionFault::Canonical(cause) => CompileFailure::Prepare {
                source_identity,
                recipe,
                cause: PrepareError::SemanticData { cause },
            },
            AdmissionFault::Prepare(cause) => CompileFailure::Prepare {
                source_identity,
                recipe,
                cause,
            },
            AdmissionFault::Write(cause) => CompileFailure::Write {
                source_identity,
                recipe,
                cause,
            },
            AdmissionFault::ExtensionAtom {
                row,
                provisional,
                atom_count,
            } => CompileFailure::ExtensionAtomUnbound {
                source_identity,
                recipe,
                row,
                provisional,
                atom_count,
            },
            AdmissionFault::ExtensionTypeParameters {
                row,
                start,
                length,
                element_count,
            } => CompileFailure::ExtensionTypeParametersUnbound {
                source_identity,
                recipe,
                row,
                start,
                length,
                element_count,
            },
        },
    }
}

fn typescript_terminal<'diagnostic>(
    diagnostic_output: Option<&'diagnostic mut [u8]>,
    source: &[u8],
    source_identity: SourceIdentity,
    recipe: CompileRecipeFact,
    cause: TypeScriptCollectError,
) -> CompileFailure<'diagnostic> {
    match cause {
        TypeScriptCollectError::Utf8(cause) => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::TypeScriptUtf8 {
                diagnostic: AuthorityDiagnostic::absent(),
                cause,
            },
        },
        TypeScriptCollectError::Authority(cause) => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::TypeScript {
                diagnostic: typescript_diagnostic(diagnostic_output, source, &cause),
                cause,
            },
        },
        TypeScriptCollectError::Rejected(rejected) => CompileFailure::LoweringUnsupported {
            source_identity,
            recipe,
            cause: rejected_lowering(rejected),
        },
        TypeScriptCollectError::Projection(fault) => CompileFailure::LoweringUnsupported {
            source_identity,
            recipe,
            cause: backend_semantic::vocabulary::LoweringUnsupported::TypeScriptProjection { fault },
        },
        TypeScriptCollectError::Span { start, end } => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::TypeScriptSpan {
                diagnostic: AuthorityDiagnostic::absent(),
                start,
                end,
            },
        },
        TypeScriptCollectError::Lowering(cause) => CompileFailure::LoweringUnsupported {
            source_identity,
            recipe,
            cause,
        },
    }
}

fn typescript_diagnostic<'diagnostic>(
    output: Option<&'diagnostic mut [u8]>,
    source: &[u8],
    cause: &backend_frontend_typescript::legacy::AuthorityError,
) -> AuthorityDiagnostic<'diagnostic> {
    let Some(span) = cause.primary_span() else {
        return AuthorityDiagnostic::absent();
    };
    let Ok(start) = usize::try_from(span.start) else {
        return AuthorityDiagnostic::absent();
    };
    let Ok(end) = usize::try_from(span.end) else {
        return AuthorityDiagnostic::absent();
    };
    let Some(primary) = source.get(start..end) else {
        return AuthorityDiagnostic::absent();
    };
    let Some(output) = output else {
        return AuthorityDiagnostic::absent();
    };
    let retained = primary.len().min(output.len());
    let (Some(source), Some(destination)) = (primary.get(..retained), output.get_mut(..retained))
    else {
        return AuthorityDiagnostic::absent();
    };
    destination.copy_from_slice(source);
    match AuthorityDiagnostic::new(destination, primary.len(), retained != primary.len()) {
        Ok(diagnostic) => diagnostic,
        Err(AuthorityDiagnosticFault::PrefixExceedsObserved { .. })
        | Err(AuthorityDiagnosticFault::TruncationMismatch { .. }) => AuthorityDiagnostic::absent(),
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use std::{
        sync::atomic::{AtomicBool, Ordering},
        time::{Duration, Instant},
    };

    use backend_semantic::ir::{EntityKind, SemanticProductConstructor};
    use backend_semantic::vocabulary::{NativeTool, RustEdition};
    use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};

    use super::*;

    const SOURCE: &[u8] = b"fn lifecycle() {}";

    fn source() -> SourceIdentity {
        SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(SOURCE),
            byte_len: u32::try_from(SOURCE.len()).unwrap_or(u32::MAX),
        }
    }

    fn recipe(source: SourceIdentity) -> CompileRecipeFact {
        CompileRecipeFact::derive(
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            NativeTool::Rustc,
            source.identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"lifecycle-toolchain"),
        )
    }

    fn facts() -> lower::FactSet<'static> {
        let mut facts = lower::FactSet::new();
        match facts.push(lower::SemanticFact::new(
            EntityKind::Function,
            b"lifecycle",
            SemanticProductConstructor::PRODUCT,
        )) {
            Ok(0) => facts,
            Ok(_) | Err(_) => panic!("lifecycle fact fixture must admit at ordinal zero"),
        }
    }

    #[test]
    fn stopped_prewrite_gate_keeps_fused_and_compact_output_untouched() {
        let source = source();
        let recipe = recipe(source);
        let facts = facts();

        // Fused path: materialize owned truth first, then stop before its
        // only output mutation. The writer must retain the exact terminal.
        let cancelled = AtomicBool::new(false);
        let ir = match facts.build_ir(
            LanguageProfile::Rust(RustEdition::Rust2024),
            source,
            recipe,
            super::super::DeclarationScope::fixture(),
        ) {
            Ok(ir) => ir,
            Err(_) => panic!("fixture owned image must build"),
        };
        cancelled.store(true, Ordering::Release);
        let permit = WorkPermit::new(
            source,
            super::super::CompileControl {
                deadline: Instant::now() + Duration::from_secs(1),
                cancelled: &cancelled,
            },
        );
        let mut fused_output = [0xa5_u8; 1024];
        match write_fragment(source, recipe, &facts, permit, &mut fused_output) {
            Err(CompileFailure::Cancelled { diagnostic, .. })
                if diagnostic.bytes.is_empty()
                    && diagnostic.observed == 0
                    && !diagnostic.truncated => {}
            _ => panic!("fused pre-write cancellation must retain its exact terminal"),
        }
        assert!(fused_output.iter().all(|byte| *byte == 0xa5));
        drop(ir);

        // Compact compatibility path reaches the identical writer boundary;
        // a deadline there is likewise observed before any output byte moves.
        let deadline_cancelled = AtomicBool::new(false);
        let deadline_permit = WorkPermit::new(
            source,
            super::super::CompileControl {
                deadline: Instant::now() - Duration::from_secs(1),
                cancelled: &deadline_cancelled,
            },
        );
        let mut compact_output = [0xa5_u8; 1024];
        match write_fragment(source, recipe, &facts, deadline_permit, &mut compact_output) {
            Err(CompileFailure::DeadlineExceeded { diagnostic, .. })
                if diagnostic.bytes.is_empty()
                    && diagnostic.observed == 0
                    && !diagnostic.truncated => {}
            _ => panic!("compact pre-write deadline must retain its exact terminal"),
        }
        assert!(compact_output.iter().all(|byte| *byte == 0xa5));
    }
}
