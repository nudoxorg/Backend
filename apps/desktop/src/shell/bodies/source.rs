//! Source, plain: the file's crumb, the text around the declaration with its
//! lines numbered (the declaration's numbers lit mint), and the
//! declaration's one sentence and callers in the margin.

pub(crate) mod paging;
mod cargo;
pub(super) use cargo::body as cargo_body;

#[cfg(test)]
use paging::{MAX_SOURCE_BYTES, MAX_SOURCE_LINE_BYTES, MAX_SOURCE_LINES, previous_cursor};
use paging::{PagingState, SourceCursor, SourcePage, initial_cursor, verified_link};

use super::state::{DisplayEvidence, display_evidence, earlier_notice, not_ready};
use super::{Ctx, Leaf};
#[cfg(test)]
use crate::model::pages::ByteSpan;
use crate::model::pages::{
    DocFragment, PageKey, RelationKind, SourceCoverage, SourceOrigin, SourceText, SourceView, SymbolRef,
};
use crate::navigation::{Intent, Route, SymbolRoute};
use crate::navigation::presentation::{ReadingChange, SourceLineDraft, VisitId};
use crate::shell::focus::{Recall, Target, TargetAction};
use crate::shell::kit::{gap_words, quiet, symbol_route, text};
use crate::shell::reader::Reader;
use crate::shell::region::Links;
use facet::tokens::ty;
use facet::{Control, Set as _, Space};
use gpui::{
    App, AppContext as _, ClickEvent, Context, ElementId, Entity, Focusable, InteractiveElement,
    InteractiveText, IntoElement, ParentElement, SharedString, StatefulInteractiveElement, Styled,
    StyledText, Subscription, Window, div, px,
};
use gpui_component::input::{InputEvent, InputState};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

const CONTEXT_BEFORE: u32 = 24;
const MAX_PAGE_REFERENCES: usize = 32;

/// One native reading of exactly the source bytes this visual row paints.
/// Hanging indentation is presentation, and a long physical line is never
/// repeated wholesale for each bounded continuation.
fn source_piece_label(number: u32, source_line: &str, piece: &crate::shell::text_fit::CodeLine) -> Option<String> {
    let words = source_line.get(piece.source_range.clone())?;
    Some(format!("Line {number}{}: {words}", if piece.continued { " continued" } else { "" }))
}

fn paired_admission(
    source: Rc<dyn Fn(&mut App) -> bool>,
    semantic: Rc<dyn Fn(&mut App) -> bool>,
) -> Rc<dyn Fn(&mut App) -> bool> {
    Rc::new(move |app| source(app) && semantic(app))
}

fn retained_page_admission(
    ctx: &Ctx<'_>,
    display: &Arc<crate::runtime::snapshot::RetainedDisplay>,
    route: &Route,
    cx: &mut Context<Reader>,
) -> Rc<dyn Fn(&mut App) -> bool> {
    let local = ctx.native_local_guard(cx);
    let expected = Arc::clone(display);
    let route = route.clone();
    let links = ctx.links.clone();
    Rc::new(move |app| {
        local(app) && {
            let store = links.store.read(app);
            super::retained::select(store, &route, store.snapshot().overlay())
                .is_some_and(|now| Arc::ptr_eq(&now, &expected))
        }
    })
}

struct Pager {
    state: Rc<RefCell<Option<PagingState>>>,
    first: u32,
    last: u32,
    input: Entity<InputState>,
    error: Option<SharedString>,
    recall: Recall,
    reveal: Rc<Cell<bool>>,
    row_id: Rc<dyn Fn(u32) -> SharedString>,
    admission: RefCell<Option<Rc<dyn Fn(&mut App) -> bool>>>,
    draft_owner: DraftOwner,
    _subscription: Subscription,
}

struct DraftOwner {
    visit: VisitId,
    route: Route,
    links: Links,
}

impl DraftOwner {
    fn from_ctx(route: Route, ctx: &Ctx<'_>, cx: &App) -> (Self, SourceLineDraft) {
        let snapshot = ctx.links.snapshot(cx);
        let reading = &snapshot.session().reading.current;
        let draft = if ctx.active && snapshot.route() == &route {
            reading.presentation.controls().source_line_draft.clone()
        } else {
            SourceLineDraft::default()
        };
        (Self { visit: reading.id, route, links: ctx.links.clone() }, draft)
    }

    fn save(&self, draft: SourceLineDraft, cx: &mut App) {
        let snapshot = self.links.snapshot(cx);
        if snapshot.route() == &self.route && snapshot.session().reading.current.id == self.visit
            && snapshot.overlay().is_none() && snapshot.page_overlay().is_none()
            && snapshot.session().preview.is_none()
        {
            self.links.dispatch(Intent::SetReading {
                visit: self.visit,
                change: ReadingChange::SourceLineDraft(draft),
            }, cx);
        }
    }
}

impl Pager {
    fn new(
        state: Rc<RefCell<Option<PagingState>>>,
        first: u32,
        last: u32,
        recall: Recall,
        reveal: Rc<Cell<bool>>,
        row_id: Rc<dyn Fn(u32) -> SharedString>,
        draft_owner: DraftOwner,
        draft: SourceLineDraft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Line number")
                .validate(|text, _| SourceLineDraft::new(text).is_some())
        });
        if !draft.as_str().is_empty() {
            input.update(cx, |input, cx| input.set_value(draft.as_str().to_owned(), window, cx));
        }
        let subscription = cx.subscribe_in(
            &input,
            window,
            |pager, input, event: &InputEvent, _window, cx| {
                match event {
                    InputEvent::Change => {
                        if let Some(draft) = SourceLineDraft::new(input.read(cx).value().to_string()) {
                            pager.draft_owner.save(draft, cx);
                        }
                    }
                    InputEvent::PressEnter { .. } => {
                        let admitted = pager.admission.borrow().as_ref().is_some_and(|admit| admit(cx));
                        if admitted { pager.jump(&input.read(cx).value().to_string(), cx); }
                    }
                    _ => {}
                }
            },
        );
        Self {
            state,
            first,
            last,
            input,
            error: None,
            recall,
            reveal,
            row_id,
            admission: RefCell::new(None),
            draft_owner,
            _subscription: subscription,
        }
    }

    fn focus_line(&mut self, line: u32, cx: &mut Context<Self>) {
        self.error = None;
        self.recall.focus((self.row_id)(line));
        self.reveal.set(true);
        cx.notify();
    }

    fn next(&mut self, expected: SourceCursor, target: SourceCursor, cx: &mut Context<Self>) {
        let line = self
            .state
            .borrow_mut()
            .as_mut()
            .filter(|state| state.cursor == expected)
            .map(|state| state.next(target).line);
        if let Some(line) = line {
            self.focus_line(line, cx);
        }
    }

    fn previous(&mut self, expected: SourceCursor, target: SourceCursor, cx: &mut Context<Self>) {
        let line = self
            .state
            .borrow_mut()
            .as_mut()
            .filter(|state| state.cursor == expected)
            .map(|state| state.previous(target).line);
        if let Some(line) = line {
            self.focus_line(line, cx);
        }
    }

    fn jump(&mut self, text: &str, cx: &mut Context<Self>) {
        match text.trim().parse::<u32>() {
            Ok(line) if (self.first..=self.last).contains(&line) => {
                let changed = self.state.borrow_mut().as_mut().is_some_and(|state| {
                    state.jump(line);
                    true
                });
                if changed {
                    self.focus_line(line, cx);
                }
            }
            _ => {
                self.error =
                    Some(format!("Enter a line from {} to {}", self.first, self.last).into());
                cx.notify();
            }
        }
    }

    fn show_references(&mut self, page: usize, cx: &mut Context<Self>) {
        if let Some(state) = self.state.borrow_mut().as_mut() {
            state.reference_page = page;
        }
        self.recall.focus(format!(
            "source-reference-{}",
            page.saturating_mul(MAX_PAGE_REFERENCES)
        ));
        self.reveal.set(true);
        cx.notify();
    }
}

pub(super) fn body(
    place: &Route,
    route: &SymbolRoute,
    store: &super::Pages,
    ctx: &mut Ctx<'_>,
    window: &mut Window,
    cx: &mut Context<Reader>,
) -> Vec<Leaf> {
    let symbol = match crate::runtime::store::route_declaration(place) {
        Ok(symbol) => symbol,
        Err(unread) => return ctx.unread(&unread),
    };
    let resource = store.source(&symbol);
    let live = ctx.links.store.read(cx);
    let root = live.snapshot().key();
    let serving = live.owner_serving();
    let symbol_resource = live.symbol(&symbol);
    let symbol_current = matches!(
        crate::core::admit_resource(&symbol_resource, root, serving),
        crate::core::ResourceAdmission::Current(page)
            if page.identity.coordinate.as_str() == symbol.as_str()
    );
    let semantic_notice = match symbol_resource.terminal() {
        crate::core::ResourceTerminal::Fault(error) => format!(
            "The source is current, but the declaration reading failed: {}. Semantic links and source controls are disabled.",
            error.message(),
        ),
        crate::core::ResourceTerminal::Unavailable(_) =>
            "The source is current, but this declaration is not served by the current producer. Semantic links and source controls are disabled.".to_owned(),
        crate::core::ResourceTerminal::Complete | crate::core::ResourceTerminal::Partial =>
            "The source is current, but this declaration's semantic reading is still pending. Semantic links and source controls are disabled.".to_owned(),
    };
    drop(live);
    let evidence = display_evidence(&resource, root, serving, |view| {
        view.symbol.coordinate.as_str() == symbol.as_str()
    });
    let view = match &evidence {
        DisplayEvidence::Current(view) if symbol_current => (**view).clone(),
        DisplayEvidence::Current(view) => {
            return read_only_source(view, &semantic_notice, route, ctx);
        }
        DisplayEvidence::Earlier { value, .. } => {
            return read_only_source(value, &earlier_notice(&evidence).unwrap_or_default(), route, ctx);
        }
        DisplayEvidence::Missing(other) => {
            let name = symbol.identity().name().to_owned();
            return not_ready(other, &PageKey::Source(symbol), &name, ctx, cx);
        }
        DisplayEvidence::WrongIdentity => return vec![Leaf::new(quiet(
            "The saved source belongs to another exact declaration or release.", &ctx.measure, ctx.palette,
        ))],
    };
    let measure = ctx.measure;
    let palette = ctx.palette;
    let mut leaves = Vec::new();
    // The crumb: package › file › declaration.
    let identity = symbol.identity();
    let mut crumb = Vec::new();
    if let Some(project) = identity.project() {
        crumb.push(project.name().to_owned());
    }
    if let Some(file) = view.file.known() {
        crumb.push(file.to_string());
    }
    crumb.push(view.symbol.name.to_string());
    let crumb = ctx.say(crumb.join("  ›  "));
    leaves.push(Leaf::new(
        text(ty::MONO_ROW, &measure, palette.ink2).role(gpui::Role::Label).aria_label(crumb.clone()).child(crumb),
    ));
    match view.text.known() {
        Some(source) => {
            if source.origin == SourceOrigin::LocalFile
                || source.coverage() == SourceCoverage::Unverified
            {
                let status = match source.coverage() {
                    SourceCoverage::LiveFileExcerptVerified { .. } => {
                        "Current local file · only the declaration excerpt matches indexed source; surrounding bytes may have changed."
                    }
                    SourceCoverage::Unverified => {
                        "Saved source text · these bytes have not been revalidated; source links are disabled."
                    }
                    SourceCoverage::CapturedExcerpt => {
                        "Local source · indexed source coverage is not established."
                    }
                };
                let words = ctx.say(status);
                leaves.push(Leaf::new(quiet(words.clone(), &measure, palette).role(gpui::Role::Status).aria_label(words)));
            }
            let note = margin(&view, store, &symbol, route, ctx, cx);
            let code = code(&view, source, route, ctx, window, cx);
            let leaf = Leaf::new(code);
            leaves.push(match note {
                Some(note) => leaf.with_note(note),
                None => leaf,
            });
            if let Some(gap) = view.identifiers.gap() {
                let words = ctx.say(format!("Source links aren't available: {}", gap.detail));
                leaves.push(Leaf::new(quiet(words.clone(), &ctx.measure, ctx.palette).role(gpui::Role::Status).aria_label(words)));
            }
        }
        None => {
            if let Some(gap) = view.text.gap() {
                let words = ctx.say(gap_words(gap));
                leaves.push(Leaf::new(quiet(words.clone(), &measure, palette).role(gpui::Role::Status).aria_label(words)));
            }
        }
    }
    leaves
}

fn read_only_source(view: &SourceView, notice: &str, route: &SymbolRoute, ctx: &mut Ctx<'_>) -> Vec<Leaf> {
    let mut leaves = Vec::new();
    let status = ctx.say(notice.to_owned());
    leaves.push(Leaf::new(quiet(status.clone(), &ctx.measure, ctx.palette).role(gpui::Role::Status).aria_label(status)));
    let heading = ctx.say(view.symbol.coordinate.as_str().to_owned());
    leaves.push(Leaf::new(text(ty::MONO_ROW, &ctx.measure, ctx.palette.ink2).role(gpui::Role::Label).aria_label(heading.clone()).child(heading)));
    match view.text.known() {
        Some(source) => {
            let line = route.line.filter(|line| source.line_range().is_some_and(|range| (range.first..=range.last).contains(line)))
                .unwrap_or(source.first_line());
            let cursor = initial_cursor(source, line, CONTEXT_BEFORE);
            let page = SourcePage::at(source, cursor);
            let rows = page.lines.iter().filter_map(|line| {
                source.text().get(line.span.range()).map(|text| format!("{}  {text}", line.number))
            }).collect::<Vec<_>>().join("\n");
            let words = ctx.say(rows);
            leaves.push(Leaf::new(div().id("read-only-source-page").role(gpui::Role::Label)
                .aria_label(words.clone())
                .child(text(ty::CODE, &ctx.measure, ctx.palette.ink1).child(words))));
            if page.next.is_some() {
                leaves.push(Leaf::new(quiet("Earlier source continues beyond this bounded excerpt; a current reading is needed for source paging.", &ctx.measure, ctx.palette)));
            }
        }
        None => leaves.push(Leaf::new(quiet("No source text was recorded in this reading.", &ctx.measure, ctx.palette))),
    }
    leaves
}

/// A saved display has bytes but no owner lease. Its local pager uses the
/// same UTF-8 bounded page calculation as current source, and each callback
/// checks the exact mounted visit and retained projection again.
pub(super) fn retained_page(
    display: Arc<crate::runtime::snapshot::RetainedDisplay>,
    route: &Route,
    ctx: &mut Ctx<'_>,
    cx: &mut Context<Reader>,
) -> gpui::AnyElement {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let Some(source) = display.source_text() else {
        return quiet("The saved display has no prepared source text.", &measure, palette).into_any_element();
    };
    let first_line = source.first_line();
    let requested = match route {
        Route::CargoSource(route) => route.line,
        _ => None,
    }.filter(|line| source.line_range().is_some_and(|range| (range.first..=range.last).contains(line)))
        .unwrap_or(first_line);
    let memory = Rc::clone(&ctx.source_paging);
    let cursor = {
        let mut state = memory.borrow_mut();
        let saved = state.get_or_insert_with(|| PagingState::new(initial_cursor(&source, requested, CONTEXT_BEFORE)));
        if source.line_span(saved.cursor.line).and_then(|span| source.text().get(span.range()))
            .is_none_or(|line| saved.cursor.byte > line.len() || !line.is_char_boundary(saved.cursor.byte)) {
            *saved = PagingState::new(initial_cursor(&source, requested, CONTEXT_BEFORE));
        }
        saved.cursor
    };
    let page = SourcePage::at(&source, cursor);
    let visible = page.lines.iter().filter_map(|line| source.text().get(line.span.range())
        .map(|words| format!("{}  {words}", line.number))).collect::<Vec<_>>().join("\n");
    let visible = ctx.say(visible);
    let mut column = div().flex().flex_col().gap(measure.space(Space::Base))
        .child(div().id("saved-display-source-page").role(gpui::Role::Label)
            .aria_label(visible.clone())
            .child(text(ty::CODE, &measure, palette.ink1).child(visible)));
    let last = source.line_range().map_or(first_line, |range| range.last);
    let first = page.lines.first().map_or(cursor.line, |line| line.number);
    let shown_last = page.lines.last().map_or(cursor.line, |line| line.number);
    column = column.child(quiet(ctx.say(format!("Saved text lines {first}–{shown_last} of {last}; links and source actions unavailable.")), &measure, palette));
    let previous = memory.borrow().as_ref().and_then(|state| state.previous_target(&source, &page));
    let next = memory.borrow().as_ref().and_then(|state| state.next_target(&source, &page));
    let mut controls = div().flex().gap(measure.space(Space::Base));
    for (target, label, forward) in [(previous, "Previous saved text", false), (next, "Next saved text", true)] {
        let Some(target) = target else { continue; };
        let state = Rc::clone(&memory);
        let admission = retained_page_admission(ctx, &display, route, cx);
        let expected = Arc::clone(&display);
        let route = route.clone();
        let links = ctx.links.clone();
        let reader = cx.weak_entity();
        let act: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_, app| {
            let live = links.store.read(app);
            if !super::retained::select(live, &route, live.snapshot().overlay())
                .is_some_and(|now| Arc::ptr_eq(&now, &expected)) { return; }
            if let Some(reader) = reader.upgrade() {
                reader.update(app, |_, cx| {
                    if let Some(paging) = state.borrow_mut().as_mut().filter(|paging| paging.cursor == cursor) {
                        if forward { paging.next(target); } else { paging.previous(target); }
                        cx.notify();
                    }
                });
            }
        });
        let target_action = TargetAction::new(admission, act);
        let act = target_action.callback();
        let id: SharedString = if forward { "saved-display-next" } else { "saved-display-previous" }.into();
        ctx.targets.push(Target { id: id.clone(), label: label.into(), action: target_action.clone(), peek: None, source: None });
        let mut control = facet::controls::button(id.clone(), label, &measure)
            .ghost().size(Control::Small).on_click(move |window, app| act(window, app));
        if let Some(focus) = ctx.native_handle(&id, cx) { control = control.focus_handle(focus); }
        controls = controls.child(ctx.targets.track(id, div().key_context(crate::shell::keys::NATIVE_CONTROL).child(control)));
    }
    column.child(controls).into_any_element()
}

/// Read-only rows share the existing per-route/per-window pager memory.
/// Only a bounded page is mounted; the full projection remains background
/// prepared and every native callback is fenced to its exact visit/display.
pub(super) fn retained_row_page(
    display: Arc<crate::runtime::snapshot::RetainedDisplay>,
    count: usize,
    route: &Route,
    ctx: &mut Ctx<'_>,
    cx: &mut Context<Reader>,
) -> (std::ops::Range<usize>, gpui::AnyElement) {
    const ROWS_PER_PAGE: usize = 32;
    let memory = Rc::clone(&ctx.source_paging);
    let cursor = {
        let mut state = memory.borrow_mut();
        let paging = state.get_or_insert_with(|| PagingState::new(SourceCursor { line: 1, byte: 0 }));
        if paging.cursor.byte != 0 || paging.cursor.line == 0 || paging.cursor.line as usize > count.max(1) {
            *paging = PagingState::new(SourceCursor { line: 1, byte: 0 });
        }
        paging.cursor
    };
    let start = (cursor.line.saturating_sub(1) as usize / ROWS_PER_PAGE) * ROWS_PER_PAGE;
    let end = start.saturating_add(ROWS_PER_PAGE).min(count);
    let mut controls = div().flex().flex_col().gap(ctx.measure.space(Space::Base))
        .child(quiet(ctx.say(format!("Saved rows {}–{end} of {count}; read-only.", if count == 0 { 0 } else { start + 1 })), &ctx.measure, ctx.palette));
    let mut buttons = div().flex().flex_wrap().gap(ctx.measure.space(Space::Base));
    for (target, label, forward) in [
        ((start > 0).then(|| SourceCursor { line: start.saturating_sub(ROWS_PER_PAGE) as u32 + 1, byte: 0 }), "Previous saved rows", false),
        ((end < count).then(|| SourceCursor { line: end as u32 + 1, byte: 0 }), "Next saved rows", true),
    ] {
        let Some(target) = target else { continue; };
        let state = Rc::clone(&memory);
        let admission = retained_page_admission(ctx, &display, route, cx);
        let expected = Arc::clone(&display);
        let route = route.clone();
        let links = ctx.links.clone();
        let reader = cx.weak_entity();
        let act: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_, app| {
            let store = links.store.read(app);
            if !super::retained::select(store, &route, store.snapshot().overlay())
                .is_some_and(|now| Arc::ptr_eq(&now, &expected)) { return; }
            if let Some(reader) = reader.upgrade() {
                reader.update(app, |_, cx| {
                    if let Some(paging) = state.borrow_mut().as_mut().filter(|paging| paging.cursor == cursor) {
                        if forward { paging.next(target); } else { paging.previous(target); }
                        cx.notify();
                    }
                });
            }
        });
        let target_action = TargetAction::new(admission, act);
        let act = target_action.callback();
        let id: SharedString = if forward { "saved-rows-next" } else { "saved-rows-previous" }.into();
        ctx.targets.push(Target { id: id.clone(), label: label.into(), action: target_action.clone(), peek: None, source: None });
        let mut button = facet::controls::button(id.clone(), label, &ctx.measure).ghost().size(Control::Small)
            .on_click(move |window, app| act(window, app));
        if let Some(focus) = ctx.native_handle(&id, cx) { button = button.focus_handle(focus); }
        buttons = buttons.child(ctx.targets.track(id, div().key_context(crate::shell::keys::NATIVE_CONTROL).child(button)));
    }
    controls = controls.child(buttons);
    (start..end, controls.into_any_element())
}

fn code(
    view: &SourceView,
    source: &SourceText,
    route: &SymbolRoute,
    ctx: &mut Ctx<'_>,
    window: &mut Window,
    cx: &mut Context<Reader>,
) -> gpui::AnyElement {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let declaration = view.declaration.known().copied();
    let Some(range) = source.line_range() else {
        let words = if source.line_count() == 0 {
            "No source lines were recorded".to_owned()
        } else {
            "Source line numbers are invalid; this text cannot be navigated safely".to_owned()
        };
        let words = ctx.say(words);
        return quiet(words.clone(), &measure, palette).role(gpui::Role::Status).aria_label(words).into_any_element();
    };
    let first_line = range.first;
    let last_line = range.last;
    let total = last_line - first_line + 1;
    let requested_line = route
        .line
        .filter(|line| *line >= first_line && *line <= last_line);
    let initial = requested_line
        .or_else(|| declaration.map(|span| span.first))
        .filter(|line| *line >= first_line && *line <= last_line)
        .map_or(
            SourceCursor {
                line: first_line,
                byte: 0,
            },
            |line| {
                initial_cursor(
                    source,
                    line,
                    if requested_line.is_some() {
                        0
                    } else {
                        CONTEXT_BEFORE
                    },
                )
            },
        );
    let pager_key: ElementId = format!(
        "source-pager-{}-{}-{}-{:?}-{:?}",
        ctx.place_key, first_line, total, route.line, ctx.source_generation
    )
    .into();
    let leaving = Route::Symbol(route.clone());
    let restore_target = ctx.targets.left_by(&leaving);
    let restored_reference_page = restore_target
        .as_ref()
        .and_then(|id| id.strip_prefix("source-reference-"))
        .and_then(|index| index.parse::<usize>().ok())
        .map_or(0, |index| index / MAX_PAGE_REFERENCES);
    let paging = Rc::clone(&ctx.source_paging);
    {
        let mut saved = paging.borrow_mut();
        let state = saved.get_or_insert_with(|| PagingState::new(initial));
        if restore_target.is_some() && state.restored_focus_place != Some(ctx.place_key) {
            state.reference_page = restored_reference_page;
        }
    }
    let recall = ctx.targets.recall();
    let reveal = Rc::clone(&ctx.reader_reveal);
    let pager_memory = Rc::clone(&paging);
    let (draft_owner, draft) = DraftOwner::from_ctx(leaving.clone(), ctx, cx);
    let pager = window.use_keyed_state(pager_key, cx, move |window, cx| {
        Pager::new(
            pager_memory,
            first_line,
            last_line,
            recall,
            reveal,
            Rc::new(crate::shell::reader::source_line_shared_id),
            draft_owner,
            draft,
            window,
            cx,
        )
    });
    let selected = crate::runtime::store::route_declaration(&leaving)
        .unwrap_or_else(|_| view.symbol.coordinate.clone());
    let source_key = PageKey::Source(selected.clone());
    let semantic_key = PageKey::Symbol(selected.clone());
    let (source_stamp, semantic_stamp, semantic_current) = {
        let live = ctx.links.store.read(cx);
        let root = live.snapshot().key();
        let serving = live.owner_serving();
        let current = matches!(
            crate::core::admit_resource(&live.symbol(&selected), root, serving),
            crate::core::ResourceAdmission::Current(page)
                if page.identity.coordinate.as_str() == selected.as_str()
        );
        (live.stamp(&source_key), live.stamp(&semantic_key), current)
    };
    let source_guard = ctx.native_dependency_guard((source_key, source_stamp), cx);
    *pager.read(cx).admission.borrow_mut() = Some(Rc::clone(&source_guard));
    let semantic_guard = semantic_current.then(|| ctx.native_dependency_guard((semantic_key.clone(), semantic_stamp), cx));
    let cursor = paging
        .borrow()
        .as_ref()
        .map_or(initial, |state| state.cursor);
    let page = SourcePage::at(source, cursor);
    let from = cursor.line;
    let mut shown = String::new();
    let mut numbers = Vec::new();
    let mut raw_lines = Vec::new();
    let mut source_starts = Vec::new();
    for line in &page.lines {
        let visible = source.text().get(line.span.range()).unwrap_or_default();
        source_starts.push(line.span.start as usize);
        numbers.push(line.number);
        raw_lines.push(visible);
        shown.push_str(visible);
        shown.push('\n');
    }
    if !shown.is_empty() {
        shown.pop();
    }
    let shown: SharedString = shown.into();
    ctx.say(shown.clone());
    let language = code_language(view);
    let highlights = language
        .and_then(|language| facet::code::highlight(language, shown.clone(), window, cx))
        .map(|highlighted| highlighted.styles(&palette));
    // The number column holds the widest number; the code gets the rest and
    // soft-wraps at token boundaries — continuation lines carry no number.
    let role = measure.role(ty::CODE);
    let digits = numbers.last().map_or(1, |number| number.to_string().len());
    let gap = measure.space(Space::Gutter);
    let number_width = crate::shell::text_fit::text_width(&"0".repeat(digits), &role, cx);
    let columns = crate::shell::text_fit::columns(measure.width() - number_width - gap, &role, cx);
    let lines =
        crate::shell::text_fit::wrap_code(&shown, highlights.as_deref().unwrap_or(&[]), columns);
    let mut grouped = BTreeMap::<usize, Vec<crate::shell::text_fit::CodeLine>>::new();
    for line in lines {
        grouped.entry(line.source).or_default().push(line);
    }
    let above = from.saturating_sub(first_line);
    let rendered_to = numbers.last().copied().unwrap_or(from);
    let below = last_line.saturating_sub(rendered_to);
    let mut column = div().flex().flex_col().gap(measure.space(Space::Base));
    column = column.child(pager_controls(
        "top", &pager, cursor, &page, source, &source_guard, ctx, cx,
    ));
    if above > 0 || cursor.byte > 0 {
        let label = if cursor.byte > 0 {
            format!(
                "{above} earlier lines · line {from} continues from byte {}",
                cursor.byte
            )
        } else {
            format!("{above} earlier lines")
        };
        column = column.child(quiet(label.clone(), &measure, palette).role(gpui::Role::Label).aria_label(label));
    }
    let mut rows = div().flex().flex_col();
    let mut visible_references = Vec::<(SymbolRef, Route)>::new();
    let mut reference_indices = BTreeMap::<SymbolRef, usize>::new();
    // The producer emits identifier spans in byte order. Select the visible
    // spans once for each source row, then use a binary interval search per
    // wrapped visual row. This avoids rescanning the entire file's identifier
    // table for every line (and every wrapped line) on each render frame.
    let mut identifiers_by_line =
        vec![Vec::<&crate::model::pages::IdentifierSpan>::new(); numbers.len()];
    let identifiers = match source.coverage() {
        SourceCoverage::CapturedExcerpt | SourceCoverage::LiveFileExcerptVerified { .. } => {
            semantic_guard.as_ref().and_then(|_| view.identifiers.known())
        }
        SourceCoverage::Unverified => None,
    };
    if let Some(identifiers) = identifiers {
        let visible_start = source_starts.first().copied().unwrap_or(usize::MAX);
        let mut cursor = identifiers.partition_point(|identifier| {
            usize::try_from(identifier.span.end).unwrap_or(usize::MAX) <= visible_start
        });
        for (source_index, line_start) in source_starts.iter().copied().enumerate() {
            let line_end = line_start.saturating_add(raw_lines[source_index].len());
            while cursor < identifiers.len()
                && usize::try_from(identifiers[cursor].span.end).unwrap_or(usize::MAX) <= line_start
            {
                cursor += 1;
            }
            let mut end = cursor;
            while end < identifiers.len()
                && usize::try_from(identifiers[end].span.start).unwrap_or(usize::MAX) < line_end
            {
                identifiers_by_line[source_index].push(&identifiers[end]);
                end += 1;
            }
            cursor = end;
        }
    }
    for (source_index, number) in numbers.iter().copied().enumerate() {
        let pieces = grouped.remove(&source_index).unwrap_or_default();
        let id = crate::shell::reader::source_line_shared_id(number);
        let focus_key = (ctx.place_key, number, ctx.source_generation);
        if requested_line == Some(number)
            && ctx.active
            && ctx.source_focus_applied.get() != Some(focus_key)
        {
            ctx.targets.focus(id.clone());
            ctx.reader_reveal.set(true);
            ctx.source_focus_applied.set(Some(focus_key));
        }
        let copy_source = source.clone();
        let copy_span = page.lines[source_index].span;
        let copy_guard = Rc::clone(&source_guard);
        let copy_line: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_, app| {
            if !copy_guard(app) { return; }
            let words = copy_source
                .text()
                .get(copy_span.range())
                .unwrap_or_default();
            app.write_to_clipboard(gpui::ClipboardItem::new_string(words.to_owned()));
        });
        let copy_label: SharedString = if page.lines[source_index].continued || page.lines[source_index].more_in_line {
            format!("Copy visible part of source line {number}")
        } else {
            format!("Copy source line {number}")
        }.into();
        let target_action = TargetAction::new(Rc::clone(&source_guard), copy_line);
        let copy_line = target_action.callback();
        ctx.targets.push(Target {
            id: id.clone(),
            label: copy_label.clone(),
            action: target_action,
            peek: None,
            source: None,
        });

        let mut visual_lines = div().flex().flex_col();
        for (piece_index, line) in pieces.into_iter().enumerate() {
            let native_line = source_piece_label(number, raw_lines[source_index], &line);
            let mut link_ranges = Vec::new();
            let mut link_routes = Vec::new();
            let mut link_ids = Vec::new();
            let line_start = source_starts[source_index];
            let piece_start = line_start.saturating_add(line.source_range.start);
            let piece_end = line_start.saturating_add(line.source_range.end);
            {
                let line_identifiers = &identifiers_by_line[source_index];
                let first = line_identifiers.partition_point(|identifier| {
                    usize::try_from(identifier.span.end).unwrap_or(usize::MAX) <= piece_start
                });
                let rest = &line_identifiers[first..];
                let count = rest.partition_point(|identifier| {
                    usize::try_from(identifier.span.start).unwrap_or(usize::MAX) < piece_end
                });
                for identifier in &rest[..count] {
                    let start = usize::try_from(identifier.span.start).unwrap_or(usize::MAX);
                    let end = usize::try_from(identifier.span.end).unwrap_or(usize::MAX);
                    if !verified_link(
                        source.coverage(),
                        identifier.span,
                        page.lines[source_index].span,
                    ) {
                        continue;
                    }
                    let source_from = start.max(piece_start).saturating_sub(line_start);
                    let source_to = end.min(piece_end).saturating_sub(line_start);
                    if source_from >= source_to {
                        continue;
                    }
                    let text_from = line
                        .text_offset
                        .saturating_add(source_from.saturating_sub(line.source_range.start));
                    let text_to = line
                        .text_offset
                        .saturating_add(source_to.saturating_sub(line.source_range.start));
                    if line.text.get(text_from..text_to).is_none() {
                        continue;
                    }
                    let target = identifier.link.target.clone();
                    let Some(target_route) = symbol_route(route.package.as_str(), &target) else {
                        continue;
                    };
                    let target_index =
                        *reference_indices.entry(target.clone()).or_insert_with(|| {
                            let index = visible_references.len();
                            visible_references.push((target.clone(), target_route.clone()));
                            index
                        });
                    link_ranges.push(text_from..text_to);
                    link_routes.push(target_route.clone());
                    link_ids.push(SharedString::from(format!(
                        "source-reference-{target_index}"
                    )));
                }
            }
            let label = if piece_index == 0 {
                if page.lines[source_index].continued {
                    format!("{number}+")
                } else {
                    number.to_string()
                }
            } else {
                String::new()
            };
            let declared =
                declaration.is_some_and(|span| span.first <= number && number <= span.last);
            let requested = requested_line == Some(number);
            let mut line_number = div()
                .id(format!("source-copy-line-{number}-{piece_index}"))
                .flex_none()
                .w(number_width)
                .flex()
                .justify_end()
                .child(
                    text(
                        ty::CODE,
                        &measure,
                        if declared {
                            palette.mint.base
                        } else if requested {
                            palette.peri.base
                        } else {
                            palette.ink3
                        },
                    )
                    .child(label),
                );
            if piece_index == 0 {
                if let Some(focus) = ctx.native_handle(&id, cx) {
                    let copy = Rc::clone(&copy_line);
                    line_number = facet::controls::button::native_button(
                        line_number.role(gpui::Role::Button).aria_label(copy_label.clone()).cursor_pointer(),
                        &focus,
                        move |window, app| copy(window, app),
                    );
                    line_number = facet::controls::button::capture_activation_admission(
                        line_number, Rc::clone(&source_guard));
                }
            } else {
                line_number = line_number.role(gpui::Role::Label)
                    .aria_label(format!("Source line {number} continuation"));
            }
            let shared_text: SharedString = line.text.clone().into();
            let styled = StyledText::new(shared_text.clone()).with_highlights(line.runs);
            let body: gpui::AnyElement = if link_ranges.is_empty() {
                styled.into_any_element()
            } else {
                let click_routes = link_routes;
                let click_ids = link_ids;
                let click_links = ctx.links.clone();
                let click_recall = ctx.targets.recall();
                let click_leaving = leaving.clone();
                let source_guard = Rc::clone(&source_guard);
                let semantic_guard = semantic_guard.clone();
                let dependency = (semantic_key.clone(), semantic_stamp);
                InteractiveText::new(
                    ElementId::Name(SharedString::from(format!(
                        "source-links-{number}-{piece_index}"
                    ))),
                    styled,
                )
                .on_click(link_ranges, move |which, _, app| {
                    if !source_guard(app) || !semantic_guard.as_ref().is_some_and(|guard| guard(app)) { return; }
                    if let (Some(target_route), Some(id)) =
                        (click_routes.get(which), click_ids.get(which))
                    {
                        click_recall.focus(id.clone());
                        click_recall.remember_leave(click_leaving.clone(), id.clone());
                        click_links.dispatch_read(Intent::Navigate(target_route.clone()), dependency.clone(), app);
                    }
                })
                .into_any_element()
            };
            let mut visual_row = div()
                    .id(format!("source-visual-row-{number}-{piece_index}"))
                    .flex()
                    .items_start()
                    .gap(gap)
                    .child(line_number)
                    .child(
                        div()
                            .set(ty::CODE, &measure)
                            .text_color(palette.ink1.hsla())
                            .min_w(px(0.0))
                            .whitespace_nowrap()
                            .child(body),
                    );
            if let Some(native_line) = native_line {
                visual_row = visual_row.role(gpui::Role::Label).aria_label(native_line);
            }
            visual_lines = visual_lines.child(visual_row);
        }
        let requested = requested_line == Some(number);
        let source_row = div()
            .flex()
            .items_start()
            .border_l_2()
            .border_color(if requested {
                palette.peri.line.hsla()
            } else {
                palette.line1.hsla()
            })
            .child(visual_lines);
        let shared_row = facet::motion::shared::shared(ElementId::Name(id.clone()), source_row);
        rows = rows.child(ctx.targets.track(id, shared_row));
        if page.lines[source_index].more_in_line {
            rows = rows.child(quiet(
                format!("Line {number} continues on the next source page"),
                &measure,
                palette,
            ));
        }
    }
    column = column.child(rows);
    if route.line.is_some() && requested_line.is_none() {
        let warning = format!("Line {} is outside the source text this reader has", route.line.unwrap_or_default());
        column = column.child(quiet(warning.clone(), &measure, palette).role(gpui::Role::Status).aria_label(warning));
    }
    if page.next.is_some() {
        let continuation = if page.next.is_some_and(|next| next.line == rendered_to) {
            format!("Line {rendered_to} continues")
        } else {
            format!("{below} later lines")
        };
        column = column.child(quiet(continuation.clone(), &measure, palette).role(gpui::Role::Label).aria_label(continuation));
        column = column.child(pager_controls(
            "bottom", &pager, cursor, &page, source, &source_guard, ctx, cx,
        ));
    }
    if let Some(editor_path) = view.editor_path.known() {
        let path: Arc<str> = Arc::clone(editor_path);
        let line = requested_line
            .or_else(|| declaration.map(|span| span.first).filter(|line| (first_line..=last_line).contains(line)))
            .unwrap_or(first_line);
        let links = ctx.links.clone();
        let editor_guard = Rc::clone(&source_guard);
        let act: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_, app| {
            if !editor_guard(app) { return; }
            links.dispatch(
                Intent::OpenSource {
                    path: Arc::clone(&path),
                    line,
                },
                app,
            );
        });
        let id: SharedString = "source-open-editor".into();
        let target_action = TargetAction::new(Rc::clone(&source_guard), act);
        let act = target_action.callback();
        ctx.targets.push(Target {
            id: id.clone(),
            label: "Open source file in editor".into(),
            action: target_action,
            peek: None,
            source: None,
        });
        let focus = ctx.native_handle(&id, cx);
        let mut button = facet::controls::button(id.clone(), "Open in editor", &measure)
            .ghost().size(Control::Small).aria_label("Request to open source file in editor")
            .when_current(Rc::clone(&source_guard))
            .on_click(move |window, app| act(window, app));
        if let Some(focus) = focus { button = button.focus_handle(focus); }
        column = column.child(ctx.targets.track(id, div().key_context(crate::shell::keys::NATIVE_CONTROL).child(button)));
    }
    if !visible_references.is_empty() {
        let reference_count = visible_references.len();
        let last_reference_page = (reference_count - 1) / MAX_PAGE_REFERENCES;
        let reference_page = paging
            .borrow()
            .as_ref()
            .map_or(0, |state| state.reference_page)
            .min(last_reference_page);
        let reference_start = reference_page * MAX_PAGE_REFERENCES;
        let reference_summary = ctx.say(format!(
                "References on these lines · {}–{} of {reference_count}",
                reference_start + 1,
                (reference_start + MAX_PAGE_REFERENCES).min(reference_count)
            ));
        column = column.child(text(ty::MONO_SMALL, &measure, palette.ink3)
            .role(gpui::Role::Label).aria_label(reference_summary.clone()).child(reference_summary));
        let mut reference_controls = div().flex().flex_wrap().gap(measure.space(Space::Base));
        for (direction, destination, label) in [
            (
                "previous",
                reference_page.checked_sub(1),
                "Previous references",
            ),
            (
                "next",
                (reference_page < last_reference_page).then_some(reference_page + 1),
                "Next references",
            ),
        ] {
            if let Some(destination) = destination {
                let id: SharedString = format!("source-references-{direction}").into();
                let state = pager.clone();
                let act: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_, app| {
                    state.update(app, |pager, cx| pager.show_references(destination, cx));
                });
                let target_action = TargetAction::new(Rc::clone(&source_guard), act);
                let act = target_action.callback();
                ctx.targets.push(Target {
                    id: id.clone(),
                    label: label.into(),
                    action: target_action.clone(),
                    peek: None,
                    source: None,
                });
                let focus = ctx.native_handle(&id, cx);
                let mut button = facet::controls::button(id.clone(), label, &measure)
                    .when_current(Rc::clone(&source_guard))
                    .ghost().size(Control::Small).on_click(move |window, app| act(window, app));
                if let Some(focus) = focus { button = button.focus_handle(focus); }
                reference_controls = reference_controls.child(
                    ctx.targets.track(
                        id.clone(),
                        div().key_context(crate::shell::keys::NATIVE_CONTROL).child(button),
                    ),
                );
            }
        }
        column = column.child(reference_controls);
        for (index, (symbol, target_route)) in visible_references
            .into_iter()
            .enumerate()
            .skip(reference_start)
            .take(MAX_PAGE_REFERENCES)
        {
            let Some(current_semantic) = semantic_guard.as_ref() else { continue; };
            let id: SharedString = format!("source-reference-{index}").into();
            let label: SharedString = symbol.identity().name().to_owned().into();
            let links = ctx.links.clone();
            let recall = ctx.targets.recall();
            let leaving = leaving.clone();
            let destination = target_route.clone();
            let target_id = id.clone();
            let source_guard = Rc::clone(&source_guard);
            let semantic_guard = semantic_guard.clone();
            let dependency = (semantic_key.clone(), semantic_stamp);
            let admission = paired_admission(Rc::clone(&source_guard), Rc::clone(current_semantic));
            let target = Target {
                id: id.clone(),
                label: label.clone(),
                action: TargetAction::new(Rc::clone(&admission), Rc::new(move |_, app| {
                    if !source_guard(app) || !semantic_guard.as_ref().is_some_and(|guard| guard(app)) { return; }
                    recall.focus(target_id.clone());
                    recall.remember_leave(leaving.clone(), target_id.clone());
                    links.dispatch_read(Intent::Navigate(destination.clone()), dependency.clone(), app);
                })),
                peek: Some(PageKey::Symbol(symbol.clone())),
                source: Some(symbol),
            };
            if restore_target.as_ref() == Some(&id) {
                let apply = paging.borrow_mut().as_mut().is_some_and(|state| {
                    if state.restored_focus_place == Some(ctx.place_key) {
                        return false;
                    }
                    state.restored_focus_place = Some(ctx.place_key);
                    true
                });
                if apply {
                    ctx.targets.focus(id.clone());
                }
            }
            let act = target.action.callback();
            ctx.targets.push(target);
            let focus = ctx.native_handle(&id, cx);
            let mut button = facet::controls::button(id.clone(), label, &measure)
                .when_current(admission)
                .ghost().size(Control::Small).on_click(move |window, app| act(window, app));
            if let Some(focus) = focus { button = button.focus_handle(focus); }
            column = column.child(ctx.targets.track(id, div().key_context(crate::shell::keys::NATIVE_CONTROL).child(button)));
        }
    }
    let snippet = shown.clone();
    let copy_guard = Rc::clone(&source_guard);
    let copy_id: SharedString = "source-copy-excerpt".into();
    let copy: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_, app| {
        if !copy_guard(app) { return; }
        app.write_to_clipboard(gpui::ClipboardItem::new_string(snippet.to_string()));
    });
    let target_action = TargetAction::new(Rc::clone(&source_guard), copy);
    let copy = target_action.callback();
    ctx.targets.push(Target {
        id: copy_id.clone(),
        label: "Copy visible source page".into(),
        action: target_action,
        peek: None,
        source: None,
    });
    let focus = ctx.native_handle(&copy_id, cx);
    let mut copy_button = facet::controls::button(copy_id.clone(), "Copy visible page", &measure)
        .when_current(Rc::clone(&source_guard))
        .ghost().size(Control::Small).on_click(move |window, app| copy(window, app));
    if let Some(focus) = focus { copy_button = copy_button.focus_handle(focus); }
    column = column.child(ctx.targets.track(copy_id, div().key_context(crate::shell::keys::NATIVE_CONTROL).child(copy_button)));
    column.into_any_element()
}

fn pager_controls(
    position: &str,
    pager: &Entity<Pager>,
    cursor: SourceCursor,
    page: &SourcePage,
    source: &SourceText,
    source_guard: &Rc<dyn Fn(&mut App) -> bool>,
    ctx: &mut Ctx<'_>,
    cx: &mut Context<Reader>,
) -> gpui::AnyElement {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let first = page.lines.first().map_or(cursor.line, |line| line.number);
    let last = page.lines.last().map_or(cursor.line, |line| line.number);
    let total_last = source.line_range().map_or(cursor.line, |range| range.last);
    let range = if source.line_count() == 0 {
        "No source lines were recorded".to_owned()
    } else if first == last
        && page
            .lines
            .first()
            .is_some_and(|line| line.continued || line.more_in_line)
    {
        format!("Line {first} in parts · last line {total_last}")
    } else {
        format!("Lines {first}–{last} of {total_last}")
    };
    let mut controls = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(measure.space(Space::Base))
        .min_w_0()
        .child(quiet(ctx.say(range.clone()), &measure, palette).role(gpui::Role::Label).aria_label(range));

    let memory = Rc::clone(&pager.read(cx).state);
    let previous = memory
        .borrow()
        .as_ref()
        .and_then(|state| state.previous_target(source, page));
    if let Some(previous) = previous {
        let id: SharedString = format!("source-page-{position}-previous").into();
        let state = pager.clone();
        let act: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_, app| {
            state.update(app, |pager, cx| pager.previous(cursor, previous, cx));
        });
        let target_action = TargetAction::new(Rc::clone(source_guard), act);
        let act = target_action.callback();
        ctx.targets.push(Target {
            id: id.clone(),
            label: "Previous source lines".into(),
            action: target_action.clone(),
            peek: None,
            source: None,
        });
        let focus = ctx.native_handle(&id, cx);
        let mut control = facet::controls::button(id.clone(), "Previous lines", &measure)
            .when_current(Rc::clone(source_guard))
            .ghost().size(Control::Small).on_click(move |window, app| act(window, app));
        if let Some(focus) = focus { control = control.focus_handle(focus); }
        controls = controls.child(
            ctx.targets.track(
                id.clone(),
                div().key_context(crate::shell::keys::NATIVE_CONTROL).child(control),
            ),
        );
    }
    let next = memory
        .borrow()
        .as_ref()
        .and_then(|state| state.next_target(source, page));
    if let Some(next) = next {
        let id: SharedString = format!("source-page-{position}-next").into();
        let state = pager.clone();
        let act: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_, app| {
            state.update(app, |pager, cx| pager.next(cursor, next, cx));
        });
        let target_action = TargetAction::new(Rc::clone(source_guard), act);
        let act = target_action.callback();
        ctx.targets.push(Target {
            id: id.clone(),
            label: "Next source lines".into(),
            action: target_action.clone(),
            peek: None,
            source: None,
        });
        let focus = ctx.native_handle(&id, cx);
        let mut control = facet::controls::button(id.clone(), "Next lines", &measure)
            .when_current(Rc::clone(source_guard))
            .ghost().size(Control::Small).on_click(move |window, app| act(window, app));
        if let Some(focus) = focus { control = control.focus_handle(focus); }
        controls = controls.child(
            ctx.targets.track(
                id.clone(),
                div().key_context(crate::shell::keys::NATIVE_CONTROL).child(control),
            ),
        );
    }
    if position == "top" && source.line_count() > 0 {
        let input = pager.read(cx).input.clone();
        let error = pager.read(cx).error.clone();
        let field_id: SharedString = "source-jump-field".into();
        ctx.native_input_handle(&field_id, input.read(cx).focus_handle(cx));
        let focus_input = input.clone();
        let focus: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |window, app| {
            focus_input.update(app, |input, cx| input.focus(window, cx));
        });
        let target_action = TargetAction::new(Rc::clone(source_guard), focus);
        let focus = target_action.callback();
        ctx.targets.push(Target {
            id: field_id.clone(),
            label: "Enter a source line number".into(),
            action: target_action.clone(),
            peek: None,
            source: None,
        });
        let mut field = facet::controls::field(field_id.clone(), &input, &measure).quiet();
        if let Some(error) = error {
            field = field.fault(error);
        }
        controls = controls.child(
            ctx.targets.track(
                field_id.clone(),
                facet::controls::button::capture_activation_admission(div()
                    .id(field_id)
                    .role(gpui::Role::Label)
                    .aria_label("Source line number input")
                    .w(px(112.0 * measure.scale()))
                    .min_w_0()
                    .on_click(move |_: &ClickEvent, window, app| focus(window, app))
                    .child(field), Rc::clone(source_guard)),
            ),
        );
        let id: SharedString = "source-jump-go".into();
        let state = pager.clone();
        let act: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_, app| {
            let typed = input.read(app).value().to_string();
            state.update(app, |pager, cx| pager.jump(&typed, cx));
        });
        let target_action = TargetAction::new(Rc::clone(source_guard), act);
        let act = target_action.callback();
        ctx.targets.push(Target {
            id: id.clone(),
            label: "Go to source line".into(),
            action: target_action.clone(),
            peek: None,
            source: None,
        });
        let focus = ctx.native_handle(&id, cx);
        let mut button = facet::controls::button(id.clone(), "Go to line", &measure)
            .when_current(Rc::clone(source_guard))
            .ghost().size(Control::Small).on_click(move |window, app| act(window, app));
        if let Some(focus) = focus { button = button.focus_handle(focus); }
        controls = controls.child(ctx.targets.track(id, div().key_context(crate::shell::keys::NATIVE_CONTROL).child(button)));
    }
    controls.into_any_element()
}

/// The producer's language is authoritative. JavaScript is the TypeScript
/// frontend's dialect, while C has its own grammar; unknown stays unstyled.
fn code_language(view: &SourceView) -> Option<facet::code::Lang> {
    use backend_present::Language;
    use facet::code::Lang;

    Some(match view.symbol.language {
        Language::Rust => Lang::Rust,
        Language::TypeScript => {
            let javascript = view.symbol.path.as_deref().is_some_and(|path| {
                [".js", ".jsx", ".mjs", ".cjs"]
                    .iter()
                    .any(|suffix| path.ends_with(suffix))
            });
            if javascript {
                Lang::JavaScript
            } else {
                Lang::TypeScript
            }
        }
        Language::Python => Lang::Python,
        Language::Go => Lang::Go,
        Language::Java => Lang::Java,
        Language::CSharp => Lang::CSharp,
        Language::C => Lang::C,
        Language::Cxx => Lang::Cpp,
        Language::Unknown => return None,
    })
}

fn incoming_verb(kind: RelationKind) -> &'static str {
    use backend_library::SemanticLinkKind as Link;
    match kind {
        RelationKind::Semantic(Link::Calls | Link::MethodCall) => "calls",
        RelationKind::Semantic(Link::TypeReference) => "uses the type",
        RelationKind::Semantic(Link::Reads) => "reads",
        RelationKind::Semantic(Link::Writes) => "writes",
        RelationKind::Semantic(Link::Imports) => "imports",
        RelationKind::Semantic(Link::Implements) => "implements",
        RelationKind::Semantic(Link::Overrides) => "overrides",
        RelationKind::Semantic(Link::Reexports) => "reexports",
        RelationKind::Semantic(Link::Inherits) => "inherits",
        RelationKind::Semantic(Link::Documents) => "documents",
        RelationKind::Contains => "contains",
    }
}

fn incoming_heading(callers: &[crate::model::pages::Relation]) -> &'static str {
    if callers.iter().all(|relation| matches!(relation.kind,
        RelationKind::Semantic(backend_library::SemanticLinkKind::Calls | backend_library::SemanticLinkKind::MethodCall))) {
        "Called by"
    } else {
        "Used by"
    }
}

fn margin(
    view: &SourceView,
    store: &super::Pages,
    symbol: &SymbolRef,
    route: &SymbolRoute,
    ctx: &mut Ctx<'_>,
    cx: &mut Context<Reader>,
) -> Option<gpui::AnyElement> {
    let page = store.symbol(symbol);
    let page = page.loaded_value()?;
    let measure = ctx.note;
    let palette = ctx.palette;
    let mut column = div().flex().flex_col().gap(measure.space(Space::Base));
    let name = ctx.say(view.symbol.name.to_string());
    column = column.child(text(ty::MONO_ROW, &measure, palette.ink0).role(gpui::Role::Label).aria_label(name.clone()).child(name));
    let prose = DocFragment::plain_text(&page.docs);
    if let Some(sentence) = prose
        .split_terminator(['.', '\n'])
        .map(str::trim)
        .find(|line| !line.is_empty())
    {
        let sentence = ctx.say(format!("{sentence}."));
        column = column.child(text(ty::MARGIN, &measure, palette.ink2).role(gpui::Role::Label).aria_label(sentence.clone()).child(sentence));
    }
    if let Some(callers) = page.rose.left.known().filter(|callers| !callers.is_empty()) {
        let source_key = PageKey::Source(symbol.clone());
        let semantic_key = PageKey::Symbol(symbol.clone());
        let (source_stamp, semantic_stamp) = {
            let store = ctx.links.store.read(cx);
            (store.stamp(&source_key), store.stamp(&semantic_key))
        };
        let source_guard = ctx.native_dependency_guard((source_key, source_stamp), cx);
        let semantic_guard = ctx.native_dependency_guard((semantic_key.clone(), semantic_stamp), cx);
        column = column.child(
            text(ty::SMALL, &measure, palette.ink3)
                .role(gpui::Role::Label).aria_label(incoming_heading(callers))
                .pt(measure.space(Space::Base))
                .child(incoming_heading(callers)),
        );
        for caller in callers.iter().take(5) {
            let name: SharedString = ctx.say(format!("{} · {}", caller.decl.name, incoming_verb(caller.kind)));
            let target = symbol_route(route.package.as_str(), &caller.decl.coordinate);
            let links = ctx.links.clone();
            let id: SharedString = format!("caller-{}", caller.decl.coordinate).into();
            let source_guard = Rc::clone(&source_guard);
            let semantic_guard = Rc::clone(&semantic_guard);
            let dependency = (semantic_key.clone(), semantic_stamp);
            let admission = paired_admission(Rc::clone(&source_guard), Rc::clone(&semantic_guard));
            let act: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_, app| {
                if !source_guard(app) || !semantic_guard(app) { return; }
                if let Some(route) = target.clone() { links.dispatch_read(Intent::Navigate(route), dependency.clone(), app); }
            });
            let target_action = TargetAction::new(Rc::clone(&admission), act);
            let act = target_action.callback();
            ctx.targets.push(Target { id: id.clone(), label: name.clone(), action: target_action, peek: None, source: None });
            let focus = ctx.native_handle(&id, cx);
            let mut button = facet::controls::button(id.clone(), name, &measure)
                .when_current(admission)
                .ghost().size(Control::Small).on_click(move |window, app| act(window, app));
            if let Some(focus) = focus { button = button.focus_handle(focus); }
            column = column.child(ctx.targets.track(id, div().key_context(crate::shell::keys::NATIVE_CONTROL).child(button)));
        }
        if callers.len() > 5 {
            let more = ctx.say(format!("and {} more", callers.len() - 5));
            column = column.child(quiet(more, &measure, palette));
        }
    }
    Some(column.into_any_element())
}

#[cfg(test)]
mod code_input_history_tests;

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    #[test]
    fn prepared_minified_source_pages_share_offsets_and_never_rescan_the_full_line() {
        let content: Arc<str> = "λ".repeat(1 << 19).into();
        let ordinary = SourceText::new(Arc::clone(&content), 1, SourceOrigin::Excerpt, true).expect("minified text");
        assert!(ordinary.prepared_line_index().is_none(), "ordinary current-source policy remains sparse");
        SourceText::reset_line_scan_probe();
        let _ = SourcePage::at(&ordinary, SourceCursor { line: 1, byte: 0 });
        assert!(SourceText::line_scan_probe() >= content.len(), "the sparse oracle exercises the demonstrated full-line scan");
        let source = Arc::new(ordinary.with_prepared_line_index(8 << 20).expect("bounded direct index"));
        let offsets = Arc::clone(source.prepared_line_index().expect("prepared offsets"));
        assert_eq!(std::mem::size_of::<crate::model::pages::ByteSpan>(), 8, "two compact u32 offsets");
        assert_eq!(source.text().as_ptr(), content.as_ptr(), "text is shared, not duplicated");
        SourceText::reset_line_scan_probe();
        let mut cursor = SourceCursor { line: 1, byte: 0 };
        for _ in 0..64 {
            assert!(Arc::ptr_eq(source.prepared_line_index().expect("same offsets"), &offsets));
            let page = SourcePage::at(&source, cursor);
            assert!(page.lines.len() <= MAX_SOURCE_LINES);
            assert!(page.lines.iter().map(|line| (line.span.end - line.span.start) as usize).sum::<usize>() <= MAX_SOURCE_BYTES);
            cursor = page.next.expect("long line continuation");
        }
        assert_eq!(SourceText::line_scan_probe(), 0, "bounded paint takes prepared offsets instead of scanning line bytes");
    }

    #[test]
    fn native_unicode_source_rows_name_only_the_bytes_in_each_bounded_visual_piece() {
        let source = SourceText::new(format!("{}\ntail", "λ".repeat(80_000)).into(), 1,
            SourceOrigin::Excerpt, true).expect("long Unicode source");
        let page = SourcePage::at(&source, SourceCursor { line: 1, byte: 0 });
        assert!(page.next.is_some(), "the physical line has later source pages");
        let visible = source.text().get(page.lines[0].span.range()).expect("UTF-8 bounded page");
        assert!(visible.len() <= MAX_SOURCE_LINE_BYTES);
        let pieces = crate::shell::text_fit::wrap_code(visible, &[], 8);
        assert!(pieces.len() > 1, "the bounded page has several visual rows");
        let reconstructed = pieces.iter().map(|piece| {
            let label = source_piece_label(1, visible, piece).expect("exact source boundary");
            assert!(label.len() <= MAX_SOURCE_LINE_BYTES + 32, "no AX row repeats the whole physical line");
            visible.get(piece.source_range.clone()).expect("piece is in source")
        }).collect::<String>();
        assert_eq!(reconstructed, visible, "every mounted source byte is named once in AX");
    }


    use super::*;
    use crate::model::pages::{
        IdentifierSpan, Known, LineSpan, PageValue, Provenance, ReadFailure, SymbolLink,
    };
    use crate::navigation::View;
    use crate::runtime::reads::{PageReader, ReadContext, ReadPool, ReadRequest};
    use gpui::TestAppContext;

    struct EditorSource;

    impl PageReader for EditorSource {
        fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
            let mut fixture = crate::shell::tests::Fixture;
            let mut value = fixture.read(request, context)?;
            if let PageValue::Source(view) = &mut value {
                let text = view.text.known().expect("fixture source").clone();
                view.text = Known::Known(text.with_verified_local_excerpt(ByteSpan { start: 8, end: 35 }));
                view.editor_path = Known::Known(Arc::from("/work/real-source/glyph.rs"));
            }
            if let PageValue::Symbol(page) = &mut value {
                let mut relation = page.rose.down.known().expect("fixture relation")[0].clone();
                relation.kind = RelationKind::Semantic(backend_library::SemanticLinkKind::Reads);
                page.rose.left = Known::Known(Arc::from([relation]));
            }
            Ok(value)
        }
    }

    #[test]
    fn incoming_relation_words_follow_the_recorded_edge_not_the_column_position() {
        use backend_library::SemanticLinkKind as Link;
        assert_eq!(incoming_verb(RelationKind::Semantic(Link::Reads)), "reads");
        assert_eq!(incoming_verb(RelationKind::Semantic(Link::TypeReference)), "uses the type");
        assert_eq!(incoming_verb(RelationKind::Semantic(Link::Calls)), "calls");
        let page = crate::shell::tests::page("RelationLabel");
        let mut row = page.rose.down.known().expect("recorded fixture relation")[0].clone();
        row.kind = RelationKind::Semantic(Link::Reads);
        assert_eq!(incoming_heading(&[row.clone()]), "Used by");
        row.kind = RelationKind::Semantic(Link::Calls);
        assert_eq!(incoming_heading(&[row]), "Called by");
    }

    #[gpui::test]
    fn mounted_code_reports_native_source_controls_and_editor_launch_result(cx: &mut TestAppContext) {
        use crate::host::editor::{Command, Launch};
        struct Reject(Rc<RefCell<Vec<Command>>>);
        impl Launch for Reject {
            fn run(&self, command: &Command) -> std::io::Result<()> {
                self.0.borrow_mut().push(command.clone());
                Err(std::io::ErrorKind::NotFound.into())
            }
        }
        let attempts = Rc::new(RefCell::new(Vec::new()));
        let route = crate::shell::tests::view_route("RelationLabel", View::Code);
        let pool = ReadPool::start(2, |_| EditorSource).expect("source read pool");
        let mut rig = crate::shell::tests::rig_with_reads(cx, Some(route.clone()), 900.0, 700.0, pool);
        rig.cx.update(|window, cx| {
            cx.set_global(gpui::TextTrace);
            window.set_a11y_forced(true);
            crate::host::editor::install(Rc::new(Reject(Rc::clone(&attempts))), cx);
        });
        rig.repaint();
        let json = rig.cx.update(|window, _| window.debug_a11y_tree_json()).expect("native Code tree");
        let tree: serde_json::Value = serde_json::from_str(&json).expect("native JSON");
        let nodes = tree["nodes"].as_object().expect("nodes");
        for label in ["Copy visible page", "Copy source line 138", "Request to open source file in editor", "Go to line"] {
            assert!(nodes.values().any(|node| node["aria"]["role"] == "Button" && node["aria"]["label"] == label),
                "missing native Code button {label}: {json}");
        }
        assert!(nodes.values().any(|node| node["aria"]["label"].as_str().is_some_and(|label| label.contains("pub enum RelationLabel"))),
            "bounded source text must be in native AX: {json}");
        assert!(nodes.values().any(|node| node["aria"]["label"] == "Used by"), "incoming edge heading must be native: {json}");
        assert!(nodes.values().any(|node| node["aria"]["role"] == "Button" && node["aria"]["label"] == "Typed · reads"),
            "a recorded value read cannot be announced as a caller: {json}");
        let editor = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx))
            .bounds_of("source-open-editor").expect("mounted editor control");
        let old_editor = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx))
            .placed().into_iter().find(|(target, _)| target.id == "source-open-editor")
            .expect("current editor action").0.action.callback();
        rig.cx.simulate_click(editor.center(), gpui::Modifiers::none());
        rig.settle();
        assert_eq!(attempts.borrow().len(), 3, "each failed launcher is tried once");
        let notice = rig.graph.store.read_with(rig.cx, |store, _| store.notice().map(|notice| notice.message.to_string()));
        assert!(notice.as_deref().is_some_and(|message| message.contains("Could not start an editor") && message.contains("glyph.rs:138")),
            "a spawn failure needs bounded visit-scoped feedback: {notice:?}");
        rig.go(Intent::Back);
        assert!(rig.graph.store.read_with(rig.cx, |store, _| store.notice().is_none()), "editor result cannot follow Back");
        rig.cx.update(|window, cx| old_editor(window, cx));
        rig.settle();
        assert_eq!(attempts.borrow().len(), 3, "a previous Code visit cannot launch after Back");
    }

    #[gpui::test]
    fn mounted_invalid_go_line_has_one_native_fault_and_valid_go_clears_it(
        cx: &mut TestAppContext,
    ) {
        let route = crate::shell::tests::view_route("RelationLabel", View::Code);
        let pool = ReadPool::start(2, |_| crate::shell::tests::Fixture).expect("source read pool");
        let mut rig = crate::shell::tests::rig_with_reads(cx, Some(route), 900.0, 700.0, pool);
        rig.cx.update(|window, cx| {
            cx.set_global(gpui::TextTrace);
            window.set_a11y_forced(true);
        });

        let field = rig
            .shell
            .read_with(rig.cx, |shell, cx| shell.reader_targets(cx))
            .bounds_of("source-jump-field")
            .expect("mounted line input");
        rig.cx
            .simulate_click(field.center(), gpui::Modifiers::none());
        rig.keys("0");
        let go = rig
            .shell
            .read_with(rig.cx, |shell, cx| shell.reader_targets(cx))
            .bounds_of("source-jump-go")
            .expect("mounted Go control");
        rig.cx.simulate_click(go.center(), gpui::Modifiers::none());
        rig.settle();
        rig.repaint();
        let json = rig
            .cx
            .update(|window, _| window.debug_a11y_tree_json())
            .expect("native Source tree");
        let tree: serde_json::Value = serde_json::from_str(&json).expect("native JSON");
        let faults: Vec<_> = tree["nodes"]
            .as_object()
            .expect("native nodes")
            .values()
            .filter(|node| {
                node["aria"]["role"] == "Status"
                    && node["aria"]["label"] == "Enter a line from 137 to 141"
            })
            .collect();
        assert_eq!(
            faults.len(),
            1,
            "the painted invalid-Go message must be a single native status: {json}"
        );

        let field = rig
            .shell
            .read_with(rig.cx, |shell, cx| shell.reader_targets(cx))
            .bounds_of("source-jump-field")
            .expect("line input remains mounted");
        rig.cx
            .simulate_click(field.center(), gpui::Modifiers::none());
        rig.keys("backspace 1 3 8");
        let go = rig
            .shell
            .read_with(rig.cx, |shell, cx| shell.reader_targets(cx))
            .bounds_of("source-jump-go")
            .expect("Go control remains mounted");
        rig.cx.simulate_click(go.center(), gpui::Modifiers::none());
        rig.settle();
        rig.repaint();
        let json = rig
            .cx
            .update(|window, _| window.debug_a11y_tree_json())
            .expect("valid Source tree");
        let tree: serde_json::Value = serde_json::from_str(&json).expect("native JSON");
        assert!(
            !tree["nodes"]
                .as_object()
                .expect("native nodes")
                .values()
                .any(|node| node["aria"]["label"] == "Enter a line from 137 to 141"),
            "a valid jump must remove the obsolete native fault: {json}"
        );
        assert!(
            rig.said().iter().any(|word| word.contains("Lines 138")),
            "the valid jump must actually page Source to line 138"
        );
    }

    #[gpui::test]
    fn mounted_source_copy_requires_current_owner_before_pointer_focus(cx: &mut TestAppContext) {
        let root = crate::core::VersionedRoot::synthetic(
            backend_library::view_state_root(&[("shell".to_owned(), "tests".to_owned())]), 4,
        );
        let gate = crate::runtime::owner::OwnerGate::ready(root, crate::model::ServiceMode::Attached);
        let route = crate::shell::tests::view_route("RelationLabel", View::Code);
        let pool = ReadPool::start(2, |_| EditorSource).expect("source read pool");
        let mut rig = crate::shell::tests::rig_with_engine_gate(
            cx, Some(route), 900.0, 700.0, pool, crate::shell::tests::RootOnly, Some(gate.clone()),
        );
        let bounds = crate::shell::tests::native_bounds(&mut rig, "Button", "Copy visible page", true)
            .expect("the painted copy control has a native Click and bounds");
        rig.cx.write_to_clipboard(gpui::ClipboardItem::new_string("before-copy".into()));
        rig.cx.simulate_click(bounds.center(), gpui::Modifiers::none());
        rig.settle();
        assert!(rig.cx.read_from_clipboard().and_then(|item| item.text())
            .is_some_and(|words| words.contains("pub enum RelationLabel")),
            "the fresh native control copies the visible bounded source");

        let other_owner = rig.cx.update(|window, cx| {
            let targets = rig.shell.read(cx).reader_targets(cx);
            assert!(targets.focus_native("source-jump-go", window, cx), "the unrelated Go control owns native focus");
            window.focused(cx).expect("Go control focus")
        });
        rig.cx.write_to_clipboard(gpui::ClipboardItem::new_string("stale-copy-sentinel".into()));
        gate.publish(crate::runtime::owner::OwnerState::Starting);
        gate.publish(crate::runtime::owner::OwnerState::Ready { key: root, mode: crate::model::ServiceMode::Attached });
        // The old button is still painted; no watcher or redraw has replaced
        // it. A stale pointer must fail before GPUI can give it native focus.
        rig.cx.simulate_click(bounds.center(), gpui::Modifiers::none());
        assert_eq!(rig.cx.update(|window, cx| window.focused(cx)), Some(other_owner));
        assert_eq!(rig.cx.read_from_clipboard().and_then(|item| item.text()).as_deref(),
            Some("stale-copy-sentinel"));
    }

    fn text(words: String) -> SourceText {
        SourceText::new(words.into(), 1, SourceOrigin::LocalFile, true)
            .expect("valid test source lines")
    }

    #[test]
    fn every_line_is_reachable_without_growing_a_page() {
        let source = text((1..=600).map(|line| format!("line {line}\n")).collect());
        let mut cursor = SourceCursor { line: 1, byte: 0 };
        let mut visited = Vec::new();
        loop {
            let page = SourcePage::at(&source, cursor);
            assert!(!page.lines.is_empty());
            assert!(page.lines.len() <= MAX_SOURCE_LINES);
            assert!(
                page.lines
                    .iter()
                    .map(|line| line.span.end - line.span.start)
                    .sum::<u32>()
                    <= MAX_SOURCE_BYTES as u32
            );
            visited.extend(page.lines.iter().map(|line| line.number));
            let Some(next) = page.next else { break };
            assert!(next.line > cursor.line || next.line == cursor.line && next.byte > cursor.byte);
            cursor = next;
        }
        assert_eq!(visited, (1..=600).collect::<Vec<_>>());
        assert_eq!(cursor.line, 577);
        let mut backward = cursor;
        let mut steps = 0;
        while let Some(previous) = previous_cursor(&source, backward) {
            assert!(
                previous.line < backward.line
                    || previous.line == backward.line && previous.byte < backward.byte
            );
            backward = previous;
            steps += 1;
            assert!(steps < 600);
        }
        assert_eq!(backward, SourceCursor { line: 1, byte: 0 });
    }

    #[test]
    fn history_inside_a_complete_31_line_page_is_terminal() {
        // A deep jump followed by Previous is the native 31-line case: the
        // whole file fits in the preceding page, including the jump's line.
        let source = text((1..=31).map(|line| format!("line {line}\n")).collect());
        let mut state = PagingState::new(SourceCursor { line: 31, byte: 0 });
        let deep = SourcePage::at(&source, state.cursor);
        assert_eq!(deep.lines.len(), 1);
        let previous = state.previous_target(&source, &deep).expect("earlier lines");
        assert_eq!(previous, SourceCursor { line: 1, byte: 0 });
        state.previous(previous);
        let whole = SourcePage::at(&source, state.cursor);
        assert_eq!(whole.lines.len(), 31);
        assert!(whole.next.is_none());
        assert!(
            state.next_target(&source, &whole).is_none(),
            "line 31 is already visible, so saved forward history is not a Next page"
        );
    }

    #[test]
    fn visible_history_yields_to_the_real_next_page_boundary() {
        let source = text((1..=100).map(|line| format!("line {line}\n")).collect());
        let mut state = PagingState::new(SourceCursor { line: 31, byte: 0 });
        let deep = SourcePage::at(&source, state.cursor);
        let previous = state.previous_target(&source, &deep).expect("earlier lines");
        state.previous(previous);
        let first = SourcePage::at(&source, state.cursor);
        assert_eq!(first.lines.last().map(|line| line.number), Some(64));
        assert_eq!(
            state.next_target(&source, &first),
            Some(SourceCursor { line: 65, byte: 0 }),
            "saved line 31 is visible; Next must follow the bounded page boundary"
        );
    }

    #[test]
    fn utf8_continuation_at_the_visible_byte_boundary_still_has_next() {
        let source = text(format!("{}\ntail", "é".repeat(1600)));
        let mut state = PagingState::new(SourceCursor { line: 1, byte: 0 });
        let first = SourcePage::at(&source, state.cursor);
        let continuation = first.next.expect("long line continues");
        assert_eq!(continuation, SourceCursor { line: 1, byte: MAX_SOURCE_LINE_BYTES });
        state.next(continuation);
        let second = SourcePage::at(&source, state.cursor);
        assert!(second.lines[0].continued);
        let previous = state.previous_target(&source, &second).expect("earlier bytes");
        state.previous(previous);
        let restored = SourcePage::at(&source, state.cursor);
        assert_eq!(
            state.next_target(&source, &restored),
            Some(continuation),
            "the first byte after a partial segment is outside the visible window"
        );
    }

    #[test]
    fn empty_and_crlf_lines_do_not_create_a_phantom_terminal_page() {
        let source = text("a\r\n\r\n🎯\r\nend".to_owned());
        let mut state = PagingState::new(SourceCursor { line: 4, byte: 0 });
        let deep = SourcePage::at(&source, state.cursor);
        let previous = state.previous_target(&source, &deep).expect("earlier lines");
        state.previous(previous);
        let whole = SourcePage::at(&source, state.cursor);
        assert_eq!(whole.lines.len(), 4);
        assert!(
            whole
                .lines
                .iter()
                .any(|line| line.number == 2 && line.span.start == line.span.end)
        );
        assert!(state.next_target(&source, &whole).is_none());
    }

    #[test]
    fn a_long_utf8_line_is_paged_without_omitting_a_character() {
        let original = format!("{}\ntail", "é".repeat(80_000));
        let source = text(original);
        let mut cursor = SourceCursor { line: 1, byte: 0 };
        let mut reconstructed = String::new();
        let mut pages = 0;
        loop {
            let page = SourcePage::at(&source, cursor);
            assert!(!page.lines.is_empty());
            for line in &page.lines {
                assert!(
                    usize::try_from(line.span.end - line.span.start).unwrap()
                        <= MAX_SOURCE_LINE_BYTES
                );
                reconstructed.push_str(source.text().get(line.span.range()).unwrap());
                if !line.more_in_line && line.number == 1 {
                    reconstructed.push('\n');
                }
            }
            pages += 1;
            assert!(pages < 100);
            let Some(next) = page.next else { break };
            assert!(next.line > cursor.line || next.line == cursor.line && next.byte > cursor.byte);
            cursor = next;
        }
        assert_eq!(reconstructed, source.text());
    }

    #[test]
    fn an_exact_deep_route_survives_an_oversized_predecessor() {
        let source = text(format!(
            "{}\n{}",
            "x".repeat(100_000),
            (2..=500)
                .map(|line| format!("line {line}\n"))
                .collect::<String>()
        ));
        let cursor = initial_cursor(&source, 450, 0);
        assert_eq!(cursor, SourceCursor { line: 450, byte: 0 });
        assert_eq!(
            SourcePage::at(&source, cursor)
                .lines
                .first()
                .map(|line| line.number),
            Some(450)
        );
    }

    #[test]
    fn only_verified_excerpt_bytes_admit_links() {
        let span = ByteSpan { start: 10, end: 15 };
        let visible = ByteSpan { start: 0, end: 20 };
        assert!(!verified_link(SourceCoverage::Unverified, span, visible));
        assert!(verified_link(
            SourceCoverage::CapturedExcerpt,
            span,
            visible
        ));
        assert!(!verified_link(
            SourceCoverage::CapturedExcerpt,
            span,
            ByteSpan { start: 12, end: 20 }
        ));
        assert!(verified_link(
            SourceCoverage::LiveFileExcerptVerified {
                bytes: ByteSpan { start: 8, end: 17 }
            },
            span,
            visible
        ));
        assert!(!verified_link(
            SourceCoverage::LiveFileExcerptVerified {
                bytes: ByteSpan { start: 11, end: 17 }
            },
            span,
            visible
        ));
    }

    #[test]
    fn final_representable_line_never_wraps_into_a_duplicate() {
        let boundary = SourceText::new(Arc::from("last"), u32::MAX, SourceOrigin::Excerpt, true)
            .expect("one final line is representable");
        assert_eq!(boundary.first_line(), u32::MAX);
        let last = SourcePage::at(
            &boundary,
            SourceCursor {
                line: u32::MAX,
                byte: 0,
            },
        );
        assert_eq!(last.lines.len(), 1);
        assert_eq!(last.lines[0].number, u32::MAX);
        assert!(last.next.is_none());

        for outside in [0, u32::MAX - 1] {
            let cursor = SourceCursor {
                line: outside,
                byte: 0,
            };
            assert!(SourcePage::at(&boundary, cursor).lines.is_empty());
            assert!(previous_cursor(&boundary, cursor).is_none());
        }
    }

    pub(super) struct LongSource;

    impl PageReader for LongSource {
        fn read(
            &mut self,
            request: &ReadRequest,
            context: &ReadContext<'_>,
        ) -> Result<PageValue, ReadFailure> {
            let mut fixture = crate::shell::tests::Fixture;
            let mut value = fixture.read(request, context)?;
            if let PageValue::Source(view) = &mut value {
                let words: String = (1..=600)
                    .map(|line| {
                        if line == 565 {
                            "RelationDirection\n".to_owned()
                        } else {
                            format!("line {line}\n")
                        }
                    })
                    .collect();
                let source = text(words);
                let span = source.line_span(565).expect("verified reference line");
                view.text = Known::Known(source.with_verified_local_excerpt(span));
                view.declaration = Known::Known(LineSpan {
                    first: 565,
                    last: 565,
                });
                view.identifiers = Known::Known(Arc::from([IdentifierSpan {
                    span,
                    link: SymbolLink {
                        target: crate::shell::tests::symbol("RelationDirection"),
                        provenance: Provenance::ByName,
                    },
                }]));
            }
            Ok(value)
        }
    }

    struct CompactSource;

    impl PageReader for CompactSource {
        fn read(
            &mut self,
            request: &ReadRequest,
            context: &ReadContext<'_>,
        ) -> Result<PageValue, ReadFailure> {
            let mut fixture = crate::shell::tests::Fixture;
            let mut value = fixture.read(request, context)?;
            if let PageValue::Source(view) = &mut value {
                view.text = Known::Known(text(
                    (1..=31).map(|line| format!("line {line}\n")).collect()
                ));
                view.declaration = Known::Known(LineSpan { first: 31, last: 31 });
                view.identifiers = Known::Known(Arc::from([]));
            }
            Ok(value)
        }
    }

    #[gpui::test]
    fn mounted_previous_click_on_whole_31_line_file_has_no_next(cx: &mut TestAppContext) {
        let Route::Symbol(mut route) = crate::shell::tests::view_route("RelationLabel", View::Code) else { unreachable!() };
        route.line = Some(31);
        let pool = ReadPool::start(2, |_| CompactSource).expect("source read pool");
        let mut rig = crate::shell::tests::rig_with_reads(cx, Some(Route::Symbol(route)), 720.0, 700.0, pool);
        let field = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx))
            .bounds_of("source-jump-field").expect("mounted line jump");
        rig.cx.simulate_click(field.center(), gpui::Modifiers::none());
        rig.keys("3 1 enter");
        assert!(rig.said().iter().any(|word| word.contains("Lines 31–31 of 31")));
        let previous = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx))
            .bounds_of("source-page-top-previous").expect("mounted previous");
        rig.cx.simulate_click(previous.center(), gpui::Modifiers::none());
        rig.settle();
        assert!(rig.said().iter().any(|word| word.contains("Lines 1–31 of 31")));
        let targets = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx));
        assert!(targets.bounds_of("source-page-top-next").is_none(),
            "the entire file is visible after Previous, so no native Next target remains");
        assert!(!targets.native_keys().iter().any(|id| id == "source-page-top-next"));
    }

    #[gpui::test]
    fn mounted_previous_keyboard_on_whole_file_has_no_next(cx: &mut TestAppContext) {
        let Route::Symbol(mut route) = crate::shell::tests::view_route("RelationLabel", View::Code) else { unreachable!() };
        route.line = Some(31);
        let pool = ReadPool::start(2, |_| CompactSource).expect("source read pool");
        let mut rig = crate::shell::tests::rig_with_reads(cx, Some(Route::Symbol(route)), 720.0, 700.0, pool);
        let field = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx))
            .bounds_of("source-jump-field").expect("mounted line jump");
        rig.cx.simulate_click(field.center(), gpui::Modifiers::none());
        rig.keys("3 1 enter");
        let field = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx))
            .bounds_of("source-jump-field").expect("jump field remains mounted");
        rig.cx.simulate_click(field.center(), gpui::Modifiers::none());
        rig.keys("shift-tab");
        let targets = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx));
        assert_eq!(rig.cx.update(|window, _| targets.native_focused(window)).as_deref(),
            Some("source-page-top-previous"));
        rig.keys("enter");
        rig.settle();
        assert!(rig.said().iter().any(|word| word.contains("Lines 1–31 of 31")));
        assert!(rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx))
            .bounds_of("source-page-top-next").is_none());
    }

    fn activate_reader_target(rig: &mut crate::shell::tests::Rig, id: &str) {
        let target = rig.shell.read_with(rig.cx, |shell, cx| {
            let targets = shell.reader_targets(cx);
            targets.focus(id);
            targets.current().expect("visible source target")
        });
        rig.cx.update(|window, cx| (target.action.callback())(window, cx));
        rig.settle();
    }

    /// The pager changes the visible source page without leaving its route,
    /// so a real pointer click can test who owns the following key.
    fn pointer_focus_current_pager(rig: &mut crate::shell::tests::Rig) -> crate::shell::focus::Targets {
        use crate::shell::focus::Zone;
        rig.cx.update(|window, cx| rig.shell.update(cx, |shell, cx| shell.take_zone(Zone::Shelf, window, cx)));
        rig.draw();
        let targets = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx));
        let bounds = targets.bounds_of("source-page-top-next").expect("mounted next-page control");
        let before = rig.route();
        rig.cx.simulate_mouse_move(bounds.center(), None, gpui::Modifiers::none());
        rig.draw();
        rig.cx.simulate_click(bounds.center(), gpui::Modifiers::none());
        rig.settle();
        assert_eq!(rig.route(), before, "the pager stays on this Source route");
        assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx).0), Zone::Shelf,
            "pointer focus did not perform a Shell zone walk");
        let targets = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx));
        assert_eq!(rig.cx.update(|window, _| targets.native_focused(window)).as_deref(),
            Some("source-page-top-next"), "the pointer focused the mounted pager button");
        targets
    }

    #[gpui::test]
    fn pointer_focused_source_pager_gives_tab_to_the_next_reader_control(cx: &mut TestAppContext) {
        use crate::shell::focus::Zone;
        let route = crate::shell::tests::view_route("RelationLabel", View::Code);
        let pool = ReadPool::start(2, |_| LongSource).expect("source read pool");
        let mut rig = crate::shell::tests::rig_with_reads(cx, Some(route), 720.0, 700.0, pool);
        let targets = pointer_focus_current_pager(&mut rig);
        let order = targets.native_keys();
        let at = order.iter().position(|id| id == "source-page-top-next").expect("pager in native order");
        let next = order.get(at + 1).expect("another mounted native control follows the pager").clone();
        rig.keys("tab");
        assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx)), (Zone::Reader, Some(next.clone())));
        assert_eq!(rig.cx.update(|window, _| targets.native_focused(window)), Some(next));
    }

    #[gpui::test]
    fn pointer_focused_source_pager_gives_j_and_k_the_reader_walk(cx: &mut TestAppContext) {
        use crate::shell::focus::Zone;
        let route = crate::shell::tests::view_route("RelationLabel", View::Code);
        let pool = ReadPool::start(2, |_| LongSource).expect("source read pool");
        let mut rig = crate::shell::tests::rig_with_reads(cx, Some(route), 720.0, 700.0, pool);
        let targets = pointer_focus_current_pager(&mut rig);
        let list = targets.list_probe().upgrade().expect("Reader target list");
        let order: Vec<_> = list.borrow().iter().map(|target| target.id.clone()).collect();
        let at = order.iter().position(|id| id == "source-page-top-next").expect("pager in Reader walk order");
        let next = order.get(at + 1).expect("Reader walk continues after pager").clone();
        rig.keys("j");
        assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx)), (Zone::Reader, Some(next)));
        rig.keys("k");
        assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx)).1.as_deref(), Some("source-page-top-next"));
        assert_eq!(rig.cx.update(|window, _| targets.native_focused(window)).as_deref(), Some("source-page-top-next"));
    }

    #[gpui::test]
    fn narrow_native_reader_opens_an_exact_deep_line(cx: &mut TestAppContext) {
        let Route::Symbol(mut route) = crate::shell::tests::view_route("RelationLabel", View::Code)
        else {
            unreachable!()
        };
        route.line = Some(500);
        let pool = ReadPool::start(2, |_| LongSource).expect("source read pool");
        let mut rig =
            crate::shell::tests::rig_with_reads(cx, Some(Route::Symbol(route)), 260.0, 700.0, pool);
        let words = rig.said();
        assert!(
            words.iter().any(|word| word.contains("line 500")),
            "deep route not visible: {words:#?}"
        );
        assert!(
            words.iter().any(|word| word.contains("Lines 500")),
            "pager not visible: {words:#?}"
        );
        let (zone, focus) = rig
            .shell
            .read_with(rig.cx, |shell, cx| shell.focus_state(cx));
        assert_eq!(zone, crate::shell::focus::Zone::Reader);
        assert_eq!(focus.as_deref(), Some("source-line-500"));
        rig.keys("j");
        let (_, walked) = rig
            .shell
            .read_with(rig.cx, |shell, cx| shell.focus_state(cx));
        assert_eq!(walked.as_deref(), Some("source-line-501"));
    }

    #[gpui::test]
    fn source_row_focus_geometry_and_native_owner_follow_the_reader_across_shelf_changes(
        cx: &mut TestAppContext,
    ) {
        use crate::shell::focus::Zone;

        let Route::Symbol(mut route) = crate::shell::tests::view_route("RelationLabel", View::Code)
        else {
            unreachable!()
        };
        route.line = Some(500);
        let pool = ReadPool::start(2, |_| LongSource).expect("source read pool");
        let mut rig = crate::shell::tests::rig_with_reads(
            cx, Some(Route::Symbol(route.clone())), 1440.0, 900.0, pool,
        );
        rig.settle();

        let mut before_frame = None;
        for sample in 0..3 {
            if sample > 0 {
                // The middle sample closes the shelf; the last opens it.
                rig.go(Intent::ToggleShelf);
            }
            rig.repaint();
            let targets = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx));
            let frame = targets.hint_frame();
            if let Some(previous) = before_frame {
                if frame != previous {
                    assert!(!targets.admits_hint("source-line-500", previous),
                        "a prior Reader target frame cannot claim the current row");
                }
            }
            before_frame = Some(frame);
            assert!(targets.admits_hint("source-line-500", frame), "the current frame owns this source row");
            assert!(rig.cx.update(|window, cx| targets.focus_native("source-line-500", window, cx)),
                "the current row has a native owner");
            let row = targets.bounds_of("source-line-500").expect("current painted source row");
            let button = crate::shell::tests::native_bounds_id(
                &mut rig,
                "source-copy-line-500-0",
                "Button",
                "Copy source line 500",
                true,
            )
            .expect("current native source line button");
            assert!(row.left() <= button.left() && button.left() - row.left() <= px(16.0),
                "the Reader bevel starts alongside its own native line, not in a prior shelf/layout: {row:?} vs {button:?}");
            assert!(row.right() >= button.right() && row.top() <= button.top() && row.bottom() >= button.bottom(),
                "the Reader target encloses its own native line: {row:?} vs {button:?}");
            assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx)),
                (Zone::Reader, Some("source-line-500".into())));
            assert_eq!(rig.cx.update(|window, _| targets.native_focused(window)).as_deref(), Some("source-line-500"));
            assert_eq!(rig.route(), Route::Symbol(route.clone()));
        }
    }

    #[gpui::test]
    fn native_jump_next_link_and_back_restore_the_same_page(cx: &mut TestAppContext) {
        let route = crate::shell::tests::view_route("RelationLabel", View::Code);
        let pool = ReadPool::start(2, |_| LongSource).expect("source read pool");
        let mut rig =
            crate::shell::tests::rig_with_reads(cx, Some(route.clone()), 320.0, 700.0, pool);

        activate_reader_target(&mut rig, "source-jump-field");
        rig.keys("5 0 0 enter");
        assert!(
            rig.said().iter().any(|word| word.contains("Lines 500")),
            "Enter must jump to the exact line"
        );
        activate_reader_target(&mut rig, "source-page-top-next");
        let later = rig.said();
        assert!(
            later.iter().any(|word| word.contains("Lines 564")),
            "later page missing: {later:#?}"
        );
        assert!(
            later.iter().any(|word| word.contains("RelationDirection")),
            "verified link line missing: {later:#?}"
        );

        rig.shell.update(rig.cx, |shell, cx| {
            shell.set_source_reader_scroll_offset(gpui::point(px(0.0), px(-96.0)), cx)
        });
        rig.repaint();
        let before = rig
            .shell
            .read_with(rig.cx, |shell, cx| shell.source_reader_scroll_offset(cx));
        activate_reader_target(&mut rig, "source-reference-0");
        assert_ne!(rig.route(), route, "the verified link must navigate");
        rig.keys("secondary-[");
        assert_eq!(rig.route(), route);
        assert!(
            rig.said().iter().any(|word| word.contains("Lines 564")),
            "Back reset the source page"
        );
        let (_, focus) = rig
            .shell
            .read_with(rig.cx, |shell, cx| shell.focus_state(cx));
        assert_eq!(
            focus.as_deref(),
            Some("source-reference-0"),
            "Back lost the exact link focus"
        );
        let restored = rig
            .shell
            .read_with(rig.cx, |shell, cx| shell.source_reader_scroll_offset(cx));
        assert_eq!(
            restored, before,
            "Back lost the source page's scroll offset"
        );
    }

    struct LongWrappedLine;

    impl PageReader for LongWrappedLine {
        fn read(
            &mut self,
            request: &ReadRequest,
            context: &ReadContext<'_>,
        ) -> Result<PageValue, ReadFailure> {
            let mut fixture = crate::shell::tests::Fixture;
            let mut value = fixture.read(request, context)?;
            if let PageValue::Source(view) = &mut value {
                view.text = Known::Known(text(format!(
                    "{}{}{}\ntail",
                    "α".repeat(1_024),
                    "β".repeat(1_024),
                    "γ".repeat(1_024)
                )));
                view.declaration = Known::Known(LineSpan { first: 1, last: 1 });
            }
            Ok(value)
        }
    }

    #[gpui::test]
    fn native_wrapped_line_continues_without_duplicate_row_ids(cx: &mut TestAppContext) {
        let route = crate::shell::tests::view_route("RelationLabel", View::Code);
        let pool = ReadPool::start(2, |_| LongWrappedLine).expect("wrapped source pool");
        let mut rig = crate::shell::tests::rig_with_reads(cx, Some(route), 260.0, 700.0, pool);
        for _ in 0..5 {
            rig.keys("secondary-=");
        }
        let scale = rig
            .cx
            .update(|_, cx| facet::ActiveFacet::facet(cx).text_scale);
        assert!(
            (scale - 2.0).abs() < 1e-6,
            "the source view really reflowed at 200% text"
        );
        assert!(
            rig.said()
                .iter()
                .any(|word| word.contains("Line 1 in parts"))
        );
        rig.repaint();
        let viewport = rig
            .cx
            .debug_bounds("reader-scroll")
            .expect("native reader viewport");
        let pointer = gpui::point(
            px(f32::from(viewport.origin.x) + f32::from(viewport.size.width) / 2.0),
            px(f32::from(viewport.origin.y) + f32::from(viewport.size.height) / 2.0),
        );
        let before_wheel = rig
            .shell
            .read_with(rig.cx, |shell, cx| shell.source_reader_scroll_offset(cx));
        rig.cx.simulate_event(gpui::ScrollWheelEvent {
            position: pointer,
            delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(-180.0))),
            modifiers: gpui::Modifiers::default(),
            touch_phase: gpui::TouchPhase::Moved,
        });
        rig.repaint();
        let after_wheel = rig
            .shell
            .read_with(rig.cx, |shell, cx| shell.source_reader_scroll_offset(cx));
        assert_ne!(
            after_wheel, before_wheel,
            "the native wheel must scroll the source reader"
        );
        let before = rig.said();
        activate_reader_target(&mut rig, "source-page-top-next");
        let after = rig.said();
        assert_ne!(
            before, after,
            "the next page must advance within the same long line"
        );
        assert!(after.iter().any(|word| word.contains("Line 1 in parts")));
        assert!(after.iter().any(|word| word.contains('β')));
        rig.cx.simulate_resize(gpui::size(px(320.0), px(700.0)));
        rig.settle();
        let resized = rig.said();
        assert!(resized.iter().any(|word| word.contains('β')));
        assert!(resized.iter().any(|word| word.contains("Line 1 in parts")));
        let (_, focus) = rig
            .shell
            .read_with(rig.cx, |shell, cx| shell.focus_state(cx));
        assert_eq!(focus.as_deref(), Some("source-line-1"));
    }
}
