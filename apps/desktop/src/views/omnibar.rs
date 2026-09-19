//! The titlebar and the omnibar sheet it drops.
//! One capsule that searches, scopes, or becomes a command palette.
//! Its head always states the coverage behind whatever it is showing.
//!
//! The titlebar is also the way back. Back, forward, and home sit at its left
//! edge, so a reader who folded the shelf away to read a wide signature can
//! still leave the page they are on without first reopening a panel.
//!
//! The sheet is anchored under the capsule rather than centred on the window,
//! so a reader's eye never has to travel: the answer appears exactly beneath
//! the question. Its first row is the coverage strip, which means the honest
//! account of what was searched is read before the results are.
//!
//! A result row has no lane tag, and that absence is deliberate. Coverage is a
//! property of the *reply*, not of a row — the engine does not say which lane
//! found which declaration — so the strip at the head states it once and
//! honestly, rather than every row carrying a guess. What each row does carry
//! is what the engine did say about it: its kind, its language, its signature
//! in colour, and — only when it is not simply ready — its publication state.

use super::chrome::Platform;
use super::keys;
use super::workspace::Workspace;
use crate::motion::{Beat, once};
use crate::presentation::chips::Standing;
use crate::presentation::crumb;
use crate::store::search::{CommandRow, Mode, Reply, ResultRow, SearchStore};
use crate::store::shell::Focus;
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Chrome, Radius, Space, TypeScale, hairline, radius, space, type_size};
use crate::ui::icon::Icon;
use crate::ui::tip::{Tip, Tipped as _};
use crate::ui::{button, chip, fault as fault_ui, glyph, specimen, surface, text};
use backend_library::CommandDomain;
use backend_present::{RecordState, domain_name};
use gpui::Focusable as _;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnimationExt as _, AnyElement, Context, Div, ElementId, FontWeight, InteractiveElement,
    IntoElement, ParentElement, SharedString, StatefulInteractiveElement, Styled, Window,
    WindowControlArea, div, px,
};
use gpui_elements::editable_text::text_input;

/// Character budget for a result row's second line.
const PREVIEW: usize = 120;

impl Workspace {
    /// Returns the titlebar row: navigation, the omnibar, and the shell keys.
    pub(super) fn titlebar(
        &mut self,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let platform = Platform::current();
        let controls = Self::window_controls(theme, window, cx);
        div()
            .id("titlebar")
            .flex_none()
            .h(px(Chrome::TITLEBAR))
            .w_full()
            .flex()
            .items_center()
            .pl(px(platform.leading_gap()))
            .gap(space(Space::Base))
            .border_b(hairline())
            .border_color(theme.paint(Paint::Hairline))
            .when(platform.owns_drag(), |bar| bar.window_control_area(WindowControlArea::Drag))
            .child(self.navigation(theme, cx))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .justify_center()
                    .child(self.omnibar(theme, cx)),
            )
            .child(self.titlebar_keys(theme, cx))
            .when_some(controls, ParentElement::child)
            .when(!platform.draws_controls(), |bar| bar.pr(space(Space::Base)))
    }

    /// Returns back, forward, and home.
    fn navigation(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let document = self.document.read(cx);
        let back = document.can_go_back();
        let forward = document.can_go_forward();
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(2.0))
            .child(nav_button(theme, "nav-back", Icon::ArrowLeft, back, Tip::new("Back").key(keys::GO_BACK), cx.listener(|this, _, _, cx| {
                this.document.update(cx, crate::store::document::DocumentStore::back);
            })))
            .child(nav_button(theme, "nav-forward", Icon::ArrowRight, forward, Tip::new("Forward").key(keys::GO_FORWARD), cx.listener(|this, _, _, cx| {
                this.document.update(cx, crate::store::document::DocumentStore::forward);
            })))
            .child(nav_button(theme, "nav-home", Icon::Home, true, Tip::new("Home").detail("Pinned, recent, and the registry.").key(keys::GO_HOME), cx.listener(|this, _, _, cx| {
                this.open_home(cx);
            })))
    }

    fn titlebar_keys(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let appearance = self.shell.read(cx).prefs().appearance();
        let mark = if appearance.is_dark() {
            Icon::Moon
        } else {
            Icon::Sun
        };
        div()
            .flex_none()
            .flex()
            .items_center()
            .justify_end()
            .gap(space(Space::Tight))
            .child(
                button::icon_button(theme, "appearance", mark)
                    .tip(Tip::new(if appearance.is_dark() { "Switch to Vellum" } else { "Switch to Ink" })
                        .detail("The light and dark palettes of this window.")
                        .key(keys::TOGGLE_APPEARANCE))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.shell.update(cx, |shell, cx| {
                            let next = shell.prefs().appearance().flipped();
                            shell.set_appearance(next, cx);
                        });
                    })),
            )
            .child(
                button::icon_button(theme, "settings", Icon::Gear)
                    .tip(Tip::new("Settings")
                        .detail("Appearance, editor, agents, diagnostics, legend.")
                        .key(keys::OPEN_SETTINGS))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.shell
                            .update(cx, super::super::store::shell::ShellStore::toggle_settings);
                    })),
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
            .child(Self::omnibar_nib(theme, &mode))
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
            .when(!focused, |bar| {
                bar.child(button::key_hint(theme, &keys::FOCUS_OMNIBAR.label()))
            })
    }

    /// Returns the mark that says what the field is currently for.
    ///
    /// Deliberately never the character the reader typed. An earlier version
    /// drew `›` for palette mode, which sat directly beside the `>` in the
    /// field and read as a stutter. The mark names the *mode*; the field shows
    /// the text.
    fn omnibar_nib(theme: &Theme, mode: &Mode) -> Div {
        let (mark, role) = match mode {
            Mode::Palette => (Icon::Command, Paint::Gilt),
            Mode::Scoped { .. } => (Icon::Folder, Paint::Gilt),
            Mode::Search => (Icon::Search, Paint::TextFaint),
        };
        div()
            .flex_none()
            .flex()
            .items_center()
            .child(crate::ui::icon::sized(theme, mark, 13.0, role))
    }

    fn focus_omnibar_from_click(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.field.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        self.shell
            .update(cx, |shell, cx| shell.focus_on(Focus::Omnibar, cx));
        self.search.update(cx, SearchStore::open);
    }
}

fn nav_button(
    theme: &Theme,
    id: &'static str,
    mark: Icon,
    enabled: bool,
    tip: Tip,
    listener: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<Div> {
    div()
        .id(ElementId::Name(SharedString::new_static(id)))
        .flex()
        .flex_none()
        .w(px(24.0))
        .h(px(24.0))
        .items_center()
        .justify_center()
        .rounded(radius(Radius::Small))
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(|style| style.bg(theme.paint(Paint::Hover)))
                .on_click(listener)
        })
        .child(crate::ui::icon::sized(
            theme,
            mark,
            14.0,
            if enabled { Paint::TextDim } else { Paint::TextFaint },
        ))
        .tip(tip)
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
        let reduced = theme.reduced_motion();
        div()
            .absolute()
            .inset_0()
            .child(
                surface::scrim(theme)
                    .id("omnibar-scrim")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.dismiss_sheet_from_scrim(window, cx);
                    })),
            )
            .child(
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
                            .child(self.sheet_body(theme, cx))
                            .with_animation(
                                "omnibar-sheet-drop",
                                once(Beat::Unfold, reduced),
                                move |sheet, delta| {
                                    sheet.opacity(crate::motion::entering_opacity(delta))
                                },
                            ),
                    ),
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
            .child(text::faint(theme).child(match search.parsed().mode() {
                Mode::Palette if !search.parsed().arguments().is_empty() => {
                    format!("runs with  {}", search.parsed().arguments())
                }
                Mode::Palette => format!("commands · {}", backend_present::registry_size()),
                Mode::Scoped { project } => format!("in {project}"),
                Mode::Search => "declarations".to_owned(),
            }))
            .child(div().flex_1())
            .children(
                search
                    .coverage()
                    .iter()
                    .filter(|lane| lane.standing() != Standing::Complete)
                    .map(|lane| chip::coverage_chip(theme, lane).into_any_element()),
            )
            .when(search.is_searching(), |head| {
                head.child(text::faint(theme).child("…"))
            })
    }

    fn sheet_body(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let reply = self.search.read(cx).reply().clone();
        let (rows, selected_child) = match self.search.read(cx).parsed().mode() {
            Mode::Palette => self.palette_rows(theme, cx),
            Mode::Search | Mode::Scoped { .. } => self.result_rows(theme, cx),
        };
        self.reveal_row(selected_child);
        div()
            .id("omnibar-rows")
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .track_scroll(&self.sheet_scroll)
            .flex()
            .flex_col()
            .py(space(Space::Tight))
            .children(rows)
            .when(self.search.read(cx).has_more(), |body| {
                body.child(more_rows(theme))
            })
            .when_some(self.sheet_notice(theme, cx), ParentElement::child)
            .when(!matches!(reply, Reply::Idle), |body| {
                body.child(Self::palette_reply(theme, &reply))
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
        if search.parsed().term().trim().is_empty() && !matches!(search.parsed().mode(), Mode::Palette) {
            return Some(hints(theme).into_any_element());
        }
        Some(empty_sheet(theme, search).into_any_element())
    }

    /// Scrolls the row Return would act on into view when the selection moves.
    ///
    /// Without this the sheet opens wherever it was last left — for a
    /// thirty-five row palette that means opening at the bottom, with the
    /// selected row off screen and the arrow keys apparently doing nothing.
    /// It only scrolls when the selection actually changed, so a reader who
    /// scrolls the sheet by hand is not fought.
    fn reveal_row(&mut self, child: Option<usize>) {
        let Some(child) = child else {
            self.revealed_row = None;
            return;
        };
        if self.revealed_row == Some(child) {
            return;
        }
        self.sheet_scroll.scroll_to_item(child);
        self.revealed_row = Some(child);
    }

    fn result_rows(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> (Vec<AnyElement>, Option<usize>) {
        let search = self.search.read(cx);
        let selected = search.selected();
        let reduced = theme.reduced_motion();
        let term = search.parsed().term().trim().to_owned();
        let rows: Vec<ResultRow> = search.results().to_vec();
        let child = (selected < rows.len()).then_some(selected);
        let elements = rows
            .into_iter()
            .enumerate()
            .map(|(at, row)| Self::result_row(theme, at, &row, &term, at == selected, reduced, cx))
            .collect();
        (elements, child)
    }

    fn result_row(
        theme: &Theme,
        at: usize,
        row: &ResultRow,
        term: &str,
        selected: bool,
        reduced: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let symbol = row.symbol();
        let record = row.record();
        let package = record.identity().shape() == backend_present::IdentityShape::Package;
        let second = result_second(theme, record, package);
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
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                if let Some(symbol) = symbol {
                    this.open_symbol(symbol, super::context::click_target(event), cx);
                    this.set_field(String::new(), cx);
                }
            }))
            .child(nib(theme, at, selected, reduced))
            .child(div().pt(px(2.0)).child(if package {
                glyph::package_tile(theme, false)
            } else {
                glyph::kind_tile(theme, record.kind(), false)
            }))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .flex()
                    .flex_col()
                    .gap(px(1.0))
                    .child(trail_line(theme, row, term))
                    .child(second),
            )
            .child(result_side(theme, record))
            .into_any_element()
    }

    /// Returns the palette rows and the child index of the selected one.
    ///
    /// The two differ: domain headers are children too, so the selected
    /// command's position in the list is not its position among the children
    /// the sheet scrolls through.
    fn palette_rows(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> (Vec<AnyElement>, Option<usize>) {
        let search = self.search.read(cx);
        let selected = search.selected();
        let reduced = theme.reduced_motion();
        let rows: Vec<CommandRow> = search.commands().to_vec();
        let mut elements = Vec::with_capacity(rows.len().saturating_add(5));
        let mut domain: Option<CommandDomain> = None;
        let mut child = None;
        for (at, row) in rows.into_iter().enumerate() {
            if domain != Some(row.domain()) {
                domain = Some(row.domain());
                elements.push(domain_header(theme, row.domain()));
            }
            if at == selected {
                child = Some(elements.len());
            }
            elements.push(Self::palette_row(theme, at, row, at == selected, reduced, cx));
        }
        (elements, child)
    }

    fn palette_row(
        theme: &Theme,
        at: usize,
        row: CommandRow,
        selected: bool,
        reduced: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let spec = row.spec();
        div()
            .id(ElementId::Name(SharedString::from(format!("command-{at}"))))
            .flex()
            .items_center()
            .gap(space(Space::Snug))
            .px(space(Space::Room))
            .py(space(Space::Snug))
            .when(selected, |element| {
                element.bg(theme.paint(Paint::Selected))
            })
            .hover(|style| style.bg(theme.paint(Paint::Hover)))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.search.update(cx, |search, cx| search.run(row, cx));
            }))
            .child(nib(theme, at, selected, reduced))
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
                    .max_w(px(280.0))
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .font_family(theme.specimen())
                    .text_size(type_size(TypeScale::Micro))
                    .text_color(theme.paint(Paint::TextFaint))
                    .child(row.grammar().usage()),
            )
            .into_any_element()
    }

    fn palette_reply(theme: &Theme, reply: &Reply) -> AnyElement {
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
            Reply::Running(_) => body
                .child(text::dim(theme).child("Running…"))
                .into_any_element(),
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

/// Returns a result row's second line: the signature in colour, or the trail.
fn result_second(theme: &Theme, record: &backend_present::Record, package: bool) -> AnyElement {
    record.signature().map_or_else(
        || {
            text::single_line(text::faint(theme))
                .child(if package {
                    "project on the shelf".to_owned()
                } else {
                    crumb::compact(record.identity())
                })
                .into_any_element()
        },
        |signature| {
            div()
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis()
                .font_family(theme.specimen())
                .text_size(type_size(TypeScale::Small))
                .child(specimen::signature_line(theme, signature, PREVIEW))
                .into_any_element()
        },
    )
}

/// Returns the language tag and, only when it is not simply ready, the state.
fn result_side(theme: &Theme, record: &backend_present::Record) -> Div {
    div()
        .flex_none()
        .pt(px(2.0))
        .flex()
        .items_center()
        .gap(space(Space::Tight))
        .child(glyph::language_tag(theme, record.language()))
        .when(record.state() != RecordState::Ready, |side| {
            side.child(
                text::faint(theme)
                    .flex_none()
                    .text_color(theme.paint(Paint::Caution))
                    .child(record.state().name().to_owned()),
            )
        })
}

fn trail_line(theme: &Theme, row: &ResultRow, term: &str) -> Div {
    let identity = row.identity();
    div()
        .flex()
        .items_center()
        .gap(space(Space::Tight))
        .min_w(px(0.0))
        .child(
            div()
                .flex_none()
                .text_size(type_size(TypeScale::Interface))
                .text_color(theme.paint(Paint::Text))
                .font_weight(FontWeight::MEDIUM)
                .child(text::highlighted(theme, identity.name(), term, TypeScale::Interface)),
        )
        .child(
            text::single_line(text::faint(theme))
                .child(text::elide(&crumb::compact(identity), 72).to_string()),
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
                .child(domain_name(domain).to_ascii_uppercase()),
        )
        .into_any_element()
}

/// Returns the gilt nib that springs open beside the selected command.
///
/// The nib is the only moving thing in the palette, and it moves for one
/// reason: it is the answer to "which row does Return run?". Under reduced
/// motion the same element appears at full height on the first frame.
fn nib(theme: &Theme, at: usize, selected: bool, reduced: bool) -> AnyElement {
    if !selected {
        return div().flex_none().w(px(2.0)).h(px(18.0)).into_any_element();
    }
    div()
        .flex_none()
        .w(px(2.0))
        .h(px(18.0))
        .rounded_full()
        .bg(theme.paint(Paint::Gilt))
        .with_animation(
            ElementId::Name(SharedString::from(format!("palette-nib-{at}"))),
            once(Beat::Touch, reduced),
            |element, delta| element.h(px(4.0 + 14.0 * delta)),
        )
        .into_any_element()
}

/// Returns what the field can do, shown when it is open and empty.
fn hints(theme: &Theme) -> Div {
    let rows = [
        ("type a name", "search every declaration on the shelf"),
        ("@project name", "search inside one project"),
        ("> command", "run a product command"),
    ];
    div()
        .px(space(Space::Room))
        .py(space(Space::Base))
        .flex()
        .flex_col()
        .gap(space(Space::Snug))
        .children(rows.into_iter().map(|(what, does)| {
            div()
                .flex()
                .items_center()
                .gap(space(Space::Snug))
                .child(
                    div()
                        .flex_none()
                        .w(px(120.0))
                        .font_family(theme.specimen())
                        .text_size(type_size(TypeScale::Small))
                        .text_color(theme.paint(Paint::Gilt))
                        .child(what),
                )
                .child(text::dim(theme).child(does))
        }))
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
                    .child("The chips above say which lanes did not answer."),
            )
        })
}

/// Returns the line that says the service held rows back beyond this page.
///
/// A bounded reply that stops at its limit and says nothing about it reads as
/// "that is all there is". This row is the difference between a complete
/// answer and a first page.
fn more_rows(theme: &Theme) -> Div {
    div()
        .px(space(Space::Room))
        .py(space(Space::Snug))
        .border_t(hairline())
        .border_color(theme.paint(Paint::Hairline))
        .child(
            text::faint(theme)
                .child("More declarations match than this page holds. Narrow the text to see them."),
        )
}

fn scope_of(mode: &Mode) -> Option<String> {
    match mode {
        Mode::Scoped { project } => Some(project.clone()),
        Mode::Search | Mode::Palette => None,
    }
}
