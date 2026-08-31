//! Deterministic actual-GPUI entity tests.

#[cfg(feature = "real-gpui")]
use gpui::{AppContext, TestAppContext};
#[cfg(feature = "real-gpui")]
use wave_application_core::{
    AdaptiveDisposition, ApplicationInput, ApplicationService, BatteryState, ByteCount, Capability,
    CapabilityDomain, CorrelationId, ExecutionState, GenerationId, InconsistentRecovery,
    IndexSnapshotId, OperationBudget, Pin, Pressure, RecoveryCause, ReplyBody, ResourceBudget,
    RetryBudget, Terminal,
};
#[cfg(feature = "real-gpui")]
use wave_application_gpui_shell::{CommandId, GpuiShellView, ProjectionState, Route};

#[cfg(feature = "real-gpui")]
fn pin() -> Pin {
    Pin {
        generation: GenerationId::from_canonical_bytes(b"gpui-entity-generation"),
        snapshot: IndexSnapshotId::from_canonical_bytes(b"gpui-entity-snapshot"),
    }
}

#[cfg(feature = "real-gpui")]
fn bundle() -> wave_application_core::ContentId<CapabilityDomain> {
    wave_application_core::ContentId::from_canonical_bytes(b"gpui-entity-analyzer")
}

#[cfg(feature = "real-gpui")]
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

#[cfg(feature = "real-gpui")]
fn bounded_text(value: &str) -> Option<wave_application_core::InputText> {
    wave_application_core::InputText::try_from_str(value).ok()
}

#[cfg(feature = "real-gpui")]
fn require_reply(
    result: &Result<
        wave_application_core::ApplicationReply,
        wave_application_gpui_shell::ApplyError,
    >,
) -> Option<wave_application_core::ApplicationReply> {
    assert!(result.is_ok());
    result.as_ref().ok().copied()
}

#[cfg(feature = "real-gpui")]
fn inconsistent_input() -> (ApplicationInput, Pin) {
    let observed = Pin {
        generation: GenerationId::from_canonical_bytes(b"gpui-observed-generation"),
        snapshot: IndexSnapshotId::from_canonical_bytes(b"gpui-observed-snapshot"),
    };
    (
        ApplicationInput::RecoverInconsistent(InconsistentRecovery {
            correlation: CorrelationId(94),
            expected: pin(),
            observed,
            bundle: bundle(),
            budget: budget(1, 1),
        }),
        observed,
    )
}

#[cfg(feature = "real-gpui")]
fn assert_inconsistent_reply(reply: &wave_application_core::ApplicationReply, observed: Pin) {
    assert!(matches!(
        reply.body,
        ReplyBody::Adaptive(AdaptiveDisposition::RetryRemote {
            cause: RecoveryCause::Inconsistent { observed: returned },
            ..
        }) if returned == observed
    ));
}

#[cfg(feature = "real-gpui")]
#[gpui::test]
fn entity_owns_service_and_projects_recover_pending_completed(cx: &mut TestAppContext) {
    let (view, cx) =
        cx.add_window_view(|window, cx| GpuiShellView::new(ApplicationService::new(), window, cx));
    let recover = ApplicationInput::RecoverLocal {
        correlation: CorrelationId(91),
        pin: pin(),
        bundle: bundle(),
        budget: budget(1, 1),
    };
    let admitted_result = view.update(cx, |view, cx| view.execute(&recover, cx));
    let Some(admitted) = require_reply(&admitted_result) else {
        return;
    };
    let ReplyBody::ExecutionStarted { operation, .. } = admitted.body else {
        assert!(matches!(admitted.body, ReplyBody::ExecutionStarted { .. }));
        return;
    };
    assert_eq!(admitted.terminal, Terminal::Accepted { operation });
    cx.read_entity(&view, |view, _| {
        assert_eq!(view.state().summaries()[2].state, ProjectionState::Accepted);
    });

    let pending_input = ApplicationInput::PollExecution {
        correlation: CorrelationId(92),
        operation,
    };
    let pending_result = view.update(cx, |view, cx| view.execute(&pending_input, cx));
    let Some(pending) = require_reply(&pending_result) else {
        return;
    };
    assert!(matches!(
        pending.body,
        ReplyBody::Execution(ExecutionState::Pending { operation: observed, .. })
            if observed == operation
    ));
    assert_eq!(pending.terminal, Terminal::Accepted { operation });
    cx.read_entity(&view, |view, _| {
        assert_eq!(view.state().summaries()[2].state, ProjectionState::Active);
    });

    let complete_input = ApplicationInput::PollExecution {
        correlation: CorrelationId(93),
        operation,
    };
    let completed_result = view.update(cx, |view, cx| view.execute(&complete_input, cx));
    let Some(completed) = require_reply(&completed_result) else {
        return;
    };
    assert!(matches!(
        completed.body,
        ReplyBody::Execution(ExecutionState::Completed { operation: observed, .. })
            if observed == operation
    ));
    assert_eq!(completed.terminal, Terminal::Complete { emitted: 1 });
    cx.read_entity(&view, |view, _| {
        assert_eq!(view.state().summaries()[2].state, ProjectionState::Ready);
    });

    let (inconsistent, observed) = inconsistent_input();
    let inconsistent_result = view.update(cx, |view, cx| view.execute(&inconsistent, cx));
    let Some(inconsistent_reply) = require_reply(&inconsistent_result) else {
        return;
    };
    assert_inconsistent_reply(&inconsistent_reply, observed);
}

#[cfg(feature = "real-gpui")]
#[gpui::test]
fn entity_cancellation_projects_cancelled_and_keeps_bundle_inactive(cx: &mut TestAppContext) {
    let (view, cx) =
        cx.add_window_view(|window, cx| GpuiShellView::new(ApplicationService::new(), window, cx));
    let recover = ApplicationInput::RecoverLocal {
        correlation: CorrelationId(101),
        pin: pin(),
        bundle: bundle(),
        budget: budget(1, 1),
    };
    let admitted_result = view.update(cx, |view, cx| view.execute(&recover, cx));
    let Some(admitted) = require_reply(&admitted_result) else {
        return;
    };
    let ReplyBody::ExecutionStarted { operation, .. } = admitted.body else {
        assert!(matches!(admitted.body, ReplyBody::ExecutionStarted { .. }));
        return;
    };

    let cancel_input = ApplicationInput::Cancel {
        correlation: CorrelationId(102),
        operation,
    };
    let cancelled_result = view.update(cx, |view, cx| view.execute(&cancel_input, cx));
    let Some(cancelled) = require_reply(&cancelled_result) else {
        return;
    };
    assert!(matches!(
        cancelled.body,
        ReplyBody::Execution(ExecutionState::Cancelled { operation: observed, .. })
            if observed == operation
    ));
    assert_eq!(cancelled.terminal, Terminal::Cancelled { emitted: 0 });
    cx.read_entity(&view, |view, _| {
        assert_eq!(
            view.state().summaries()[2].state,
            ProjectionState::Cancelled
        );
    });

    let health_input = ApplicationInput::Health {
        correlation: CorrelationId(103),
    };
    let health_result = view.update(cx, |view, cx| view.execute(&health_input, cx));
    let Some(health) = require_reply(&health_result) else {
        return;
    };
    let ReplyBody::Health(facts) = health.body else {
        assert!(matches!(health.body, ReplyBody::Health(_)));
        return;
    };
    assert!(
        facts.contains(&wave_application_core::CapabilityHealth::Unavailable(
            Capability::LocalAnalyzer,
        ))
    );
}

#[cfg(feature = "real-gpui")]
#[gpui::test]
fn entity_projects_unavailable_compiler_output_without_claiming_artifact(cx: &mut TestAppContext) {
    let (view, cx) =
        cx.add_window_view(|window, cx| GpuiShellView::new(ApplicationService::new(), window, cx));
    let source = bounded_text("fn entity() {} ");
    let language = bounded_text("rust");
    let stage = bounded_text("parse");
    let package = bounded_text("demo");
    assert!(source.is_some());
    assert!(language.is_some());
    assert!(stage.is_some());
    assert!(package.is_some());
    let Some(source) = source else { return };
    let Some(language) = language else { return };
    let Some(stage) = stage else { return };
    let Some(package) = package else { return };
    let input = ApplicationInput::Generate {
        correlation: CorrelationId(111),
        language,
        stage,
        package,
        source,
    };
    let result = view.update(cx, |view, cx| view.execute(&input, cx));
    let Some(reply) = require_reply(&result) else {
        return;
    };
    assert!(matches!(
        reply.body,
        ReplyBody::DependencyUnavailable {
            capability: Capability::CompilerOutput,
        }
    ));
    assert_eq!(
        reply.terminal,
        Terminal::Degraded {
            emitted: 0,
            unavailable: Capability::CompilerOutput,
        }
    );
    cx.read_entity(&view, |view, _| {
        assert_eq!(
            view.state().summaries()[0].state,
            ProjectionState::Degraded(Capability::CompilerOutput)
        );
    });
}

#[cfg(feature = "real-gpui")]
#[gpui::test]
fn entity_keyboard_palette_keeps_selection_by_command_identity(cx: &mut TestAppContext) {
    let (view, cx) =
        cx.add_window_view(|window, cx| GpuiShellView::new(ApplicationService::new(), window, cx));

    cx.simulate_keystrokes("cmd-k");
    cx.read_entity(&view, |view, _| {
        assert!(view.state().navigation.palette.visible);
        assert_eq!(
            view.state().navigation.palette.selected,
            CommandId::OpenHome
        );
    });

    cx.simulate_keystrokes("down");
    cx.read_entity(&view, |view, _| {
        assert_eq!(
            view.state().navigation.palette.selected,
            CommandId::OpenLibraries
        );
    });

    cx.simulate_keystrokes("enter");
    cx.read_entity(&view, |view, _| {
        assert!(!view.state().navigation.palette.visible);
        assert_eq!(view.state().navigation.route, Route::Libraries);
    });
}
