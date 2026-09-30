//! Defines types compile behavior for the `backend-engine` driver, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the types compile invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use std::fmt::Write as _;

use backend_semantic::ir::{FragmentView, PrepareError};
use backend_semantic::registry::{AdapterRoute, FullRegistry};
use backend_semantic::vocabulary::{Language, LanguageProfile, MAX_NATIVE_DIAGNOSTIC_BYTES, Stage};

use crate::driver::lower::{self, AdmissionFault, typescript::TypeScriptCollectError};

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
        project: Option<&'source backend_frontend_clang::ClangProject>,
        environment: &'source backend_frontend_clang::ClangAuthorityEnvironment,
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
    RustWorkspace {
        profile: backend_semantic::vocabulary::RustEdition,
        workspace: &'source backend_frontend_rust::legacy::RustWorkspace,
        source_path: &'source std::path::Path,
        maximum_source_bytes: backend_frontend_rust::legacy::SourceByteLimit,
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
    type Authority<'source> = (
        LanguageProfile,
        Option<&'source backend_frontend_clang::ClangProject>,
        &'source backend_frontend_clang::ClangAuthorityEnvironment,
    );
    type Extension = backend_semantic::ir::ClangFacts;

    fn collect<'source, 'cancel, 'diagnostic>(
        authority: &Self::Authority<'source>,
        prepared: &PreparedCompile<'source, 'cancel>,
        _: Option<&'diagnostic mut [u8]>,
        facts: &mut lower::FactSet<'source>,
    ) -> Result<(), CompileFailure<'diagnostic>> {
        lower::clang::collect(
            authority.0,
            authority.1,
            authority.2,
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
                                backend_frontend_typescript::legacy::AuthorityError::Checker {
                                    cause,
                                },
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
                let module = backend_frontend_python::legacy::extract(source, authority.0)
                    .map_err(|cause| {
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
        diagnostic_output: Option<&'diagnostic mut [u8]>,
        facts: &mut lower::FactSet<'source>,
    ) -> Result<(), CompileFailure<'diagnostic>> {
        let is_build_script = authority
            .0
            .source_path
            .file_name()
            .is_some_and(|name| name == "build.rs");
        lower::rust::collect(
            authority.0,
            authority.1,
            authority.2,
            prepared.permit.control(),
            prepared.lease.bytes(),
            facts,
        )
        .map_err(|cause| {
            rust_terminal(
                prepared.source,
                prepared.recipe,
                diagnostic_output,
                is_build_script,
                cause,
            )
        })
    }
}

impl RustSpec {
    fn collect_workspace<'source, 'cancel, 'diagnostic>(
        workspace: &'source backend_frontend_rust::legacy::RustWorkspace,
        source_path: &'source std::path::Path,
        maximum_source_bytes: backend_frontend_rust::legacy::SourceByteLimit,
        prepared: &PreparedCompile<'source, 'cancel>,
        diagnostic_output: Option<&'diagnostic mut [u8]>,
        facts: &mut lower::FactSet<'source>,
    ) -> Result<(), CompileFailure<'diagnostic>> {
        let is_build_script = source_path
            .file_name()
            .is_some_and(|name| name == "build.rs");
        lower::rust::collect_workspace(
            workspace,
            source_path,
            maximum_source_bytes,
            prepared.permit.control(),
            prepared.lease.bytes(),
            facts,
        )
        .map_err(|cause| {
            rust_terminal(
                prepared.source,
                prepared.recipe,
                diagnostic_output,
                is_build_script,
                cause,
            )
        })
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
        (LanguageProfile::C(profile), SemanticAuthorityInput::Clang { project }) => {
            Ok(EnteredAuthority::Clang {
                profile: LanguageProfile::C(profile),
                project: Some(project),
                environment: project.environment(),
            })
        }
        (LanguageProfile::C(profile), SemanticAuthorityInput::ClangBuffer { environment }) => {
            Ok(EnteredAuthority::Clang {
                profile: LanguageProfile::C(profile),
                project: None,
                environment,
            })
        }
        (LanguageProfile::C(_), SemanticAuthorityInput::None) => Err(required()),
        (LanguageProfile::Cxx(profile), SemanticAuthorityInput::Clang { project }) => {
            Ok(EnteredAuthority::Clang {
                profile: LanguageProfile::Cxx(profile),
                project: Some(project),
                environment: project.environment(),
            })
        }
        (LanguageProfile::Cxx(profile), SemanticAuthorityInput::ClangBuffer { environment }) => {
            Ok(EnteredAuthority::Clang {
                profile: LanguageProfile::Cxx(profile),
                project: None,
                environment,
            })
        }
        (LanguageProfile::Cxx(_), SemanticAuthorityInput::None) => Err(required()),
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
        (
            LanguageProfile::Rust(profile),
            SemanticAuthorityInput::RustWorkspace {
                workspace,
                source_path,
                maximum_source_bytes,
            },
        ) => Ok(EnteredAuthority::RustWorkspace {
            profile,
            workspace,
            source_path,
            maximum_source_bytes,
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
        EnteredAuthority::Clang {
            profile,
            project,
            environment,
        } => ClangSpec::collect(&(*profile, *project, *environment), prepared, None, facts)?,
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
            diagnostic_output,
            facts,
        )?,
        EnteredAuthority::RustWorkspace {
            workspace,
            source_path,
            maximum_source_bytes,
            ..
        } => RustSpec::collect_workspace(
            workspace,
            source_path,
            *maximum_source_bytes,
            prepared,
            diagnostic_output,
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
    // An empty fact set is a typed rejection for every lane whose source
    // ought to carry declarations. Three authorities legally admit
    // declaration-free sources and prove the empty product themselves:
    // a Go package whose only file is a `doc.go` package clause, a C/C++
    // translation unit whose every declaration sits behind an unmet
    // preprocessor gate (a `#ifdef USE_OPENSSL` vtls backend, a config-gated
    // mbedtls build unit), and a Rust crate root whose every written item
    // stayed out of the lane behind an unmet `#[cfg]` gate, an unresolved
    // facade re-export, or a `compile_error!` stub — the Rust lane rejects
    // a source with no written item at all before this gate, so an
    // admitted-empty Rust product always carries that written-surface
    // proof. Each authority succeeded, so the collected-empty product is
    // the honest parity output — rejecting it would contradict an
    // authority that succeeded.
    if facts.len() == 0
        && !matches!(
            prepared.authority,
            EnteredAuthority::Go { .. }
                | EnteredAuthority::Clang { .. }
                | EnteredAuthority::Rust { .. }
                | EnteredAuthority::RustWorkspace { .. }
        )
    {
        require_facts(prepared.source, prepared.recipe, facts)?;
    }
    Ok(())
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
                cause: backend_semantic::vocabulary::LoweringUnsupported::PythonProjection {
                    fault,
                },
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
    diagnostic_output: Option<&'diagnostic mut [u8]>,
    is_build_script: bool,
    cause: lower::rust::RustCollectError,
) -> CompileFailure<'diagnostic> {
    match cause {
        lower::rust::RustCollectError::Authority(cause) => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::Rust {
                diagnostic: rust_authority_diagnostic(diagnostic_output, &cause, is_build_script),
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

/// Writes a bounded Rust authority summary while retaining the exact typed cause separately.
/// Cargo's raw error chain can contain absolute paths and process configuration, so the public
/// bytes contain only a closed explanation and a validated package name when Cargo supplies one.
pub(crate) fn rust_authority_diagnostic<'diagnostic>(
    output: Option<&'diagnostic mut [u8]>,
    cause: &backend_frontend_rust::legacy::RustAuthorityError,
    is_build_script: bool,
) -> AuthorityDiagnostic<'diagnostic> {
    let Some(output) = output else {
        return AuthorityDiagnostic::absent();
    };
    let mut message = BoundedAuthorityMessage::new(output);
    use backend_frontend_rust::legacy::RustAuthorityError as RustError;
    match cause {
        RustError::Workspace { source, .. } => {
            if let Some(package) = cargo_missing_package(source.as_ref()) {
                let target = if is_build_script {
                    "build-script"
                } else {
                    "crate"
                };
                let _ = write!(
                    message,
                    "Rust {target} workspace resolution could not find Cargo package `{package}` in the offline cache."
                );
            } else if cargo_workspace_was_offline(source.as_ref()) {
                let target = if is_build_script {
                    "build-script"
                } else {
                    "crate"
                };
                let _ = write!(
                    message,
                    "Rust {target} workspace resolution failed while Cargo was offline."
                );
            } else if is_build_script {
                let _ = message.write_str("Rust build-script Cargo workspace loading failed.");
            } else {
                let _ = message.write_str("Rust Cargo workspace loading failed.");
            }
        }
        RustError::CargoMetadataIncomplete { policy, .. } => {
            let policy = match policy {
                backend_frontend_rust::legacy::RustCargoMetadataPolicy::Online => "online",
                backend_frontend_rust::legacy::RustCargoMetadataPolicy::Offline => "offline",
            };
            let _ = write!(
                message,
                "Full Cargo dependency and feature resolution is incomplete under the {policy} policy; no-dependency metadata cannot authorize Rust semantics."
            );
        }
        RustError::SourceNotLoaded { .. } => {
            let _ = message.write_str("rust-analyzer did not load the selected Cargo source.");
        }
        RustError::DetachedSource { .. } => {
            let _ = message.write_str(
                "selected Rust source is cfg-inactive or detached from every active Cargo target.",
            );
        }
        RustError::EditionMismatch { .. } => {
            let _ = message.write_str("Cargo edition differs from the requested Rust profile.");
        }
        RustError::MissingSemanticFact { fact } => {
            let _ = write!(
                message,
                "rust-analyzer omitted required semantic fact {fact:?}."
            );
        }
        RustError::UnresolvedInferredType => {
            let _ = message.write_str("rust-analyzer could not resolve an inferred Rust type.");
        }
        RustError::SourceBinding { expected, observed } => {
            let _ = write!(
                message,
                "Rust source differs from Cargo VFS text: request {expected} bytes, VFS {observed} bytes."
            );
        }
        RustError::Toolchain(_) => {
            let _ = message.write_str("Rust toolchain authority configuration failed.");
        }
        RustError::ProjectRoot { .. } | RustError::ProjectSource { .. } => {
            let _ = message.write_str("Rust authority could not open its selected project source.");
        }
        RustError::SourceOutsidePackage { .. } => {
            let _ = message.write_str("selected Rust source is outside its Cargo package root.");
        }
        RustError::MissingManifest { .. } => {
            let _ = message.write_str("Rust authority could not find a Cargo package manifest.");
        }
        RustError::SourceNotFile { .. } => {
            let _ = message.write_str("Rust authority source is not a regular file.");
        }
        RustError::SourceBudget { .. } => {
            let _ = message.write_str("Rust source exceeded its admitted byte limit.");
        }
        RustError::SourceRead { .. } => {
            let _ = message.write_str("Rust authority could not read the selected source.");
        }
        RustError::DocumentationInputMissing { .. } => {
            let _ = message.write_str("Rustdoc include file was missing.");
        }
        RustError::DocumentationInputRead { .. } => {
            let _ = message.write_str("Rustdoc include file could not be inspected or read.");
        }
        RustError::DocumentationInputBudget { .. } => {
            let _ = message.write_str("Rustdoc include file exceeded its admitted byte limit.");
        }
        RustError::DocumentationInputUtf8 { .. } => {
            let _ = message.write_str("Rustdoc include file was not valid UTF-8.");
        }
        RustError::DocumentationInputLimit { .. } => {
            let _ = message
                .write_str("Rustdoc include inputs exceeded the bounded count or byte limit.");
        }
        RustError::UnsupportedDocumentationExpression { .. } => {
            let _ = message.write_str(
                "Rustdoc include expression could not be admitted by the bounded preloader.",
            );
        }
        RustError::SessionFrontierMismatch
        | RustError::SessionSourcePath { .. }
        | RustError::SessionSourceCardinality { .. }
        | RustError::WorkspaceBindingMismatch => {
            let _ = message.write_str("Rust workspace source selection could not be admitted.");
        }
        RustError::SessionSourceRootAmbiguous => {
            let _ = message.write_str(
                "rust-analyzer could not place a new editor source in one Cargo source root.",
            );
        }
        RustError::SessionSourceRootLimit { .. } => {
            let _ = message.write_str("Rust workspace source-root update exceeded its limit.");
        }
        RustError::InvalidSpan { .. } | RustError::Coordinate { .. } => {
            let _ = message.write_str(
                "rust-analyzer returned a source coordinate outside its admitted range.",
            );
        }
        RustError::Admission { .. } => {
            let _ = message.write_str("Rust semantic fact admission failed.");
        }
        RustError::Cancelled => {
            let _ = message.write_str("Rust semantic analysis was cancelled.");
        }
        RustError::DeadlineExceeded => {
            let _ = message.write_str("Rust semantic analysis exceeded its deadline.");
        }
    }
    message.finish()
}

/// Returns a validated Cargo package token from known resolver messages.
fn cargo_missing_package(source: &(dyn std::error::Error + 'static)) -> Option<String> {
    let mut error = Some(source);
    while let Some(current) = error {
        let message = current.to_string();
        for (prefix, terminator) in [
            ("no matching package named `", '`'),
            ("no matching package named '", '\''),
            ("failed to get `", '`'),
            ("failed to select a version for the requirement `", '`'),
        ] {
            let Some(candidate) = message
                .split_once(prefix)
                .and_then(|(_, remainder)| remainder.split_once(terminator))
                .map(|(candidate, _)| candidate)
            else {
                continue;
            };
            let Some(first) = candidate.split_ascii_whitespace().next() else {
                continue;
            };
            let package = first.split_once('=').map_or(first, |(name, _)| name);
            if !package.is_empty()
                && package.len() <= 64
                && package
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
            {
                return Some(package.to_owned());
            }
        }
        error = current.source();
    }
    None
}

/// Detects offline Cargo resolution failures without returning any source text.
fn cargo_workspace_was_offline(source: &(dyn std::error::Error + 'static)) -> bool {
    let mut error = Some(source);
    while let Some(current) = error {
        let message = current.to_string();
        if message.contains("offline") || message.contains("no matching package") {
            return true;
        }
        error = current.source();
    }
    false
}

/// Fixed-capacity UTF-8 writer used to keep Rust authority messages within caller scratch.
struct BoundedAuthorityMessage<'output> {
    output: &'output mut [u8],
    observed: usize,
}

impl<'output> BoundedAuthorityMessage<'output> {
    fn new(output: &'output mut [u8]) -> Self {
        let capacity = output.len().min(MAX_NATIVE_DIAGNOSTIC_BYTES);
        Self {
            output: &mut output[..capacity],
            observed: 0,
        }
    }

    fn finish(self) -> AuthorityDiagnostic<'output> {
        let retained = self.observed.min(self.output.len());
        let primary = &self.output[..retained];
        match AuthorityDiagnostic::new(primary, self.observed, retained != self.observed) {
            Ok(diagnostic) => diagnostic,
            Err(AuthorityDiagnosticFault::PrefixExceedsObserved { .. })
            | Err(AuthorityDiagnosticFault::TruncationMismatch { .. }) => {
                AuthorityDiagnostic::absent()
            }
        }
    }
}

impl std::fmt::Write for BoundedAuthorityMessage<'_> {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        let retained = self
            .output
            .len()
            .saturating_sub(self.observed)
            .min(text.len());
        let start = self.observed.min(self.output.len());
        if let (Some(source), Some(destination)) = (
            text.as_bytes().get(..retained),
            self.output.get_mut(start..start.saturating_add(retained)),
        ) {
            destination.copy_from_slice(source);
        }
        self.observed = self.observed.saturating_add(text.len());
        Ok(())
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
            cause: backend_semantic::vocabulary::LoweringUnsupported::TypeScriptProjection {
                fault,
            },
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
    use backend_semantic::vocabulary::{
        AuthorityDiagnosticClass, AuthorityPhase, NativeTool, RustEdition,
    };
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

    #[test]
    fn rust_workspace_failure_emits_bounded_path_free_diagnostic() {
        let source = source();
        let recipe = recipe(source);
        let private_root = std::path::PathBuf::from("/private/work/serde_json");
        let cause = lower::rust::RustCollectError::Authority(
            backend_frontend_rust::legacy::RustAuthorityError::Workspace {
                root: private_root.clone(),
                source: std::io::Error::other(
                    "error: no matching package named `serde_core` found; CARGO_HOME=/private/secret; root=/private/work/serde_json",
                )
                .into(),
            },
        );
        let mut scratch = [0xa5; MAX_NATIVE_DIAGNOSTIC_BYTES];
        let failure = rust_terminal(source, recipe, Some(&mut scratch), true, cause);
        let CompileFailure::Authority { failure, .. } = failure else {
            panic!("Rust workspace authority failure must remain an authority terminal");
        };
        let projection = failure.projection();
        assert_eq!(projection.phase, AuthorityPhase::Resolve);
        assert_eq!(projection.class, AuthorityDiagnosticClass::Binding);
        assert_eq!(
            projection.diagnostic.primary,
            b"Rust build-script workspace resolution could not find Cargo package `serde_core` in the offline cache."
        );
        assert_eq!(
            projection.diagnostic.observed,
            projection.diagnostic.primary.len()
        );
        assert!(
            !projection
                .diagnostic
                .primary
                .windows(b"/private".len())
                .any(|window| window == b"/private")
        );
        assert!(
            !projection
                .diagnostic
                .primary
                .windows(b"CARGO_HOME".len())
                .any(|window| window == b"CARGO_HOME")
        );
        let AuthorityFailure::Rust {
            cause: backend_frontend_rust::legacy::RustAuthorityError::Workspace { root, .. },
            ..
        } = failure
        else {
            panic!("the exact Rust workspace cause must remain attached");
        };
        assert_eq!(root, private_root);
    }
}
