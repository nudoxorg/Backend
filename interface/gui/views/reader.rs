//! Defines the reader column for `interface-gui`.
//! This module owns the tab strip, the page filter, and the declaration page beneath them.
//! Its narrow surface keeps one page on screen at reading measure, whatever the window does.
//! The page itself is drawn only through the shared walk, so this column cannot drift from the
//! other renderers of the same model.

use core::cell::RefCell;
use core::ops::Range;
use core::time::Duration;
use std::rc::Rc;

use interface_documents::{
    Block, Direction, Inline, MemberGroup, MemberRow, Page, PageTruncation, PageVisitor,
    RelationGroup, RelationRow, RelationRole, Signature, SourceLocation, Symbol, Target, Token,
    TokenKind, walk_page,
};
use gpui::{
    AnyElement, App, ClickEvent, Context, Div, FontWeight, HighlightStyle, InteractiveElement,
    InteractiveText, IntoElement, KeyDownEvent, MouseMoveEvent, SharedString, Stateful,
    StatefulInteractiveElement, Styled, Window, div, prelude::*,
};

use crate::app::{Workspace, open_external, write_clipboard};
use crate::store::document::{DocumentStore, PageKey, PageSlot, Tab, target_key};
use crate::theme::{Color, Radius, Role, Space, Status, Theme, Weight, kind_color};
use crate::ui::{self, hsla};

/// How long a copy glyph shows its check before returning to the plain affordance.
const COPY_FLASH: Duration = Duration::from_millis(1400);

/// The placeholder the page filter carries while it holds nothing.
const FILTER_PLACEHOLDER: &str = "filter this page";

/// The reader's own transient state: what the page filter holds and what was just copied.
///
/// The reader stores no per-page state on the workspace, so it keeps one small global. One
/// window per process is the whole product surface today; a second window would share the
/// filter text, which is a visible but honest limitation.
#[derive(Clone, Debug, Default)]
struct ReaderState {
    /// The page filter's text, empty when the page is unfiltered.
    query: String,
    /// The text whose copy glyph is still showing its check.
    copied: Option<String>,
}

impl gpui::Global for ReaderState {}

/// Draws the reader: tab strip, page filter, then the current page slot.
pub(crate) fn column(
    workspace: &Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    if !cx.has_global::<ReaderState>() {
        cx.set_global(ReaderState::default());
    }
    let state = reader_state(cx);
    let theme = *workspace.theme();
    let slot = workspace.documents().slot();
    let strip = tab_strip(workspace, cx);
    let body = match slot {
        PageSlot::Idle => idle_body(&theme),
        PageSlot::Loading { previous } => match previous {
            Some(page) => page_body(&theme, page, workspace.documents(), &state, cx),
            None => reserved_body(&theme),
        },
        PageSlot::Ready { page } => page_body(&theme, page, workspace.documents(), &state, cx),
        PageSlot::Failed { fault, candidates } => {
            let mut failed = div()
                .id("reader-fault")
                .flex()
                .flex_col()
                .gap(gpui::px(theme.pixels(Space::Tight.rems())))
                .child(ui::fault_block(&theme, fault));
            for candidate in candidates {
                let key = PageKey::of(&candidate.address);
                let name = candidate.name.clone();
                failed = failed.child(ui::button(&theme, candidate.name.as_str(), false, {
                    cx.listener(move |workspace, event: &ClickEvent, _, cx| {
                        let background = event.modifiers().secondary();
                        route_open(workspace, &key, name.as_str(), background, cx);
                    })
                }));
            }
            failed.into_any_element()
        }
    };
    let entity = cx.entity();
    let mut reader = div()
        .id("reader")
        .track_focus(workspace.reader_focus())
        .on_key_down(filter_keys(entity))
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .bg(hsla(theme.palette().app_background()))
        .child(strip);
    if slot.visible().is_some() {
        reader = reader.child(filter_row(workspace, window, &theme, &state, &cx.entity()));
    }
    reader
        .child(
            div()
                .id("reader-scroll")
                .flex_1()
                .overflow_y_scroll()
                .px(gpui::px(theme.pixels(Space::Gutter.rems())))
                .py(gpui::px(theme.pixels(Space::Loose.rems())))
                .max_w(gpui::px(crate::theme::MEASURE_PIXELS * 2.0))
                .child(body),
        )
        .into_any_element()
}

/// The strip of open tabs, drawn only when there are two or more.
fn tab_strip(workspace: &Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    if !workspace.documents().shows_tab_strip() {
        return div().into_any_element();
    }
    let theme = *workspace.theme();
    let entity = cx.entity();
    let active = workspace.documents().active_index();
    let mut strip = div()
        .id("tabs")
        .flex()
        .flex_wrap()
        .gap(gpui::px(theme.pixels(Space::Hair.rems())));
    for (index, tab) in workspace.documents().tabs().iter().enumerate() {
        strip = strip.child(tab_chip(&theme, tab, index, index == active, &entity));
    }
    strip.into_any_element()
}

/// One tab: kind glyph, title, and a close control that does not also select the tab.
fn tab_chip(
    theme: &Theme,
    tab: &Tab,
    index: usize,
    active: bool,
    entity: &gpui::Entity<Workspace>,
) -> Stateful<Div> {
    let ground = if active {
        theme.palette().element_active()
    } else {
        theme.palette().element()
    };
    let select = {
        let entity = entity.clone();
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            entity.update(cx, |workspace, cx| {
                workspace.documents_mut().select_tab(index);
                workspace.refetch_active();
                cx.notify();
            });
        }
    };
    div()
        .id(("tab", index))
        .flex()
        .items_center()
        .gap(gpui::px(theme.pixels(Space::Tight.rems())))
        .px(gpui::px(theme.pixels(Space::Tight.rems())))
        .py(gpui::px(theme.pixels(Space::Hair.rems())))
        .rounded(gpui::px(theme.pixels(Radius::Chip.rems())))
        .bg(hsla(ground))
        .border_1()
        .border_color(hsla(if active {
            theme.palette().border_strong()
        } else {
            theme.palette().border()
        }))
        .cursor_pointer()
        .hover(|style| style.bg(hsla(theme.palette().element_active())))
        .on_click(select)
        .children(tab.kind().map(|kind| ui::kind_glyph_chip(theme, kind)))
        .child(
            ui::with_role(div(), theme, Role::Ui)
                .text_color(hsla(theme.palette().text()))
                .child(tab.title().to_owned()),
        )
        .child(close_control(theme, index, entity))
}

/// The close control on one tab, occluded so a close click never selects the tab beneath it.
fn close_control(
    theme: &Theme,
    index: usize,
    entity: &gpui::Entity<Workspace>,
) -> Stateful<Div> {
    let close = {
        let entity = entity.clone();
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            entity.update(cx, |workspace, cx| {
                if workspace.documents_mut().close_tab(index).is_some() {
                    workspace.refetch_active();
                }
                cx.notify();
            });
        }
    };
    div()
        .id(("tab-close", index))
        .occlude()
        .flex()
        .items_center()
        .justify_center()
        .px(gpui::px(theme.pixels(Space::Hair.rems())))
        .rounded(gpui::px(theme.pixels(Radius::Chip.rems())))
        .cursor_pointer()
        .hover(|style| style.bg(hsla(theme.palette().element_active())))
        .text_color(hsla(theme.palette().text_low()))
        .on_click(close)
        .child("×")
}

/// The first-run reader.
fn idle_body(theme: &Theme) -> AnyElement {
    ui::text_low(theme, Role::Body, "open a package from the shelf, or search").into_any_element()
}

/// The geometry a first load holds open, so the column never collapses while a page is on its way.
fn reserved_body(theme: &Theme) -> AnyElement {
    div()
        .id("reader-reserved")
        .h(gpui::px(theme.pixels(Space::Chapter.rems()) * 3.0))
        .rounded(gpui::px(theme.pixels(Radius::Control.rems())))
        .bg(hsla(theme.palette().element()))
        .into_any_element()
}

/// The dense filter field above the page, with its clear affordance once it holds anything.
///
/// The field borrows the reader's own focus handle: clicking it moves the reader's focus there,
/// and the key-down listener on the reader column edits the filter while that handle is focused.
fn filter_row(
    workspace: &Workspace,
    window: &mut Window,
    theme: &Theme,
    state: &ReaderState,
    entity: &gpui::Entity<Workspace>,
) -> Stateful<Div> {
    let focused = workspace.reader_focus().is_focused(window);
    let mut row = div()
        .id("reader-filter")
        .flex()
        .items_center()
        .gap(gpui::px(theme.pixels(Space::Tight.rems())));
    row = row.child(
        ui::field(theme, Role::Ui, state.query.as_str(), FILTER_PLACEHOLDER, focused)
            .id("reader-filter-field")
            .cursor_pointer()
            .on_click({
                let entity = entity.clone();
                move |_: &ClickEvent, window, cx| {
                    entity.update(cx, |workspace, cx| {
                        workspace.reader_focus().focus(window, cx);
                        cx.notify();
                    });
                }
            }),
    );
    if !state.query.is_empty() {
        row = row.child(
            div()
                .id("reader-filter-clear")
                .cursor_pointer()
                .px(gpui::px(theme.pixels(Space::Tight.rems())))
                .py(gpui::px(theme.pixels(Space::Hair.rems())))
                .rounded(gpui::px(theme.pixels(Radius::Chip.rems())))
                .hover(|style| style.bg(hsla(theme.palette().element())))
                .on_click({
                    let entity = entity.clone();
                    move |_: &ClickEvent, _, cx| {
                        entity.update(cx, |_, cx| {
                            cx.update_global::<ReaderState, ()>(|state, _| state.query.clear());
                            cx.notify();
                        });
                    }
                })
                .child(ui::text_low(theme, Role::Dense, "clear")),
        );
    }
    row
}

/// The key-down listener the reader column hosts for the page filter.
///
/// It acts only while the reader's focus handle is focused: printable characters extend the
/// filter, backspace shortens it, and escape empties it. A key the filter consumed stops
/// propagation, so a typing session never also fires a shortcut; every other key, arrows and
/// shortcuts included, passes through untouched.
fn filter_keys(
    entity: gpui::Entity<Workspace>,
) -> impl Fn(&KeyDownEvent, &mut Window, &mut App) + 'static {
    move |event: &KeyDownEvent, window, cx| {
        entity.update(cx, |workspace, cx| {
            if !workspace.reader_focus().is_focused(window) {
                return;
            }
            let keystroke = &event.keystroke;
            let consumed = match keystroke.key.as_str() {
                "escape" => {
                    cx.update_global::<ReaderState, ()>(|state, _| state.query.clear());
                    true
                }
                "backspace" | "delete" => {
                    cx.update_global::<ReaderState, ()>(|state, _| {
                        state.query.pop();
                    });
                    true
                }
                "enter" | "tab" | "up" | "down" | "left" | "right" | "home" | "end" | "pageup"
                | "pagedown" => false,
                _ => {
                    let shortcut = keystroke.modifiers.control
                        || keystroke.modifiers.alt
                        || keystroke.modifiers.platform
                        || keystroke.modifiers.function;
                    match keystroke.key_char.as_ref() {
                        Some(inserted) if !shortcut && !inserted.is_empty() => {
                            cx.update_global::<ReaderState, ()>(|state, _| {
                                state.query.push_str(inserted);
                            });
                            true
                        }
                        _ => false,
                    }
                }
            };
            if consumed {
                cx.stop_propagation();
                cx.notify();
            }
        });
    }
}

/// Reads the reader's state; before the first render installed it there is nothing to read.
fn reader_state(cx: &App) -> ReaderState {
    cx.try_global::<ReaderState>().cloned().unwrap_or_default()
}

/// One declaration page, driven by the shared walk so this surface cannot disagree about what a
/// page contains. The filter narrows member and relation rows; everything the walk draws above
/// them, and the truncation line below, stays whole.
///
/// The body keeps the page-scroll div it has always had rather than a virtualized list, so a
/// matching row is not scrolled into view programmatically: the walk draws heterogeneous,
/// differently sized sections whose offsets only the scroller knows, and inventing a second
/// scroll geometry to guess them is not a trade this card takes. Matching rows are drawn in
/// full and non-matching groups collapse to counts, so the row a filter points at is on screen
/// or one disclosure away.
fn page_body(
    theme: &Theme,
    page: &Page,
    documents: &DocumentStore,
    state: &ReaderState,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let filter = if state.query.is_empty() {
        None
    } else {
        Some(state.query.to_lowercase())
    };
    let mut visitor = PageBuilder {
        theme: *theme,
        column: div().flex().flex_col().gap(gpui::px(
            theme.pixels(Space::Snug.rems()),
        )),
        entity: cx.entity(),
        documents,
        truncation: page.truncation,
        filter,
        copied: state.copied.clone(),
        prose_seen: 0,
        relations_seen: 0,
    };
    walk_page(page, &mut visitor);
    visitor.draw_truncation();
    visitor.column.into_any_element()
}

/// Builds reader elements in walk order.
struct PageBuilder<'a> {
    theme: Theme,
    column: Div,
    entity: gpui::Entity<Workspace>,
    documents: &'a DocumentStore,
    truncation: PageTruncation,
    /// The lowercased filter needle, absent when the page is unfiltered.
    filter: Option<String>,
    /// The text whose copy glyph still shows its check.
    copied: Option<String>,
    prose_seen: usize,
    relations_seen: usize,
}

impl PageBuilder<'_> {
    fn push(&mut self, element: impl IntoElement) {
        let next = core::mem::replace(&mut self.column, div());
        self.column = next.child(element);
    }

    /// Wraps one identity element in the copy affordance: a click writes the payload to the
    /// platform clipboard and flashes the check glyph.
    fn copy_row(&self, id: &str, payload: String, element: impl IntoElement) -> Stateful<Div> {
        let theme = self.theme;
        div()
            .id(SharedString::from(id.to_owned()))
            .flex()
            .cursor_pointer()
            .rounded(gpui::px(theme.pixels(Radius::Chip.rems())))
            .hover(|style| style.bg(hsla(theme.palette().element())))
            .on_click(copy_listener(self.entity.clone(), payload))
            .child(element)
    }

    /// The one navigable text run: the open and hover listeners attached to the affordance.
    fn navigable(&self, role: Role, color: Color, text: &str, key: &PageKey, id: String) -> Stateful<Div> {
        let theme = self.theme;
        ui::nav_text_at(&theme, role, color, text)
            .id(SharedString::from(id))
            .cursor_pointer()
            .hover(|style| style.bg(hsla(theme.palette().element())))
            .on_click(open_listener(self.entity.clone(), key, text))
            .on_hover(hover_listener(self.entity.clone(), key))
    }

    /// One symbol row: kind glyph, name with the nav affordance, click to open, hover for a card.
    /// The namespace keeps the element id distinct when one symbol appears in several sections.
    fn nav_row(&mut self, namespace: &str, symbol: &Symbol) {
        let theme = self.theme;
        let key = PageKey::of(&symbol.address);
        let row = div()
            .id(SharedString::from(format!("{namespace} {}", key.as_str())))
            .flex()
            .items_center()
            .gap(gpui::px(theme.pixels(Space::Tight.rems())))
            .cursor_pointer()
            .rounded(gpui::px(theme.pixels(Radius::Chip.rems())))
            .hover(|style| style.bg(hsla(theme.palette().element())))
            .on_click(open_listener(self.entity.clone(), &key, symbol.name.as_str()))
            .on_hover(hover_listener(self.entity.clone(), &key))
            .child(ui::kind_glyph_chip(&theme, symbol.kind))
            .child(ui::nav_text(&theme, symbol.name.as_str()));
        self.push(row);
    }

    /// One ancestor crumb as an inline navigable link.
    fn crumb_link(&self, crumb: &Symbol) -> Stateful<Div> {
        let theme = self.theme;
        let key = PageKey::of(&crumb.address);
        ui::nav_text(&theme, crumb.name.as_str())
            .id(SharedString::from(format!("crumb {}", key.as_str())))
            .cursor_pointer()
            .hover(|style| style.bg(hsla(theme.palette().element())))
            .on_click(open_listener(self.entity.clone(), &key, crumb.name.as_str()))
            .on_hover(hover_listener(self.entity.clone(), &key))
    }

    /// One signature token, coloured by its render role, navigable when its target is local and
    /// outbound when its target is external.
    fn token_element(&self, token: &Token, id: &str) -> AnyElement {
        let theme = self.theme;
        let (color, weight) = token_style(&theme, token);
        let weight = FontWeight(f32::from(weight.value()));
        if let Some(path) = external_path(token.target.as_ref()) {
            ui::nav_text_at(&theme, Role::Specimen, color, token.text.as_str())
                .id(SharedString::from(id.to_owned()))
                .cursor_pointer()
                .font_weight(weight)
                .on_click(external_listener(self.entity.clone(), path))
                .into_any_element()
        } else if let Some(key) = token.target.as_ref().and_then(target_key) {
            self.navigable(Role::Specimen, color, token.text.as_str(), &key, id.to_owned())
                .font_weight(weight)
                .into_any_element()
        } else {
            ui::with_role(div(), &theme, Role::Specimen)
                .text_color(hsla(color))
                .font_weight(weight)
                .child(token.text.as_str().to_owned())
                .into_any_element()
        }
    }

    /// One paragraph as a single flowing text element.
    ///
    /// The walk's inline runs are concatenated into one string exactly as the producer wrote
    /// them, so the spacing at every run boundary is the spacing the source carried. Code spans
    /// and resolvable links become byte ranges over that string: code ranges take the mono
    /// family and links take the hairline underline, and the link ranges alone are clickable
    /// and hoverable. Unresolvable links keep their label in the flow without any affordance.
    fn paragraph(&self, index: usize, inlines: &[Inline]) -> AnyElement {
        let theme = self.theme;
        let mut text = String::new();
        let mut highlights: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
        let mut families: Vec<(Range<usize>, SharedString)> = Vec::new();
        let mut links: Vec<ProseLink> = Vec::new();
        for inline in inlines {
            match inline {
                Inline::Text(run) => text.push_str(run.as_str()),
                Inline::Code(run) => {
                    let start = text.len();
                    text.push_str(run.as_str());
                    let range = start..text.len();
                    families.push((
                        range.clone(),
                        SharedString::from(theme.family(Role::Mono.style().face())),
                    ));
                    highlights.push((range, code_highlight(&theme)));
                }
                Inline::Link { label, target } => {
                    let start = text.len();
                    text.push_str(label.as_str());
                    let range = start..text.len();
                    if let Some(path) = external_path(Some(target)) {
                        highlights.push((range.clone(), link_highlight(&theme)));
                        links.push(ProseLink {
                            range,
                            target: ProseTarget::External(path),
                            title: label.as_str().to_owned(),
                        });
                    } else if let Some(key) = target_key(target) {
                        highlights.push((range.clone(), link_highlight(&theme)));
                        links.push(ProseLink {
                            range,
                            target: ProseTarget::Local(key),
                            title: label.as_str().to_owned(),
                        });
                    }
                }
                Inline::Break => text.push('\n'),
            }
        }
        if text.is_empty() {
            return div().into_any_element();
        }
        let ranges: Vec<Range<usize>> = links.iter().map(|link| link.range.clone()).collect();
        let hovered: Rc<RefCell<Option<PageKey>>> = Rc::new(RefCell::new(None));
        InteractiveText::new(
            SharedString::from(format!("prose {index}")),
            ui::styled_paragraph(
                &theme,
                Role::Body,
                theme.palette().text(),
                SharedString::from(text),
                highlights,
                families,
            ),
        )
        .on_click(ranges, {
            let links = links.clone();
            let entity = self.entity.clone();
            move |index, _, cx| {
                let Some(link) = links.get(index) else {
                    return;
                };
                let target = link.target.clone();
                let title = link.title.clone();
                entity.update(cx, |workspace, cx| match target {
                    ProseTarget::Local(key) => {
                        route_open(workspace, &key, title.as_str(), false, cx);
                    }
                    ProseTarget::External(path) => open_external(path.as_str(), cx),
                });
            }
        })
        .on_hover({
            let links = links;
            let entity = self.entity.clone();
            let hovered = Rc::clone(&hovered);
            move |index: Option<usize>, _: MouseMoveEvent, _, cx| {
                let key = index.and_then(|at| {
                    links.iter().find(|link| link.range.contains(&at)).and_then(
                        |link| match &link.target {
                            ProseTarget::Local(key) => Some(key.clone()),
                            ProseTarget::External(_) => None,
                        },
                    )
                });
                let previous = {
                    let mut held = hovered.borrow_mut();
                    if *held == key {
                        return;
                    }
                    let previous = held.take();
                    held.clone_from(&key);
                    previous
                };
                entity.update(cx, |workspace, cx| {
                    if let Some(previous) = previous {
                        workspace.forget_hover(&previous);
                    }
                    if let Some(key) = key {
                        workspace.open_card(&key);
                    }
                    cx.notify();
                });
            }
        })
        .into_any_element()
    }

    /// One member: navigable name, its own mono signature, and its first documentation line.
    fn member_row(&mut self, row: &MemberRow) {
        let theme = self.theme;
        let key = PageKey::of(&row.symbol.address);
        let on_open = open_listener(self.entity.clone(), &key, row.symbol.name.as_str());
        let on_hover = hover_listener(self.entity.clone(), &key);
        self.push(
            div()
                .id(SharedString::from(format!("member {}", key.as_str())))
                .flex()
                .items_center()
                .gap(gpui::px(theme.pixels(Space::Tight.rems())))
                .cursor_pointer()
                .rounded(gpui::px(theme.pixels(Radius::Chip.rems())))
                .hover(|style| style.bg(hsla(theme.palette().element())))
                .on_click(on_open)
                .on_hover(on_hover)
                .child(ui::kind_glyph_chip(&theme, row.symbol.kind))
                .child(ui::nav_text(&theme, row.symbol.name.as_str())),
        );
        let mut line = ui::with_role(div().flex().flex_wrap(), &theme, Role::Mono);
        for (index, token) in row.signature.tokens().iter().enumerate() {
            let id = format!("member {} token {index}", key.as_str());
            line = line.child(self.token_element(token, &id));
        }
        self.push(line);
        if let Some(summary) = &row.summary {
            self.push(ui::text_low(&theme, Role::Dense, summary.as_str()));
        }
    }

    /// One relation end: navigable when local, outbound when external, inert when unresolved.
    /// The two indices keep the element id distinct within and across relation groups.
    fn relation_row(&mut self, row: &RelationRow, group_index: usize, row_index: usize) {
        let theme = self.theme;
        match &row.target {
            Target::Local(symbol) => self.nav_row("relation-row", symbol),
            Target::External(reference) => {
                let label = format!(
                    "{} · {}",
                    reference.display.as_str(),
                    reference.path.as_str()
                );
                let on_open = external_listener(
                    self.entity.clone(),
                    reference.path.as_str().to_owned(),
                );
                self.push(
                    ui::nav_text_at(&theme, Role::Dense, theme.palette().text_low(), &label)
                        .id(SharedString::from(format!(
                            "relation-external {group_index} {row_index}"
                        )))
                        .cursor_pointer()
                        .hover(|style| style.bg(hsla(theme.palette().element())))
                        .on_click(on_open),
                );
            }
            Target::Unresolved(text) => {
                self.push(ui::inert_text(&theme, Role::Dense, text.as_str()));
            }
        }
    }

    /// The warn line naming exactly what the projection budget left out, when anything was.
    fn draw_truncation(&mut self) {
        if self.truncation.is_complete() {
            return;
        }
        let theme = self.theme;
        let mut withheld: Vec<String> = Vec::new();
        if let Some(count) = self.truncation.members {
            withheld.push(format!("{} member rows", count.0));
        }
        if let Some(count) = self.truncation.relations {
            withheld.push(format!("{} relation rows", count.0));
        }
        if let Some(count) = self.truncation.signature_tokens {
            withheld.push(format!("{} signature tokens", count.0));
        }
        let detail = withheld.join(", ");
        self.push(
            ui::with_role(div(), &theme, Role::Dense)
                .text_color(hsla(Status::Warn.color(theme.appearance())))
                .child(format!("page truncated: {detail} withheld")),
        );
    }
}

/// One clickable prose range and where it goes.
#[derive(Clone)]
struct ProseLink {
    /// Byte range the link spells in the paragraph's single string.
    range: Range<usize>,
    /// Where the link lands.
    target: ProseTarget,
    /// The label, which names a background tab opened from this link.
    title: String,
}

/// Where one prose link lands.
#[derive(Clone)]
enum ProseTarget {
    /// A page on the shelf.
    Local(PageKey),
    /// A remote path outside the shelf.
    External(String),
}

/// The canonical remote path of one external target, for links that leave the shelf.
fn external_path(target: Option<&Target>) -> Option<String> {
    match target {
        Some(Target::External(reference)) => Some(reference.path.as_str().to_owned()),
        _ => None,
    }
}

/// The link highlight inside a flowing paragraph: the hairline underline the nav affordance
/// carries, in the border ink rather than the accent, at the reading ink itself.
fn link_highlight(theme: &Theme) -> HighlightStyle {
    HighlightStyle {
        underline: Some(gpui::UnderlineStyle {
            thickness: gpui::px(theme.pixels(Space::Hair.rems())),
            color: Some(hsla(theme.palette().border())),
            wavy: false,
        }),
        ..HighlightStyle::default()
    }
}

/// The code highlight inside a flowing paragraph: the reading ink on the paragraph's own
/// family, with the mono family override carried separately.
fn code_highlight(theme: &Theme) -> HighlightStyle {
    HighlightStyle {
        color: Some(hsla(theme.palette().text())),
        ..HighlightStyle::default()
    }
}

/// Whether one member row survives the filter: its name or its summary carries the needle.
fn member_matches(row: &MemberRow, needle: &str) -> bool {
    carries(row.symbol.name.as_str(), needle)
        || row
            .summary
            .as_ref()
            .is_some_and(|summary| carries(summary.as_str(), needle))
}

/// Whether one relation row survives the filter: whatever it names carries the needle.
fn relation_matches(row: &RelationRow, needle: &str) -> bool {
    match &row.target {
        Target::Local(symbol) => carries(symbol.name.as_str(), needle),
        Target::External(reference) => {
            carries(reference.display.as_str(), needle) || carries(reference.path.as_str(), needle)
        }
        Target::Unresolved(text) => carries(text.as_str(), needle),
    }
}

/// Case-folded substring test; an empty needle lets everything through.
fn carries(haystack: &str, needle: &str) -> bool {
    needle.is_empty() || haystack.to_lowercase().contains(needle)
}

/// The chip label one relation role is drawn with, traversal direction included.
fn relation_label(role: RelationRole) -> String {
    match role.direction {
        Direction::Incoming => format!("incoming {}", ui::relation_word(role.kind)),
        Direction::Outgoing => ui::relation_word(role.kind).to_owned(),
    }
}

/// One token's ink and weight: the specimen's colour rule, one arm per render role.
fn token_style(theme: &Theme, token: &Token) -> (Color, Weight) {
    match token.kind {
        TokenKind::Name => (theme.palette().text(), Weight::Semibold),
        TokenKind::Binding => (theme.palette().text(), Weight::Regular),
        TokenKind::Type => match &token.target {
            Some(Target::Local(symbol)) => (
                kind_color(symbol.kind, theme.appearance()),
                Weight::Regular,
            ),
            Some(Target::External(_) | Target::Unresolved(_)) | None => {
                (theme.palette().text_low(), Weight::Regular)
            }
        },
        TokenKind::Keyword
        | TokenKind::Lifetime
        | TokenKind::Literal
        | TokenKind::Punctuation
        | TokenKind::Text => (theme.palette().text_low(), Weight::Regular),
    }
}

/// The one router every navigable row goes through.
///
/// A foreground open asks the workspace for the page as always. A background open files the
/// tab through the document store and leaves the reader alone; the tab is opened without a
/// fetch, because every fetch the reader owns lands through the front slot and would disturb
/// the page on screen, and a newly selected tab fetches on selection.
fn route_open(
    workspace: &mut Workspace,
    key: &PageKey,
    title: &str,
    background: bool,
    cx: &mut Context<Workspace>,
) {
    if background {
        workspace.documents_mut().open_tab(key.clone(), title, true);
    } else {
        workspace.open_page(key);
    }
    cx.notify();
}

/// The click listener one navigable target installs.
///
/// A plain click opens the target's page in front; a secondary-modifier click, command on this
/// platform and control elsewhere, files a background tab instead.
fn open_listener(
    entity: gpui::Entity<Workspace>,
    key: &PageKey,
    title: &str,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let key = key.clone();
    let title = title.to_owned();
    move |event: &ClickEvent, _, cx| {
        let background = event.modifiers().secondary();
        let key = key.clone();
        let title = title.clone();
        entity.update(cx, |workspace, cx| {
            route_open(workspace, &key, title.as_str(), background, cx);
        });
    }
}

/// The hover listener one navigable target installs: a card is asked for on entry and forgotten
/// again when the pointer leaves.
fn hover_listener(
    entity: gpui::Entity<Workspace>,
    key: &PageKey,
) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
    let key = key.clone();
    move |hovered: &bool, _, cx| {
        let key = key.clone();
        let entered = *hovered;
        entity.update(cx, move |workspace, cx| {
            if entered {
                workspace.open_card(&key);
            } else {
                workspace.forget_hover(&key);
            }
            cx.notify();
        });
    }
}

/// The click listener one outbound link installs: the click hands the path to the platform's
/// browser through the crate's one opener.
fn external_listener(
    entity: gpui::Entity<Workspace>,
    path: String,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    move |_: &ClickEvent, _, cx| {
        entity.update(cx, |_, cx| {
            open_external(path.as_str(), cx);
        });
    }
}

/// The click listener one copy affordance installs: the click writes the payload to the
/// platform clipboard and flashes the check glyph until it decays.
fn copy_listener(
    entity: gpui::Entity<Workspace>,
    payload: String,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    move |_: &ClickEvent, _, cx| {
        entity.update(cx, |_, cx| {
            write_clipboard(payload.as_str(), cx);
            cx.update_global::<ReaderState, ()>(|state, _| {
                state.copied = Some(payload.clone());
            });
            cx.notify();
            let flash = payload.clone();
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(COPY_FLASH).await;
                let _ = this.update(cx, |_, cx| {
                    cx.update_global::<ReaderState, ()>(|state, _| {
                        if state.copied.as_deref() == Some(flash.as_str()) {
                            state.copied = None;
                        }
                    });
                    cx.notify();
                });
            })
            .detach();
        });
    }
}

impl PageVisitor for PageBuilder<'_> {
    fn header(&mut self, symbol: &Symbol) {
        let theme = self.theme;
        self.push(
            div()
                .flex()
                .items_center()
                .gap(gpui::px(theme.pixels(Space::Tight.rems())))
                .child(ui::kind_glyph_chip(&theme, symbol.kind))
                .child(
                    ui::with_role(div(), &theme, Role::Display)
                        .child(symbol.name.as_str().to_owned()),
                ),
        );
        let address = symbol.address.to_string();
        let key = symbol.key().to_string();
        let address_copied = self.copied.as_deref() == Some(address.as_str());
        let key_copied = self.copied.as_deref() == Some(key.as_str());
        self.push(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(gpui::px(theme.pixels(Space::Tight.rems())))
                .child(self.copy_row(
                    "header-address",
                    address.clone(),
                    ui::copyable_identity(&theme, address.as_str(), address_copied),
                ))
                .child(self.copy_row(
                    "header-key",
                    key.clone(),
                    key_tag(&theme, symbol, key_copied),
                )),
        );
    }

    fn trail(&mut self, crumbs: &[Symbol]) {
        let theme = self.theme;
        let mut line = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(gpui::px(theme.pixels(Space::Hair.rems())));
        for crumb in crumbs {
            line = line.child(self.crumb_link(crumb));
            line = line.child(ui::text_low(&theme, Role::Dense, "›"));
        }
        self.push(line);
    }

    fn signature(&mut self, signature: &Signature) {
        let theme = self.theme;
        let mut specimen = ui::with_role(div().flex().flex_wrap(), &theme, Role::Specimen);
        for (index, token) in signature.tokens().iter().enumerate() {
            let id = format!("signature-token {index}");
            specimen = specimen.child(self.token_element(token, &id));
        }
        self.push(specimen);
    }

    fn prose_block(&mut self, block: &Block) {
        match block {
            Block::Paragraph(inlines) => {
                let index = self.prose_seen;
                self.prose_seen = index.saturating_add(1);
                self.push(self.paragraph(index, inlines));
            }
            Block::Code(run) => {
                let theme = self.theme;
                self.push(
                    ui::with_role(div(), &theme, Role::Mono)
                        .text_color(hsla(theme.palette().text()))
                        .px(gpui::px(theme.pixels(Space::Snug.rems())))
                        .py(gpui::px(theme.pixels(Space::Tight.rems())))
                        .rounded(gpui::px(theme.pixels(Radius::Control.rems())))
                        .bg(hsla(theme.palette().element()))
                        .child(run.as_str().to_owned()),
                );
            }
        }
    }

    fn members(&mut self, group: &MemberGroup) {
        let theme = self.theme;
        let collapsed = self.documents.is_collapsed(group.kind);
        let needle = self.filter.clone();
        let matched: Vec<&MemberRow> = match needle.as_deref() {
            Some(needle) => group
                .rows
                .iter()
                .filter(|row| member_matches(row, needle))
                .collect(),
            None => Vec::new(),
        };
        let open = match needle.as_deref() {
            Some(_) => !matched.is_empty(),
            None => !collapsed,
        };
        let disclosure = if open { "▾" } else { "▸" };
        let count_line = match needle.as_deref() {
            Some(_) => format!(
                "{} of {} {}",
                matched.len(),
                group.rows.len(),
                ui::kind_word(group.kind)
            ),
            None => format!("{} {}", group.rows.len(), ui::kind_word(group.kind)),
        };
        let toggle = {
            let entity = self.entity.clone();
            let kind = group.kind;
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                entity.update(cx, |workspace, cx| {
                    workspace.documents_mut().toggle_group(kind);
                    cx.notify();
                });
            }
        };
        self.push(
            div()
                .id(SharedString::from(format!(
                    "group {}",
                    ui::kind_word(group.kind)
                )))
                .flex()
                .items_center()
                .gap(gpui::px(theme.pixels(Space::Tight.rems())))
                .cursor_pointer()
                .on_click(toggle)
                .child(
                    ui::with_role(div(), &theme, Role::Caption)
                        .text_color(hsla(theme.palette().text_low()))
                        .child(disclosure.to_owned()),
                )
                .child(ui::kind_glyph_chip(&theme, group.kind))
                .child(
                    ui::with_role(div(), &theme, Role::Caption)
                        .text_color(hsla(theme.palette().text_low()))
                        .child(count_line.to_uppercase()),
                ),
        );
        if needle.is_some() {
            if matched.is_empty() {
                return;
            }
            for row in matched {
                self.member_row(row);
            }
        } else {
            if collapsed {
                return;
            }
            for row in &group.rows {
                self.member_row(row);
            }
        }
    }

    fn relations(&mut self, group: &RelationGroup) {
        let theme = self.theme;
        let expanded = self.documents.is_expanded(group.role);
        let needle = self.filter.clone();
        let index = self.relations_seen;
        self.relations_seen = index.saturating_add(1);
        let matched: Vec<&RelationRow> = match needle.as_deref() {
            Some(needle) => group
                .rows
                .iter()
                .filter(|row| relation_matches(row, needle))
                .collect(),
            None => Vec::new(),
        };
        let open = match needle.as_deref() {
            Some(_) => !matched.is_empty(),
            None => expanded,
        };
        let disclosure = if open { "▾" } else { "▸" };
        let count_line = match needle.as_deref() {
            Some(_) => format!("{} of {}", matched.len(), group.rows.len()),
            None => group.rows.len().to_string(),
        };
        let toggle = {
            let entity = self.entity.clone();
            let role = group.role;
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                entity.update(cx, |workspace, cx| {
                    workspace.documents_mut().toggle_relation(role);
                    cx.notify();
                });
            }
        };
        self.push(
            div()
                .id(SharedString::from(format!("relation-chip {index}")))
                .flex()
                .items_center()
                .gap(gpui::px(theme.pixels(Space::Hair.rems())))
                .px(gpui::px(theme.pixels(Space::Tight.rems())))
                .py(gpui::px(theme.pixels(Space::Hair.rems())))
                .rounded(gpui::px(theme.pixels(Radius::Chip.rems())))
                .bg(hsla(theme.palette().element()))
                .cursor_pointer()
                .on_click(toggle)
                .child(
                    ui::with_role(div(), &theme, Role::Caption)
                        .text_color(hsla(theme.palette().text_low()))
                        .child(disclosure.to_owned()),
                )
                .child(
                    ui::with_role(div(), &theme, Role::Ui)
                        .text_color(hsla(theme.palette().text()))
                        .child(relation_label(group.role)),
                )
                .child(
                    ui::with_role(div(), &theme, Role::Dense)
                        .text_color(hsla(theme.palette().text_low()))
                        .child(count_line),
                ),
        );
        if needle.is_some() {
            if matched.is_empty() {
                return;
            }
            for (row_index, row) in matched.into_iter().enumerate() {
                self.relation_row(row, index, row_index);
            }
        } else {
            if !expanded {
                return;
            }
            for (row_index, row) in group.rows.iter().enumerate() {
                self.relation_row(row, index, row_index);
            }
        }
    }

    fn source(&mut self, location: &SourceLocation) {
        let theme = self.theme;
        self.push(ui::text_low(
            &theme,
            Role::Dense,
            &format!(
                "{} · bytes {}..{}",
                location.file.as_str(),
                location.span.start.0,
                location.span.end.0
            ),
        ));
    }
}

/// The key tag: the family and variant halves in lower hex, the accent's second lawful use,
/// with its own copy glyph.
fn key_tag(theme: &Theme, symbol: &Symbol, copied: bool) -> Div {
    ui::identity_text(theme, &symbol.key().to_string())
        .flex()
        .items_center()
        .gap(gpui::px(theme.pixels(Space::Hair.rems())))
        .px(gpui::px(theme.pixels(Space::Tight.rems())))
        .py(gpui::px(theme.pixels(Space::Hair.rems())))
        .rounded(gpui::px(theme.pixels(Radius::Chip.rems())))
        .border_1()
        .border_color(hsla(theme.palette().border()))
        .child(ui::copy_glyph(copied).to_owned())
}

/// The key a page's own symbol points at, for tests that walk the reader.
#[must_use]
pub fn self_key(page: &Page) -> PageKey {
    PageKey::of(&page.symbol.address)
}

/// Whether one target is local, mirroring the store's own rule.
#[must_use]
pub fn is_local(target: &Target) -> bool {
    target_key(target).is_some()
}
