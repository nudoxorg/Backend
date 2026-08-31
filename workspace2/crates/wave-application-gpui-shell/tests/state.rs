//! Deterministic headless projection tests.

#[allow(dead_code, unreachable_pub)]
#[path = "../../wave-application-protocol/tests/support/golden_corpus.rs"]
mod golden_corpus;

use nudox_compile_vocab::{CompileRecipeFact, Language, NativeTool, Stage};
use nudox_id::{
    ArtifactId, CompilePublicationDomain, CompilePublicationEncoding, ContentId,
    DependencySetDomain, IrFragmentDomain, IrFragmentEncoding, IrManifestDomain,
    IrManifestEncoding, SourceFactDomain, ToolchainDomain,
};
use wave_application_core::{
    AdaptiveDisposition, ApplicationDisposition, ApplicationInput, ApplicationOutcome,
    ApplicationReply, ApplicationService, BatteryState, ByteCount, Capability, CapabilityDomain,
    CapabilityHealth, CorrelationId, Diagnostic, DiagnosticCode, DiagnosticDetail, ExecutionState,
    GeneratedArtifact, GenerationAuthority, GenerationId, IndexSnapshotId, InputText,
    InputTextError, OperationBudget, Pin, Pressure, PublicationAuthority, ReplyBody,
    ResourceBudget, RetryBudget, SourceAuthority,
};
use wave_application_gpui_shell::{
    AdaptiveProjection, ApplyError, BatchReceipt, CommandId, ExecutionProjection, FormError,
    FormField, GeneratedProjection, HealthProjection, MAX_BATCH_REPLIES, MotionPreference,
    PaletteDirection, PaletteEditError, ProjectionState, ROUTES, ResultLimit, Route, SURFACE_COUNT,
    ServiceAction, ShellState, Surface, SurfaceStatus,
};

#[derive(Debug)]
enum TestError {
    Input(InputTextError),
    Apply(ApplyError),
    Form(FormError),
    Golden(golden_corpus::GoldenError),
    Unexpected(&'static str),
}

impl std::fmt::Display for TestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Input(error) => write!(formatter, "input error: {error:?}"),
            Self::Apply(error) => write!(formatter, "projection error: {error:?}"),
            Self::Form(error) => write!(formatter, "form error: {error:?}"),
            Self::Golden(error) => error.fmt(formatter),
            Self::Unexpected(message) => write!(formatter, "unexpected reply: {message}"),
        }
    }
}

impl std::error::Error for TestError {}

impl From<InputTextError> for TestError {
    fn from(error: InputTextError) -> Self {
        Self::Input(error)
    }
}

impl From<ApplyError> for TestError {
    fn from(error: ApplyError) -> Self {
        Self::Apply(error)
    }
}

impl From<FormError> for TestError {
    fn from(value: FormError) -> Self {
        Self::Form(value)
    }
}

impl From<golden_corpus::GoldenError> for TestError {
    fn from(value: golden_corpus::GoldenError) -> Self {
        Self::Golden(value)
    }
}

fn text(value: &str) -> Result<InputText, InputTextError> {
    InputText::try_from_str(value)
}

fn pin() -> Pin {
    Pin {
        generation: GenerationId::from_canonical_bytes(b"gpui-generation"),
        snapshot: IndexSnapshotId::from_canonical_bytes(b"gpui-snapshot"),
    }
}

fn bundle() -> ContentId<CapabilityDomain> {
    ContentId::from_canonical_bytes(b"gpui-analyzer-bundle")
}

fn generated_artifact() -> GeneratedArtifact {
    let source_identity = ContentId::<SourceFactDomain>::from_canonical_bytes(b"gpui-source");
    let toolchain = ContentId::<ToolchainDomain>::from_canonical_bytes(b"gpui-toolchain");
    GeneratedArtifact {
        source: SourceAuthority {
            identity: source_identity,
            byte_len: 11,
        },
        recipe: CompileRecipeFact::derive(
            Language::Rust,
            Stage::LowerIr,
            NativeTool::Rustc,
            source_identity,
            toolchain,
        ),
        fragment: ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_encoded_bytes(
            b"gpui-fragment",
        ),
        publication: PublicationAuthority {
            generation: GenerationAuthority {
                pinned_root: GenerationId::from_canonical_bytes(b"gpui-generated-root"),
                dep_set: ContentId::<DependencySetDomain>::from_canonical_bytes(b"gpui-generated-deps"),
            },
            manifest: ArtifactId::<IrManifestEncoding, IrManifestDomain>::from_encoded_bytes(
                b"gpui-manifest",
            ),
            binding: ArtifactId::<CompilePublicationEncoding, CompilePublicationDomain>::from_encoded_bytes(
                b"gpui-binding",
            ),
        },
    }
}

fn budget(operations: u8, retries: u8) -> ResourceBudget {
    ResourceBudget {
        ram_free: ByteCount::from(4096),
        nvme_free: ByteCount::from(8192),
        operations: OperationBudget::from(operations),
        retries: RetryBudget::from(retries),
        memory_pressure: Pressure::Relaxed,
        storage_pressure: Pressure::Relaxed,
        battery: BatteryState::Normal,
    }
}

fn reply(correlation: u64, body: &ReplyBody) -> ApplicationReply {
    ApplicationReply {
        correlation: CorrelationId(correlation),
        outcome: ApplicationOutcome::Resolved(*body),
    }
}

fn apply(state: &mut ShellState, reply: ApplicationReply) -> Result<BatchReceipt, ApplyError> {
    state.apply(reply)
}

#[test]
fn first_frame_is_stable_without_polling_or_allocated_rows() {
    let state = ShellState::default();
    let summaries = state.summaries();

    assert_eq!(summaries.len(), SURFACE_COUNT);
    assert_eq!(
        summaries.map(|summary| summary.surface),
        [
            Surface::Generation,
            Surface::Adaptive,
            Surface::Execution,
            Surface::Index,
            Surface::Graph,
            Surface::Vector,
            Surface::Health,
        ]
    );
    assert!(
        summaries
            .into_iter()
            .all(|summary| summary.state == ProjectionState::Checking)
    );
    assert_eq!(state.last_reply, None);
    assert_eq!(state.notification_epoch, 0);
    assert_eq!(state.navigation.route, Route::Home);
    assert!(!state.navigation.palette.visible);
}

#[test]
fn palette_selection_is_a_command_identity_and_returns_its_virtual_reveal_row() {
    let mut state = ShellState::default();
    state.open_palette();

    assert!(state.navigation.palette.visible);
    assert_eq!(
        state.move_palette_selection(PaletteDirection::Next),
        Some(1)
    );
    assert_eq!(
        state.move_palette_selection(PaletteDirection::Next),
        Some(2)
    );
    assert_eq!(
        state.confirm_palette(),
        Some(CommandId::OpenRoute(Route::Search))
    );
    assert_eq!(state.navigation.route, Route::Search);
    assert!(!state.navigation.palette.visible);
}

#[test]
fn palette_and_navigation_share_the_connections_destination() {
    let mut state = ShellState::default();
    state.select_route(Route::Connections);
    assert_eq!(state.navigation.route, Route::Connections);

    state.open_palette();
    state.select_palette_command(CommandId::OpenRoute(Route::Settings));
    assert_eq!(
        state.confirm_palette(),
        Some(CommandId::OpenRoute(Route::Settings))
    );
    assert_eq!(state.navigation.route, Route::Settings);
}

#[test]
fn palette_filter_reselects_a_matching_stable_identity() -> Result<(), TestError> {
    let mut state = ShellState::default();
    state.open_palette();
    state.replace_palette_query(text("settings")?);

    assert_eq!(state.navigation.palette.result_count(), 1);
    assert_eq!(
        state.confirm_palette(),
        Some(CommandId::OpenRoute(Route::Settings))
    );
    assert_eq!(state.navigation.route, Route::Settings);
    Ok(())
}

#[test]
fn palette_edit_rejections_retain_the_exact_closed_cause() {
    let mut state = ShellState::default();
    state.open_palette();

    assert_eq!(
        state.erase_palette_character(),
        Err(PaletteEditError::NothingToErase)
    );
    assert_eq!(state.palette_error, Some(PaletteEditError::NothingToErase));

    let over_bound = "x".repeat(wave_application_core::INPUT_TEXT_BYTES + 1);
    let expected = PaletteEditError::InputTooLong {
        actual: over_bound.len(),
        maximum: wave_application_core::INPUT_TEXT_BYTES,
    };
    assert_eq!(state.append_palette_text(&over_bound), Err(expected));
    assert_eq!(state.palette_error, Some(expected));
}

#[test]
fn keyboard_shortcuts_and_status_destinations_are_closed_visible_facts() {
    assert!(ROUTES[4].shortcut.is_some());
    let Some(shortcut) = ROUTES[4].shortcut else {
        return;
    };
    assert_eq!(shortcut.apple, "⌘,");
    assert_eq!(shortcut.other, "Ctrl ,");

    let mut state = ShellState::default();
    state.select_palette_command(CommandId::InspectSurface(Surface::Health));
    assert_eq!(
        state.confirm_palette(),
        Some(CommandId::InspectSurface(Surface::Health))
    );
    assert_eq!(state.navigation.route, Route::Settings);

    state.open_palette();
    state.select_palette_command(CommandId::InspectSurface(Surface::Execution));
    assert_eq!(
        state.confirm_palette(),
        Some(CommandId::InspectSurface(Surface::Execution))
    );
    assert_eq!(state.navigation.route, Route::Connections);
}

#[test]
fn reduced_and_no_motion_are_explicit_stable_projection_facts() {
    let mut state = ShellState::default();
    assert_eq!(state.motion, MotionPreference::Standard);
    state.set_motion_preference(MotionPreference::Reduced);
    assert_eq!(state.motion, MotionPreference::Reduced);
    state.set_motion_preference(MotionPreference::None);
    assert_eq!(state.motion, MotionPreference::None);
    assert_eq!(state.notification_epoch, 0);
}

#[test]
fn golden_corpus_projects_the_independent_core_journey_into_visible_gui_facts()
-> Result<(), TestError> {
    let expected = golden_corpus::direct_replies()?;
    let replies = golden_corpus::direct_application_replies()?;
    let mut state = ShellState::default();
    for reply in replies {
        apply(&mut state, reply)?;
    }

    assert_eq!(
        state.last_reply.as_ref().map(|reply| reply.correlation.0),
        Some(expected[3].correlation)
    );
    assert_eq!(
        state.pages.home.generation,
        ProjectionState::Degraded(Capability::CompilerOutput)
    );
    assert_eq!(
        state.pages.connections.execution,
        ProjectionState::Cancelled
    );
    assert_eq!(
        state.pages.settings.health,
        ProjectionState::Degraded(Capability::CompilerOutput)
    );
    Ok(())
}

#[test]
fn typed_generate_form_requires_each_field_and_constructs_only_application_input()
-> Result<(), TestError> {
    let mut state = ShellState::default();
    state.select_action(ServiceAction::Generate);
    assert_eq!(
        state.submit_form(CorrelationId(60)),
        Err(FormError::MissingText(FormField::Language))
    );
    assert_eq!(
        state.form_error,
        Some(FormError::MissingText(FormField::Language))
    );

    state.replace_form_text(FormField::Language, text("rust")?)?;
    state.replace_form_text(FormField::Stage, text("parse")?)?;
    state.replace_form_text(FormField::Source, text("fn main() {}")?)?;
    assert!(matches!(
        state.submit_form(CorrelationId(61)),
        Ok(ApplicationInput::Generate {
            correlation: CorrelationId(61),
            ..
        })
    ));
    Ok(())
}

#[test]
fn field_and_limit_transitions_retain_exact_form_rejections() -> Result<(), TestError> {
    let mut state = ShellState::default();
    assert_eq!(
        state.select_form_field(FormField::Query),
        Err(FormError::NoActiveForm)
    );
    assert_eq!(state.form_error, Some(FormError::NoActiveForm));

    state.select_action(ServiceAction::Health);
    assert_eq!(
        state.select_form_field(FormField::Snapshot),
        Err(FormError::FieldUnavailable(FormField::Snapshot))
    );
    assert_eq!(
        state.form_error,
        Some(FormError::FieldUnavailable(FormField::Snapshot))
    );

    let Ok(limit) = ResultLimit::new(2) else {
        return Err(TestError::Unexpected(
            "the fixed visible result limit must remain accepted",
        ));
    };
    assert_eq!(
        state.replace_form_limit(limit),
        Err(FormError::LimitUnavailable)
    );
    assert_eq!(state.form_error, Some(FormError::LimitUnavailable));
    Ok(())
}

#[test]
fn search_limit_is_typed_and_cannot_exceed_the_core_command_bound() -> Result<(), TestError> {
    let mut state = ShellState::default();
    state.select_action(ServiceAction::Search);
    state.replace_form_text(FormField::Snapshot, text("published")?)?;
    state.replace_form_text(FormField::Query, text("needle")?)?;
    assert_eq!(
        ResultLimit::new(5),
        Err(FormError::LimitExceeded {
            requested: 5,
            maximum: 4,
        })
    );
    let Ok(limit) = ResultLimit::new(2) else {
        return Err(TestError::Unexpected(
            "known typed limit should be accepted",
        ));
    };
    state.replace_form_limit(limit)?;
    assert!(matches!(
        state.submit_form(CorrelationId(62)),
        Ok(ApplicationInput::Search { limit: 2, .. })
    ));
    Ok(())
}

#[test]
fn unavailable_compiler_output_is_projected_without_an_echo_artifact() -> Result<(), TestError> {
    let mut service = ApplicationService::new();
    let source = text("fn gpui() {}")?;
    let reply = service.execute(&ApplicationInput::Generate {
        correlation: CorrelationId(11),
        language: text("rust")?,
        stage: text("lower-ir")?,
        source,
    });
    let mut state = ShellState::default();
    apply(&mut state, reply)?;

    assert!(matches!(
        state.last_reply.as_ref().map(|reply| &reply.outcome),
        Some(ApplicationOutcome::Resolved(
            ReplyBody::DependencyUnavailable {
                capability: Capability::CompilerOutput,
            }
        ))
    ));
    assert_eq!(
        state.generation,
        SurfaceStatus::Resolved(ApplicationDisposition::Degraded {
            emitted: 0,
            unavailable: Capability::CompilerOutput,
        })
    );
    assert_eq!(
        state.summaries()[0].state,
        ProjectionState::Degraded(Capability::CompilerOutput)
    );
    Ok(())
}

#[test]
fn generated_artifact_is_retained_verbatim_in_the_visible_generation_projection()
-> Result<(), TestError> {
    let artifact = generated_artifact();
    let incoming = reply(31, &ReplyBody::Generated(artifact));
    let mut state = ShellState::default();
    apply(&mut state, incoming)?;

    let expected = Some(GeneratedProjection { artifact });
    assert_eq!(state.generated, expected);
    assert_eq!(state.pages.home.generated, expected);
    assert!(matches!(
        state.last_reply.as_ref().map(|reply| &reply.outcome),
        Some(ApplicationOutcome::Resolved(ReplyBody::Generated(observed))) if *observed == artifact
    ));
    assert_eq!(
        state.generation,
        SurfaceStatus::Resolved(ApplicationDisposition::Complete { emitted: 1 })
    );
    Ok(())
}

#[test]
fn adaptive_decision_is_projected_without_replacing_its_typed_cause() -> Result<(), TestError> {
    let mut service = ApplicationService::new();
    let reply = service.execute(&ApplicationInput::RecoverLocal {
        correlation: CorrelationId(12),
        pin: pin(),
        bundle: bundle(),
        budget: budget(0, 1),
    });
    let mut state = ShellState::default();
    apply(&mut state, reply)?;

    assert!(matches!(
        state.adaptive,
        AdaptiveProjection::Reported {
            disposition: AdaptiveDisposition::Overloaded(_),
            result: ApplicationDisposition::Degraded {
                unavailable: Capability::LocalAnalyzer,
                ..
            },
        }
    ));
    assert_eq!(
        state.summaries()[1].state,
        ProjectionState::Degraded(Capability::LocalAnalyzer)
    );
    Ok(())
}

#[test]
fn execution_started_and_terminal_are_projected_as_distinct_states() -> Result<(), TestError> {
    let mut service = ApplicationService::new();
    let admitted = service.execute(&ApplicationInput::RecoverLocal {
        correlation: CorrelationId(13),
        pin: pin(),
        bundle: bundle(),
        budget: budget(1, 1),
    });
    let ApplicationOutcome::Resolved(ReplyBody::ExecutionStarted {
        operation,
        transition,
    }) = admitted.outcome
    else {
        return Err(TestError::Unexpected(
            "recover should start a local operation",
        ));
    };
    let mut state = ShellState::default();
    apply(&mut state, admitted)?;
    assert_eq!(
        state.execution,
        ExecutionProjection::Started {
            operation,
            transition,
        }
    );
    assert_eq!(state.summaries()[2].state, ProjectionState::Accepted);

    let pending = service.execute(&ApplicationInput::PollExecution {
        correlation: CorrelationId(14),
        operation,
    });
    apply(&mut state, pending)?;
    assert!(matches!(
        state.execution,
        ExecutionProjection::Reported {
            state: ExecutionState::Pending { operation: observed, .. },
        } if observed == operation
    ));
    assert_eq!(state.summaries()[2].state, ProjectionState::Active);

    let completed = service.execute(&ApplicationInput::PollExecution {
        correlation: CorrelationId(15),
        operation,
    });
    apply(&mut state, completed)?;
    assert!(matches!(
        state.execution,
        ExecutionProjection::Reported {
            state: ExecutionState::Completed { operation: observed, .. },
        } if observed == operation
    ));
    assert_eq!(state.summaries()[2].state, ProjectionState::Ready);
    Ok(())
}

#[test]
fn health_preserves_all_six_capability_facts() -> Result<(), TestError> {
    let facts = [
        CapabilityHealth::LocalReady(Capability::CompilerRegistry),
        CapabilityHealth::Unavailable(Capability::CompilerOutput),
        CapabilityHealth::Unavailable(Capability::Index),
        CapabilityHealth::Unavailable(Capability::Graph),
        CapabilityHealth::Unavailable(Capability::Vector),
        CapabilityHealth::Unavailable(Capability::LocalAnalyzer),
    ];
    let incoming = reply(20, &ReplyBody::Health(facts));
    let mut state = ShellState::default();
    apply(&mut state, incoming)?;

    assert_eq!(
        state.health,
        HealthProjection::Reported {
            facts,
            result: ApplicationDisposition::Partial {
                emitted: 1,
                unavailable: Capability::CompilerOutput,
            },
        }
    );
    assert!(matches!(
        state.health,
        HealthProjection::Reported { facts: observed, .. } if observed == facts
    ));
    assert_eq!(
        state.summaries()[6].state,
        ProjectionState::Degraded(Capability::CompilerOutput)
    );
    assert_eq!(
        state.last_reply.as_ref().map(|reply| reply.correlation),
        Some(CorrelationId(20))
    );
    Ok(())
}

#[test]
fn unavailable_lower_plane_is_typed_and_bounded() -> Result<(), TestError> {
    let incoming = reply(
        30,
        &ReplyBody::DependencyUnavailable {
            capability: Capability::Vector,
        },
    );
    let mut state = ShellState::default();
    apply(&mut state, incoming)?;
    assert_eq!(
        state.summaries()[5].state,
        ProjectionState::Degraded(Capability::Vector)
    );
    Ok(())
}

#[test]
fn rejected_diagnostic_is_retained_without_erasing_its_code() -> Result<(), TestError> {
    let incoming = ApplicationReply {
        correlation: CorrelationId(40),
        outcome: ApplicationOutcome::Failed {
            diagnostic: Diagnostic {
                code: DiagnosticCode::DependencyUnavailable,
                detail: DiagnosticDetail::Capability(Capability::Index),
            },
        },
    };
    let mut state = ShellState::default();
    apply(&mut state, incoming)?;
    assert!(matches!(
        state.last_reply.as_ref(),
        Some(ApplicationReply {
            outcome: ApplicationOutcome::Failed {
                diagnostic: Diagnostic {
                    code: DiagnosticCode::DependencyUnavailable,
                    detail: DiagnosticDetail::Capability(Capability::Index),
                }
            },
            ..
        })
    ));
    Ok(())
}

#[test]
fn oversized_batch_is_rejected_before_state_mutation() {
    let replies: [ApplicationReply; MAX_BATCH_REPLIES + 1] = core::array::from_fn(|_| {
        reply(
            50,
            &ReplyBody::DependencyUnavailable {
                capability: Capability::Index,
            },
        )
    });
    let mut state = ShellState::default();
    let result = state.apply_batch(replies);

    assert_eq!(
        result,
        Err(ApplyError::BatchTooLarge {
            limit: MAX_BATCH_REPLIES,
            actual: MAX_BATCH_REPLIES + 1,
        })
    );
    assert_eq!(state.last_reply, None);
    assert_eq!(state.notification_epoch, 0);
    assert_eq!(
        state.projection_error,
        Some(ApplyError::BatchTooLarge {
            limit: MAX_BATCH_REPLIES,
            actual: MAX_BATCH_REPLIES + 1,
        })
    );
}

#[test]
fn empty_boundary_is_fused_without_notification() -> Result<(), ApplyError> {
    let mut state = ShellState::default();
    let receipt = state.apply_batch([])?;
    assert_eq!(receipt.applied_replies, 0);
    assert_eq!(receipt.notifications, 0);
    assert_eq!(receipt.notification_epoch, 0);
    Ok(())
}
