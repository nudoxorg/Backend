//! Exercises the `interface-gui` tests gpui contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//! Deterministic actual-GPUI entity tests.

#[cfg(feature = "real-gpui")]
use compiler_vocabulary::{LanguageProfile, RustEdition, Stage};
#[cfg(feature = "real-gpui")]
use gpui::{
    AppContext, Bounds, Context, EntityInputHandler, IntoElement, Render, ScrollStrategy,
    TestAppContext, UniformListScrollHandle, Window, div, point, prelude::*, px, size,
    uniform_list,
};
#[cfg(feature = "real-gpui")]
use interface_core::{
    AdaptiveDisposition, ApplicationInput, ApplicationOutcome, ApplicationService, BatteryState,
    ByteCount, Capability, CapabilityDomain, CorrelationId, DiagnosticCode, ExecutionState,
    GenerateRequest, GenerateTarget, GenerationId, INPUT_TEXT_BYTES, InconsistentRecovery,
    IndexSnapshotId, OperationBudget, OperationKey, Pin, Pressure, RecoveryCause, ReplyBody,
    ResourceBudget, RetryBudget, SourceText,
};
#[cfg(feature = "real-gpui")]
use interface_gui::{
    CommandId, DOCUMENT_ITEMS, DocumentSearchRow, DocumentSearchScope, FormField, FormState,
    GpuiShellView, NativeTextInputError, PaletteEditError, ProjectionState, Route, TextInputTarget,
};

#[cfg(feature = "real-gpui")]
struct VirtualCatalogFixture {
    entries: [usize; 200],
    selected: usize,
    first_rendered: usize,
    rendered_end: usize,
    scroll: UniformListScrollHandle,
}

#[cfg(feature = "real-gpui")]
impl VirtualCatalogFixture {
    fn new() -> Self {
        Self {
            entries: core::array::from_fn(|index| index),
            selected: 0,
            first_rendered: 0,
            rendered_end: 0,
            scroll: UniformListScrollHandle::new(),
        }
    }

    fn select_next_keyboard_row(&mut self) -> bool {
        let Some(index) = self
            .entries
            .iter()
            .position(|entry| *entry == self.selected)
        else {
            return false;
        };
        let Some(next) = self.entries.get(index.saturating_add(1)).copied() else {
            return false;
        };
        self.selected = next;
        self.scroll
            .scroll_to_item(index + 1, ScrollStrategy::Nearest);
        true
    }
}

#[cfg(feature = "real-gpui")]
impl Render for VirtualCatalogFixture {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.entries.len();
        uniform_list(
            "synthetic-command-catalog",
            count,
            cx.processor(|this, range: core::ops::Range<usize>, _, _| {
                this.first_rendered = range.start;
                this.rendered_end = range.end;
                range
                    .map(|index| {
                        div()
                            .id(("synthetic-command", this.entries[index]))
                            .h(px(28.0))
                            .child("Synthetic test command")
                    })
                    .collect()
            }),
        )
        .h(px(168.0))
        .track_scroll(&self.scroll)
    }
}

#[cfg(feature = "real-gpui")]
fn pin() -> Pin {
    Pin {
        generation: GenerationId::from_canonical_bytes(b"gpui-entity-generation"),
        snapshot: IndexSnapshotId::from_canonical_bytes(b"gpui-entity-snapshot"),
    }
}

#[cfg(feature = "real-gpui")]
fn bundle() -> interface_core::ContentId<CapabilityDomain> {
    interface_core::ContentId::from_canonical_bytes(b"gpui-entity-analyzer")
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
fn bounded_text(value: &str) -> Option<interface_core::InputText> {
    interface_core::InputText::try_from_str(value).ok()
}

#[cfg(feature = "real-gpui")]
fn assert_projected(result: &Result<(), interface_gui::ApplyError>) {
    assert!(result.is_ok());
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
fn assert_inconsistent_reply(view: &GpuiShellView, observed: Pin) {
    assert!(matches!(
        view.last_reply.as_ref().map(|reply| &reply.outcome),
        Some(ApplicationOutcome::Resolved(ReplyBody::Adaptive(AdaptiveDisposition::RetryRemote {
            cause: RecoveryCause::Inconsistent { observed: returned },
            ..
        }))) if *returned == observed
    ));
}

#[cfg(feature = "real-gpui")]
#[gpui::test]
fn entity_owns_service_and_completes_from_its_registered_wake(cx: &mut TestAppContext) {
    let (view, cx) =
        cx.add_window_view(|window, cx| GpuiShellView::new(ApplicationService::new(), window, cx));
    let recover = ApplicationInput::RecoverLocal {
        correlation: CorrelationId(91),
        pin: pin(),
        bundle: bundle(),
        budget: budget(1, 1),
    };
    let admitted_result = view.update(cx, |view, cx| view.execute(&recover, cx));
    assert_projected(&admitted_result);
    cx.read_entity(&view, |view, _| {
        assert!(matches!(
            view.last_reply.as_ref().map(|reply| &reply.outcome),
            Some(ApplicationOutcome::Resolved(
                ReplyBody::ExecutionStarted { .. }
            ))
        ));
        assert_eq!(view.summaries()[2].state, ProjectionState::Accepted);
        assert!(view.foreground_operation.is_some());
    });

    cx.run_until_parked();
    cx.read_entity(&view, |view, _| {
        assert_eq!(view.summaries()[2].state, ProjectionState::Ready);
        assert!(matches!(
            view.execution,
            interface_gui::ExecutionProjection::Reported {
                state: ExecutionState::Completed { .. },
            }
        ));
        assert_eq!(view.projection_error, None);
        assert_eq!(view.foreground_operation, None);
    });

    let (inconsistent, observed) = inconsistent_input();
    let inconsistent_result = view.update(cx, |view, cx| view.execute(&inconsistent, cx));
    assert_projected(&inconsistent_result);
    cx.read_entity(&view, |view, _| assert_inconsistent_reply(view, observed));
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
    assert_projected(&admitted_result);
    let operation = cx.read_entity(&view, |view, _| view.foreground_operation);
    assert!(operation.is_some());
    let Some(operation) = operation else {
        return;
    };

    let cancel_input = ApplicationInput::Cancel {
        correlation: CorrelationId(102),
        operation,
    };
    let cancelled_result = view.update(cx, |view, cx| view.execute(&cancel_input, cx));
    assert_projected(&cancelled_result);
    cx.read_entity(&view, |view, _| {
        assert!(matches!(
            view.last_reply.as_ref().map(|reply| &reply.outcome),
            Some(ApplicationOutcome::Resolved(ReplyBody::Execution(
                interface_core::ExecutionReply::Cancelled { operation: observed, .. }
            ))) if *observed == operation
        ));
    });
    cx.read_entity(&view, |view, _| {
        assert_eq!(view.summaries()[2].state, ProjectionState::Cancelled);
        assert_eq!(view.foreground_operation, None);
    });

    let health_input = ApplicationInput::Health {
        correlation: CorrelationId(103),
    };
    let health_result = view.update(cx, |view, cx| view.execute(&health_input, cx));
    assert_projected(&health_result);
    cx.read_entity(&view, |view, _| {
        assert!(matches!(
            view.last_reply.as_ref().map(|reply| &reply.outcome),
            Some(ApplicationOutcome::Resolved(ReplyBody::Health(facts)))
                if facts.contains(&interface_core::CapabilityHealth::Unavailable(
                    Capability::LocalAnalyzer,
                ))
        ));
    });
}

#[cfg(feature = "real-gpui")]
#[gpui::test]
fn stale_cancel_keeps_the_admitted_execution_wake_driver(cx: &mut TestAppContext) {
    let (view, cx) =
        cx.add_window_view(|window, cx| GpuiShellView::new(ApplicationService::new(), window, cx));
    let recover = ApplicationInput::RecoverLocal {
        correlation: CorrelationId(104),
        pin: pin(),
        bundle: bundle(),
        budget: budget(1, 1),
    };
    let admitted_result = view.update(cx, |view, cx| view.execute(&recover, cx));
    assert_projected(&admitted_result);
    let operation = cx.read_entity(&view, |view, _| view.foreground_operation);
    assert!(operation.is_some());
    let Some(operation) = operation else {
        return;
    };

    let stale_cancel = ApplicationInput::Cancel {
        correlation: CorrelationId(105),
        operation: OperationKey(operation.0.saturating_add(1)),
    };
    let stale_result = view.update(cx, |view, cx| view.execute(&stale_cancel, cx));
    assert_projected(&stale_result);
    cx.read_entity(&view, |view, _| {
        assert!(matches!(
            view.last_reply.as_ref().map(|reply| &reply.outcome),
            Some(ApplicationOutcome::Failed { diagnostic })
                if diagnostic.code == DiagnosticCode::OperationUnavailable
        ));
    });

    cx.run_until_parked();
    cx.read_entity(&view, |view, _| {
        assert!(matches!(
            view.execution,
            interface_gui::ExecutionProjection::Reported {
                state: ExecutionState::Completed {
                    operation: observed,
                    ..
                },
            } if observed == operation
        ));
        assert_eq!(view.summaries()[2].state, ProjectionState::Ready);
        assert_eq!(view.foreground_operation, None);
    });
}

#[cfg(feature = "real-gpui")]
#[gpui::test]
fn entity_projects_unavailable_compiler_output_without_claiming_artifact(cx: &mut TestAppContext) {
    let (view, cx) =
        cx.add_window_view(|window, cx| GpuiShellView::new(ApplicationService::new(), window, cx));
    let source = bounded_text("fn entity() {} ");
    assert!(source.is_some());
    let Some(source) = source else { return };
    let Ok(source) = SourceText::try_from(String::from(&*source)) else {
        return;
    };
    let input = ApplicationInput::Generate(GenerateRequest {
        target: GenerateTarget {
            correlation: CorrelationId(111),
            profile: LanguageProfile::Rust(RustEdition::Rust2024),
            stage: Stage::LowerIr,
        },
        source,
    });
    let result = view.update(cx, |view, cx| view.execute(&input, cx));
    assert_projected(&result);
    cx.read_entity(&view, |view, _| {
        assert!(matches!(
            view.last_reply.as_ref().map(|reply| &reply.outcome),
            Some(ApplicationOutcome::Resolved(
                ReplyBody::DependencyUnavailable {
                    capability: Capability::CompilerOutput,
                }
            ))
        ));
        assert_eq!(
            view.summaries()[0].state,
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
        assert!(view.navigation.palette.visible);
        assert_eq!(
            view.navigation.palette.selected,
            CommandId::OpenRoute(Route::Home)
        );
    });

    cx.simulate_keystrokes("down");
    cx.read_entity(&view, |view, _| {
        assert_eq!(
            view.navigation.palette.selected,
            CommandId::OpenRoute(Route::Libraries)
        );
    });

    cx.simulate_keystrokes("enter");
    cx.read_entity(&view, |view, _| {
        assert!(!view.navigation.palette.visible);
        assert_eq!(view.navigation.route, Route::Libraries);
    });
}

#[cfg(feature = "real-gpui")]
#[gpui::test]
fn entity_palette_filters_typed_text_without_a_polling_owner(cx: &mut TestAppContext) {
    let (view, cx) =
        cx.add_window_view(|window, cx| GpuiShellView::new(ApplicationService::new(), window, cx));

    cx.simulate_keystrokes("cmd-k");
    cx.simulate_input("s");
    cx.read_entity(&view, |view, _| {
        assert!(view.navigation.palette.visible);
        assert_eq!(view.navigation.palette.result_count(), 15);
        assert_eq!(
            view.navigation.palette.selected,
            CommandId::OpenRoute(Route::Libraries)
        );
    });

    cx.simulate_keystrokes("backspace");
    cx.read_entity(&view, |view, _| {
        // Every command, now including the keyboard map.
        assert_eq!(view.navigation.palette.result_count(), 26);
    });
}

#[cfg(feature = "real-gpui")]
fn find_character_index(
    view: &mut GpuiShellView,
    expected: usize,
    window: &mut Window,
    cx: &mut Context<GpuiShellView>,
) -> Option<usize> {
    for x in i16::MIN..=i16::MAX {
        let candidate = EntityInputHandler::character_index_for_point(
            view,
            point(px(f32::from(x)), px(0.0)),
            window,
            cx,
        );
        if candidate == Some(expected) {
            return candidate;
        }
    }
    None
}

#[cfg(feature = "real-gpui")]
#[gpui::test]
fn native_text_handler_normalizes_surrogate_selections_and_composition(cx: &mut TestAppContext) {
    let (view, cx) =
        cx.add_window_view(|window, cx| GpuiShellView::new(ApplicationService::new(), window, cx));
    cx.simulate_keystrokes("cmd-k");
    cx.simulate_input("😀x");

    let selection = view.update_in(cx, |view, window, cx| {
        EntityInputHandler::set_selected_text_range(view, 1..1, window, cx);
        EntityInputHandler::selected_text_range(view, false, window, cx)
    });
    assert_eq!(selection.map(|selection| selection.range), Some(0..0));

    let (composition_view, cx) =
        cx.add_window_view(|window, cx| GpuiShellView::new(ApplicationService::new(), window, cx));
    cx.simulate_keystrokes("cmd-k");
    let (composition_selection, marked_range) =
        composition_view.update_in(cx, |view, window, cx| {
            EntityInputHandler::replace_and_mark_text_in_range(
                view,
                None,
                "😀",
                Some(1..1),
                window,
                cx,
            );
            (
                EntityInputHandler::selected_text_range(view, false, window, cx),
                EntityInputHandler::marked_text_range(view, window, cx),
            )
        });
    assert_eq!(
        composition_selection.map(|selection| selection.range),
        Some(0..0)
    );
    assert_eq!(marked_range, Some(0..2));
}

#[cfg(feature = "real-gpui")]
#[gpui::test]
fn native_text_handler_maps_ranges_and_field_hit_tests(cx: &mut TestAppContext) {
    let (view, cx) =
        cx.add_window_view(|window, cx| GpuiShellView::new(ApplicationService::new(), window, cx));
    cx.simulate_keystrokes("cmd-k");
    cx.simulate_input("😀x");

    let (composition_bounds, left_index, right_index, clicked_index, text, adjusted) = view
        .update_in(cx, |view, window, cx| {
            let composition_bounds = EntityInputHandler::bounds_for_range(
                view,
                1..2,
                Bounds::new(point(px(10.0), px(20.0)), size(px(90.0), px(18.0))),
                window,
                cx,
            );
            let left_index = EntityInputHandler::character_index_for_point(
                view,
                point(px(-1_000_000.0), px(0.0)),
                window,
                cx,
            );
            let right_index = EntityInputHandler::character_index_for_point(
                view,
                point(px(1_000_000.0), px(0.0)),
                window,
                cx,
            );
            let clicked_index = find_character_index(view, 2, window, cx);
            if let Some(clicked_index) = clicked_index {
                EntityInputHandler::set_selected_text_range(
                    view,
                    clicked_index..clicked_index,
                    window,
                    cx,
                );
                EntityInputHandler::replace_text_in_range(view, None, "!", window, cx);
            }
            let mut adjusted = None;
            let text = EntityInputHandler::text_for_range(view, 0..4, &mut adjusted, window, cx);
            (
                composition_bounds,
                left_index,
                right_index,
                clicked_index,
                text,
                adjusted,
            )
        });

    let Some(composition_bounds) = composition_bounds else {
        assert!(composition_bounds.is_some());
        return;
    };
    assert_eq!(composition_bounds.origin, point(px(10.0), px(20.0)));
    assert_eq!(composition_bounds.size, size(px(60.0), px(18.0)));
    assert_eq!(left_index, Some(0));
    assert_eq!(right_index, Some(3));
    assert_eq!(clicked_index, Some(2));
    assert_eq!(text.as_deref(), Some("😀!x"));
    assert_eq!(adjusted, Some(0..4));
}

#[cfg(feature = "real-gpui")]
#[gpui::test]
fn native_text_handler_accepts_multibyte_palette_and_form_input(cx: &mut TestAppContext) {
    let (view, cx) =
        cx.add_window_view(|window, cx| GpuiShellView::new(ApplicationService::new(), window, cx));

    cx.simulate_keystrokes("cmd-k");
    cx.simulate_input("sé");
    cx.read_entity(&view, |view, _| {
        assert!(view.navigation.palette.visible);
        assert_eq!(view.navigation.palette.result_count(), 0);
    });
    cx.simulate_keystrokes("enter");
    cx.read_entity(&view, |view, _| {
        assert!(view.navigation.palette.visible);
        assert_eq!(view.navigation.palette.result_count(), 0);
    });

    cx.simulate_keystrokes("escape cmd-k");
    cx.simulate_input("generate");
    cx.simulate_keystrokes("enter");
    cx.simulate_input("rüst");
    cx.read_entity(&view, |view, _| {
        let Some(FormState::Generate { language, .. }) = view.form else {
            assert!(matches!(view.form, Some(FormState::Generate { .. })));
            return;
        };
        let Some(language) = language else {
            assert!(language.is_some());
            return;
        };
        assert_eq!(language.as_ref(), b"r\xC3\xBCst");
    });
}

#[cfg(feature = "real-gpui")]
#[gpui::test]
fn empty_palette_backspace_retains_a_visible_query_rejection(cx: &mut TestAppContext) {
    let (view, cx) =
        cx.add_window_view(|window, cx| GpuiShellView::new(ApplicationService::new(), window, cx));

    cx.simulate_keystrokes("cmd-k backspace");
    cx.read_entity(&view, |view, _| {
        assert_eq!(view.palette_error, Some(PaletteEditError::NothingToErase));
        assert!(view.navigation.palette.visible);
    });
}

#[cfg(feature = "real-gpui")]
#[gpui::test]
fn released_view_closes_the_admitted_completion_delivery(cx: &mut TestAppContext) {
    let (view, cx) =
        cx.add_window_view(|window, cx| GpuiShellView::new(ApplicationService::new(), window, cx));
    let recover = ApplicationInput::RecoverLocal {
        correlation: CorrelationId(119),
        pin: pin(),
        bundle: bundle(),
        budget: budget(1, 1),
    };
    let admitted = view.update(cx, |view, cx| view.execute(&recover, cx));
    assert_projected(&admitted);
    cx.read_entity(&view, |view, _| {
        assert!(matches!(
            view.last_reply.as_ref().map(|reply| &reply.outcome),
            Some(ApplicationOutcome::Resolved(
                ReplyBody::ExecutionStarted { .. }
            ))
        ));
    });

    cx.update(|window, _cx| window.remove_window());
    drop(view);
    cx.run_until_parked();
}

#[cfg(feature = "real-gpui")]
#[gpui::test]
fn native_text_handler_retains_exact_palette_and_form_overflow(cx: &mut TestAppContext) {
    const OVER_BOUND_INPUT: &str = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";

    let (palette_view, cx) =
        cx.add_window_view(|window, cx| GpuiShellView::new(ApplicationService::new(), window, cx));
    cx.simulate_keystrokes("cmd-k");
    cx.simulate_input(OVER_BOUND_INPUT);
    cx.read_entity(&palette_view, |view, _| {
        assert_eq!(
            view.input_error,
            Some(NativeTextInputError::InputTooLong {
                target: TextInputTarget::Palette,
                actual: OVER_BOUND_INPUT.len(),
                maximum: INPUT_TEXT_BYTES,
            })
        );
        assert_eq!(view.navigation.palette.result_count(), 0);
    });

    let (form_view, cx) =
        cx.add_window_view(|window, cx| GpuiShellView::new(ApplicationService::new(), window, cx));
    cx.simulate_keystrokes("cmd-k");
    cx.simulate_input("generate");
    cx.simulate_keystrokes("enter");
    cx.simulate_input(OVER_BOUND_INPUT);
    cx.read_entity(&form_view, |view, _| {
        assert_eq!(
            view.input_error,
            Some(NativeTextInputError::InputTooLong {
                target: TextInputTarget::Form(FormField::Language),
                actual: OVER_BOUND_INPUT.len(),
                maximum: INPUT_TEXT_BYTES,
            })
        );
        assert!(matches!(
            view.form_error,
            Some(interface_gui::FormError::InputTooLong {
                field: FormField::Language,
                actual,
                maximum: INPUT_TEXT_BYTES,
            }) if actual == OVER_BOUND_INPUT.len()
        ));
    });
}

#[cfg(feature = "real-gpui")]
#[gpui::test]
fn virtual_catalog_fixture_keeps_keyboard_identity_and_reveals_a_far_row(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_, _| VirtualCatalogFixture::new());

    cx.read_entity(&view, |view, _| {
        assert_eq!(view.entries.len(), 200);
        assert!(view.rendered_end > view.first_rendered);
        assert!(view.rendered_end - view.first_rendered < view.entries.len());
    });

    let reveal_requested = view.update(cx, |view, cx| {
        for _ in 0..199 {
            assert!(view.select_next_keyboard_row());
        }
        cx.notify();
        assert_eq!(view.selected, 199);
        matches!(
            view.scroll.0.borrow().deferred_scroll_to_item,
            Some(gpui::DeferredScrollToItem {
                item_index: 199,
                strategy: ScrollStrategy::Nearest,
                scroll_strict: false,
                ..
            })
        )
    });
    assert!(reveal_requested);
}

#[cfg(feature = "real-gpui")]
#[gpui::test]
fn entity_platform_settings_shortcuts_route_to_the_visible_destination(cx: &mut TestAppContext) {
    let (command_view, cx) =
        cx.add_window_view(|window, cx| GpuiShellView::new(ApplicationService::new(), window, cx));
    cx.simulate_keystrokes("cmd-,");
    cx.read_entity(&command_view, |view, _| {
        assert_eq!(view.navigation.route, Route::Settings);
    });

    let (control_view, cx) =
        cx.add_window_view(|window, cx| GpuiShellView::new(ApplicationService::new(), window, cx));
    cx.simulate_keystrokes("ctrl-,");
    cx.read_entity(&control_view, |view, _| {
        assert_eq!(view.navigation.route, Route::Settings);
    });
}

#[cfg(feature = "real-gpui")]
#[gpui::test]
fn entity_form_keyboard_navigation_and_escape_operate_on_typed_fields(cx: &mut TestAppContext) {
    let (view, cx) =
        cx.add_window_view(|window, cx| GpuiShellView::new(ApplicationService::new(), window, cx));

    cx.simulate_keystrokes("cmd-k g e n e r a t e enter");
    cx.read_entity(&view, |view, _| {
        assert!(matches!(
            view.form,
            Some(FormState::Generate {
                focused: FormField::Language,
                ..
            })
        ));
    });

    cx.simulate_keystrokes("enter");
    cx.read_entity(&view, |view, _| {
        assert_eq!(
            view.form_error,
            Some(interface_gui::FormError::MissingText(FormField::Language))
        );
    });

    cx.simulate_keystrokes("tab");
    cx.read_entity(&view, |view, _| {
        assert!(matches!(
            view.form,
            Some(FormState::Generate {
                focused: FormField::Stage,
                ..
            })
        ));
    });

    cx.simulate_keystrokes("shift-tab escape");
    cx.read_entity(&view, |view, _| {
        assert_eq!(view.form, None);
    });
}

#[cfg(feature = "real-gpui")]
#[gpui::test]
fn documentation_search_focus_ranks_and_opens_the_exact_symbol(cx: &mut TestAppContext) {
    let (view, cx) =
        cx.add_window_view(|window, cx| GpuiShellView::new(ApplicationService::new(), window, cx));

    cx.simulate_keystrokes("cmd-l");
    cx.simulate_input("Ir");
    cx.read_entity(&view, |view, _| {
        assert_eq!(view.navigation.route, Route::Search);
        assert_eq!(view.documentation.query_text(), "Ir");
        assert!(matches!(
            view.documentation.selected_search_row(),
            Some(DocumentSearchRow::Symbol(hit)) if DOCUMENT_ITEMS[hit.item].name == "Ir"
        ));
    });

    cx.simulate_keystrokes("enter");
    cx.read_entity(&view, |view, _| {
        assert_eq!(view.navigation.route, Route::Libraries);
        assert_eq!(DOCUMENT_ITEMS[view.documentation.selected_item].name, "Ir");
    });
}

#[cfg(feature = "real-gpui")]
#[gpui::test]
fn documentation_search_has_its_own_wide_ime_safe_query_boundary(cx: &mut TestAppContext) {
    let (view, cx) =
        cx.add_window_view(|window, cx| GpuiShellView::new(ApplicationService::new(), window, cx));
    let wide_query = "semantic-package-coordinate/".repeat(8);
    assert!(wide_query.len() > INPUT_TEXT_BYTES);

    cx.simulate_keystrokes("cmd-l");
    cx.simulate_input(&wide_query);
    cx.read_entity(&view, |view, _| {
        assert_eq!(view.documentation.query_text(), wide_query);
        assert_eq!(view.input_error, None);
    });

    let rejected = "x".repeat(interface_gui::MAX_DOCUMENT_QUERY_BYTES + 1);
    let (overflow_view, cx) =
        cx.add_window_view(|window, cx| GpuiShellView::new(ApplicationService::new(), window, cx));
    cx.simulate_keystrokes("cmd-l");
    cx.simulate_input(&rejected);
    cx.read_entity(&overflow_view, |view, _| {
        assert_eq!(
            view.documentation.query_text(),
            &rejected[..interface_gui::MAX_DOCUMENT_QUERY_BYTES]
        );
        assert_eq!(
            view.input_error,
            Some(NativeTextInputError::InputTooLong {
                target: TextInputTarget::DocumentationSearch,
                actual: rejected.len(),
                maximum: interface_gui::MAX_DOCUMENT_QUERY_BYTES,
            })
        );
    });
}

#[cfg(feature = "real-gpui")]
#[gpui::test]
fn the_palette_reaches_the_keyboard_map_by_name(cx: &mut TestAppContext) {
    let (view, cx) =
        cx.add_window_view(|window, cx| GpuiShellView::new(ApplicationService::new(), window, cx));

    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("keyboard");
    cx.read_entity(&view, |view, _| {
        assert!(view.navigation.palette.visible);
        assert_eq!(
            view.navigation.palette.result_count(),
            1,
            "typing the map's name should leave exactly one row"
        );
    });

    cx.simulate_keystrokes("enter");
    cx.read_entity(&view, |view, _| {
        assert_eq!(
            view.navigation.route,
            Route::Settings,
            "the map is rendered on the settings page"
        );
        assert!(!view.navigation.palette.visible, "the palette closed");
    });
}

#[gpui::test]
fn palette_selection_wraps_at_both_ends(cx: &mut TestAppContext) {
    // The form-field walk already wraps, so the palette clamping at the last
    // row disagreed with the rest of the shell.
    let (view, cx) =
        cx.add_window_view(|window, cx| GpuiShellView::new(ApplicationService::new(), window, cx));

    cx.simulate_keystrokes("ctrl-k");
    cx.read_entity(&view, |view, _| {
        assert_eq!(view.navigation.palette.selected_index(), Some(0));
    });

    cx.simulate_keystrokes("up");
    cx.read_entity(&view, |view, _| {
        assert_eq!(
            view.navigation.palette.selected_index(),
            Some(view.navigation.palette.result_count() - 1),
            "up from the first row reaches the last"
        );
    });

    cx.simulate_keystrokes("down");
    cx.read_entity(&view, |view, _| {
        assert_eq!(
            view.navigation.palette.selected_index(),
            Some(0),
            "down from the last row comes back to the first"
        );
    });
}

#[gpui::test]
fn every_documented_accelerator_is_really_bound(cx: &mut TestAppContext) {
    // The settings keyboard map stores the exact strings handed to
    // KeyBinding::new, so this asks the keymap itself whether each one
    // reaches the action the map claims. A shortcut renamed in one place and
    // not the other fails here rather than in front of a reader.
    let (_view, mut cx) =
        cx.add_window_view(|window, cx| GpuiShellView::new(ApplicationService::new(), window, cx));

    let unbound = cx.update(|_window, app| {
        let mut missing = Vec::new();
        for facts in interface_gui::KEYMAP {
            for raw in [facts.apple, facts.other] {
                let Ok(keystroke) = gpui::Keystroke::parse(raw) else {
                    missing.push(format!("{} <- {raw} (unparseable)", facts.action));
                    continue;
                };
                let bound = app
                    .all_bindings_for_input(&[keystroke])
                    .iter()
                    .any(|binding| binding.action().name().ends_with(facts.action));
                if !bound {
                    missing.push(format!("{} <- {raw}", facts.action));
                }
            }
        }
        missing
    });

    assert!(
        unbound.is_empty(),
        "the keyboard map documents accelerators that are not bound: {unbound:?}"
    );
}

#[gpui::test]
fn the_keyboard_map_binds_each_keystroke_once(cx: &mut TestAppContext) {
    let _ = cx;
    let mut seen: Vec<&str> = Vec::new();
    for facts in interface_gui::KEYMAP {
        // A keystroke identical on both platforms is one binding, not two.
        let mut keys = vec![facts.apple];
        if facts.other != facts.apple {
            keys.push(facts.other);
        }
        for raw in keys {
            assert!(
                !seen.contains(&raw),
                "{raw} is documented against more than one action"
            );
            seen.push(raw);
        }
        assert!(
            interface_gui::KEY_GROUPS.contains(&facts.group),
            "{} sits in a group the map never renders: {}",
            facts.action,
            facts.group
        );
    }
}

#[gpui::test]
fn removing_a_library_package_hides_it_and_keeps_the_reader_somewhere_real(
    cx: &mut TestAppContext,
) {
    let (view, cx) =
        cx.add_window_view(|window, cx| GpuiShellView::new(ApplicationService::new(), window, cx));

    // Package 3 sits outside the library until discovery puts it there.
    cx.simulate_keystrokes("cmd-shift-p");
    cx.simulate_input("trustfall");
    cx.simulate_keystrokes("enter");
    cx.read_entity(&view, |view, _| {
        assert!(view.documentation.library_packages[3]);
        assert_eq!(view.documentation.selected_package(), Some(3));
    });

    cx.simulate_keystrokes("cmd-backspace");
    cx.read_entity(&view, |view, _| {
        assert!(
            !view.documentation.library_packages[3],
            "the package left the library"
        );
        assert_ne!(
            view.documentation.selected_package(),
            Some(3),
            "the tree renders only library packages, so the selection cannot stay in one it removed"
        );
        assert!(
            view.documentation
                .selected_package()
                .is_some_and(|package| view.documentation.library_packages[package]),
            "the selection landed on a package still in the library"
        );
    });
}

#[gpui::test]
fn package_discovery_adds_a_search_match_to_the_collapsible_library(cx: &mut TestAppContext) {
    let (view, cx) =
        cx.add_window_view(|window, cx| GpuiShellView::new(ApplicationService::new(), window, cx));

    cx.simulate_keystrokes("cmd-shift-p");
    cx.simulate_input("trustfall");
    cx.read_entity(&view, |view, _| {
        assert_eq!(view.navigation.route, Route::Search);
        assert_eq!(view.documentation.scope, DocumentSearchScope::Packages);
        assert!(!view.documentation.library_packages[3]);
        assert_eq!(
            view.documentation.selected_search_row(),
            Some(DocumentSearchRow::Package(3))
        );
    });

    cx.simulate_keystrokes("enter cmd-b");
    cx.read_entity(&view, |view, _| {
        assert!(view.documentation.library_packages[3]);
        assert_eq!(view.navigation.route, Route::Libraries);
        assert!(view.documentation.sidebar_collapsed);
    });
}
