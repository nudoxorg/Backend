//! Real GPUI product shell for the typed application service.

mod input;

use crate::navigation::{
    action_label, command_facts, route_facts, surface_destination, surface_facts,
};
use crate::{
    ApplicationInput, ApplicationReply, ApplicationService, ApplyError, CommandId,
    ConfirmPaletteCommand, DismissForm, DismissPalette, FormError, FormField, FormState,
    NextFormField, OpenPalette, OpenSettings, PaletteDirection, PreviousFormField, ResultLimit,
    Route, SelectFirstPaletteCommand, SelectLastPaletteCommand, SelectNextPaletteCommand,
    SelectNextPalettePage, SelectPreviousPaletteCommand, SelectPreviousPalettePage, ServiceAction,
    ShellState,
};
use core::ops::Deref;
use gpui::{
    Animation, AnimationExt, AnyElement, Context, FocusHandle, IntoElement, KeyBinding,
    KeyDownEvent, Render, ScrollStrategy, SharedString, Task, UniformListScrollHandle, Window, div,
    prelude::*, px, rgb, uniform_list,
};
use std::{cell::RefCell, future::poll_fn, rc::Rc, time::Duration};
use wave_application_core::{
    Capability, CorrelationId, Diagnostic, DiagnosticCode, DiagnosticDetail, ExecutionState,
    OperationKey, ReplyBody,
};

const SHELL_CONTEXT: &str = "wave-application-shell";
const RAIL_WIDTH: f32 = 208.0;
const PALETTE_WIDTH: f32 = 640.0;
const PALETTE_HEIGHT: f32 = 360.0;
const ROW_HEIGHT: f32 = 36.0;
const RAIL_BACKGROUND: u32 = 0x0017_1a1f;
const RAIL_TEXT: u32 = 0x00e6_e9ee;
const PANEL_BACKGROUND: u32 = 0x0025_2a32;
const PANEL_HOVER: u32 = 0x0030_3741;
const METADATA_TEXT: u32 = 0x00b9_c1cc;
const PAGE_BACKGROUND: u32 = 0x001e_2228;
const FOREGROUND: u32 = 0x00f2_f4f8;
const BORDER: u32 = 0x0034_3b46;
const BACKDROP: u32 = 0x0000_0000;
const PALETTE_BORDER: u32 = 0x0046_505f;
const PALETTE_BACKGROUND: u32 = 0x0020_252c;
const SELECTED_ROW: u32 = 0x0035_4052;
const HOVERED_ROW: u32 = 0x002a_3039;
const INTERACTION_DURATION: Duration = Duration::from_millis(90);

/// Entity-backed production GPUI view for the bounded application shell.
///
/// The view has one service owner. All domain state crosses the UI boundary as an
/// [`ApplicationInput`] and returns as an [`ApplicationReply`]; navigation and the palette only
/// project those facts. The one retained foreground task is woken by the admitted execution future;
/// no timer, polling loop, accessibility driver, or second command decoder participates in the
/// shell.
pub struct GpuiShellView {
    service: Rc<RefCell<ApplicationService>>,
    state: ShellState,
    focus: FocusHandle,
    palette_focus: FocusHandle,
    palette_scroll: UniformListScrollHandle,
    next_correlation: u64,
    driven_execution: Option<DrivenExecution>,
    native_input: input::NativeInputState,
    native_input_bounds: input::SharedNativeInputGeometry,
}

struct DrivenExecution {
    operation: OperationKey,
    task: Task<()>,
}

impl GpuiShellView {
    /// Creates a focused product shell around the service that owns application behavior.
    #[must_use]
    pub fn new(service: ApplicationService, window: &mut Window, cx: &mut Context<Self>) -> Self {
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
        ]);
        let focus = cx.focus_handle();
        let palette_focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            service: Rc::new(RefCell::new(service)),
            state: ShellState::default(),
            focus,
            palette_focus,
            palette_scroll: UniformListScrollHandle::new(),
            next_correlation: 1,
            driven_execution: None,
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
    ) -> Result<ApplicationReply, ApplyError> {
        let reply = self.service.borrow_mut().execute(input);
        self.state.apply_batch(&[reply])?;
        cx.notify();
        match reply.body {
            ReplyBody::ExecutionStarted { operation, .. }
            | ReplyBody::Execution(ExecutionState::Pending { operation, .. }) => {
                self.drive_admitted_execution(reply.correlation, operation, cx);
            }
            ReplyBody::Execution(
                ExecutionState::Completed { operation, .. }
                | ExecutionState::Cancelled { operation, .. }
                | ExecutionState::Failed { operation, .. },
            ) => {
                if self
                    .driven_execution
                    .as_ref()
                    .is_some_and(|driven| driven.operation == operation)
                {
                    self.driven_execution = None;
                    self.state.set_foreground_operation(None);
                }
            }
            ReplyBody::DependencyUnavailable { .. }
            | ReplyBody::Health(_)
            | ReplyBody::Adaptive(_)
            | ReplyBody::Rejected => {}
        }
        Ok(reply)
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
                match view.state.apply_batch(&[reply]) {
                    Ok(_receipt) => cx.notify(),
                    Err(error) => {
                        debug_assert_eq!(view.state.projection_error, Some(error));
                        cx.notify();
                    }
                }
            }) {
                Ok(()) | Err(_) => {}
            }
        });
        self.driven_execution = Some(DrivenExecution { operation, task });
        self.state.set_foreground_operation(Some(operation));
    }

    fn open_palette(&mut self, _: &OpenPalette, window: &mut Window, cx: &mut Context<Self>) {
        self.state.open_palette();
        window.focus(&self.palette_focus, cx);
        cx.notify();
    }

    fn open_settings(&mut self, _: &OpenSettings, _: &mut Window, cx: &mut Context<Self>) {
        self.select_route(Route::Settings, cx);
    }

    fn dismiss_palette(&mut self, _: &DismissPalette, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.navigation.palette.visible {
            self.state.dismiss_palette();
        } else {
            self.state.cancel_form();
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn dismiss_form(&mut self, _: &DismissForm, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.navigation.palette.visible {
            self.state.dismiss_palette();
        } else {
            self.state.cancel_form();
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn next_form_field(&mut self, _: &NextFormField, _: &mut Window, cx: &mut Context<Self>) {
        if !self.state.navigation.palette.visible && self.state.move_form_field(true).is_ok() {
            cx.notify();
        }
    }

    fn previous_form_field(
        &mut self,
        _: &PreviousFormField,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.state.navigation.palette.visible && self.state.move_form_field(false).is_ok() {
            cx.notify();
        }
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
            let _ = self.state.confirm_palette();
            window.focus(&self.focus, cx);
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
        }
        cx.notify();
    }

    fn select_route(&mut self, route: Route, cx: &mut Context<Self>) {
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
        if !self.state.navigation.palette.visible {
            self.update_form_text(event, cx);
            return;
        }
        let changed =
            event.keystroke.key == "backspace" && self.state.erase_palette_character().is_ok();
        if changed {
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn update_form_text(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        if self.state.form.is_none() {
            return;
        }
        let changed = event.keystroke.key == "backspace" && self.state.erase_form_text().is_ok();
        if changed {
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
                if self.execute(&input, cx).is_err() {
                    cx.notify();
                }
            }
            Err(_) => cx.notify(),
        }
    }

    fn navigation_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.state.navigation.route;
        div()
            .id("application-rail")
            .w(px(RAIL_WIDTH))
            .h_full()
            .flex()
            .flex_col()
            .justify_between()
            .p(px(16.0))
            .gap(px(8.0))
            .bg(rgb(RAIL_BACKGROUND))
            .text_color(rgb(RAIL_TEXT))
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Nudox"),
            )
            .child(div().flex().flex_col().gap(px(4.0)).children([
                Self::route_button(Route::Home, selected, cx),
                Self::route_button(Route::Libraries, selected, cx),
                Self::route_button(Route::Search, selected, cx),
                Self::route_button(Route::Connections, selected, cx),
                Self::route_button(Route::Settings, selected, cx),
            ]))
            .child(
                div()
                    .id("open-command-palette")
                    .h(px(36.0))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .rounded(px(6.0))
                    .bg(rgb(PANEL_BACKGROUND))
                    .hover(|style| style.bg(rgb(PANEL_HOVER)))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.state.open_palette();
                        window.focus(&this.palette_focus, cx);
                        cx.notify();
                    }))
                    .child("Command palette")
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(METADATA_TEXT))
                            .child("⌘K · Ctrl K"),
                    ),
            )
    }

    fn route_button(
        route: Route,
        selected: Route,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let facts = route_facts(route);
        div()
            .id(facts.element_id)
            .h(px(36.0))
            .px(px(10.0))
            .flex()
            .items_center()
            .rounded(px(6.0))
            .cursor_pointer()
            .when(selected == route, |style| style.bg(rgb(PANEL_HOVER)))
            .when(selected != route, |style| {
                style.hover(|hovered| hovered.bg(rgb(PANEL_BACKGROUND)))
            })
            .on_click(cx.listener(move |this, _, _, cx| this.select_route(route, cx)))
            .child(facts.label)
            .when_some(facts.shortcut, |row, shortcut| {
                row.child(
                    div()
                        .ml_auto()
                        .text_sm()
                        .text_color(rgb(METADATA_TEXT))
                        .child(shortcut.apple)
                        .child(" · ")
                        .child(shortcut.other),
                )
            })
    }

    fn page(&self, cx: &mut Context<Self>) -> AnyElement {
        let route = self.state.navigation.route;
        let facts = route_facts(route);
        let snapshot = self.route_snapshot(route, cx);
        div()
            .id("application-page")
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .p(px(32.0))
            .gap(px(16.0))
            .bg(rgb(PAGE_BACKGROUND))
            .text_color(rgb(FOREGROUND))
            .child(
                div()
                    .text_xl()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(facts.label),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(METADATA_TEXT))
                    .child(route_description(route)),
            )
            .child(
                div()
                    .id("route-capability-card")
                    .p(px(16.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(PANEL_BACKGROUND))
                    .child(snapshot),
            )
            .when_some(self.state.form, |page, form| {
                page.child(self.action_form(form, cx))
            })
            .into_any_element()
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
            .flex()
            .flex_col()
            .gap(px(8.0))
            .children([
                Self::snapshot_row(
                    "home-generation",
                    "Compiler / publication",
                    self.state.pages.home.generation,
                ),
                Self::snapshot_row(
                    "home-execution",
                    "Local operation",
                    self.state.pages.home.execution,
                ),
                Self::snapshot_row(
                    "home-health",
                    "Capability health",
                    self.state.pages.home.health,
                ),
            ])
            .child(Self::action_button(
                ServiceAction::Generate,
                "Open generation form",
                cx,
            ))
            .into_any_element()
    }

    fn libraries_snapshot(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("libraries-snapshot")
            .flex()
            .flex_col()
            .gap(px(8.0))
            .children([
                Self::snapshot_row(
                    "libraries-index",
                    "Local index coverage",
                    self.state.pages.libraries.index,
                ),
                Self::snapshot_row(
                    "libraries-graph",
                    "Graph coverage",
                    self.state.pages.libraries.graph,
                ),
                Self::snapshot_row(
                    "libraries-vector",
                    "Vector coverage",
                    self.state.pages.libraries.vector,
                ),
            ])
            .children([
                Self::action_button(ServiceAction::SnapshotStatus, "Inspect snapshot status", cx),
                Self::action_button(ServiceAction::Locality, "Inspect local placement", cx),
            ])
            .into_any_element()
    }

    fn search_snapshot(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("search-snapshot")
            .flex()
            .flex_col()
            .gap(px(8.0))
            .children([
                Self::snapshot_row("search-exact", "Exact", self.state.pages.search.exact),
                Self::snapshot_row("search-lexical", "Lexical", self.state.pages.search.lexical),
                Self::snapshot_row("search-graph", "Graph", self.state.pages.search.graph),
                Self::snapshot_row("search-vector", "Vector", self.state.pages.search.vector),
            ])
            .children([
                Self::action_button(ServiceAction::Search, "Open exact / lexical search", cx),
                Self::action_button(ServiceAction::Graph, "Open graph search", cx),
                Self::action_button(ServiceAction::Vector, "Open vector search", cx),
            ])
            .into_any_element()
    }

    fn connections_snapshot(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("connections-snapshot")
            .flex()
            .flex_col()
            .gap(px(8.0))
            .children([
                Self::snapshot_row(
                    "connections-placement",
                    "Placement",
                    self.state.pages.connections.placement,
                ),
                Self::snapshot_row(
                    "connections-execution",
                    "Local operation",
                    self.state.pages.connections.execution,
                ),
                Self::snapshot_row(
                    "connections-health",
                    "Connection health",
                    self.state.pages.connections.health,
                ),
            ])
            .children([
                Self::action_button(ServiceAction::RecoverLocal, "Recover local analyzer", cx),
                Self::action_button(
                    ServiceAction::RecoverInconsistent,
                    "Recover immutable mismatch",
                    cx,
                ),
                Self::action_button(ServiceAction::ReleaseLocal, "Release local analyzer", cx),
                Self::action_button(
                    ServiceAction::PollExecution,
                    "Observe selected operation",
                    cx,
                ),
                Self::action_button(ServiceAction::Cancel, "Cancel selected operation", cx),
            ])
            .into_any_element()
    }

    fn settings_snapshot(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("settings-snapshot")
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(Self::snapshot_row(
                "settings-health",
                "Capability health",
                self.state.pages.settings.health,
            ))
            .child(Self::diagnostic_row(self.state.pages.settings.diagnostic))
            .child(Self::action_button(
                ServiceAction::Health,
                "Inspect capability health",
                cx,
            ))
            .child(self.motion_selector(cx))
            .child(
                div()
                    .id("settings-notification-epoch")
                    .text_sm()
                    .text_color(rgb(METADATA_TEXT))
                    .child("Coalesced UI epoch: ")
                    .child(self.state.pages.settings.notification_epoch.to_string()),
            )
            .into_any_element()
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
    ) -> impl IntoElement + use<> {
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

    fn diagnostic_row(diagnostic: Option<Diagnostic>) -> impl IntoElement + use<> {
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
    ) -> impl IntoElement + use<> {
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
            .border_color(rgb(PALETTE_BORDER))
            .bg(rgb(PALETTE_BACKGROUND))
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
                package,
                source,
            } => self.generate_form_fields(focused, language, stage, package, source, cx),
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
        language: Option<wave_application_core::InputText>,
        stage: Option<wave_application_core::InputText>,
        package: Option<wave_application_core::InputText>,
        source: Option<wave_application_core::InputText>,
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
                self.form_field(FormField::Package, "Package", package, focused, cx),
                self.form_field(FormField::Source, "Source", source, focused, cx),
            ])
            .into_any_element()
    }

    fn snapshot_form_fields(
        &self,
        focused: FormField,
        snapshot: Option<wave_application_core::InputText>,
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
        value: Option<wave_application_core::InputText>,
        focused: FormField,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        div()
            .id(("typed-form-field", form_field_id(field)))
            .h(px(32.0))
            .px(px(10.0))
            .flex()
            .items_center()
            .justify_between()
            .rounded(px(5.0))
            .when(focused == field, |row| row.bg(rgb(SELECTED_ROW)))
            .when(focused != field, |row| row.bg(rgb(PAGE_BACKGROUND)))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                let _ = this.state.select_form_field(field);
                cx.notify();
            }))
            .child(label)
            .child(
                div()
                    .id("palette-native-input-field")
                    .debug_selector(|| "palette-native-input-field".to_owned())
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
                        if let Ok(limit) = ResultLimit::new(limit) {
                            let _ = this.state.replace_form_limit(limit);
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
    ) -> impl IntoElement + use<> {
        let label = match input_error
            .filter(|error| matches!(error.target, crate::TextInputTarget::Form(_)))
        {
            Some(error) => SharedString::from(format!(
                "{} input: {} / {} bytes",
                text_input_target_label(error.target),
                error.actual,
                error.maximum
            )),
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
        let query_label = match self
            .state
            .input_error
            .filter(|error| error.target == crate::TextInputTarget::Palette)
        {
            Some(error) => SharedString::from(format!(
                "{} input: {} / {} bytes",
                text_input_target_label(error.target),
                error.actual,
                error.maximum
            )),
            None => match self.state.navigation.palette.query_text() {
                Ok("") | Err(_) => SharedString::from("Type to filter · ↑↓ Enter Esc"),
                Ok(query) => SharedString::from(query),
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
                    .border_color(rgb(PALETTE_BORDER))
                    .bg(rgb(PALETTE_BACKGROUND))
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

impl Render for GpuiShellView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette_visible = self.state.navigation.palette.visible;
        div()
            .id("wave-application-shell")
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
            .on_action(cx.listener(Self::next_form_field))
            .on_action(cx.listener(Self::previous_form_field))
            .on_key_down(cx.listener(Self::update_palette_query))
            .size_full()
            .relative()
            .flex()
            .flex_col()
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

impl Deref for GpuiShellView {
    type Target = ShellState;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

fn route_description(route: Route) -> &'static str {
    match route {
        Route::Home => "Start with a stable overview of search, indexing, and connection health.",
        Route::Libraries => "Manage local library coverage without touching source files.",
        Route::Search => "Run exact, lexical, graph, and vector retrieval through one service.",
        Route::Connections => "Configure and inspect AI-client and MCP connections.",
        Route::Settings => "Set preferences and inspect product diagnostics.",
    }
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

fn command_label(command: CommandId) -> SharedString {
    let facts = command_facts(command);
    SharedString::from(facts.label)
}

fn diagnostic_text(diagnostic: Option<Diagnostic>) -> (&'static str, &'static str) {
    let Some(diagnostic) = diagnostic else {
        return (
            "No diagnostic",
            "No core rejection has reached this window.",
        );
    };
    let code = match diagnostic.code {
        DiagnosticCode::UnknownLanguage => "Unknown compiler language",
        DiagnosticCode::UnknownStage => "Unknown compiler stage",
        DiagnosticCode::SemanticTextTooLong => "Semantic text limit",
        DiagnosticCode::ResultLimitExceeded => "Result limit",
        DiagnosticCode::DependencyUnavailable => "Dependency unavailable",
        DiagnosticCode::UnsupportedCompilerStage => "Unsupported compiler stage",
        DiagnosticCode::OperationUnavailable => "Operation unavailable",
        DiagnosticCode::AdaptivePolicyRejected => "Adaptive policy rejected",
    };
    let detail = match diagnostic.detail {
        DiagnosticDetail::Capability(capability) => capability_label(capability),
        DiagnosticDetail::Frontend(_) => "The compiler vocabulary rejected the selected stage.",
        DiagnosticDetail::Text(_) => "The supplied typed field failed semantic validation.",
        DiagnosticDetail::Limit { .. } => "The requested result bound exceeds this local slice.",
        DiagnosticDetail::TextLength { .. } => "The supplied field exceeds the semantic width.",
        DiagnosticDetail::Operation(_) => "The requested operation is no longer active.",
        DiagnosticDetail::Policy(_) => "The adaptive policy rejected the supplied immutable facts.",
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

const fn action_form_hint(action: ServiceAction) -> &'static str {
    match action {
        ServiceAction::Generate => {
            "Provide language, stage, package, and source; output stays unavailable until the canonical compiler/publication seam arrives."
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

fn form_value_label(value: Option<wave_application_core::InputText>) -> SharedString {
    match value {
        Some(value) => String::from_utf8_lossy(value.as_ref()).into_owned().into(),
        None => SharedString::from("Required"),
    }
}

const fn form_field_id(field: FormField) -> u64 {
    match field {
        FormField::Language => 1,
        FormField::Stage => 2,
        FormField::Package => 3,
        FormField::Source => 4,
        FormField::Snapshot => 5,
        FormField::Query => 6,
    }
}

const fn text_input_target_label(target: crate::TextInputTarget) -> &'static str {
    match target {
        crate::TextInputTarget::Palette => "Palette",
        crate::TextInputTarget::Form(FormField::Language) => "Language",
        crate::TextInputTarget::Form(FormField::Stage) => "Stage",
        crate::TextInputTarget::Form(FormField::Package) => "Package",
        crate::TextInputTarget::Form(FormField::Source) => "Source",
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
        FormError::LimitUnavailable => "This typed action has no caller-controlled result limit.",
        FormError::NoEditableField => "This typed action has no editable text field.",
    }
}
