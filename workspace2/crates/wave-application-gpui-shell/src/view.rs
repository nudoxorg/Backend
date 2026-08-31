//! Real GPUI product shell for the typed application service.

use crate::{
    ApplicationInput, ApplicationReply, ApplicationService, ApplyError, CommandId,
    ConfirmPaletteCommand, DismissPalette, OpenPalette, PaletteDirection, Route,
    SelectNextPaletteCommand, SelectPreviousPaletteCommand, ShellState,
};
use gpui::{
    Context, FocusHandle, IntoElement, KeyBinding, Render, ScrollStrategy, UniformListScrollHandle,
    Window, div, prelude::*, px, rgb, uniform_list,
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

/// Entity-backed production GPUI view for the bounded application shell.
///
/// The view has one service owner. All domain state crosses the UI boundary as an
/// [`ApplicationInput`] and returns as an [`ApplicationReply`]; navigation and the palette only
/// project those facts. No timer, task, poll loop, accessibility driver, or second command decoder
/// participates in the shell.
pub struct GpuiShellView {
    service: ApplicationService,
    state: ShellState,
    focus: FocusHandle,
    palette_scroll: UniformListScrollHandle,
}

impl GpuiShellView {
    /// Creates a focused product shell around the service that owns application behavior.
    #[must_use]
    pub fn new(service: ApplicationService, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.bind_keys([
            KeyBinding::new("cmd-k", OpenPalette, Some(SHELL_CONTEXT)),
            KeyBinding::new("ctrl-k", OpenPalette, Some(SHELL_CONTEXT)),
            KeyBinding::new("escape", DismissPalette, Some(SHELL_CONTEXT)),
            KeyBinding::new("down", SelectNextPaletteCommand, Some(SHELL_CONTEXT)),
            KeyBinding::new("up", SelectPreviousPaletteCommand, Some(SHELL_CONTEXT)),
            KeyBinding::new("enter", ConfirmPaletteCommand, Some(SHELL_CONTEXT)),
        ]);
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            service,
            state: ShellState::default(),
            focus,
            palette_scroll: UniformListScrollHandle::new(),
        }
    }

    /// Borrows the projected state for parent composition and deterministic tests.
    #[must_use]
    pub const fn state(&self) -> &ShellState {
        &self.state
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
        let reply = self.service.execute(input);
        self.state.apply_batch(&[reply])?;
        cx.notify();
        Ok(reply)
    }

    fn open_palette(&mut self, _: &OpenPalette, _: &mut Window, cx: &mut Context<Self>) {
        self.state.open_palette();
        cx.notify();
    }

    fn dismiss_palette(&mut self, _: &DismissPalette, _: &mut Window, cx: &mut Context<Self>) {
        self.state.dismiss_palette();
        cx.notify();
    }

    fn select_next_palette_command(
        &mut self,
        _: &SelectNextPaletteCommand,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reveal_selection(PaletteDirection::Next);
        cx.notify();
    }

    fn select_previous_palette_command(
        &mut self,
        _: &SelectPreviousPaletteCommand,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reveal_selection(PaletteDirection::Previous);
        cx.notify();
    }

    fn confirm_palette_command(
        &mut self,
        _: &ConfirmPaletteCommand,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let _ = self.state.confirm_palette();
        cx.notify();
    }

    fn reveal_selection(&mut self, direction: PaletteDirection) {
        if self.state.navigation.palette.visible
            && let Some(index) = self.state.move_palette_selection(direction)
        {
            self.palette_scroll
                .scroll_to_item(index, ScrollStrategy::Nearest);
        }
    }

    fn select_route(&mut self, route: Route, cx: &mut Context<Self>) {
        self.state.select_route(route);
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
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.open_palette();
                        cx.notify();
                    }))
                    .child("Command palette")
                    .child(div().text_sm().text_color(rgb(METADATA_TEXT)).child("⌘K")),
            )
    }

    fn route_button(
        route: Route,
        selected: Route,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        div()
            .id(route.element_id())
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
            .child(route.label())
    }

    fn page(&self) -> impl IntoElement {
        let route = self.state.navigation.route;
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
                    .child(route.label()),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(METADATA_TEXT))
                    .child(route_description(route)),
            )
            .child(
                div()
                    .id("stable-capability-card")
                    .p(px(16.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(PANEL_BACKGROUND))
                    .child("Application facts appear here without replacing the page frame."),
            )
    }

    fn status_strip(&self) -> impl IntoElement {
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
                div()
                    .id(summary.surface.element_id())
                    .text_sm()
                    .child(summary.surface.label())
                    .child(": ")
                    .child(summary.state.label())
            }))
    }

    fn palette(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
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
                    .w(px(PALETTE_WIDTH))
                    .h(px(PALETTE_HEIGHT))
                    .flex()
                    .flex_col()
                    .rounded(px(10.0))
                    .border_1()
                    .border_color(rgb(PALETTE_BORDER))
                    .bg(rgb(PALETTE_BACKGROUND))
                    .text_color(rgb(FOREGROUND))
                    .child(Self::palette_header())
                    .child(self.palette_rows(cx)),
            )
    }

    fn palette_header() -> impl IntoElement {
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
                    .text_sm()
                    .text_color(rgb(METADATA_TEXT))
                    .child("↑↓ Enter Esc"),
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
                        Some(
                            div()
                                .id(("command-palette-row", index))
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
                                .child(command.label()),
                        )
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
            .on_action(cx.listener(Self::select_next_palette_command))
            .on_action(cx.listener(Self::select_previous_palette_command))
            .on_action(cx.listener(Self::confirm_palette_command))
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
                    .child(self.page()),
            )
            .child(self.status_strip())
            .when(palette_visible, |shell| shell.child(self.palette(cx)))
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
