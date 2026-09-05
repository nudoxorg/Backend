//! Defines view behavior for `interface-gui`, whose purpose is to render and control the unified application service through GPUI.
//! This module owns the view invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Real GPUI product shell for the typed application service.

mod input;

use crate::navigation::{
    action_label, command_facts, route_facts, surface_destination, surface_facts,
};
use crate::semantic_documents::project_generated_documents;
use crate::{
    ApplicationInput, ApplicationReply, ApplicationService, ApplyError, CommandId,
    ConfirmPaletteCommand, DOCUMENT_ITEMS, DOCUMENT_PACKAGES, DiscoverPackages, DismissForm,
    DismissPalette, DocumentFilter, DocumentKind, DocumentSearchRow, DocumentSearchScope,
    FocusDocumentationSearch, FormError, FormField, FormState, KEY_GROUPS, KEYMAP,
    NavigateDocumentBack, NavigateDocumentForward, NextFormField, OpenPalette, OpenSettings,
    PaletteDirection, PreviousFormField, RemoveLibraryPackage, ResultLimit, Route,
    SelectFirstPaletteCommand, SelectLastPaletteCommand, SelectNextPaletteCommand,
    SelectNextPalettePage, SelectPreviousPaletteCommand, SelectPreviousPalettePage, ServiceAction,
    ShellState, ToggleSidebar,
};
use core::ops::Deref;
use gpui::{
    Animation, AnimationExt, AnyElement, Context, FocusHandle, IntoElement, KeyBinding,
    KeyDownEvent, Render, ScrollStrategy, SharedString, Task, UniformListScrollHandle, Window, div,
    prelude::*, px, rgb, uniform_list,
};
use heart_observe::Probe;
use interface_core::{
    ApplicationEvent, ApplicationObservation, ApplicationOutcome, Capability, CompilerCapability,
    CorrelationId, Diagnostic, DiagnosticCode, DiagnosticDetail, ExecutionReply, ExecutionState,
    OperationKey, PackageCompileRequest, ReplyBody, UnavailableCompiler,
};
use std::{cell::RefCell, future::poll_fn, rc::Rc, time::Duration};

const SHELL_CONTEXT: &str = "interface-shell";
const RAIL_WIDTH: f32 = 276.0;
const COLLAPSED_RAIL_WIDTH: f32 = 58.0;
const OUTLINE_WIDTH: f32 = 210.0;
const TOP_BAR_HEIGHT: f32 = 58.0;
const PALETTE_WIDTH: f32 = 640.0;
const PALETTE_HEIGHT: f32 = 360.0;
const ROW_HEIGHT: f32 = 36.0;
const RAIL_BACKGROUND: u32 = 0x000d_1117;
const RAIL_TEXT: u32 = 0x00e6_edf3;
const PANEL_BACKGROUND: u32 = 0x0016_1d27;
const PANEL_HOVER: u32 = 0x001f_2937;
const METADATA_TEXT: u32 = 0x0095_a3b5;
const PAGE_BACKGROUND: u32 = 0x0010_151c;
const FOREGROUND: u32 = 0x00f0_f6fc;
const BORDER: u32 = 0x0029_3545;
const BORDER_SOFT: u32 = 0x001f_2937;
const ACCENT: u32 = 0x0067_e8f9;
const ACCENT_STRONG: u32 = 0x0022_d3ee;
const ACCENT_WASH: u32 = 0x0013_3342;
const CODE_BACKGROUND: u32 = 0x000b_1017;
const SUCCESS: u32 = 0x004a_de80;
const WARNING: u32 = 0x00fb_bd23;
const BACKDROP: u32 = 0x0000_0000;
const SELECTED_ROW: u32 = 0x0020_4050;
const HOVERED_ROW: u32 = 0x001b_2531;
const INTERACTION_DURATION: Duration = Duration::from_millis(90);

/// Entity-backed production GPUI view for the bounded application shell.
///
/// The view has one service owner. All domain state crosses the UI boundary as an
/// [`ApplicationInput`] and returns as an [`ApplicationReply`]; navigation and the palette only
/// project those facts. The one retained foreground task is woken by the admitted execution future;
/// no timer, polling loop, accessibility driver, or second command decoder participates in the
/// shell.
pub struct GpuiShellView<Compiler: CompilerCapability = UnavailableCompiler> {
    service: Rc<RefCell<ApplicationService<Compiler>>>,
    state: ShellState,
    focus: FocusHandle,
    palette_focus: FocusHandle,
    search_focus: FocusHandle,
    palette_scroll: UniformListScrollHandle,
    search_scroll: UniformListScrollHandle,
    search_active: bool,
    next_correlation: u64,
    driven_execution: Option<DrivenExecution>,
    driven_package: Option<DrivenPackage>,
    native_input: input::NativeInputState,
    native_input_bounds: input::SharedNativeInputGeometry,
}

struct DrivenExecution {
    operation: OperationKey,
    task: Task<CompletionDelivery>,
}

struct DrivenPackage {
    correlation: CorrelationId,
    task: Task<CompletionDelivery>,
}

enum PackageDelivery {
    Phase(ApplicationEvent),
    Documentation(crate::PackageDocumentationOutcome),
    Reply(ApplicationReply),
}

struct PackagePhaseProbe<Compiler> {
    delivery: async_channel::Sender<PackageDelivery>,
    compiler: Compiler,
}

impl<Compiler: CompilerCapability> Probe<ApplicationEvent> for PackagePhaseProbe<Compiler> {
    fn record_with<Build>(&mut self, build: Build)
    where
        Build: FnOnce() -> ApplicationEvent,
    {
        let event = build();
        if matches!(event.outcome, ApplicationObservation::PackagePhase { .. })
            && self
                .delivery
                .send_blocking(PackageDelivery::Phase(event))
                .is_err()
        {
            self.compiler.cancel_active();
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CompletionDelivery {
    Projected,
    ViewReleased,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExecutionDelivery {
    Drive(OperationKey),
    Clear(OperationKey),
    None,
}

fn execution_delivery(reply: &ApplicationReply) -> ExecutionDelivery {
    match &reply.outcome {
        ApplicationOutcome::Resolved(
            ReplyBody::ExecutionStarted { operation, .. }
            | ReplyBody::Execution(ExecutionReply::Pending { operation, .. }),
        ) => ExecutionDelivery::Drive(*operation),
        ApplicationOutcome::Resolved(ReplyBody::Execution(
            ExecutionReply::Completed { operation, .. }
            | ExecutionReply::Cancelled { operation, .. },
        ))
        | ApplicationOutcome::Failed {
            diagnostic:
                Diagnostic {
                    detail: DiagnosticDetail::Execution(ExecutionState::Failed { operation, .. }),
                    ..
                },
        } => ExecutionDelivery::Clear(*operation),
        ApplicationOutcome::Resolved(
            ReplyBody::Generated(_)
            | ReplyBody::DependencyUnavailable { .. }
            | ReplyBody::Health(_)
            | ReplyBody::Adaptive(_)
            | ReplyBody::Snapshot(_)
            | ReplyBody::Retrieval(_)
            | ReplyBody::IndexRemoved(_),
        )
        | ApplicationOutcome::Failed { .. } => ExecutionDelivery::None,
    }
}

impl<Compiler: CompilerCapability + Clone + Send + 'static> GpuiShellView<Compiler> {
    /// Creates a focused product shell around the service that owns application behavior.
    #[must_use]
    pub fn new(
        mut service: ApplicationService<Compiler>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.bind_keys([
            KeyBinding::new("cmd-k", OpenPalette, Some(SHELL_CONTEXT)),
            KeyBinding::new("ctrl-k", OpenPalette, Some(SHELL_CONTEXT)),
            KeyBinding::new("escape", DismissForm, Some(SHELL_CONTEXT)),
            KeyBinding::new("down", SelectNextPaletteCommand, Some(SHELL_CONTEXT)),
            KeyBinding::new("up", SelectPreviousPaletteCommand, Some(SHELL_CONTEXT)),
            KeyBinding::new("enter", ConfirmPaletteCommand, Some(SHELL_CONTEXT)),
            KeyBinding::new("home", SelectFirstPaletteCommand, Some(SHELL_CONTEXT)),
            KeyBinding::new("end", SelectLastPaletteCommand, Some(SHELL_CONTEXT)),
            KeyBinding::new("pageup", SelectPreviousPalettePage, Some(SHELL_CONTEXT)),
            KeyBinding::new("pagedown", SelectNextPalettePage, Some(SHELL_CONTEXT)),
            KeyBinding::new("cmd-,", OpenSettings, Some(SHELL_CONTEXT)),
            KeyBinding::new("ctrl-,", OpenSettings, Some(SHELL_CONTEXT)),
            KeyBinding::new("tab", NextFormField, Some(SHELL_CONTEXT)),
            KeyBinding::new("shift-tab", PreviousFormField, Some(SHELL_CONTEXT)),
            KeyBinding::new("cmd-l", FocusDocumentationSearch, Some(SHELL_CONTEXT)),
            KeyBinding::new("ctrl-l", FocusDocumentationSearch, Some(SHELL_CONTEXT)),
            KeyBinding::new("cmd-b", ToggleSidebar, Some(SHELL_CONTEXT)),
            KeyBinding::new("ctrl-b", ToggleSidebar, Some(SHELL_CONTEXT)),
            KeyBinding::new("cmd-shift-p", DiscoverPackages, Some(SHELL_CONTEXT)),
            KeyBinding::new("ctrl-shift-p", DiscoverPackages, Some(SHELL_CONTEXT)),
            KeyBinding::new("cmd-backspace", RemoveLibraryPackage, Some(SHELL_CONTEXT)),
            KeyBinding::new("ctrl-backspace", RemoveLibraryPackage, Some(SHELL_CONTEXT)),
            KeyBinding::new("alt-left", NavigateDocumentBack, Some(SHELL_CONTEXT)),
            KeyBinding::new("alt-right", NavigateDocumentForward, Some(SHELL_CONTEXT)),
        ]);
        let focus = cx.focus_handle();
        let palette_focus = cx.focus_handle();
        let search_focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut state = ShellState::default();
        let initial_health = service.execute(&ApplicationInput::Health {
            correlation: CorrelationId(1),
        });
        if let Err(error) = state.apply(initial_health) {
            debug_assert_eq!(state.projection_error, Some(error));
        }
        Self {
            service: Rc::new(RefCell::new(service)),
            state,
            focus,
            palette_focus,
            search_focus,
            palette_scroll: UniformListScrollHandle::new(),
            search_scroll: UniformListScrollHandle::new(),
            search_active: false,
            next_correlation: 2,
            driven_execution: None,
            driven_package: None,
            native_input: input::NativeInputState::default(),
            native_input_bounds: Rc::new(std::cell::Cell::new(None)),
        }
    }

    /// Executes one typed application command and projects its exact reply.
    ///
    /// # Errors
    ///
    /// Returns [`ApplyError::NotificationEpochExhausted`] only if the fixed notification epoch
    /// cannot advance.
    pub fn execute(
        &mut self,
        input: &ApplicationInput,
        cx: &mut Context<Self>,
    ) -> Result<(), ApplyError> {
        if let ApplicationInput::CompilePackage(request) = input {
            return self.drive_package(request.clone(), cx);
        }
        let reply = self.service.borrow_mut().execute(input);
        let delivery = execution_delivery(&reply);
        self.state.apply(reply)?;
        cx.notify();
        match delivery {
            ExecutionDelivery::Drive(operation) => {
                self.drive_admitted_execution(input.correlation(), operation, cx);
            }
            ExecutionDelivery::Clear(operation) => {
                if self
                    .driven_execution
                    .as_ref()
                    .is_some_and(|driven| driven.operation == operation)
                {
                    self.driven_execution = None;
                    self.state.set_foreground_operation(None);
                }
            }
            ExecutionDelivery::None => {}
        }
        Ok(())
    }

    fn drive_package(
        &mut self,
        request: PackageCompileRequest,
        cx: &mut Context<Self>,
    ) -> Result<(), ApplyError> {
        let correlation = request.target.correlation;
        if let Some(active) = self.driven_package.as_ref() {
            return self
                .state
                .retain_projection_error(ApplyError::PackageRequestInFlight {
                    active: active.correlation,
                    observed: correlation,
                });
        }
        let compiler = self.service.borrow().compiler.clone();
        let probe_compiler = compiler.clone();
        let (delivery, deliveries) = async_channel::bounded(1);
        let final_delivery = delivery.clone();
        let background = cx.background_executor().spawn(async move {
            let mut service = ApplicationService::with_compiler(compiler);
            let mut probe = PackagePhaseProbe {
                delivery,
                compiler: probe_compiler,
            };
            let reply =
                service.execute_observed(&ApplicationInput::CompilePackage(request), &mut probe);
            let generated = match &reply.outcome {
                ApplicationOutcome::Resolved(ReplyBody::Generated(artifact)) => Some(*artifact),
                ApplicationOutcome::Resolved(
                    ReplyBody::DependencyUnavailable { .. }
                    | ReplyBody::Health(_)
                    | ReplyBody::Adaptive(_)
                    | ReplyBody::ExecutionStarted { .. }
                    | ReplyBody::Execution(_)
                    | ReplyBody::Snapshot(_)
                    | ReplyBody::Retrieval(_)
                    | ReplyBody::IndexRemoved(_),
                )
                | ApplicationOutcome::Failed { .. } => None,
            };
            if let Some(artifact) = generated {
                let documentation =
                    project_generated_documents(&mut service.compiler, correlation, artifact);
                if final_delivery
                    .send_blocking(PackageDelivery::Documentation(documentation))
                    .is_err()
                {
                    service.compiler.cancel_active();
                    return;
                }
            }
            if final_delivery
                .send_blocking(PackageDelivery::Reply(reply))
                .is_err()
            {
                service.compiler.cancel_active();
            }
        });
        let task = cx.spawn(async move |this, cx| {
            while let Ok(delivered) = deliveries.recv().await {
                let update = this.update(cx, |view, cx| {
                    let result = match delivered {
                        PackageDelivery::Phase(ApplicationEvent {
                            correlation,
                            outcome: ApplicationObservation::PackagePhase { phase },
                        }) => view.state.apply_package_phase(correlation, phase),
                        PackageDelivery::Phase(_) => Ok(()),
                        PackageDelivery::Documentation(outcome) => {
                            view.state.apply_package_documentation(outcome)
                        }
                        PackageDelivery::Reply(reply) => view.state.apply(reply).map(|_| ()),
                    };
                    match result {
                        Ok(()) => cx.notify(),
                        Err(error) => {
                            debug_assert_eq!(view.state.projection_error, Some(error));
                            cx.notify();
                        }
                    }
                });
                if update.is_err() {
                    break;
                }
            }
            background.await;
            match this.update(cx, |view, cx| {
                if view
                    .driven_package
                    .as_ref()
                    .is_some_and(|driven| driven.correlation == correlation)
                {
                    let driven = view.driven_package.take();
                    if let Some(driven) = driven {
                        driven.task.detach();
                    }
                }
                cx.notify();
            }) {
                Ok(()) => CompletionDelivery::Projected,
                Err(view_released) => {
                    drop(view_released);
                    CompletionDelivery::ViewReleased
                }
            }
        });
        self.driven_package = Some(DrivenPackage { correlation, task });
        Ok(())
    }

    fn drive_admitted_execution(
        &mut self,
        correlation: CorrelationId,
        operation: OperationKey,
        cx: &mut Context<Self>,
    ) {
        if self
            .driven_execution
            .as_ref()
            .is_some_and(|driven| driven.operation == operation)
        {
            return;
        }
        let service = Rc::clone(&self.service);
        let task = cx.spawn(async move |this, cx| {
            let reply = poll_fn(|context| {
                service
                    .borrow_mut()
                    .poll_admitted_execution(correlation, operation, context)
            })
            .await;
            match this.update(cx, |view, cx| {
                if view
                    .driven_execution
                    .as_ref()
                    .is_some_and(|driven| driven.operation == operation)
                {
                    let driven = view.driven_execution.take();
                    if let Some(driven) = driven {
                        driven.task.detach();
                    }
                    view.state.set_foreground_operation(None);
                }
                match view.state.apply(reply) {
                    Ok(_receipt) => cx.notify(),
                    Err(error) => {
                        debug_assert_eq!(view.state.projection_error, Some(error));
                        cx.notify();
                    }
                }
            }) {
                Ok(()) => CompletionDelivery::Projected,
                Err(view_released) => {
                    drop(view_released);
                    CompletionDelivery::ViewReleased
                }
            }
        });
        self.driven_execution = Some(DrivenExecution { operation, task });
        self.state.set_foreground_operation(Some(operation));
    }

    fn open_palette(&mut self, _: &OpenPalette, window: &mut Window, cx: &mut Context<Self>) {
        self.search_active = false;
        self.state.open_palette();
        window.focus(&self.palette_focus, cx);
        cx.notify();
    }

    fn open_settings(&mut self, _: &OpenSettings, _: &mut Window, cx: &mut Context<Self>) {
        self.search_active = false;
        self.select_route(Route::Settings, cx);
    }

    fn focus_documentation_search(
        &mut self,
        _: &FocusDocumentationSearch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.dismiss_palette();
        self.state.cancel_form();
        self.state.select_route(Route::Search);
        self.search_active = true;
        window.focus(&self.search_focus, cx);
        cx.notify();
    }

    fn toggle_sidebar_action(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        self.state.toggle_sidebar();
        cx.notify();
    }

    fn remove_library_package_action(
        &mut self,
        _: &RemoveLibraryPackage,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.remove_selected_document_package();
        cx.notify();
    }

    fn discover_packages_action(
        &mut self,
        _: &DiscoverPackages,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.discover_packages();
        self.search_active = true;
        window.focus(&self.search_focus, cx);
        cx.notify();
    }

    fn navigate_document_back_action(
        &mut self,
        _: &NavigateDocumentBack,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.search_active = false;
        self.state.navigate_document_back();
        cx.notify();
    }

    fn navigate_document_forward_action(
        &mut self,
        _: &NavigateDocumentForward,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.search_active = false;
        self.state.navigate_document_forward();
        cx.notify();
    }

    fn dismiss_palette(&mut self, _: &DismissPalette, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.navigation.palette.visible {
            self.state.dismiss_palette();
        } else if self.search_active {
            self.search_active = false;
        } else {
            self.state.cancel_form();
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn dismiss_form(&mut self, _: &DismissForm, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.navigation.palette.visible {
            self.state.dismiss_palette();
        } else if self.search_active {
            self.search_active = false;
        } else {
            self.state.cancel_form();
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn next_form_field(&mut self, _: &NextFormField, _: &mut Window, cx: &mut Context<Self>) {
        if self.state.navigation.palette.visible {
            return;
        }
        match self.state.move_form_field(true) {
            Ok(()) => {}
            Err(error) => debug_assert_eq!(self.state.form_error, Some(error)),
        }
        cx.notify();
    }

    fn previous_form_field(
        &mut self,
        _: &PreviousFormField,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.navigation.palette.visible {
            return;
        }
        match self.state.move_form_field(false) {
            Ok(()) => {}
            Err(error) => debug_assert_eq!(self.state.form_error, Some(error)),
        }
        cx.notify();
    }

    fn select_next_palette_command(
        &mut self,
        _: &SelectNextPaletteCommand,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_palette(PaletteDirection::Next, cx);
    }

    fn select_previous_palette_command(
        &mut self,
        _: &SelectPreviousPaletteCommand,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_palette(PaletteDirection::Previous, cx);
    }

    fn select_first_palette_command(
        &mut self,
        _: &SelectFirstPaletteCommand,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_palette(PaletteDirection::First, cx);
    }

    fn select_last_palette_command(
        &mut self,
        _: &SelectLastPaletteCommand,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_palette(PaletteDirection::Last, cx);
    }

    fn select_next_palette_page(
        &mut self,
        _: &SelectNextPalettePage,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_palette(PaletteDirection::NextPage, cx);
    }

    fn select_previous_palette_page(
        &mut self,
        _: &SelectPreviousPalettePage,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_palette(PaletteDirection::PreviousPage, cx);
    }

    fn confirm_palette_command(
        &mut self,
        _: &ConfirmPaletteCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.navigation.palette.visible {
            if let Some(confirmed) = self.state.confirm_palette() {
                debug_assert_eq!(
                    self.state.navigation.route,
                    command_facts(confirmed).destination
                );
                window.focus(&self.focus, cx);
            }
            cx.notify();
        } else if self.search_active {
            match self.state.documentation.selected_search_row() {
                Some(DocumentSearchRow::Symbol(hit)) => {
                    self.state.select_document(hit.item);
                    self.search_active = false;
                    window.focus(&self.focus, cx);
                }
                Some(DocumentSearchRow::Package(package)) => {
                    self.state.set_document_package_added(package, true);
                    if let Some(facts) = DOCUMENT_PACKAGES.get(package) {
                        self.state.select_document(facts.first_item);
                    }
                    self.search_active = false;
                    window.focus(&self.focus, cx);
                }
                None => {}
            }
            cx.notify();
        } else {
            self.submit_active_form(cx);
        }
    }

    fn move_palette(&mut self, direction: PaletteDirection, cx: &mut Context<Self>) {
        if self.state.navigation.palette.visible
            && let Some(index) = self.state.move_palette_selection(direction)
        {
            self.palette_scroll
                .scroll_to_item(index, ScrollStrategy::Nearest);
        } else if self.search_active {
            let forward = matches!(
                direction,
                PaletteDirection::Next | PaletteDirection::NextPage | PaletteDirection::Last
            );
            let steps = if matches!(
                direction,
                PaletteDirection::NextPage | PaletteDirection::PreviousPage
            ) {
                crate::PALETTE_PAGE_ROWS
            } else {
                1
            };
            if direction == PaletteDirection::First {
                self.state.select_documentation_result(0);
            } else if direction == PaletteDirection::Last {
                let last = self
                    .state
                    .documentation
                    .search_rows()
                    .len()
                    .saturating_sub(1);
                self.state.select_documentation_result(last);
            } else {
                for _ in 0..steps {
                    self.state.move_documentation_result(forward);
                }
            }
            self.search_scroll.scroll_to_item(
                self.state.documentation.selected_result,
                ScrollStrategy::Nearest,
            );
        }
        cx.notify();
    }

    fn select_route(&mut self, route: Route, cx: &mut Context<Self>) {
        self.search_active = false;
        self.state.select_route(route);
        cx.notify();
    }

    fn select_action(&mut self, action: ServiceAction, cx: &mut Context<Self>) {
        self.state.select_action(action);
        cx.notify();
    }

    fn select_palette_command(
        &mut self,
        command: CommandId,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.select_palette_command(command);
        cx.notify();
    }

    fn update_palette_query(
        &mut self,
        event: &KeyDownEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.is_held {
            return;
        }
        if !self.state.navigation.palette.visible && self.search_active {
            if event.keystroke.key == "backspace" {
                let current = self.state.documentation.query_text();
                let shortened = current
                    .char_indices()
                    .next_back()
                    .map_or("", |(index, _)| &current[..index]);
                self.state.replace_documentation_query(shortened.to_owned());
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        if !self.state.navigation.palette.visible {
            self.update_form_text(event, cx);
            return;
        }
        if event.keystroke.key == "backspace" {
            match self.state.erase_palette_character() {
                Ok(()) => {}
                Err(error) => debug_assert_eq!(self.state.palette_error, Some(error)),
            }
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn update_form_text(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        if self.state.form.is_none() {
            return;
        }
        if event.keystroke.key == "backspace" {
            match self.state.erase_form_text() {
                Ok(()) => {}
                Err(error) => debug_assert_eq!(self.state.form_error, Some(error)),
            }
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn submit_active_form(&mut self, cx: &mut Context<Self>) {
        let correlation = CorrelationId(self.next_correlation);
        let input = self.state.submit_form(correlation);
        match input {
            Ok(input) => {
                self.next_correlation = self.next_correlation.saturating_add(1);
                if let Err(error) = self.execute(&input, cx) {
                    debug_assert_eq!(self.state.projection_error, Some(error));
                    cx.notify();
                }
            }
            Err(error) => {
                debug_assert_eq!(self.state.form_error, Some(error));
                cx.notify();
            }
        }
    }

    fn top_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let document = DOCUMENT_ITEMS[self.state.documentation.selected_item];
        let package = DOCUMENT_PACKAGES[document.package];
        let query = self.state.documentation.query_text();
        let query_label = if query.is_empty() {
            SharedString::from("Search packages, symbols, types, and docs…")
        } else {
            SharedString::from(query)
        };
        div()
            .id("documentation-top-bar")
            .h(px(TOP_BAR_HEIGHT))
            .px(px(14.0))
            .flex()
            .items_center()
            .border_b_1()
            .border_color(rgb(BORDER))
            .bg(rgb(RAIL_BACKGROUND))
            .text_color(rgb(FOREGROUND))
            .child(
                div()
                    .w_1_3()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .id("toggle-documentation-sidebar")
                            .size(px(34.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(7.0))
                            .cursor_pointer()
                            .hover(|style| style.bg(rgb(PANEL_HOVER)))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.state.toggle_sidebar();
                                cx.notify();
                            }))
                            .child("☰"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .id("document-history-back")
                                    .size(px(30.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(6.0))
                                    .text_color(if self.state.documentation.can_go_back() {
                                        rgb(FOREGROUND)
                                    } else {
                                        rgb(METADATA_TEXT).opacity(0.35)
                                    })
                                    .cursor_pointer()
                                    .hover(|style| style.bg(rgb(PANEL_HOVER)))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.search_active = false;
                                        this.state.navigate_document_back();
                                        cx.notify();
                                    }))
                                    .child("←"),
                            )
                            .child(
                                div()
                                    .id("document-history-forward")
                                    .size(px(30.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(6.0))
                                    .text_color(if self.state.documentation.can_go_forward() {
                                        rgb(FOREGROUND)
                                    } else {
                                        rgb(METADATA_TEXT).opacity(0.35)
                                    })
                                    .cursor_pointer()
                                    .hover(|style| style.bg(rgb(PANEL_HOVER)))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.search_active = false;
                                        this.state.navigate_document_forward();
                                        cx.notify();
                                    }))
                                    .child("→"),
                            ),
                    )
                    .child(
                        div()
                            .ml(px(5.0))
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .size(px(28.0))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(8.0))
                                    .bg(rgb(ACCENT_STRONG))
                                    .text_color(rgb(RAIL_BACKGROUND))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child("H"),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("Hummingbird Docs"),
                            ),
                    ),
            )
            .child(
                div()
                    .w_1_3()
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .id("global-documentation-search")
                            .track_focus(&self.search_focus)
                            .relative()
                            .h(px(38.0))
                            .w_full()
                            .max_w(px(620.0))
                            .px(px(12.0))
                            .flex()
                            .items_center()
                            .gap(px(9.0))
                            .rounded(px(10.0))
                            .border_1()
                            .border_color(if self.search_active {
                                rgb(ACCENT)
                            } else {
                                rgb(BORDER)
                            })
                            .bg(rgb(PAGE_BACKGROUND))
                            .cursor_text()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.state.select_route(Route::Search);
                                this.search_active = true;
                                window.focus(&this.search_focus, cx);
                                cx.notify();
                            }))
                            .child(div().text_color(rgb(ACCENT)).child("⌕"))
                            .child(
                                div()
                                    .relative()
                                    .flex_1()
                                    .truncate()
                                    .text_sm()
                                    .when(query.is_empty(), |text| {
                                        text.text_color(rgb(METADATA_TEXT))
                                    })
                                    .child(query_label)
                                    .when(self.search_active, |input| {
                                        input.child(self.native_input_bridge(
                                            crate::TextInputTarget::DocumentationSearch,
                                            &self.search_focus,
                                            cx,
                                        ))
                                    }),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .px(px(6.0))
                                    .py(px(2.0))
                                    .rounded(px(4.0))
                                    .bg(rgb(PANEL_BACKGROUND))
                                    .text_xs()
                                    .text_color(rgb(METADATA_TEXT))
                                    .child("⌘L"),
                            ),
                    ),
            )
            .child(
                div()
                    .w_1_3()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(8.0))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(rgb(METADATA_TEXT))
                            // One text run, not three: the .truncate() above
                            // ellipsizes a single run, and three children under
                            // block display stacked onto three lines.
                            .child(format!("{} / {}", package.name, document.name)),
                    )
                    .child(
                        div()
                            .id("top-add-package")
                            .h(px(36.0))
                            .flex_none()
                            .px(px(13.0))
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .rounded(px(8.0))
                            .bg(rgb(ACCENT_STRONG))
                            .text_color(rgb(RAIL_BACKGROUND))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .cursor_pointer()
                            .hover(|style| style.bg(rgb(ACCENT)))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.state.discover_packages();
                                this.search_active = true;
                                window.focus(&this.search_focus, cx);
                                cx.notify();
                            }))
                            .child("+")
                            .child("Add package"),
                    )
                    .when(!self.state.documentation.outline_visible, |bar| {
                        bar.child(
                            div()
                                .id("show-document-outline")
                                .h(px(32.0))
                                .flex_none()
                                .px(px(9.0))
                                .flex()
                                .items_center()
                                .rounded(px(6.0))
                                .border_1()
                                .border_color(rgb(BORDER))
                                .text_xs()
                                .text_color(rgb(METADATA_TEXT))
                                .cursor_pointer()
                                .hover(|style| {
                                    style.bg(rgb(PANEL_HOVER)).text_color(rgb(FOREGROUND))
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.state.set_documentation_outline_visible(true);
                                    cx.notify();
                                }))
                                .child("Outline"),
                        )
                    }),
            )
    }

    fn navigation_rail(&self, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.state.navigation.route;
        let collapsed = self.state.documentation.sidebar_collapsed;
        let mut rail = div()
            .id("application-rail")
            .w(px(if collapsed {
                COLLAPSED_RAIL_WIDTH
            } else {
                RAIL_WIDTH
            }))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .p(px(if collapsed { 8.0 } else { 12.0 }))
            .gap(px(8.0))
            .border_r_1()
            .border_color(rgb(BORDER_SOFT))
            .bg(rgb(RAIL_BACKGROUND))
            .text_color(rgb(RAIL_TEXT));

        rail = rail.child(div().flex().flex_col().gap(px(3.0)).children([
            Self::route_button(Route::Home, selected, collapsed, "⌂", cx),
            Self::route_button(Route::Search, selected, collapsed, "⌕", cx),
            Self::route_button(Route::Libraries, selected, collapsed, "◇", cx),
            Self::route_button(Route::Connections, selected, collapsed, "↗", cx),
            Self::route_button(Route::Settings, selected, collapsed, "⚙", cx),
        ]));

        if !collapsed {
            rail = rail
                .child(
                    div()
                        .mt(px(10.0))
                        .px(px(8.0))
                        .flex()
                        .items_center()
                        .justify_between()
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(rgb(METADATA_TEXT))
                        .child("YOUR LIBRARY")
                        .child(
                            div()
                                .id("rail-add-package")
                                .px(px(5.0))
                                .rounded(px(4.0))
                                .cursor_pointer()
                                .hover(|style| {
                                    style.bg(rgb(PANEL_HOVER)).text_color(rgb(FOREGROUND))
                                })
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.state.discover_packages();
                                    this.search_active = true;
                                    window.focus(&this.search_focus, cx);
                                    cx.notify();
                                }))
                                .child("+"),
                        ),
                )
                .child(self.package_tree(cx))
                .child(
                    div()
                        .id("open-command-palette")
                        .mt_auto()
                        .h(px(38.0))
                        .px(px(10.0))
                        .flex()
                        .items_center()
                        .justify_between()
                        .rounded(px(7.0))
                        .border_1()
                        .border_color(rgb(BORDER_SOFT))
                        .hover(|style| style.bg(rgb(PANEL_HOVER)))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.search_active = false;
                            this.state.open_palette();
                            window.focus(&this.palette_focus, cx);
                            cx.notify();
                        }))
                        .child(div().text_sm().child("Command palette"))
                        .child(div().text_xs().text_color(rgb(METADATA_TEXT)).child("⌘K")),
                );
        }
        rail.into_any_element()
    }

    fn package_tree(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("documentation-package-tree")
            .flex_1()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .overflow_y_scroll()
            .children(DOCUMENT_PACKAGES.iter().enumerate().filter_map(
                |(package_index, package)| {
                    if !self.state.documentation.library_packages[package_index] {
                        return None;
                    }
                    let expanded = self.state.documentation.expanded_packages[package_index];
                    let mut group = div().flex().flex_col().child(
                        div()
                            .id(("package-tree-header", package_index))
                            .h(px(32.0))
                            .px(px(7.0))
                            .flex()
                            .items_center()
                            .gap(px(7.0))
                            .rounded(px(6.0))
                            .cursor_pointer()
                            .hover(|style| style.bg(rgb(PANEL_HOVER)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.state.toggle_document_package(package_index);
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .w(px(10.0))
                                    .text_xs()
                                    .text_color(rgb(METADATA_TEXT))
                                    .child(if expanded { "▾" } else { "▸" }),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(package.name),
                            )
                            .child(
                                div()
                                    .ml_auto()
                                    .text_xs()
                                    .text_color(rgb(METADATA_TEXT))
                                    .child(package.item_count.to_string()),
                            )
                            .child(
                                // The row itself toggles disclosure, so this
                                // stops propagation; without it removing a
                                // package would also expand or collapse it on
                                // the way out.
                                div()
                                    .id(("package-tree-remove", package_index))
                                    .w(px(18.0))
                                    .h(px(18.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(4.0))
                                    .text_xs()
                                    .text_color(rgb(METADATA_TEXT))
                                    .hover(|style| {
                                        style.bg(rgb(PANEL_BACKGROUND)).text_color(rgb(FOREGROUND))
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.state.set_document_package_added(package_index, false);
                                        cx.notify();
                                    }))
                                    .child("×"),
                            ),
                    );
                    if expanded {
                        group = group.children(
                            (package.first_item..package.first_item + package.item_count).map(
                                |item_index| {
                                    let item = DOCUMENT_ITEMS[item_index];
                                    let selected = item_index
                                        == self.state.documentation.selected_item
                                        && self.state.navigation.route == Route::Libraries;
                                    div()
                                        .id(("package-tree-item", item_index))
                                        .h(px(29.0))
                                        .pl(px(24.0))
                                        .pr(px(7.0))
                                        .flex()
                                        .items_center()
                                        .gap(px(7.0))
                                        .rounded(px(6.0))
                                        .cursor_pointer()
                                        .when(selected, |row| {
                                            row.bg(rgb(ACCENT_WASH)).text_color(rgb(ACCENT))
                                        })
                                        .when(!selected, |row| {
                                            row.hover(|style| style.bg(rgb(PANEL_HOVER)))
                                        })
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.search_active = false;
                                            this.state.select_document(item_index);
                                            cx.notify();
                                        }))
                                        .child(Self::kind_badge(item.kind, true))
                                        .child(div().text_sm().overflow_hidden().child(item.name))
                                },
                            ),
                        );
                    }
                    Some(group)
                },
            ))
    }

    fn route_button(
        route: Route,
        selected: Route,
        collapsed: bool,
        icon: &'static str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<Compiler> {
        let facts = route_facts(route);
        div()
            .id(facts.element_id)
            .h(px(36.0))
            .px(px(if collapsed { 0.0 } else { 9.0 }))
            .flex()
            .items_center()
            .rounded(px(6.0))
            .cursor_pointer()
            .justify_center()
            .when(!collapsed, |row| row.justify_start())
            .when(selected == route, |style| {
                style.bg(rgb(ACCENT_WASH)).text_color(rgb(ACCENT))
            })
            .when(selected != route, |style| {
                style.hover(|hovered| hovered.bg(rgb(PANEL_BACKGROUND)))
            })
            .on_click(cx.listener(move |this, _, _, cx| this.select_route(route, cx)))
            .child(div().w(px(22.0)).text_center().child(icon))
            .when(!collapsed, |row| row.child(facts.label))
            .when(!collapsed, |row| {
                row.when_some(facts.shortcut, |row, shortcut| {
                    row.child(
                        div()
                            .flex()
                            .items_center()
                            .flex()
                            .items_center()
                            .ml_auto()
                            .text_sm()
                            .text_color(rgb(METADATA_TEXT))
                            .child(shortcut.apple)
                            .child(" · ")
                            .child(shortcut.other),
                    )
                })
            })
    }

    fn page(&self, cx: &mut Context<Self>) -> AnyElement {
        let route = self.state.navigation.route;
        let snapshot = self.route_snapshot(route, cx);
        let page = div()
            .id("application-page")
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(rgb(PAGE_BACKGROUND))
            .text_color(rgb(FOREGROUND))
            .child(snapshot)
            .when_some(self.state.form, |page, form| {
                page.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(rgb(BACKDROP).opacity(0.58))
                        .child(div().w(px(620.0)).child(self.action_form(form, cx))),
                )
            });
        page.into_any_element()
    }

    fn route_snapshot(&self, route: Route, cx: &mut Context<Self>) -> AnyElement {
        match route {
            Route::Home => self.home_snapshot(cx),
            Route::Libraries => self.libraries_snapshot(cx),
            Route::Search => self.search_snapshot(cx),
            Route::Connections => self.connections_snapshot(cx),
            Route::Settings => self.settings_snapshot(cx),
        }
    }

    fn home_snapshot(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("home-snapshot")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .p(px(30.0))
            .gap(px(28.0))
            .child(
                div()
                    .max_w(px(980.0))
                    .flex()
                    .flex_col()
                    .gap(px(13.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(Self::eyebrow("LOCAL-FIRST DOCUMENTATION"))
                            .child(Self::status_pill(
                                "compiler",
                                self.state.pages.home.generation,
                            )),
                    )
                    .child(
                        div()
                            .text_size(px(34.0))
                            .line_height(px(42.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Understand an entire codebase, not just one crate."),
                    )
                    .child(
                        div()
                            .max_w(px(760.0))
                            .text_size(px(16.0))
                            .line_height(px(25.0))
                            .text_color(rgb(METADATA_TEXT))
                            .child("Browse canonical signatures, documentation, semantic relationships, index authority, and source provenance in one fast native reader."),
                    )
                    .child(
                        div()
                            .mt(px(5.0))
                            .flex()
                            .gap(px(9.0))
                            .child(
                                div()
                                    .id("home-search-docs")
                                    .h(px(38.0))
                                    .px(px(14.0))
                                    .flex()
                                    .items_center()
                                    .gap(px(7.0))
                                    .rounded(px(8.0))
                                    .bg(rgb(ACCENT_STRONG))
                                    .text_color(rgb(RAIL_BACKGROUND))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.state.select_route(Route::Search);
                                        this.search_active = true;
                                        window.focus(&this.search_focus, cx);
                                        cx.notify();
                                    }))
                                    .child("⌕")
                                    .child("Search the index"),
                            )
                            .child(
                                div()
                                    .id("home-add-package")
                                    .h(px(38.0))
                                    .px(px(14.0))
                                    .flex()
                                    .items_center()
                                    .rounded(px(8.0))
                                    .border_1()
                                    .border_color(rgb(BORDER))
                                    .hover(|style| style.bg(rgb(PANEL_HOVER)))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.select_action(ServiceAction::CompilePackage, cx);
                                    }))
                                    .child("Compile a PURL"),
                            ),
                    ),
            )
            .child(
                div()
                    .max_w(px(1100.0))
                    .grid()
                    .grid_cols(3)
                    .gap(px(12.0))
                    .children([
                        Self::metric_card("20", "documented symbols", "Across five first-party packages"),
                        Self::metric_card("7", "semantic lanes", "Signatures, docs, types, links, source, versions, index"),
                        Self::metric_card("0", "fabricated capabilities", "Unavailable providers remain explicit"),
                    ]),
            )
            .when_some(self.state.pages.home.package_journey, |home, journey| {
                home.child(
                    div()
                        .max_w(px(1100.0))
                        .p(px(16.0))
                        .flex()
                        .items_center()
                        .justify_between()
                        .rounded(px(9.0))
                        .border_1()
                        .border_color(rgb(BORDER))
                        .bg(rgb(PANEL_BACKGROUND))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(4.0))
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child("Pinned package compilation"),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(rgb(METADATA_TEXT))
                                        .child(package_phase_label(journey.last_phase)),
                                ),
                        )
                        .child(Self::status_pill(
                            if journey.complete { "terminal" } else { "active" },
                            if journey.complete {
                                self.state.pages.home.generation
                            } else {
                                crate::ProjectionState::Active
                            },
                        )),
                )
            })
            .when_some(self.state.pages.home.generated, |home, generated| {
                home.child(
                    div()
                        .max_w(px(1100.0))
                        .child(Self::generated_details(&generated)),
                )
            })
            .when_some(
                self.state.package_documentation.as_ref(),
                |home, documentation| {
                    home.child(
                        div()
                            .max_w(px(1100.0))
                            .child(Self::semantic_document_details(documentation)),
                    )
                },
            )
            .child(
                div()
                    .max_w(px(1100.0))
                    .flex()
                    .flex_col()
                    .gap(px(11.0))
                    .child(Self::section_heading("Featured APIs", "Start from the semantic center of the workspace"))
                    .child(
                        div()
                            .grid()
                            .grid_cols(2)
                            .gap(px(10.0))
                            .children(
                                DOCUMENT_ITEMS
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, item)| item.featured)
                                    .take(6)
                                    .map(|(item, _)| self.document_card(item, cx)),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn libraries_snapshot(&self, cx: &mut Context<Self>) -> AnyElement {
        self.document_reader(cx)
    }

    fn search_snapshot(&self, cx: &mut Context<Self>) -> AnyElement {
        let rows = self.state.documentation.search_rows();
        let row_count = rows.len();
        let selected = self.state.documentation.selected_result;
        div()
            .id("search-snapshot")
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(
                div()
                    .px(px(26.0))
                    .pt(px(24.0))
                    .pb(px(14.0))
                    .flex()
                    .flex_col()
                    .gap(px(14.0))
                    .border_b_1()
                    .border_color(rgb(BORDER_SOFT))
                    .child(
                        div()
                            .flex()
                            .items_end()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(4.0))
                                    .child(
                                        div()
                                            .text_size(px(24.0))
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child(if self.state.documentation.scope == DocumentSearchScope::Packages { "Add a package" } else { "Search documentation" }),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(rgb(METADATA_TEXT))
                                            .child(if self.state.documentation.scope == DocumentSearchScope::Packages { "Find packages already described by this workspace and pin them to your library." } else { "Package records and public symbols share one keyboard-first result surface." }),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .items_end()
                                    .gap(px(8.0))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .text_xs()
                                            .text_color(rgb(METADATA_TEXT))
                                            .child(row_count.to_string())
                                            .child(" results · ↑↓ to move · Enter to open"),
                                    )
                                    .when(
                                        self.state.documentation.scope
                                            != DocumentSearchScope::Packages,
                                        |controls| {
                                            controls.child(Self::action_button(
                                                ServiceAction::Search,
                                                "Query snapshot index",
                                                cx,
                                            ))
                                        },
                                    ),
                            ),
                    )
                    .child(self.search_scope_controls(cx))
                    .when(self.state.documentation.scope != DocumentSearchScope::Packages, |header| {
                        header.child(self.search_filter_controls(cx))
                    })
                    .child(self.index_provenance_strip()),
            )
            .when(row_count != 0, |search| {
                search.child(
                    uniform_list(
                        "documentation-search-results",
                        row_count,
                        cx.processor(move |this, range: core::ops::Range<usize>, _, cx| {
                            range
                                .filter_map(|index| {
                                    rows.get(index).copied().map(|row| {
                                        this.search_result_row(row, index, index == selected, cx)
                                    })
                                })
                                .collect()
                        }),
                    )
                    .track_scroll(&self.search_scroll)
                    .flex_1(),
                )
            })
            .when(row_count == 0, |search| {
                search.child(
                    div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .max_w(px(460.0))
                                .p(px(24.0))
                                .flex()
                                .flex_col()
                                .items_center()
                                .gap(px(9.0))
                                .rounded(px(12.0))
                                .border_1()
                                .border_color(rgb(BORDER))
                                .bg(rgb(PANEL_BACKGROUND))
                                .child(
                                    div()
                                        .text_size(px(24.0))
                                        .text_color(rgb(ACCENT))
                                        .child("⌕"),
                                )
                                .child(
                                    div()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child("No indexed match"),
                                )
                                .child(
                                    div()
                                        .text_center()
                                        .text_sm()
                                        .line_height(px(20.0))
                                        .text_color(rgb(METADATA_TEXT))
                                        .child("Try a package name, qualified symbol path, API kind, or a word from the documentation summary."),
                                ),
                        ),
                )
            })
            .into_any_element()
    }

    fn connections_snapshot(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("connections-snapshot")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .p(px(28.0))
            .gap(px(22.0))
            .child(Self::page_intro("Connections", "Bring the semantic documentation graph to editors, agents, and local tools without giving up authority or provenance."))
            .child(
                div()
                    .grid()
                    .grid_cols(3)
                    .gap(px(12.0))
                    .children([
                        Self::connection_card("MCP", "Model Context Protocol", "Expose package search and document lookup to connected clients.", self.state.pages.connections.health),
                        Self::connection_card("LOCAL", "Analyzer runtime", "Wake-driven local capability execution with no UI polling loop.", self.state.pages.connections.execution),
                        Self::connection_card("REMOTE", "Federated provider", "Optional graph and vector coverage under immutable snapshot authority.", self.state.pages.connections.placement),
                    ]),
            )
            .child(
                div()
                    .max_w(px(860.0))
                    .p(px(18.0))
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .rounded(px(10.0))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(PANEL_BACKGROUND))
                    .child(Self::section_heading("Capability controls", "Typed operations routed through the same application service as every other interface"))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(8.0))
                            .children([
                                Self::action_button(ServiceAction::Health, "Refresh health", cx),
                                Self::action_button(ServiceAction::RecoverLocal, "Recover locally", cx),
                                Self::action_button(ServiceAction::RecoverInconsistent, "Resolve mismatch", cx),
                                Self::action_button(ServiceAction::ReleaseLocal, "Release analyzer", cx),
                                Self::action_button(ServiceAction::PollExecution, "Observe operation", cx),
                                Self::action_button(ServiceAction::Cancel, "Cancel operation", cx),
                            ]),
                    ),
            )
            .into_any_element()
    }

    fn settings_snapshot(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("settings-snapshot")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .p(px(28.0))
            .gap(px(18.0))
            .child(Self::page_intro("Settings", "Tune the reading experience and inspect the exact application boundary without leaving the docs workspace."))
            .child(
                div()
                    .max_w(px(820.0))
                    .p(px(18.0))
                    .rounded(px(10.0))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(PANEL_BACKGROUND))
                    .child(Self::section_heading("Reading", "Motion affects discrete presentation changes only"))
                    .child(self.motion_selector(cx)),
            )
            .child(
                div()
                    .max_w(px(820.0))
                    .p(px(18.0))
                    .rounded(px(10.0))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(PANEL_BACKGROUND))
                    .child(Self::section_heading("Service diagnostics", "Source-preserving facts from interface-core"))
                    .child(Self::snapshot_row("settings-health", "Capability health", self.state.pages.settings.health))
                    .child(Self::diagnostic_row(self.state.last_reply.as_ref().and_then(|reply| {
                        if let ApplicationOutcome::Failed { diagnostic } = &reply.outcome { Some(diagnostic) } else { None }
                    })))
                    .child(Self::action_button(ServiceAction::Health, "Inspect capability health", cx)),
            )
            .child(
                div()
                    .max_w(px(820.0))
                    .p(px(18.0))
                    .rounded(px(10.0))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(PANEL_BACKGROUND))
                    .child(Self::section_heading(
                        "Keyboard",
                        "Every accelerator the shell binds, as bound",
                    ))
                    .child(Self::keyboard_map()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .id("settings-notification-epoch")
                    .text_sm()
                    .text_color(rgb(METADATA_TEXT))
                    .child("Coalesced UI epoch: ")
                    .child(self.state.pages.settings.notification_epoch.to_string()),
            )
            .into_any_element()
    }

    fn document_reader(&self, cx: &mut Context<Self>) -> AnyElement {
        let item_index = self.state.documentation.selected_item;
        let item = DOCUMENT_ITEMS[item_index];
        let package = DOCUMENT_PACKAGES[item.package];
        div()
            .id("documentation-reader")
            .size_full()
            .flex()
            .overflow_hidden()
            .child(
                div()
                    .id("documentation-content")
                    .flex_1()
                    .h_full()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .max_w(px(900.0))
                            .ml_auto()
                            .mr_auto()
                            .px(px(38.0))
                            .pt(px(28.0))
                            .pb(px(80.0))
                            .flex()
                            .flex_col()
                            .gap(px(22.0))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(7.0))
                                    .text_sm()
                                    .text_color(rgb(METADATA_TEXT))
                                    .child(
                                        div()
                                            .id("document-package-crumb")
                                            .cursor_pointer()
                                            .hover(|style| style.text_color(rgb(ACCENT)))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.state.select_document(package.first_item);
                                                cx.notify();
                                            }))
                                            .child(package.name),
                                    )
                                    .child("/")
                                    .child(item.path.split("::").nth(1).unwrap_or(item.name))
                                    .child(div().ml_auto().child(package.version)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(10.0))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(10.0))
                                            .child(Self::kind_badge(item.kind, false))
                                            .child(
                                                div()
                                                    .text_size(px(30.0))
                                                    .line_height(px(36.0))
                                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                                    .child(item.name),
                                            )
                                            .child(
                                                div()
                                                    .px(px(7.0))
                                                    .py(px(3.0))
                                                    .rounded(px(5.0))
                                                    .bg(rgb(ACCENT_WASH))
                                                    .text_xs()
                                                    .text_color(rgb(ACCENT))
                                                    .child("verified"),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(16.0))
                                            .line_height(px(24.0))
                                            .text_color(rgb(METADATA_TEXT))
                                            .child(item.summary),
                                    ),
                            )
                            .child(Self::signature_block(item.signature))
                            .child(Self::reader_section_title(
                                "Documentation",
                                "The canonical public contract",
                            ))
                            .children(item.paragraphs.iter().map(|paragraph| {
                                div()
                                    .max_w(px(720.0))
                                    .text_size(px(15.0))
                                    .line_height(px(25.0))
                                    .text_color(rgb(FOREGROUND))
                                    .child(*paragraph)
                            }))
                            .when(!item.members.is_empty(), |reader| {
                                reader
                                    .child(Self::reader_section_title(
                                        "Members",
                                        "Public fields, methods, and variants",
                                    ))
                                    .child(Self::member_table(item.members))
                            })
                            .when_some(item.example, |reader, example| {
                                reader
                                    .child(Self::reader_section_title(
                                        "Example",
                                        "A minimal usage path",
                                    ))
                                    .child(Self::code_block(example))
                            })
                            .when(!item.related.is_empty(), |reader| {
                                reader
                                    .child(Self::reader_section_title(
                                        "Related",
                                        "Follow semantic neighbors without losing context",
                                    ))
                                    .child(div().flex().flex_wrap().gap(px(8.0)).children(
                                        item.related.iter().map(|path| self.related_link(path, cx)),
                                    ))
                            })
                            .child(self.source_disclosure(item, cx)),
                    ),
            )
            .when(self.state.documentation.outline_visible, |reader| {
                reader.child(self.document_outline(item, cx))
            })
            .into_any_element()
    }

    fn document_outline(
        &self,
        item: crate::DocumentItem,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<Compiler> {
        div()
            .id("documentation-outline")
            .w(px(OUTLINE_WIDTH))
            .h_full()
            .flex_none()
            .p(px(18.0))
            .flex()
            .flex_col()
            .gap(px(7.0))
            .border_l_1()
            .border_color(rgb(BORDER_SOFT))
            .bg(rgb(RAIL_BACKGROUND))
            .child(
                div()
                    .mb(px(5.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(METADATA_TEXT))
                    .child("ON THIS PAGE")
                    .child(
                        div()
                            .id("hide-document-outline")
                            .cursor_pointer()
                            .hover(|style| style.text_color(rgb(FOREGROUND)))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.state.set_documentation_outline_visible(false);
                                cx.notify();
                            }))
                            .child("×"),
                    ),
            )
            .child(Self::outline_row("Documentation", true))
            .when(!item.members.is_empty(), |outline| outline.child(Self::outline_row("Members", false)))
            .when(item.example.is_some(), |outline| outline.child(Self::outline_row("Example", false)))
            .when(!item.related.is_empty(), |outline| outline.child(Self::outline_row("Related", false)))
            .child(Self::outline_row("Source", false))
            .child(
                div()
                    .mt_auto()
                    .p(px(10.0))
                    .rounded(px(8.0))
                    .bg(rgb(PANEL_BACKGROUND))
                    .text_xs()
                    .line_height(px(18.0))
                    .text_color(rgb(METADATA_TEXT))
                    .child("This outline is derived from the document shape and remains stable while content changes."),
            )
    }

    fn source_disclosure(
        &self,
        item: crate::DocumentItem,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<Compiler> {
        let expanded = self.state.documentation.source_expanded;
        div()
            .id("document-source-section")
            .mt(px(4.0))
            .rounded(px(9.0))
            .border_1()
            .border_color(rgb(BORDER))
            .overflow_hidden()
            .child(
                div()
                    .id("toggle-document-source")
                    .h(px(44.0))
                    .px(px(13.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(PANEL_HOVER)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.toggle_documentation_source();
                        cx.notify();
                    }))
                    .child(if expanded { "▾" } else { "▸" })
                    .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("Source"))
                    .child(div().ml_auto().text_sm().text_color(rgb(ACCENT)).child(item.source)),
            )
            .when(expanded, |section| {
                section.child(
                    div()
                        .p(px(14.0))
                        .border_t_1()
                        .border_color(rgb(BORDER))
                        .bg(rgb(CODE_BACKGROUND))
                        .font_family("monospace")
                        .text_sm()
                        .line_height(px(21.0))
                        .text_color(rgb(METADATA_TEXT))
                        .child("This location is repository-relative and backed by the symbol catalog. Source opening is enabled only for declared locations."),
                )
            })
    }

    fn related_link(&self, path: &&'static str, cx: &mut Context<Self>) -> AnyElement {
        let target = DOCUMENT_ITEMS
            .iter()
            .position(|candidate| candidate.path == *path);
        div()
            .id(("related-document-link", target.unwrap_or(usize::MAX)))
            .px(px(9.0))
            .py(px(6.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(PANEL_BACKGROUND))
            .text_sm()
            .text_color(rgb(ACCENT))
            .when(target.is_some(), |link| {
                let target = target.unwrap_or_default();
                link.cursor_pointer()
                    .hover(|style| style.bg(rgb(ACCENT_WASH)).border_color(rgb(ACCENT)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.state.select_document(target);
                        cx.notify();
                    }))
            })
            .child(*path)
            .into_any_element()
    }

    fn search_scope_controls(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(Self::control_label("SCOPE"))
            .children(
                [
                    DocumentSearchScope::Everything,
                    DocumentSearchScope::Symbols,
                    DocumentSearchScope::Packages,
                ]
                .map(|scope| {
                    let selected = self.state.documentation.scope == scope;
                    div()
                        .id(("documentation-search-scope", search_scope_id(scope)))
                        .px(px(9.0))
                        .py(px(5.0))
                        .rounded(px(6.0))
                        .text_sm()
                        .cursor_pointer()
                        .when(selected, |chip| {
                            chip.bg(rgb(ACCENT_WASH)).text_color(rgb(ACCENT))
                        })
                        .when(!selected, |chip| {
                            chip.text_color(rgb(METADATA_TEXT)).hover(|style| {
                                style.bg(rgb(PANEL_HOVER)).text_color(rgb(FOREGROUND))
                            })
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.state.select_documentation_scope(scope);
                            cx.notify();
                        }))
                        .child(scope.label())
                }),
            )
    }

    fn search_filter_controls(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(Self::control_label("KIND"))
            .children(
                [
                    DocumentFilter::All,
                    DocumentFilter::Types,
                    DocumentFilter::APIs,
                    DocumentFilter::Modules,
                ]
                .map(|filter| {
                    let selected = self.state.documentation.filter == filter;
                    div()
                        .id(("documentation-search-filter", document_filter_id(filter)))
                        .px(px(9.0))
                        .py(px(5.0))
                        .rounded(px(6.0))
                        .text_sm()
                        .cursor_pointer()
                        .when(selected, |chip| {
                            chip.bg(rgb(PANEL_HOVER)).text_color(rgb(FOREGROUND))
                        })
                        .when(!selected, |chip| {
                            chip.text_color(rgb(METADATA_TEXT))
                                .hover(|style| style.text_color(rgb(FOREGROUND)))
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.state.select_documentation_filter(filter);
                            cx.notify();
                        }))
                        .child(filter.label())
                }),
            )
    }

    fn index_provenance_strip(&self) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap(px(12.0))
            .text_xs()
            .text_color(rgb(METADATA_TEXT))
            .child(Self::mini_status("catalog", crate::ProjectionState::Ready))
            .child(Self::mini_status("exact", self.state.pages.search.exact))
            .child(Self::mini_status(
                "lexical",
                self.state.pages.search.lexical,
            ))
            .child(Self::mini_status("graph", self.state.pages.search.graph))
            .child(Self::mini_status("vector", self.state.pages.search.vector))
    }

    fn search_result_row(
        &self,
        row: DocumentSearchRow,
        row_index: usize,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match row {
            DocumentSearchRow::Package(package_index) => {
                let package = DOCUMENT_PACKAGES[package_index];
                let added = self.state.documentation.library_packages[package_index];
                div()
                    .id(("documentation-package-result", package_index))
                    .h(px(86.0))
                    .mx(px(18.0))
                    .px(px(14.0))
                    .flex()
                    .items_center()
                    .gap(px(13.0))
                    .border_b_1()
                    .border_color(rgb(BORDER_SOFT))
                    .cursor_pointer()
                    .when(selected, |result| result.bg(rgb(ACCENT_WASH)))
                    .when(!selected, |result| {
                        result.hover(|style| style.bg(rgb(HOVERED_ROW)))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.state.select_documentation_result(row_index);
                        this.state.set_document_package_added(package_index, true);
                        this.state.select_document(package.first_item);
                        this.search_active = false;
                        cx.notify();
                    }))
                    .child(
                        div()
                            .size(px(42.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(9.0))
                            .bg(rgb(PANEL_BACKGROUND))
                            .text_color(rgb(ACCENT))
                            .font_weight(gpui::FontWeight::BOLD)
                            .child("PKG"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .child(
                                        div()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child(package.name),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(METADATA_TEXT))
                                            .child(package.version),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .text_xs()
                                            .text_color(rgb(METADATA_TEXT))
                                            .child(package.item_count.to_string())
                                            .child(" symbols"),
                                    ),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(METADATA_TEXT))
                                    .child(package.summary),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap(px(5.0))
                                    .children(package.tags.iter().map(|tag| Self::tag(tag))),
                            ),
                    )
                    .child(
                        div()
                            .px(px(10.0))
                            .py(px(6.0))
                            .rounded(px(6.0))
                            .border_1()
                            .border_color(if added { rgb(BORDER) } else { rgb(ACCENT) })
                            .text_sm()
                            .text_color(if added {
                                rgb(METADATA_TEXT)
                            } else {
                                rgb(ACCENT)
                            })
                            .child(if added { "In library" } else { "+ Add" }),
                    )
                    .into_any_element()
            }
            DocumentSearchRow::Symbol(hit) => {
                let item = DOCUMENT_ITEMS[hit.item];
                let package = DOCUMENT_PACKAGES[item.package];
                div()
                    .id(("documentation-symbol-result", hit.item))
                    .h(px(82.0))
                    .mx(px(18.0))
                    .px(px(14.0))
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .border_b_1()
                    .border_color(rgb(BORDER_SOFT))
                    .cursor_pointer()
                    .when(selected, |result| result.bg(rgb(ACCENT_WASH)))
                    .when(!selected, |result| {
                        result.hover(|style| style.bg(rgb(HOVERED_ROW)))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.state.select_documentation_result(row_index);
                        this.state.select_document(hit.item);
                        this.search_active = false;
                        cx.notify();
                    }))
                    .child(Self::kind_badge(item.kind, false))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap(px(5.0))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .child(
                                        div()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child(item.name),
                                    )
                                    .child(
                                        div()
                                            .font_family("monospace")
                                            .text_xs()
                                            .text_color(rgb(METADATA_TEXT))
                                            .child(item.path),
                                    )
                                    .child(
                                        div()
                                            .ml_auto()
                                            .text_xs()
                                            .text_color(rgb(METADATA_TEXT))
                                            .child(package.name),
                                    ),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(METADATA_TEXT))
                                    .child(item.summary),
                            )
                            .child(
                                div()
                                    .font_family("monospace")
                                    .text_xs()
                                    .text_color(rgb(ACCENT))
                                    .overflow_hidden()
                                    .child(item.signature),
                            ),
                    )
                    .child(
                        div()
                            .px(px(7.0))
                            .py(px(4.0))
                            .rounded(px(5.0))
                            .bg(rgb(PANEL_BACKGROUND))
                            .text_xs()
                            .text_color(rgb(METADATA_TEXT))
                            .child(search_match_label(hit.score)),
                    )
                    .into_any_element()
            }
        }
    }

    fn document_card(&self, item_index: usize, cx: &mut Context<Self>) -> AnyElement {
        let item = DOCUMENT_ITEMS[item_index];
        let package = DOCUMENT_PACKAGES[item.package];
        div()
            .id(("featured-document", item_index))
            .min_h(px(118.0))
            .p(px(15.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .rounded(px(10.0))
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(PANEL_BACKGROUND))
            .cursor_pointer()
            .hover(|style| style.border_color(rgb(ACCENT)).bg(rgb(PANEL_HOVER)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.state.select_document(item_index);
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(Self::kind_badge(item.kind, true))
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(item.name),
                    )
                    .child(
                        div()
                            .ml_auto()
                            .text_xs()
                            .text_color(rgb(METADATA_TEXT))
                            .child(package.name),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .line_height(px(20.0))
                    .text_color(rgb(METADATA_TEXT))
                    .child(item.summary),
            )
            .child(
                div()
                    .mt_auto()
                    .font_family("monospace")
                    .text_xs()
                    .text_color(rgb(ACCENT))
                    .child("Open documentation →"),
            )
            .into_any_element()
    }

    fn kind_badge(kind: DocumentKind, compact: bool) -> impl IntoElement {
        let color = kind_color(kind);
        div()
            .w(px(if compact { 27.0 } else { 44.0 }))
            .h(px(if compact { 18.0 } else { 24.0 }))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(5.0))
            .bg(rgb(color).opacity(0.14))
            .text_xs()
            .font_family("monospace")
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(rgb(color))
            .child(if compact {
                kind_short_label(kind)
            } else {
                kind.label()
            })
    }

    fn signature_block(signature: &'static str) -> impl IntoElement {
        div()
            .p(px(15.0))
            .rounded(px(9.0))
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(CODE_BACKGROUND))
            .font_family("monospace")
            .text_size(px(14.0))
            .line_height(px(22.0))
            .text_color(rgb(ACCENT))
            .child(signature)
    }

    fn code_block(code: &'static str) -> impl IntoElement {
        div()
            .max_w(px(760.0))
            .p(px(16.0))
            .rounded(px(9.0))
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(CODE_BACKGROUND))
            .font_family("monospace")
            .text_sm()
            .line_height(px(22.0))
            .text_color(rgb(RAIL_TEXT))
            .child(code)
    }

    fn member_table(members: &'static [crate::DocumentMember]) -> impl IntoElement {
        div()
            .max_w(px(780.0))
            .rounded(px(9.0))
            .border_1()
            .border_color(rgb(BORDER))
            .overflow_hidden()
            .children(members.iter().enumerate().map(|(index, member)| {
                div()
                    .p(px(13.0))
                    .flex()
                    .flex_col()
                    .gap(px(5.0))
                    .when(index != 0, |row| {
                        row.border_t_1().border_color(rgb(BORDER_SOFT))
                    })
                    .bg(rgb(PANEL_BACKGROUND))
                    .child(
                        div()
                            .font_family("monospace")
                            .text_sm()
                            .text_color(rgb(ACCENT))
                            .child(member.signature),
                    )
                    .child(
                        div()
                            .text_sm()
                            .line_height(px(20.0))
                            .text_color(rgb(METADATA_TEXT))
                            .child(member.summary),
                    )
            }))
    }

    fn reader_section_title(title: &'static str, subtitle: &'static str) -> impl IntoElement {
        div()
            .mt(px(7.0))
            .flex()
            .items_baseline()
            .gap(px(10.0))
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(METADATA_TEXT))
                    .child(subtitle),
            )
    }

    fn outline_row(label: &'static str, active: bool) -> impl IntoElement {
        div()
            .h(px(30.0))
            .px(px(8.0))
            .flex()
            .items_center()
            .rounded(px(5.0))
            .text_sm()
            .when(active, |row| {
                row.bg(rgb(ACCENT_WASH)).text_color(rgb(ACCENT))
            })
            .when(!active, |row| {
                row.text_color(rgb(METADATA_TEXT))
                    .hover(|style| style.text_color(rgb(FOREGROUND)))
            })
            .child(label)
    }

    fn metric_card(
        value: &'static str,
        label: &'static str,
        note: &'static str,
    ) -> impl IntoElement {
        div()
            .min_h(px(116.0))
            .p(px(15.0))
            .flex()
            .flex_col()
            .gap(px(5.0))
            .rounded(px(10.0))
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(PANEL_BACKGROUND))
            .child(
                div()
                    .text_size(px(25.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(ACCENT))
                    .child(value),
            )
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(label),
            )
            .child(
                div()
                    .text_xs()
                    .line_height(px(18.0))
                    .text_color(rgb(METADATA_TEXT))
                    .child(note),
            )
    }

    /// Renders every bound accelerator, grouped, with its platform spellings.
    fn keyboard_map() -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .children(KEY_GROUPS.iter().map(|group| {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(div().text_xs().text_color(rgb(METADATA_TEXT)).child(*group))
                    .children(
                        KEYMAP
                            .iter()
                            .filter(|facts| facts.group == *group)
                            .map(|facts| {
                                let apple = present_keystroke(facts.apple, true);
                                let other = present_keystroke(facts.other, false);
                                // Only spell both when the platforms differ;
                                // "Enter · Enter" tells a reader nothing.
                                let keys = if apple == other {
                                    apple
                                } else {
                                    format!("{apple} · {other}")
                                };
                                div()
                                    .h(px(24.0))
                                    .flex()
                                    .items_center()
                                    .text_sm()
                                    .child(facts.description)
                                    .child(
                                        div()
                                            .ml_auto()
                                            .text_xs()
                                            .text_color(rgb(METADATA_TEXT))
                                            .child(keys),
                                    )
                            }),
                    )
            }))
    }

    fn section_heading(title: &'static str, subtitle: &'static str) -> impl IntoElement {
        div()
            .flex()
            .items_baseline()
            .gap(px(10.0))
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(METADATA_TEXT))
                    .child(subtitle),
            )
    }

    fn page_intro(title: &'static str, subtitle: &'static str) -> impl IntoElement {
        div()
            .max_w(px(820.0))
            .flex()
            .flex_col()
            .gap(px(7.0))
            .child(
                div()
                    .text_size(px(27.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(
                div()
                    .text_size(px(15.0))
                    .line_height(px(23.0))
                    .text_color(rgb(METADATA_TEXT))
                    .child(subtitle),
            )
    }

    fn connection_card(
        eyebrow: &'static str,
        title: &'static str,
        body: &'static str,
        state: crate::ProjectionState,
    ) -> impl IntoElement {
        div()
            .min_h(px(166.0))
            .p(px(16.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .rounded(px(10.0))
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(PANEL_BACKGROUND))
            .child(Self::eyebrow(eyebrow))
            .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
            .child(
                div()
                    .text_sm()
                    .line_height(px(20.0))
                    .text_color(rgb(METADATA_TEXT))
                    .child(body),
            )
            .child(div().mt_auto().child(Self::status_pill("status", state)))
    }

    fn eyebrow(label: &'static str) -> impl IntoElement {
        div()
            .text_xs()
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(rgb(ACCENT))
            .child(label)
    }

    fn control_label(label: &'static str) -> impl IntoElement {
        div()
            .w(px(48.0))
            .text_xs()
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(rgb(METADATA_TEXT))
            .child(label)
    }

    fn tag(tag: &&'static str) -> impl IntoElement {
        div()
            .px(px(5.0))
            .py(px(2.0))
            .rounded(px(4.0))
            .bg(rgb(PANEL_BACKGROUND))
            .text_xs()
            .text_color(rgb(METADATA_TEXT))
            .child(*tag)
    }

    fn mini_status(label: &'static str, state: crate::ProjectionState) -> impl IntoElement {
        let ready = matches!(state, crate::ProjectionState::Ready);
        div()
            .flex()
            .items_center()
            .gap(px(5.0))
            .child(div().size(px(6.0)).rounded_full().bg(rgb(if ready {
                SUCCESS
            } else {
                WARNING
            })))
            .child(label)
            .child(": ")
            .child(projection_label(state))
    }

    fn status_pill(label: &'static str, state: crate::ProjectionState) -> impl IntoElement {
        let ready = matches!(state, crate::ProjectionState::Ready);
        div()
            .px(px(7.0))
            .py(px(4.0))
            .flex()
            .items_center()
            .gap(px(5.0))
            .rounded(px(999.0))
            .bg(rgb(if ready { 0x0014_3226 } else { 0x0037_2c13 }))
            .text_xs()
            .text_color(rgb(if ready { SUCCESS } else { WARNING }))
            .child(label)
            .child(" · ")
            .child(projection_label(state))
    }

    fn motion_selector(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.state.motion;
        div()
            .id("settings-motion-preference")
            .flex()
            .items_center()
            .gap(px(4.0))
            .child("Motion")
            .children(
                [
                    (
                        crate::MotionPreference::Standard,
                        "Standard",
                        "motion-standard",
                    ),
                    (
                        crate::MotionPreference::Reduced,
                        "Reduced",
                        "motion-reduced",
                    ),
                    (crate::MotionPreference::None, "None", "motion-none"),
                ]
                .map(|(motion, label, id)| {
                    div()
                        .id(id)
                        .h(px(28.0))
                        .px(px(8.0))
                        .flex()
                        .items_center()
                        .rounded(px(4.0))
                        .when(selected == motion, |button| button.bg(rgb(SELECTED_ROW)))
                        .when(selected != motion, |button| {
                            button.bg(rgb(PANEL_BACKGROUND))
                        })
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.state.set_motion_preference(motion);
                            cx.notify();
                        }))
                        .child(label)
                }),
            )
    }

    fn snapshot_row(
        id: &'static str,
        label: &'static str,
        state: crate::ProjectionState,
    ) -> impl IntoElement + use<Compiler> {
        div()
            .id(id)
            .h(px(32.0))
            .px(px(10.0))
            .flex()
            .items_center()
            .justify_between()
            .rounded(px(5.0))
            .bg(rgb(PAGE_BACKGROUND))
            .child(label)
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(METADATA_TEXT))
                    .child(projection_label(state)),
            )
    }

    fn generated_details(generated: &crate::GeneratedProjection) -> impl IntoElement {
        let artifact = generated.artifact;
        div()
            .id("home-generated-artifact")
            .p(px(10.0))
            .rounded(px(5.0))
            .bg(rgb(PAGE_BACKGROUND))
            .child(div().text_sm().child("Published generated artifact"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .text_sm()
                    .text_color(rgb(METADATA_TEXT))
                    .child("Source bytes: ")
                    .child(artifact.source.byte_len.to_string()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .text_sm()
                    .text_color(rgb(METADATA_TEXT))
                    .child("Fragment: ")
                    .child(artifact.fragment.to_string()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .text_sm()
                    .text_color(rgb(METADATA_TEXT))
                    .child("Manifest: ")
                    .child(artifact.publication.manifest.to_string()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .text_sm()
                    .text_color(rgb(METADATA_TEXT))
                    .child("Binding: ")
                    .child(artifact.publication.binding.to_string()),
            )
    }

    fn semantic_document_details(outcome: &crate::PackageDocumentationOutcome) -> AnyElement {
        match outcome {
            crate::PackageDocumentationOutcome::Rendered(projection) => div()
                .id("home-semantic-documents")
                .p(px(14.0))
                .flex()
                .flex_col()
                .gap(px(9.0))
                .rounded(px(9.0))
                .border_1()
                .border_color(rgb(BORDER))
                .bg(rgb(PANEL_BACKGROUND))
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("Verified package semantic documents"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(METADATA_TEXT))
                        .child(format!(
                            "{} entities · {} types · {} links · {} documentation fragments · {} canonical bytes",
                            projection.census.entities,
                            projection.census.types,
                            projection.census.links,
                            projection.census.documentation_fragments,
                            projection.encoded_bytes,
                        )),
                )
                .children(projection.documents.iter().take(12).map(|document| {
                    div()
                        .p(px(9.0))
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .rounded(px(6.0))
                        .bg(rgb(CODE_BACKGROUND))
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(ACCENT))
                                .child(format!("{:?}", document.entity)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .line_height(px(18.0))
                                .text_color(rgb(RAIL_TEXT))
                                .child(SharedString::from(&document.text)),
                        )
                }))
                .when(projection.documents.len() > 12, |documents| {
                    documents.child(
                        div()
                            .text_xs()
                            .text_color(rgb(METADATA_TEXT))
                            .child(format!(
                                "{} additional canonical documents retained",
                                projection.documents.len() - 12
                            )),
                    )
                })
                .into_any_element(),
            crate::PackageDocumentationOutcome::Failed(failure) => div()
                .id("home-semantic-document-failure")
                .p(px(14.0))
                .flex()
                .flex_col()
                .gap(px(6.0))
                .rounded(px(9.0))
                .border_1()
                .border_color(rgb(WARNING))
                .bg(rgb(PANEL_BACKGROUND))
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("Published image could not be projected"),
                )
                .child(
                    div()
                        .text_xs()
                        .line_height(px(18.0))
                        .text_color(rgb(METADATA_TEXT))
                        .child(format!("{:?}", failure.source)),
                )
                .into_any_element(),
        }
    }

    fn diagnostic_row(diagnostic: Option<&Diagnostic>) -> impl IntoElement + use<Compiler> {
        let (code, detail) = diagnostic_text(diagnostic);
        div()
            .id("settings-last-diagnostic")
            .p(px(10.0))
            .rounded(px(5.0))
            .bg(rgb(PAGE_BACKGROUND))
            .child(div().text_sm().child(code))
            .child(div().text_sm().text_color(rgb(METADATA_TEXT)).child(detail))
    }

    fn action_button(
        action: ServiceAction,
        label: &'static str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<Compiler> {
        div()
            .id(("application-action", action_element_id(action)))
            .h(px(32.0))
            .px(px(10.0))
            .flex()
            .items_center()
            .rounded(px(5.0))
            .bg(rgb(PANEL_BACKGROUND))
            .hover(|style| style.bg(rgb(PANEL_HOVER)))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| this.select_action(action, cx)))
            .child(label)
    }

    fn action_form(&self, form: FormState, cx: &mut Context<Self>) -> AnyElement {
        let action = form_action(form);
        div()
            .id(("typed-action-form", action_element_id(action)))
            .p(px(16.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(PANEL_BACKGROUND))
            .child(div().text_lg().child(action_label(action)))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(METADATA_TEXT))
                    .child(action_form_hint(action)),
            )
            .child(self.form_fields(form, cx))
            .child(Self::limit_selector(form, cx))
            .child(Self::form_error(
                self.state.form_error,
                self.state.input_error,
            ))
            .child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .child(
                        div()
                            .id("submit-typed-action")
                            .h(px(32.0))
                            .px(px(10.0))
                            .flex()
                            .items_center()
                            .rounded(px(5.0))
                            .bg(rgb(SELECTED_ROW))
                            .hover(|style| style.bg(rgb(PANEL_HOVER)))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| this.submit_active_form(cx)))
                            .child("Submit"),
                    )
                    .child(
                        div()
                            .id("cancel-typed-action")
                            .h(px(32.0))
                            .px(px(10.0))
                            .flex()
                            .items_center()
                            .rounded(px(5.0))
                            .bg(rgb(PANEL_BACKGROUND))
                            .hover(|style| style.bg(rgb(PANEL_HOVER)))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.state.cancel_form();
                                cx.notify();
                            }))
                            .child("Cancel"),
                    ),
            )
            .into_any_element()
    }

    fn form_fields(&self, form: FormState, cx: &mut Context<Self>) -> AnyElement {
        match form {
            FormState::Generate {
                focused,
                language,
                stage,
                source,
            } => self.generate_form_fields(focused, language, stage, source, cx),
            FormState::CompilePackage {
                focused,
                language,
                stage,
                package_url,
            } => self.package_form_fields(focused, language, stage, package_url, cx),
            FormState::Snapshot {
                focused, snapshot, ..
            } => self.snapshot_form_fields(focused, snapshot, cx),
            FormState::Search {
                focused,
                snapshot,
                query,
                ..
            } => div()
                .id("search-form-fields")
                .flex()
                .flex_col()
                .gap(px(4.0))
                .children([
                    self.form_field(FormField::Snapshot, "Snapshot", snapshot, focused, cx),
                    self.form_field(FormField::Query, "Query", query, focused, cx),
                ])
                .into_any_element(),
            FormState::Health => div()
                .id("health-form-fields")
                .text_sm()
                .text_color(rgb(METADATA_TEXT))
                .child("No arguments required.")
                .into_any_element(),
            FormState::Recovery { .. } => div()
                .id("recovery-form-fields")
                .text_sm()
                .text_color(rgb(METADATA_TEXT))
                .child("Waiting for a canonical pin, verified bundle, and resource budget.")
                .into_any_element(),
            FormState::Operation { operation, .. } => div()
                .id("operation-form-fields")
                .text_sm()
                .text_color(rgb(METADATA_TEXT))
                .child(operation.map_or(
                    "No active operation",
                    |_| "Actual service operation selected.",
                ))
                .into_any_element(),
        }
    }

    fn generate_form_fields(
        &self,
        focused: FormField,
        language: Option<interface_core::InputText>,
        stage: Option<interface_core::InputText>,
        source: Option<interface_core::InputText>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id("generate-form-fields")
            .flex()
            .flex_col()
            .gap(px(4.0))
            .children([
                self.form_field(FormField::Language, "Language", language, focused, cx),
                self.form_field(FormField::Stage, "Stage", stage, focused, cx),
                self.form_field(FormField::Source, "Source", source, focused, cx),
            ])
            .into_any_element()
    }

    fn package_form_fields(
        &self,
        focused: FormField,
        language: Option<interface_core::InputText>,
        stage: Option<interface_core::InputText>,
        package_url: Option<interface_core::InputText>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id("package-form-fields")
            .flex()
            .flex_col()
            .gap(px(4.0))
            .children([
                self.form_field(FormField::Language, "Language", language, focused, cx),
                self.form_field(FormField::Stage, "Stage", stage, focused, cx),
                self.form_field(
                    FormField::PackageUrl,
                    "Pinned package URL",
                    package_url,
                    focused,
                    cx,
                ),
            ])
            .into_any_element()
    }

    fn snapshot_form_fields(
        &self,
        focused: FormField,
        snapshot: Option<interface_core::InputText>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id("snapshot-form-fields")
            .child(self.form_field(FormField::Snapshot, "Snapshot", snapshot, focused, cx))
            .into_any_element()
    }

    fn form_field(
        &self,
        field: FormField,
        label: &'static str,
        value: Option<interface_core::InputText>,
        focused: FormField,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<Compiler> {
        div()
            .id(("typed-form-field", form_field_id(field)))
            .min_h(px(32.0))
            .py(px(5.0))
            .px(px(10.0))
            .flex()
            .items_center()
            .justify_between()
            .rounded(px(5.0))
            .when(focused == field, |row| row.bg(rgb(SELECTED_ROW)))
            .when(focused != field, |row| row.bg(rgb(PAGE_BACKGROUND)))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                match this.state.select_form_field(field) {
                    Ok(()) => {}
                    Err(error) => debug_assert_eq!(this.state.form_error, Some(error)),
                }
                cx.notify();
            }))
            .child(
                div().flex().flex_col().gap(px(1.0)).child(label).child(
                    div()
                        .text_xs()
                        .text_color(rgb(METADATA_TEXT))
                        .child(form_field_hint(field)),
                ),
            )
            .child(
                div()
                    .relative()
                    .text_sm()
                    .text_color(rgb(METADATA_TEXT))
                    .child(form_value_label(value))
                    .when(focused == field, |value| {
                        value.child(self.native_input_bridge(
                            crate::TextInputTarget::Form(field),
                            &self.focus,
                            cx,
                        ))
                    }),
            )
    }

    fn limit_selector(form: FormState, cx: &mut Context<Self>) -> AnyElement {
        let selected = match form {
            FormState::Snapshot { limit, .. } | FormState::Search { limit, .. } => Some(limit),
            FormState::Generate { .. }
            | FormState::CompilePackage { .. }
            | FormState::Health
            | FormState::Recovery { .. }
            | FormState::Operation { .. } => None,
        };
        let Some(selected) = selected else {
            return div().id("no-result-limit").into_any_element();
        };
        div()
            .id("typed-result-limit")
            .flex()
            .items_center()
            .gap(px(4.0))
            .child("Results")
            .children([1_u8, 2, 3, 4].map(|limit| {
                div()
                    .id(("result-limit", u64::from(limit)))
                    .h(px(28.0))
                    .px(px(8.0))
                    .flex()
                    .items_center()
                    .rounded(px(4.0))
                    .when(selected.0 == limit, |button| button.bg(rgb(SELECTED_ROW)))
                    .when(selected.0 != limit, |button| {
                        button.bg(rgb(PANEL_BACKGROUND))
                    })
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let result = ResultLimit::new(limit)
                            .and_then(|limit| this.state.replace_form_limit(limit));
                        match result {
                            Ok(()) => {}
                            Err(error) => {
                                this.state.retain_form_error(Some(error));
                                debug_assert_eq!(this.state.form_error, Some(error));
                            }
                        }
                        cx.notify();
                    }))
                    .child(limit.to_string())
            }))
            .into_any_element()
    }

    fn form_error(
        error: Option<FormError>,
        input_error: Option<crate::NativeTextInputError>,
    ) -> impl IntoElement + use<Compiler> {
        let label = match input_error.filter(native_error_targets_form) {
            Some(error) => native_input_error_label(error),
            None => SharedString::from(error.map_or("", form_error_label)),
        };
        div()
            .id("typed-form-error")
            .text_sm()
            .text_color(rgb(METADATA_TEXT))
            .child(label)
    }

    fn status_strip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let summaries = self.state.summaries();
        div()
            .id("application-status")
            .h(px(32.0))
            .px(px(16.0))
            .flex()
            .items_center()
            .gap(px(14.0))
            .bg(rgb(RAIL_BACKGROUND))
            .text_color(rgb(METADATA_TEXT))
            .children(summaries.into_iter().map(|summary| {
                let facts = surface_facts(summary.surface);
                div()
                    .flex()
                    .items_center()
                    .id(facts.element_id)
                    .cursor_pointer()
                    .hover(|style| style.text_color(rgb(FOREGROUND)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_route(surface_destination(summary.surface), cx);
                    }))
                    .text_sm()
                    .child(facts.label)
                    .child(": ")
                    .child(projection_label(summary.state))
            }))
    }

    fn palette(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let query_label = match self.state.input_error.filter(native_error_targets_palette) {
            Some(error) => native_input_error_label(error),
            None => match self.state.palette_error {
                Some(error) => palette_error_label(error),
                None => match self.state.navigation.palette.query_text() {
                    Some(query) => SharedString::from(query),
                    None => SharedString::from("Type to filter · ↑↓ Enter Esc"),
                },
            },
        };
        let palette = div()
            .id("command-palette-backdrop")
            .absolute()
            .size_full()
            .flex()
            .justify_center()
            .items_center()
            .bg(rgb(BACKDROP).opacity(0.35))
            .child(
                div()
                    .id("command-palette")
                    .track_focus(&self.palette_focus)
                    .w(px(PALETTE_WIDTH))
                    .h(px(PALETTE_HEIGHT))
                    .flex()
                    .flex_col()
                    .rounded(px(10.0))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(PANEL_BACKGROUND))
                    .text_color(rgb(FOREGROUND))
                    .child(self.palette_header(query_label, cx))
                    .child(self.palette_rows(cx)),
            );
        if self.state.motion == crate::MotionPreference::Standard {
            palette
                .with_animation(
                    "command-palette-open",
                    Animation::new(INTERACTION_DURATION),
                    |element, progress| element.opacity(0.85 + (0.15 * progress)),
                )
                .into_any_element()
        } else {
            palette.into_any_element()
        }
    }

    fn palette_header(&self, query: SharedString, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .h(px(56.0))
            .px(px(18.0))
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(rgb(BORDER))
            .child(div().text_lg().child("Command palette"))
            .child(
                div()
                    .relative()
                    .text_sm()
                    .text_color(rgb(METADATA_TEXT))
                    .child(query)
                    .child(self.native_input_bridge(
                        crate::TextInputTarget::Palette,
                        &self.palette_focus,
                        cx,
                    )),
            )
    }

    fn palette_rows(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.state.navigation.palette.result_count();
        let selected = self.state.navigation.palette.selected;
        uniform_list(
            "command-palette-results",
            count,
            cx.processor(move |this, range: core::ops::Range<usize>, _, cx| {
                range
                    .filter_map(|index| {
                        let command = this.state.navigation.palette.command_at(index)?;
                        let row_selected = command == selected;
                        let row = div()
                            .id(("command-palette-row", command_element_id(command)))
                            .h(px(ROW_HEIGHT))
                            .px(px(18.0))
                            .flex()
                            .items_center()
                            .rounded(px(4.0))
                            .cursor_pointer()
                            .when(row_selected, |style| style.bg(rgb(SELECTED_ROW)))
                            .when(!row_selected, |style| {
                                style.hover(|hovered| hovered.bg(rgb(HOVERED_ROW)))
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.select_palette_command(command, window, cx);
                            }))
                            .child(command_label(command))
                            .when_some(command_facts(command).shortcut, |row, shortcut| {
                                row.child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .ml_auto()
                                        .text_sm()
                                        .text_color(rgb(METADATA_TEXT))
                                        .child(shortcut.apple)
                                        .child(" · ")
                                        .child(shortcut.other),
                                )
                            });
                        if row_selected && this.state.motion == crate::MotionPreference::Standard {
                            Some(
                                row.with_animation(
                                    ("command-palette-selected", command_element_id(command)),
                                    Animation::new(INTERACTION_DURATION),
                                    |element, progress| {
                                        element
                                            .bg(rgb(SELECTED_ROW).opacity(0.75 + (0.25 * progress)))
                                    },
                                )
                                .into_any_element(),
                            )
                        } else {
                            Some(row.into_any_element())
                        }
                    })
                    .collect()
            }),
        )
        .track_scroll(&self.palette_scroll)
        .flex_1()
    }
}

impl<Compiler: CompilerCapability + Clone + Send + 'static> Render for GpuiShellView<Compiler> {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette_visible = self.state.navigation.palette.visible;
        div()
            .id("interface-shell")
            .key_context(SHELL_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::open_palette))
            .on_action(cx.listener(Self::dismiss_palette))
            .on_action(cx.listener(Self::dismiss_form))
            .on_action(cx.listener(Self::select_next_palette_command))
            .on_action(cx.listener(Self::select_previous_palette_command))
            .on_action(cx.listener(Self::confirm_palette_command))
            .on_action(cx.listener(Self::select_first_palette_command))
            .on_action(cx.listener(Self::select_last_palette_command))
            .on_action(cx.listener(Self::select_next_palette_page))
            .on_action(cx.listener(Self::select_previous_palette_page))
            .on_action(cx.listener(Self::open_settings))
            .on_action(cx.listener(Self::focus_documentation_search))
            .on_action(cx.listener(Self::toggle_sidebar_action))
            .on_action(cx.listener(Self::discover_packages_action))
            .on_action(cx.listener(Self::remove_library_package_action))
            .on_action(cx.listener(Self::navigate_document_back_action))
            .on_action(cx.listener(Self::navigate_document_forward_action))
            .on_action(cx.listener(Self::next_form_field))
            .on_action(cx.listener(Self::previous_form_field))
            .on_key_down(cx.listener(Self::update_palette_query))
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .child(self.top_bar(cx))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .overflow_hidden()
                    .child(self.navigation_rail(cx))
                    .child(self.page(cx)),
            )
            .child(self.status_strip(cx))
            .when(palette_visible, |shell| shell.child(self.palette(cx)))
    }
}

impl<Compiler: CompilerCapability> Deref for GpuiShellView<Compiler> {
    type Target = ShellState;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

impl<Compiler: CompilerCapability> Drop for GpuiShellView<Compiler> {
    fn drop(&mut self) {
        if self.driven_package.is_some()
            && let Ok(service) = self.service.try_borrow()
        {
            service.compiler.cancel_active();
        }
    }
}

/// Spells one raw GPUI keystroke the way a reader expects to see it.
///
/// The table stores exactly what `KeyBinding::new` was handed, so this is the
/// only place a presentation form exists and the two cannot disagree.
fn present_keystroke(raw: &str, apple: bool) -> String {
    raw.split('-')
        .map(|part| match part {
            "cmd" => "\u{2318}".to_owned(),
            "ctrl" => {
                if apple {
                    "\u{2303}".to_owned()
                } else {
                    "Ctrl".to_owned()
                }
            }
            "shift" => {
                if apple {
                    "\u{21e7}".to_owned()
                } else {
                    "Shift".to_owned()
                }
            }
            "alt" => {
                if apple {
                    "\u{2325}".to_owned()
                } else {
                    "Alt".to_owned()
                }
            }
            "backspace" => {
                if apple {
                    "\u{232b}".to_owned()
                } else {
                    "Backspace".to_owned()
                }
            }
            "escape" => "Esc".to_owned(),
            "enter" => "Enter".to_owned(),
            "tab" => "Tab".to_owned(),
            "home" => "Home".to_owned(),
            "end" => "End".to_owned(),
            "pageup" => "PgUp".to_owned(),
            "pagedown" => "PgDn".to_owned(),
            "left" => "\u{2190}".to_owned(),
            "right" => "\u{2192}".to_owned(),
            "up" => "\u{2191}".to_owned(),
            "down" => "\u{2193}".to_owned(),
            other => {
                let mut chars = other.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join(if apple { "" } else { " " })
}

fn projection_label(state: crate::ProjectionState) -> &'static str {
    match state {
        crate::ProjectionState::Checking => "Checking",
        crate::ProjectionState::Accepted => "Accepted",
        crate::ProjectionState::Ready => "Ready",
        crate::ProjectionState::Degraded(_) => "Degraded",
        crate::ProjectionState::Cancelled => "Cancelled",
        crate::ProjectionState::Failed => "Failed",
        crate::ProjectionState::Active => "Active",
    }
}

const fn package_phase_label(phase: interface_core::PackageCompilePhase) -> &'static str {
    match phase {
        interface_core::PackageCompilePhase::Locate => "Locating the exact pinned package",
        interface_core::PackageCompilePhase::EnterSource => "Entering exact package source",
        interface_core::PackageCompilePhase::Authority => "Collecting native semantic authority",
        interface_core::PackageCompilePhase::Lower => "Lowering canonical semantic IR",
        interface_core::PackageCompilePhase::Publish => "Publishing a durable generation",
        interface_core::PackageCompilePhase::Reopen => "Reopening and validating semantic bytes",
        interface_core::PackageCompilePhase::Discover => "Building discovery facts",
        interface_core::PackageCompilePhase::Render => "Projecting documentation and links",
    }
}

fn command_label(command: CommandId) -> SharedString {
    let facts = command_facts(command);
    SharedString::from(facts.label)
}

fn diagnostic_text(diagnostic: Option<&Diagnostic>) -> (&'static str, &'static str) {
    let Some(diagnostic) = diagnostic else {
        return (
            "No diagnostic",
            "No core rejection has reached this window.",
        );
    };
    let code = match diagnostic.code {
        DiagnosticCode::SemanticTextTooLong => "Semantic text limit",
        DiagnosticCode::ResultLimitExceeded => "Result limit",
        DiagnosticCode::DependencyUnavailable => "Dependency unavailable",
        DiagnosticCode::UnsupportedCompilerStage => "Unsupported compiler stage",
        DiagnosticCode::OperationUnavailable => "Operation unavailable",
        DiagnosticCode::AdaptivePolicyRejected => "Adaptive policy rejected",
        DiagnosticCode::CompilerTerminal => "Compiler or publication terminal",
        DiagnosticCode::ExecutionFailed => "Local execution failed",
        DiagnosticCode::RetrievalFailed => "Retrieval failed",
    };
    let detail = match &diagnostic.detail {
        DiagnosticDetail::Capability(capability) => capability_label(*capability),
        DiagnosticDetail::Frontend(_) => "The compiler vocabulary rejected the selected stage.",
        DiagnosticDetail::Text(_) => "The supplied typed field failed semantic validation.",
        DiagnosticDetail::Limit { .. } => "The requested result bound exceeds this local slice.",
        DiagnosticDetail::TextLength { .. } => "The supplied field exceeds the semantic width.",
        DiagnosticDetail::Operation(_) => "The requested operation is no longer active.",
        DiagnosticDetail::Policy(_) => "The adaptive policy rejected the supplied immutable facts.",
        DiagnosticDetail::Compiler(_) => {
            "The compiler or publication owner retained a typed terminal."
        }
        DiagnosticDetail::Execution(_) => {
            "The local capability execution retained its exact failed transition."
        }
        DiagnosticDetail::Retrieval(_) => {
            "The retrieval capability retained its exact typed rejection."
        }
    };
    (code, detail)
}

const fn capability_label(capability: Capability) -> &'static str {
    match capability {
        Capability::CompilerRegistry => "Compiler registry is local and ready.",
        Capability::CompilerOutput => "A validated compiler/IR publication seam is unavailable.",
        Capability::Index => "The local immutable index seam is unavailable.",
        Capability::Graph => "The validated graph provider is unavailable.",
        Capability::Vector => "The validated vector provider is unavailable.",
        Capability::LocalAnalyzer => "The local analyzer bundle is unavailable.",
        Capability::Remote => "The remote recovery adapter is unavailable.",
    }
}

const fn command_element_id(command: CommandId) -> u64 {
    match command {
        CommandId::OpenRoute(Route::Home) => 1,
        CommandId::OpenRoute(Route::Libraries) => 2,
        CommandId::OpenRoute(Route::Search) => 3,
        CommandId::OpenRoute(Route::Connections) => 4,
        CommandId::OpenRoute(Route::Settings) => 5,
        CommandId::InspectSurface(crate::Surface::Generation) => 10,
        CommandId::InspectSurface(crate::Surface::Adaptive) => 11,
        CommandId::InspectSurface(crate::Surface::Execution) => 12,
        CommandId::InspectSurface(crate::Surface::Index) => 13,
        CommandId::InspectSurface(crate::Surface::Graph) => 14,
        CommandId::InspectSurface(crate::Surface::Vector) => 15,
        CommandId::InspectSurface(crate::Surface::Health) => 16,
        CommandId::FocusAction(action) => 100 + action_element_id(action),
    }
}

const fn action_element_id(action: ServiceAction) -> u64 {
    match action {
        ServiceAction::Generate => 1,
        ServiceAction::CompilePackage => 13,
        ServiceAction::SnapshotStatus => 2,
        ServiceAction::Search => 3,
        ServiceAction::Graph => 4,
        ServiceAction::Vector => 5,
        ServiceAction::Locality => 6,
        ServiceAction::Health => 7,
        ServiceAction::RecoverLocal => 8,
        ServiceAction::RecoverInconsistent => 9,
        ServiceAction::ReleaseLocal => 10,
        ServiceAction::PollExecution => 11,
        ServiceAction::Cancel => 12,
    }
}

/// What one typed field accepts, in the words of whatever parses it.
///
/// `Language` is checked by `LanguageProfile::try_from`, which admits
/// thirty-two spellings; naming four and the shape they share is more use than
/// a list nobody can read in a 32px row. `Stage` quotes the two variants'
/// own documentation. `Source` names the byte budget the submit path enforces
/// through `PORTABLE_LOCAL_SOURCE_LIMIT`.
const fn form_field_hint(field: FormField) -> &'static str {
    match field {
        FormField::Language => "profile token, e.g. rust-2024, python-3.13, typescript, go-1.25",
        FormField::Stage => "parse validates syntax; lower-ir also lowers facts into canonical IR",
        FormField::Source => "the source text itself, not a path; up to 1 MiB",
        // A purl: pkg: scheme, one of the seven accepted ecosystems, a name,
        // and a pinned version -- PackageUrlError has a variant for each of
        // those being absent.
        FormField::PackageUrl => "pinned package URL, e.g. pkg:cargo/serde@1.0.219",
        FormField::Snapshot => "identity of an immutable published snapshot",
        FormField::Query => "bounded query text; at most 4 rows come back",
    }
}

const fn action_form_hint(action: ServiceAction) -> &'static str {
    match action {
        ServiceAction::Generate => {
            "Provide language, stage, and source; output is available only after canonical compiler/publication succeeds."
        }
        ServiceAction::CompilePackage => {
            "Provide a canonical pinned package URL, matching language profile, and stage; progress remains typed through locate, authority, lowering, publication, and reopen."
        }
        ServiceAction::SnapshotStatus | ServiceAction::Locality => {
            "Select an immutable published snapshot before submitting this typed request."
        }
        ServiceAction::Search | ServiceAction::Graph | ServiceAction::Vector => {
            "Select a published snapshot and enter a bounded query; result rows appear only from the canonical retrieval seam."
        }
        ServiceAction::Health => {
            "This action has no arguments and can be run from the settings route."
        }
        ServiceAction::RecoverLocal
        | ServiceAction::RecoverInconsistent
        | ServiceAction::ReleaseLocal => {
            "Select canonical immutable pins and a verified bundle before the service can act."
        }
        ServiceAction::PollExecution | ServiceAction::Cancel => {
            "Select the actual operation emitted by the service; the operation authority is never guessed by this view."
        }
    }
}

const fn form_action(form: FormState) -> ServiceAction {
    match form {
        FormState::Generate { .. } => ServiceAction::Generate,
        FormState::CompilePackage { .. } => ServiceAction::CompilePackage,
        FormState::Snapshot { action, .. } => match action {
            crate::SnapshotAction::Status => ServiceAction::SnapshotStatus,
            crate::SnapshotAction::Locality => ServiceAction::Locality,
            crate::SnapshotAction::Graph => ServiceAction::Graph,
            crate::SnapshotAction::Vector => ServiceAction::Vector,
        },
        FormState::Search { .. } => ServiceAction::Search,
        FormState::Health => ServiceAction::Health,
        FormState::Recovery { action, .. } => match action {
            crate::RecoveryAction::Local => ServiceAction::RecoverLocal,
            crate::RecoveryAction::Inconsistent => ServiceAction::RecoverInconsistent,
            crate::RecoveryAction::Release => ServiceAction::ReleaseLocal,
        },
        FormState::Operation { action, .. } => match action {
            crate::OperationAction::Poll => ServiceAction::PollExecution,
            crate::OperationAction::Cancel => ServiceAction::Cancel,
        },
    }
}

fn form_value_label(value: Option<interface_core::InputText>) -> SharedString {
    match value {
        Some(value) => value.to_string().into(),
        None => SharedString::from("Required"),
    }
}

const fn form_field_id(field: FormField) -> u64 {
    match field {
        FormField::Language => 1,
        FormField::Stage => 2,
        FormField::Source => 3,
        FormField::PackageUrl => 4,
        FormField::Snapshot => 5,
        FormField::Query => 6,
    }
}

const fn text_input_target_label(target: crate::TextInputTarget) -> &'static str {
    match target {
        crate::TextInputTarget::DocumentationSearch => "Documentation search",
        crate::TextInputTarget::Palette => "Palette",
        crate::TextInputTarget::Form(FormField::Language) => "Language",
        crate::TextInputTarget::Form(FormField::Stage) => "Stage",
        crate::TextInputTarget::Form(FormField::Source) => "Source",
        crate::TextInputTarget::Form(FormField::PackageUrl) => "Pinned package URL",
        crate::TextInputTarget::Form(FormField::Snapshot) => "Snapshot",
        crate::TextInputTarget::Form(FormField::Query) => "Query",
    }
}

const fn form_error_label(error: FormError) -> &'static str {
    match error {
        FormError::NoActiveForm => "Open a typed action form first.",
        FormError::FieldUnavailable(_) => "That field does not belong to this typed action.",
        FormError::MissingText(_) => "Complete the required typed field before submitting.",
        FormError::MissingRecoverySelection => "Select canonical immutable recovery facts first.",
        FormError::MissingObservedPin => {
            "Inconsistent recovery requires the observed pinned authority."
        }
        FormError::MissingOperation => "Select an actual service operation before submitting.",
        FormError::LimitExceeded { .. } => "Choose a result count inside the local service bound.",
        FormError::EmptyTextEdit { .. } => "Enter text in the selected typed field.",
        FormError::InputTooLong { .. } => "That edit exceeds the fixed transport field width.",
        FormError::InputLengthOverflow { .. } => {
            "That edit cannot be represented by the native text boundary."
        }
        FormError::UnknownLanguage(_) => "Choose one of the canonical compiler languages.",
        FormError::UnknownStage(_) => "Choose one of the canonical compiler stages.",
        FormError::SourceTooLong { .. } => {
            "That source exceeds the portable local compiler budget."
        }
        FormError::InvalidPackageUrl(_) => "Enter a canonical pinned package URL.",
        FormError::PackageProfileMismatch(_) => {
            "The selected language profile does not match this package ecosystem."
        }
        FormError::LimitUnavailable => "This typed action has no caller-controlled result limit.",
        FormError::NoEditableField => "This typed action has no editable text field.",
    }
}

const fn native_error_targets_form(error: &crate::NativeTextInputError) -> bool {
    matches!(
        error,
        crate::NativeTextInputError::InputTooLong {
            target: crate::TextInputTarget::Form(_),
            ..
        } | crate::NativeTextInputError::InputLengthOverflow {
            target: crate::TextInputTarget::Form(_),
            ..
        }
    )
}

const fn native_error_targets_palette(error: &crate::NativeTextInputError) -> bool {
    matches!(
        error,
        crate::NativeTextInputError::InputTooLong {
            target: crate::TextInputTarget::Palette,
            ..
        } | crate::NativeTextInputError::InputLengthOverflow {
            target: crate::TextInputTarget::Palette,
            ..
        }
    )
}

fn native_input_error_label(error: crate::NativeTextInputError) -> SharedString {
    match error {
        crate::NativeTextInputError::InputTooLong {
            target,
            actual,
            maximum,
        } => SharedString::from(format!(
            "{} input: {actual} / {maximum} bytes",
            text_input_target_label(target)
        )),
        crate::NativeTextInputError::InputLengthOverflow {
            target,
            prefix,
            inserted,
            suffix,
        } => SharedString::from(format!(
            "{} input length overflow: {prefix} + {inserted} + {suffix} bytes",
            text_input_target_label(target)
        )),
    }
}

fn palette_error_label(error: crate::PaletteEditError) -> SharedString {
    match error {
        crate::PaletteEditError::EmptyInput => SharedString::from("Enter a palette query."),
        crate::PaletteEditError::InputTooLong { actual, maximum } => {
            SharedString::from(format!("Palette input: {actual} / {maximum} bytes"))
        }
        crate::PaletteEditError::InputLengthOverflow {
            prefix,
            inserted,
            suffix,
        } => SharedString::from(format!(
            "Palette input length overflow: {prefix} + {inserted} + {suffix} bytes"
        )),
        crate::PaletteEditError::NothingToErase => {
            SharedString::from("The palette query is already empty.")
        }
    }
}

const fn kind_color(kind: DocumentKind) -> u32 {
    match kind {
        DocumentKind::Struct => 0x0067_e8f9,
        DocumentKind::Enum => 0x00c0_84fc,
        DocumentKind::Trait => 0x00fb_bd23,
        DocumentKind::Function => 0x004a_de80,
        DocumentKind::Module => 0x00f4_728b,
    }
}

const fn kind_short_label(kind: DocumentKind) -> &'static str {
    match kind {
        DocumentKind::Struct => "S",
        DocumentKind::Enum => "E",
        DocumentKind::Trait => "T",
        DocumentKind::Function => "ƒ",
        DocumentKind::Module => "M",
    }
}

const fn search_match_label(score: u16) -> &'static str {
    match score {
        120.. => "exact",
        90..=119 => "prefix",
        48..=89 => "path",
        _ => "docs",
    }
}

const fn search_scope_id(scope: DocumentSearchScope) -> u64 {
    match scope {
        DocumentSearchScope::Everything => 0,
        DocumentSearchScope::Symbols => 1,
        DocumentSearchScope::Packages => 2,
    }
}

const fn document_filter_id(filter: DocumentFilter) -> u64 {
    match filter {
        DocumentFilter::All => 0,
        DocumentFilter::Types => 1,
        DocumentFilter::APIs => 2,
        DocumentFilter::Modules => 3,
    }
}
