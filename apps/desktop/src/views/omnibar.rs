//! The titlebar and the omnibar sheet it drops.
//! One capsule that searches, scopes, or becomes a command palette.
//! Its head always states the coverage behind whatever it is showing.
//!
//! The sheet is anchored under the capsule rather than centred on the window,
//! so a reader's eye never has to travel: the answer appears exactly beneath
//! the question. Its first row is the coverage strip, which means the honest
//! account of what was searched is read before the results are.

use super::workspace::Workspace;
use crate::presentation::status::Standing;
use crate::store::search::{CommandRow, Mode, Reply, ResultRow, SearchStore};
use crate::store::shell::Focus;
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Chrome, Radius, Space, TypeScale, hairline, radius, space, type_size};
use crate::ui::{button, chip, fault as fault_ui, glyph, specimen, surface, text};
use backend_library::{CommandDomain, CommandSpec};
use gpui::prelude::FluentBuilder as _;
use gpui::Focusable as _;
use gpui::{
    AnyElement, Context, Div, ElementId, FontWeight, InteractiveElement, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};
use gpui_elements::editable_text::text_input;

/// Space reserved on the left of the titlebar for the platform's window buttons.
const TRAFFIC_LIGHTS: f32 = 78.0;

impl Workspace {
    /// Returns the titlebar row: window buttons, the omnibar, and the shell keys.
    pub(super) fn titlebar(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_none()
            .h(px(Chrome::TITLEBAR))
            .w_full()
            .flex()
            .items_center()
            .px(space(Space::Base))
            .gap(space(Space::Base))
            .border_b(hairline())
            .border_color(theme.paint(Paint::Hairline))
            .child(div().flex_none().w(px(TRAFFIC_LIGHTS)))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .justify_center()
                    .child(self.omnibar(theme, cx)),
            )
            .child(self.titlebar_keys(theme, cx))
    }

    fn titlebar_keys(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let appearance = self.shell.read(cx).prefs().appearance();
        div()
            .flex_none()
            .w(px(TRAFFIC_LIGHTS))
            .flex()
            .items_center()
            .justify_end()
            .gap(space(Space::Tight))
            .child(
                button::icon_button(theme, "appearance", if appearance.is_dark() { crate::ui::icon::Icon::Moon } else { crate::ui::icon::Icon::Sun })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.shell.update(cx, |shell, cx| {
                            let next = shell.prefs().appearance().flipped();
                            shell.set_appearance(next, cx);
                        });
                    })),
            )
            .child(
                button::icon_button(theme, "settings", crate::ui::icon::Icon::Gear).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.shell.update(cx, |shell, cx| shell.toggle_settings(cx));
                    },
                )),
            )
    }

    fn omnibar(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let search = self.search.read(cx);
        let focused = self.shell.read(cx).focus() == Focus::Omnibar;
        let mode = search.parsed().mode().clone();
        let placeholder = mode.placeholder();
        div()
            .id("omnibar")
            .w(px(Chrome::OMNIBAR))
            .max_w(px(Chrome::OMNIBAR))
            .h(px(28.0))
            .flex()
            .items_center()
            .gap(space(Space::Snug))
            .px(space(Space::Base))
            .rounded(radius(Radius::Capsule))
            .bg(theme.paint(Paint::Panel))
            .border(hairline())
            .border_color(if focused {
                theme.paint(Paint::Focus)
            } else {
                theme.paint(Paint::Hairline)
            })
            .cursor_pointer()
            .on_click(cx.listener(|this, _, window, cx| {
                this.focus_omnibar_from_click(window, cx);
            }))
            .child(self.omnibar_nib(theme, &mode))
            .when_some(scope_of(&mode), |bar, project| {
                bar.child(chip::scope_chip(theme, &project))
            })
            .child(
                div().flex_1().min_w(px(0.0)).child(
                    text_input("omnibar-field")
                        .state(self.field.downgrade())
                        .placeholder(placeholder)
                        .placeholder_color(theme.paint(Paint::TextFaint))
                        .selection_color(theme.paint(Paint::GiltWash))
                        .caret_color(theme.paint(Paint::Gilt))
                        .text_size(type_size(TypeScale::Interface))
                        .text_color(theme.paint(Paint::TextStrong)),
                ),
            )
            .child(button::key_hint(theme, "⌘K"))
    }

    fn omnibar_nib(&self, theme: &Theme, mode: &Mode) -> Div {
        let (mark, role) = match mode {
            Mode::Palette => ("›", Paint::Gilt),
            Mode::Scoped { .. } => ("@", Paint::Gilt),
            Mode::Search => ("⌕", Paint::TextFaint),
        };
        div()
            .flex_none()
            .text_size(type_size(TypeScale::Interface))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme.paint(role))
            .child(mark)
    }

    fn focus_omnibar_from_click(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.field.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        self.shell
            .update(cx, |shell, cx| shell.focus_on(Focus::Omnibar, cx));
        self.search.update(cx, |search, cx| search.open(cx));
    }
}

/// The sheet.
impl Workspace {
    /// Returns the dropped sheet, or an empty layer when it is closed.
    pub(super) fn omnibar_sheet(
        &mut self,
        theme: &Theme,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        if !self.search.read(cx).is_open() {
            return div();
        }
        div()
            .absolute()
            .top(px(Chrome::TITLEBAR + 4.0))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(
                surface::raised(theme)
                    .id("omnibar-sheet")
                    .w(px(Chrome::SHEET))
                    .max_h(px(Chrome::SHEET_MAX))
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .child(self.sheet_head(theme, cx))
                    .child(self.sheet_body(theme, cx)),
            )
    }

    fn sheet_head(&self, theme: &Theme, cx: &Context<Self>) -> impl IntoElement {
        let search = self.search.read(cx);
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(space(Space::Snug))
            .px(space(Space::Room))
            .py(space(Space::Snug))
            .border_b(hairline())
            .border_color(theme.paint(Paint::Hairline))
            .child(
                text::faint(theme).child(match search.parsed().mode() {
                    Mode::Palette => "commands".to_owned(),
                    Mode::Scoped { project } => format!("in {project}"),
                    Mode::Search => "declarations".to_owned(),
                }),
            )
            .child(div().flex_1())
            .children(
                search
                    .coverage()
                    .iter()
                    .map(|lane| chip::coverage_chip(theme, lane).into_any_element()),
            )
            .when(search.is_searching(), |head| {
                head.child(text::faint(theme).child("…"))
            })
    }

    fn sheet_body(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let reply = self.search.read(cx).reply().clone();
        let rows = match self.search.read(cx).parsed().mode() {
            Mode::Palette => self.palette_rows(theme, cx),
            _ => self.result_rows(theme, cx),
        };
        div()
            .id("omnibar-rows")
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .py(space(Space::Tight))
            .children(rows)
            .when_some(self.sheet_notice(theme, cx), ParentElement::child)
            .when(!matches!(reply, Reply::Idle), |body| {
                body.child(self.palette_reply(theme, &reply))
            })
    }

    fn sheet_notice(&self, theme: &Theme, cx: &Context<Self>) -> Option<AnyElement> {
        let search = self.search.read(cx);
        if let Some(fault) = search.fault() {
            return Some(
                div()
                    .px(space(Space::Room))
                    .py(space(Space::Snug))
                    .child(fault_ui::block(theme, fault, Vec::new()))
                    .into_any_element(),
            );
        }
        if search.len() > 0 || search.is_searching() {
            return None;
        }
        Some(empty_sheet(theme, search).into_any_element())
    }

    fn result_rows(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let search = self.search.read(cx);
        let selected = search.selected();
        let rows: Vec<ResultRow> = search.results().to_vec();
        rows.into_iter()
            .enumerate()
            .map(|(at, row)| self.result_row(theme, at, &row, at == selected, cx))
            .collect()
    }

    fn result_row(
        &mut self,
        theme: &Theme,
        at: usize,
        row: &ResultRow,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let symbol = row.symbol();
        div()
            .id(ElementId::Name(SharedString::from(format!("result-{at}"))))
            .flex()
            .items_start()
            .gap(space(Space::Snug))
            .px(space(Space::Room))
            .py(space(Space::Snug))
            .when(selected, |row| row.bg(theme.paint(Paint::Selected)))
            .hover(|style| style.bg(theme.paint(Paint::Hover)))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(symbol) = symbol {
                    this.open_symbol(symbol, crate::store::document::Target::Here, cx);
                    this.set_field(String::new(), cx);
                }
            }))
            .child(div().pt(px(2.0)).child(glyph::kind_tile(theme, row.kind(), false)))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .flex()
                    .flex_col()
                    .gap(px(1.0))
                    .child(trail_line(theme, row))
                    .child(
                        text::single_line(text::dim(theme))
                            .font_family(theme.specimen())
                            .child(row.preview().to_owned()),
                    ),
            )
            .into_any_element()
    }

    fn palette_rows(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let search = self.search.read(cx);
        let selected = search.selected();
        let rows: Vec<CommandRow> = search.commands().to_vec();
        let mut elements = Vec::with_capacity(rows.len().saturating_add(5));
        let mut domain: Option<CommandDomain> = None;
        for (at, row) in rows.into_iter().enumerate() {
            if domain != Some(row.domain()) {
                domain = Some(row.domain());
                elements.push(domain_header(theme, row.domain()));
            }
            elements.push(self.palette_row(theme, at, row.spec(), at == selected, cx));
        }
        elements
    }

    fn palette_row(
        &mut self,
        theme: &Theme,
        at: usize,
        spec: CommandSpec,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(ElementId::Name(SharedString::from(format!("command-{at}"))))
            .flex()
            .items_center()
            .gap(space(Space::Snug))
            .px(space(Space::Room))
            .py(space(Space::Snug))
            .when(selected, |row| row.bg(theme.paint(Paint::Selected)))
            .hover(|style| style.bg(theme.paint(Paint::Hover)))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.search.update(cx, |search, cx| search.run(spec, cx));
            }))
            .child(nib(theme, selected))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .flex()
                    .flex_col()
                    .gap(px(1.0))
                    .child(
                        text::label(theme)
                            .font_weight(FontWeight::MEDIUM)
                            .child(spec.title.to_owned()),
                    )
                    .child(text::single_line(text::dim(theme)).child(spec.description.to_owned())),
            )
            .child(
                div()
                    .flex_none()
                    .font_family(theme.specimen())
                    .text_size(type_size(TypeScale::Micro))
                    .text_color(theme.paint(Paint::TextFaint))
                    .child(spec.name.to_owned()),
            )
            .into_any_element()
    }

    fn palette_reply(&self, theme: &Theme, reply: &Reply) -> AnyElement {
        let body = div()
            .px(space(Space::Room))
            .py(space(Space::Snug))
            .border_t(hairline())
            .border_color(theme.paint(Paint::Hairline))
            .flex()
            .flex_col()
            .gap(space(Space::Snug));
        match reply {
            Reply::Idle => div().into_any_element(),
            Reply::Running(_) => body.child(text::dim(theme).child("Running…")).into_any_element(),
            Reply::Guidance(message) => body
                .child(text::dim(theme).child(message.clone()))
                .into_any_element(),
            Reply::Lines(lines) => body
                .children(lines.iter().map(|line| {
                    text::single_line(text::dim(theme))
                        .font_family(theme.specimen())
                        .child(line.clone())
                }))
                .into_any_element(),
            Reply::Capabilities(chips) => body
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(space(Space::Tight))
                        .children(
                            chips
                                .iter()
                                .map(|chip| chip::capability_chip(theme, chip).into_any_element()),
                        ),
                )
                .into_any_element(),
            Reply::Faulted(fault) => body
                .child(fault_ui::block(theme, fault, Vec::new()))
                .into_any_element(),
        }
    }
}

fn trail_line(theme: &Theme, row: &ResultRow) -> Div {
    let identity = row.identity();
    div()
        .flex()
        .items_center()
        .gap(space(Space::Tight))
        .min_w(px(0.0))
        .child(
            text::identity_text(theme, TypeScale::Interface)
                .flex_none()
                .child(identity.name().to_owned()),
        )
        .child(
            text::single_line(text::faint(theme))
                .child(text::elide(&identity.compact(), 72).to_string()),
        )
}

fn domain_header(theme: &Theme, domain: CommandDomain) -> AnyElement {
    div()
        .px(space(Space::Room))
        .pt(space(Space::Snug))
        .pb(px(2.0))
        .child(
            text::faint(theme)
                .font_weight(FontWeight::SEMIBOLD)
                .child(domain_name(domain)),
        )
        .into_any_element()
}

const fn domain_name(domain: CommandDomain) -> &'static str {
    match domain {
        CommandDomain::Library => "LIBRARY",
        CommandDomain::Registry => "REGISTRY",
        CommandDomain::Home => "HOME",
        CommandDomain::Session => "SESSION",
        CommandDomain::System => "SYSTEM",
    }
}

fn nib(theme: &Theme, selected: bool) -> Div {
    div()
        .flex_none()
        .w(px(2.0))
        .h(px(18.0))
        .rounded_full()
        .bg(if selected {
            theme.paint(Paint::Gilt)
        } else {
            gpui::transparent_black()
        })
}

fn empty_sheet(theme: &Theme, search: &SearchStore) -> Div {
    let unavailable = search
        .coverage()
        .iter()
        .filter(|lane| lane.standing() != Standing::Complete)
        .count();
    div()
        .px(space(Space::Room))
        .py(space(Space::Base))
        .flex()
        .flex_col()
        .gap(space(Space::Tight))
        .child(text::label(theme).child("No declarations match this text."))
        .when(unavailable > 0, |body| {
            body.child(
                text::dim(theme)
                    .child("The chips above say which lanes answered and which did not."),
            )
        })
}

fn scope_of(mode: &Mode) -> Option<String> {
    match mode {
        Mode::Scoped { project } => Some(project.clone()),
        _ => None,
    }
}

/// Returns the signature preview builder the sheet shares with the reader.
pub(super) fn preview_line(theme: &Theme, signature: &crate::presentation::signature::Signature) -> Div {
    specimen::signature_line(theme, signature, TypeScale::Small)
}
