//! A comparison is an alignment of recorded declarations, not a promise
//! that replacing a dependency is safe. Names align by exact kind; selecting
//! one reveals each package's own inputs, output and failure exit.

use super::find::Answer;
use super::view::{KeyboardReveal, child, words, words_ellipsis};
use crate::controls::button::button;
use crate::icons::{Icon, Kind, KindSize, kind_mark};
use crate::measure::{Control, Measure, Space};
use crate::motion::flow::Flow;
use crate::theme::ActiveFacet;
use crate::tokens::ty;
use gpui::{AnyElement, App, AppContext as _, ElementId, Entity, FocusHandle, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    ScrollHandle, SharedString, StatefulInteractiveElement, Styled, Window, div, px};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

/// A recorded declaration and its exact package-relative path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Operation { pub answer: Answer, pub path: Option<SharedString> }

/// One selected package's evidence. `None` is an unread outline; an empty
/// vector is an authoritative empty outline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub key: SharedString,
    pub name: SharedString,
    pub version: Option<SharedString>,
    pub description: Option<SharedString>,
    pub facts: Vec<(SharedString, SharedString)>,
    pub operations: Option<Vec<Operation>>,
    pub complete: bool,
    pub coverage: Option<SharedString>,
}

/// Compare's immutable evidence, in selected order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Model {
    pub candidates: Vec<Candidate>,
    /// Prepared once from the immutable read; focus and pointer motion only
    /// borrow these rows. No maps or source parsing in a render callback.
    pub alignments: Vec<Alignment>,
    pub kind_counts: Vec<[usize; 4]>,
}

impl Model {
    #[must_use]
    pub fn new(candidates: Vec<Candidate>) -> Self {
        let alignments = align(&candidates);
        let kind_counts = candidates.iter().map(|candidate| {
            let mut counts = [0; 4];
            if let Some(operations) = &candidate.operations {
                for operation in operations { counts[family(operation.answer.kind) as usize] += 1; }
            }
            counts
        }).collect();
        Self { candidates, alignments, kind_counts }
    }
}

#[derive(Clone)]
pub struct Actions {
    /// Only the current route claims keyboard focus on its first mount.
    pub active: bool,
    /// Reader viewport used only after an explicit keyboard selection walk.
    pub scroll: ScrollHandle,
    pub open_package: Rc<dyn Fn(SharedString, &mut Window, &mut App)>,
    pub open_symbol: Rc<dyn Fn(SharedString, &mut Window, &mut App)>,
    pub open_code: Rc<dyn Fn(SharedString, &mut Window, &mut App)>,
    pub remove_package: Rc<dyn Fn(SharedString, &mut Window, &mut App)>,
}

/// The exact typed name shared by slots. Every overload is retained.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Alignment {
    pub name: SharedString,
    pub kind: Kind,
    pub slots: Vec<Vec<usize>>,
    pub evidence: Vec<CellEvidence>,
}

/// A blank cell is not necessarily absence. This distinction is prepared
/// with the immutable source and used by both filters and the inspector.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CellEvidence { Present, Absent, Unread, Partial }

impl Alignment {
    #[must_use] pub fn present(&self) -> usize { self.slots.iter().filter(|slot| !slot.is_empty()).count() }
    #[must_use] pub fn differs(&self) -> bool { self.evidence.contains(&CellEvidence::Absent) }
}

/// Align by exact name AND kind. A derive never becomes a similarly named
/// type, and an unread package never contributes an invented empty API.
#[must_use]
pub fn align(candidates: &[Candidate]) -> Vec<Alignment> {
    let mut rows: BTreeMap<(SharedString, Kind), Alignment> = BTreeMap::new();
    for (column, candidate) in candidates.iter().enumerate() {
        if let Some(operations) = &candidate.operations {
            for (at, operation) in operations.iter().enumerate() {
                let answer = &operation.answer;
                let key = (answer.name.clone(), answer.kind);
                rows.entry(key).or_insert_with(|| Alignment {
                    name: answer.name.clone(), kind: answer.kind, slots: vec![Vec::new(); candidates.len()], evidence: vec![],
                }).slots[column].push(at);
            }
        }
    }
    let mut out = rows.into_values().collect::<Vec<_>>();
    for row in &mut out {
        row.evidence = row.slots.iter().enumerate().map(|(at, slot)| {
            if !slot.is_empty() { CellEvidence::Present }
            else if candidates[at].operations.is_none() { CellEvidence::Unread }
            else if candidates[at].complete { CellEvidence::Absent }
            else { CellEvidence::Partial }
        }).collect();
    }
    out.sort_by_key(|row| (std::cmp::Reverse(row.present()), family(row.kind), row.name.clone()));
    out
}

fn chosen_visible<'a>(visible: &[&'a Alignment], remembered: Option<&(SharedString, Kind)>) -> Option<&'a Alignment> {
    remembered.and_then(|(name, kind)| visible.iter().find(|row| row.name == *name && row.kind == *kind).copied())
        .or_else(|| visible.iter().find(|row| row.name.as_ref() == "from_str" && family(row.kind) == Family::Callables).copied())
        .or_else(|| visible.iter().find(|row| family(row.kind) == Family::Callables).copied())
        .or_else(|| visible.first().copied())
}

const ROW_PAGE: usize = 18;

fn page_span(total: usize, page: usize) -> (usize, usize) {
    let last = total.saturating_sub(1) / ROW_PAGE * ROW_PAGE;
    let offset = page.saturating_mul(ROW_PAGE).min(last);
    (offset, total.saturating_sub(offset).min(ROW_PAGE))
}

/// Package headers stay parallel while each can still hold its name and
/// compact controls at the current text scale. A fourth choice wraps 2+2
/// before the columns become too narrow to read.
fn header_columns(effective_width: f32, count: usize, gap: f32) -> usize {
    let fits = |columns: usize| effective_width >= columns as f32 * 120.0 + columns.saturating_sub(1) as f32 * gap;
    if fits(count) { count } else if count >= 2 && fits(2) { 2 } else { 1 }
}

/// The silhouette groups only recorded kinds, never inferred capabilities.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Family { Callables, Types, Modules, Other }

impl Family {
    const ALL: [Self; 4] = [Self::Callables, Self::Types, Self::Modules, Self::Other];
    const fn words(self) -> &'static str { match self { Self::Callables => "callables", Self::Types => "types", Self::Modules => "modules", Self::Other => "other declarations" } }
    const fn icon(self) -> Icon { match self { Self::Callables => Icon::Play, Self::Types => Icon::Diamond, Self::Modules => Icon::Folder, Self::Other => Icon::Layers } }
}

#[must_use]
pub const fn family(kind: Kind) -> Family {
    match kind {
        Kind::Function | Kind::Method | Kind::Constructor | Kind::Macro => Family::Callables,
        Kind::Struct | Kind::Class | Kind::Enum | Kind::Union | Kind::Type | Kind::Trait | Kind::Interface => Family::Types,
        Kind::Module => Family::Modules,
        _ => Family::Other,
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Scope { All, Shared, Distinct }

struct State { selected: Option<(SharedString, Kind)>, family: Option<Family>, scope: Option<Scope>, page: usize, facts: bool, more_overloads: bool, reveal: KeyboardReveal, focus: FocusHandle, focus_claimed: bool }

impl State {
    fn new(scroll: ScrollHandle, cx: &mut gpui::Context<Self>) -> Self {
        Self { selected: None, family: None, scope: None, page: 0, facts: false, more_overloads: false, reveal: KeyboardReveal::new(scroll), focus: cx.focus_handle(), focus_claimed: false }
    }
}

/// Build a comparison from real package evidence.
#[must_use]
pub fn compare(id: impl Into<ElementId>, model: Arc<Model>, actions: Actions, measure: &Measure) -> Compare {
    Compare { id: id.into(), model, actions, measure: *measure, #[cfg(test)] test_state: None }
}

#[derive(IntoElement)]
pub struct Compare { id: ElementId, model: Arc<Model>, actions: Actions, measure: Measure, #[cfg(test)] test_state: Option<Entity<State>> }

impl RenderOnce for Compare {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let m = self.measure;
        let p = cx.palette();
        #[cfg(test)]
        let state = self.test_state.unwrap_or_else(|| window.use_keyed_state(child(&self.id, "state"), cx, |_, cx| State::new(self.actions.scroll.clone(), cx)));
        #[cfg(not(test))]
        let state = window.use_keyed_state(child(&self.id, "state"), cx, |_, cx| State::new(self.actions.scroll.clone(), cx));
        let claim = state.update(cx, |state, _| {
            if !self.actions.active { state.focus_claimed = false; None }
            else if state.focus_claimed { None }
            else { state.focus_claimed = true; Some(state.focus.clone()) }
        });
        if let Some(focus) = claim { window.focus(&focus, cx); }
        let alignments = &self.model.alignments;
        let known_columns = self.model.candidates.iter().filter(|candidate| candidate.operations.is_some()).count();
        let complete = self.model.candidates.iter().all(|candidate| candidate.complete && candidate.operations.is_some());
        let shared = alignments.iter().filter(|row| row.present() == self.model.candidates.len()).count();
        let shared_words = if complete { format!("{shared} shared names") } else { format!("{shared} shared names so far") };
        let mut page = div().id(self.id.clone()).key_context("BrowseCompare").track_focus(&state.read(cx).focus).flex().flex_col().w(m.width()).gap(m.space(Space::Gutter))
            .child(words(child(&self.id, "eyebrow"), "COMPARE RECORDED DECLARATIONS", ty::CAPTION, p.ink1, &m))
            .child(words(child(&self.id, "hero"), "Where do these packages differ?", ty::TITLE, p.ink0, &m))
            .child(words(child(&self.id, "lede"), format!("{shared_words} by exact kind. Select a name to inspect each package's inputs and exits."), ty::SMALL, p.ink2, &m));
        let count = self.model.candidates.len().max(1);
        let gap = m.space(Space::Gutter);
        let columns = header_columns(m.effective(), count, f32::from(gap) / m.scale());
        // Give flex a definite row width and a few pixels of rounding slack.
        // Exact fractional fits wrapped the second header in the native shell.
        let row_slack = if columns > 1 { px(4.0) } else { px(0.0) };
        let column_m = m.within((m.width() - gap * (columns.saturating_sub(1) as f32) - row_slack) / columns as f32);
        let mut heads = if columns > 1 { div().w(m.width()).flex().flex_wrap().items_start().gap(gap) } else { div().w(m.width()).flex().flex_col().gap(gap) };
        for (at, candidate) in self.model.candidates.iter().enumerate() {
            let key = child(&self.id, format!("candidate-{at}"));
            let open = Rc::clone(&self.actions.open_package);
            let package = candidate.key.clone();
            let remove = Rc::clone(&self.actions.remove_package);
            let remove_key = candidate.key.clone();
            let mut head = div().w(column_m.width()).min_w_0().flex_none().flex().flex_col().gap(m.space(Space::Snug))
                .child(div().flex().items_center().gap(m.space(Space::Snug))
                    .child(crate::paint::gem(Kind::Package).size(18.0 * m.scale()))
                    .child(div().flex_1().min_w_0().child(words_ellipsis(child(&key, "name"), candidate.name.clone(), ty::MONO_ROW, p.ink0, &column_m))))
                .children(candidate.version.as_ref().map(|version| words(child(&key, "version"), version.clone(), ty::MONO_SMALL, p.ink2, &column_m)))
                .child(silhouette(&key, candidate, &self.model.kind_counts[at], &state, &column_m, cx))
                .child(div().flex().flex_wrap().gap(m.space(Space::Tight))
                    .child(button(child(&key, "open"), "Explore", &column_m).ghost().size(Control::Small).on_click(move |window, cx| open(package.clone(), window, cx)))
                    .child(button(child(&key, "remove"), "Remove", &column_m).ghost().size(Control::Small).disabled(count <= 2)
                        .on_click(move |window, cx| remove(remove_key.clone(), window, cx))));
            if let Some(coverage) = &candidate.coverage { head = head.child(words(child(&key, "coverage"), coverage.clone(), ty::CAPTION, p.ink2, &column_m)); }
            heads = heads.child(head);
        }
        // Headers participate in normal flow. No translucent sticky band can
        // cut a scrolling signature into legible fragments.
        page = page.child(heads).child(words(child(&self.id, "claim"), "An aligned name is evidence of a recorded declaration, not compatibility. ◆ recorded  — absent from a complete outline  · partial  ? unread", ty::CAPTION, p.ink2, &m));
        let scope = state.read(cx).scope.unwrap_or(Scope::All);
        let mut filters = div().flex().flex_wrap().gap(m.space(Space::Base));
        for (at, (choice, name)) in [(Scope::All, "All names"), (Scope::Shared, "Shared names"), (Scope::Distinct, "Different names")].into_iter().enumerate() {
            let selected = state.clone();
            filters = filters.child(button(child(&self.id, format!("scope-{at}")), name, &m).size(Control::Small)
                .intent(if choice == scope { crate::controls::button::Intent::Edge } else { crate::controls::button::Intent::Ghost })
                .on_click(move |_, cx| selected.update(cx, |state, cx| { state.scope = Some(choice); state.page = 0; cx.notify(); })));
        }
        if let Some(filter) = state.read(cx).family {
            let clear = state.clone();
            filters = filters.child(button(child(&self.id, "clear-family"), format!("{} ×", filter.words()), &m).ghost().size(Control::Small)
                .on_click(move |_, cx| clear.update(cx, |state, cx| { state.family = None; state.page = 0; cx.notify(); })));
        }
        page = page.child(filters);
        let visible = alignments.iter().filter(|row| {
            state.read(cx).family.is_none_or(|selected| family(row.kind) == selected) && match scope {
                Scope::All => true, Scope::Shared => row.present() == count,
                Scope::Distinct => row.differs(),
            }
        }).collect::<Vec<_>>();
        let (offset, shown) = page_span(visible.len(), state.read(cx).page);
        let page_rows = &visible[offset..offset + shown];
        // Keep the remembered identity across filters and pages, but inspect
        // only a row actually shown now. Returning restores the old focus.
        let selected = state.read(cx).selected.clone();
        let chosen = chosen_visible(page_rows, selected.as_ref());
        let beside = chosen.is_some() && m.effective() >= 760.0;
        let detail_width = (m.effective() * 0.52).clamp(350.0, 520.0) * m.scale();
        let rows_m = if beside { m.within(m.width() - px(detail_width) - m.space(Space::Section)) } else { m };
        let detail_m = if beside { m.within(px(detail_width)) } else { m };
        let flow = Flow::scoped(format!("compare-{:?}", self.id), cx);
        flow.epoch((m.room(), state.read(cx).family, scope, state.read(cx).page));
        let keys = page_rows.iter().map(|row| (row.name.clone(), row.kind)).collect::<Vec<_>>();
        let keyboard = state.clone();
        page = page.on_key_down(move |event, _, cx| {
            let delta = match event.keystroke.key.as_str() { "down" | "j" => 1_isize, "up" | "k" => -1, _ => return };
            keyboard.update(cx, |state, cx| {
                let at = state.selected.as_ref().and_then(|selected| keys.iter().position(|key| key == selected));
                let next = at.map_or(0, |at| at.saturating_add_signed(delta).min(keys.len().saturating_sub(1)));
                if let Some(key) = keys.get(next) && state.selected.as_ref() != Some(key) {
                    state.selected = Some(key.clone());
                    state.reveal.request();
                }
                cx.notify();
            });
            cx.stop_propagation();
        });
        let mut rows = div().w(rows_m.width()).flex().flex_col().gap(m.space(Space::Tight));
        for (at, row) in page_rows.iter().enumerate() {
            let active = chosen.is_some_and(|selected| selected.name == row.name && selected.kind == row.kind);
            let row_id = child(&self.id, format!("row-{at}"));
            let painted = aligned_row(&row_id, row, active, &self.model, &state, &rows_m, cx);
            let painted = if active { state.read(cx).reveal.selected(painted) } else { painted };
            rows = rows.child(flow.item(format!("{:?}-{}", row.kind, row.name), painted));
        }
        if visible.is_empty() {
            let text = if known_columns == 0 { "Declaration evidence has not been read for these packages." } else { "No recorded declarations in this selection." };
            rows = rows.child(words(child(&self.id, "empty"), text, ty::LEDE, p.ink2, &rows_m));
        }
        if visible.len() > ROW_PAGE {
            let previous = state.clone();
            let next = state.clone();
            rows = rows.child(div().flex().flex_wrap().gap(m.space(Space::Roomy))
                .child(button(child(&self.id, "previous"), "Previous names", &rows_m).ghost().disabled(offset == 0)
                    .on_click(move |_, cx| previous.update(cx, |state, cx| { state.page = state.page.saturating_sub(1); cx.notify(); })))
                .child(words(child(&self.id, "page-reading"), format!("{}–{} of {} recorded names", offset + 1, offset + shown, visible.len()), ty::CAPTION, p.ink2, &rows_m))
                .child(button(child(&self.id, "next"), "Next names", &rows_m).ghost().disabled(offset + shown >= visible.len())
                    .on_click(move |_, cx| next.update(cx, |state, cx| { state.page += 1; cx.notify(); }))));
        }
        let mut comparison = if beside { div().flex().items_start().gap(m.space(Space::Section)) } else { div().flex().flex_col().gap(m.space(Space::Wide)) };
        comparison = comparison.child(rows);
        if let Some(chosen) = chosen {
            let detail = div().w(detail_m.width()).flex().flex_col().gap(m.space(Space::Base))
                .child(div().flex().items_center().gap(m.space(Space::Base))
                    .child(kind_mark(chosen.kind, KindSize::Md, p))
                    .child(words(child(&self.id, "focused-name"), chosen.name.clone(), ty::HEAD, p.ink0, &detail_m)))
                .child(operation_detail(&child(&self.id, "focus"), chosen, &self.model, &state, &self.actions, &detail_m, cx));
            comparison = comparison.child(detail);
        }
        page = page.child(comparison);
        let facts = state.clone();
        page = page.child(button(child(&self.id, "facts-toggle"), if state.read(cx).facts { "Hide package facts" } else { "Compare package facts" }, &m).ghost().icon(Icon::Seal)
            .on_click(move |_, cx| facts.update(cx, |state, cx| { state.facts = !state.facts; cx.notify(); })));
        if state.read(cx).facts {
            let mut facts = div().flex().flex_wrap().items_start().gap(gap);
            for (at, candidate) in self.model.candidates.iter().enumerate() {
                let mut column = div().w(column_m.width()).flex().flex_col().gap(m.space(Space::Roomy))
                    .child(words(child(&self.id, format!("facts-{at}-name")), candidate.name.clone(), ty::HEAD, p.ink0, &column_m));
                if let Some(description) = &candidate.description { column = column.child(words(child(&self.id, format!("facts-{at}-description")), description.clone(), ty::LEDE, p.ink2, &column_m)); }
                for (fact, (label, value)) in candidate.facts.iter().enumerate() {
                    column = column.child(div().flex().flex_col().gap(m.space(Space::Tight))
                        .child(words(child(&self.id, format!("facts-{at}-{fact}-label")), label.clone(), ty::CAPTION, p.ink2, &column_m))
                        .child(words(child(&self.id, format!("facts-{at}-{fact}-value")), value.clone(), ty::SMALL, p.ink1, &column_m)));
                }
                facts = facts.child(column);
            }
            page = page.child(facts);
        }
        page
    }
}

fn silhouette(id: &ElementId, candidate: &Candidate, counts: &[usize; 4], state: &Entity<State>, m: &Measure, cx: &mut App) -> AnyElement {
    let mut strip = div().flex().flex_wrap().gap(m.space(Space::Tight));
    for (at, family_) in Family::ALL.into_iter().enumerate() {
        let count = candidate.operations.as_ref().map(|_| counts[at]);
        if count == Some(0) { continue; }
        let label = count.map_or_else(|| "unread".to_owned(), |count| format!("{count} {}", family_.words()));
        let selected = state.clone();
        strip = strip.child(button(child(id, format!("family-{at}")), label, m).ghost().size(Control::Small).icon(family_.icon())
            .disabled(count.is_none()).on_click(move |_, cx| selected.update(cx, |state, cx| { state.family = if state.family == Some(family_) { None } else { Some(family_) }; state.page = 0; cx.notify(); })));
    }
    strip.into_any_element()
}

fn aligned_row(id: &ElementId, row: &Alignment, active: bool, model: &Model, state: &Entity<State>, m: &Measure, cx: &mut App) -> AnyElement {
    let p = cx.palette();
    let select = state.clone();
    let selection = (row.name.clone(), row.kind);
    let label = format!("{}/{}", row.present(), model.candidates.len());
    let mut constellation = div().flex().items_center().gap(m.space(Space::Snug));
    for (at, candidate) in model.candidates.iter().enumerate() {
        let available = candidate.operations.is_some();
        let present = !row.slots[at].is_empty();
        let glyph = if !available { "?" } else if present { "◆" } else if candidate.complete { "—" } else { "·" };
        constellation = constellation.child(words(child(id, format!("slot-{at}")), glyph, ty::MONO_SMALL,
            if present { p.peri.base.hsla() } else { p.ink2.hsla() }, m));
    }
    div().id(id.clone()).flex().items_center().gap(m.space(Space::Base))
        .px(m.space(Space::Base)).py(m.space(Space::Base)).cursor_pointer()
        .bg(if active { p.tint.hsla() } else { gpui::transparent_black() }).hover(|style| style.bg(p.tint))
        .child(kind_mark(row.kind, KindSize::Sm, p))
        .child(div().flex_1().min_w_0().child(words_ellipsis(child(id, "name"), row.name.clone(), ty::MONO_ROW, if active { p.ink0 } else { p.ink1 }, m)))
        .child(constellation)
        .child(words(child(id, "presence"), label, ty::CAPTION, p.ink2, m))
        .on_click(move |_, _, cx| select.update(cx, |state, cx| { state.selected = Some(selection.clone()); state.more_overloads = false; cx.notify(); }))
        .into_any_element()
}

fn operation_detail(id: &ElementId, row: &Alignment, model: &Model, state: &Entity<State>, actions: &Actions, m: &Measure, cx: &mut App) -> AnyElement {
    let p = cx.palette();
    let count = model.candidates.len().max(1);
    let wide = m.effective() >= 850.0;
    let gap = m.space(Space::Wide);
    let column_m = if wide { m.within((m.width() - gap * (count.saturating_sub(1) as f32)) / count as f32) } else { *m };
    let mut spread = if wide { div().flex().items_start().gap(gap) } else { div().flex().flex_col().gap(gap) };
    for (at, candidate) in model.candidates.iter().enumerate() {
        let candidate_id = child(id, format!("column-{at}"));
        let mut column = div().w(column_m.width()).flex().flex_col().gap(m.space(Space::Base))
            .border_l_2().border_color(p.peri.base.hsla()).pl(m.space(Space::Base))
            .child(words(child(&candidate_id, "name"), candidate.name.clone(), ty::MONO_SMALL, p.ink0, &column_m));
        if row.slots[at].is_empty() {
            let text = if candidate.operations.is_none() { "Declarations not read" } else if candidate.complete { "No declaration with this name and kind in the indexed outline" } else { "Not found in the portion read" };
            column = column.child(words(child(&candidate_id, "gap"), text, ty::CAPTION, p.ink2, &column_m));
        } else if let Some(operations) = &candidate.operations {
            let shown = if state.read(cx).more_overloads { 4 } else { 1 };
            for (variant, index) in row.slots[at].iter().take(shown).enumerate() {
                let operation = &operations[*index];
                let operation_id = child(&candidate_id, format!("variant-{variant}"));
                if let Some(pipe) = &operation.answer.pipe {
                    column = column.child(crate::anatomy::pipe(child(&operation_id, "shape"), pipe.clone(), &column_m, &crate::anatomy::Links::plain()));
                } else if let Some(summary) = &operation.answer.summary {
                    column = column.child(words(child(&operation_id, "summary"), summary.clone(), ty::LEDE, p.ink2, &column_m));
                } else {
                    column = column.child(words(child(&operation_id, "kind"), format!("Recorded {:?} declaration", operation.answer.kind).to_lowercase(), ty::SMALL, p.ink2, &column_m));
                }
                if let Some(path) = &operation.path { column = column.child(words(child(&operation_id, "path"), path.clone(), ty::CAPTION, p.ink2, &column_m)); }
                let mut actions_row = div().flex().flex_wrap().gap(m.space(Space::Tight));
                if operation.answer.signature.is_some() {
                    let open = Rc::clone(&actions.open_code);
                    let key = operation.answer.key.clone();
                    actions_row = actions_row.child(button(child(&operation_id, "code"), "Code", &column_m).ghost().size(Control::Small).icon(Icon::Peel)
                        .on_click(move |window, cx| open(key.clone(), window, cx)));
                }
                let open = Rc::clone(&actions.open_symbol);
                let key = operation.answer.key.clone();
                actions_row = actions_row.child(button(child(&operation_id, "open"), "Explore", &column_m).ghost().size(Control::Small).icon(Icon::Trail)
                    .on_click(move |window, cx| open(key.clone(), window, cx)));
                column = column.child(actions_row);
            }
            if row.slots[at].len() > shown {
                if !state.read(cx).more_overloads {
                    let reveal = state.clone();
                    column = column.child(button(child(&candidate_id, "more-overloads"), more_declarations(row.slots[at].len() - 1), &column_m).ghost().size(Control::Small)
                        .on_click(move |_, cx| reveal.update(cx, |state, cx| { state.more_overloads = true; cx.notify(); })));
                } else {
                    column = column.child(words(child(&candidate_id, "bounded-overloads"), format!("{} further declarations; explore the package for the full outline", row.slots[at].len() - shown), ty::CAPTION, p.ink2, &column_m));
                }
            }
        }
        spread = spread.child(column);
    }
    div().flex().flex_col().gap(m.space(Space::Base)).py(m.space(Space::Base))
        .child(spread)
        .into_any_element()
}

fn more_declarations(count: usize) -> String {
    if count == 1 { "1 more matching declaration".to_owned() }
    else { format!("{count} more matching declarations") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{Facet, set_facet};
    use gpui::{AppContext as _, Context, Render, TestAppContext, VisualTestContext};

    struct MountedCompare { state: Entity<State>, scroll: ScrollHandle, model: Arc<Model>, actions: Actions }
    impl Render for MountedCompare {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            crate::probe::draw_started(cx);
            let id: ElementId = "mounted-compare".into();
            let measure = Measure::new(px(620.0), &cx.facet());
            div().id("mounted-compare-scroll").w(px(620.0)).h(px(240.0)).overflow_y_scroll().track_scroll(&self.scroll)
                .child(Compare { id, model: Arc::clone(&self.model), actions: self.actions.clone(), measure, test_state: Some(self.state.clone()) })
        }
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.run_until_parked();
        cx.update(|window, cx| { window.simulate_next_frame(cx); window.draw(cx).clear(cx); });
    }

    fn assert_alignment_visible(cx: &mut VisualTestContext, scroll: &ScrollHandle, name: &str) {
        let viewport = scroll.bounds();
        let ledger = cx.update(|_, cx| crate::probe::take(cx));
        let text = ledger.texts.iter().find(|text| text.content == name && text.key.contains("row-")).expect("selected alignment row paints natively");
        let box_ = &text.bounds;
        assert!(box_.y >= f32::from(viewport.top()) - 0.5 && box_.y + box_.height <= f32::from(viewport.bottom()) + 0.5,
            "selected {name} must fit the real scroll viewport: {text:?}, {viewport:?}");
    }

    #[gpui::test]
    fn mounted_compare_arrow_reveals_selected_alignment_without_pointer_auto_scroll(cx: &mut TestAppContext) {
        cx.update(|cx| { gpui_component::init(cx); set_facet(Facet { reduced_motion: true, ..Facet::default() }, cx); crate::probe::enable(cx); });
        let scroll = ScrollHandle::new();
        let operations = (0..18).map(|at| Operation { path: None, answer: Answer {
            key: format!("symbol-{at}").into(), name: format!("function_{at:02}").into(), kind: Kind::Function,
            context: None, reason: "indexed".into(), summary: None, pipe: None, signature: None,
        }}).collect::<Vec<_>>();
        let candidate = |name: &str| Candidate { key: name.to_owned().into(), name: name.to_owned().into(), version: None, description: None, facts: vec![],
            operations: Some(operations.clone()), complete: true, coverage: None };
        let model = Arc::new(Model::new(vec![candidate("one"), candidate("two")]));
        let actions = Actions { active: true, scroll: scroll.clone(), open_package: Rc::new(|_, _, _| {}), open_symbol: Rc::new(|_, _, _| {}),
            open_code: Rc::new(|_, _, _| {}), remove_package: Rc::new(|_, _, _| {}) };
        let (host, cx) = cx.add_window_view(|window, cx| {
            let state = cx.new(|cx| State::new(scroll.clone(), cx));
            let focus = state.read(cx).focus.clone();
            window.focus(&focus, cx);
            MountedCompare { state, scroll: scroll.clone(), model, actions }
        });
        draw(cx);
        let state = host.read_with(cx, |host, _| host.state.clone());
        assert_eq!(scroll.offset().y, px(0.0));
        state.update(cx, |state, cx| { state.selected = Some(("function_17".into(), Kind::Function)); cx.notify(); });
        draw(cx);
        assert_eq!(scroll.offset().y, px(0.0));
        state.update(cx, |state, cx| { state.selected = None; cx.notify(); });
        draw(cx);
        for at in 0..18 {
            cx.simulate_keystrokes("down");
            draw(cx); draw(cx);
            assert_eq!(state.read_with(cx, |state, _| state.selected.clone()), Some((format!("function_{at:02}").into(), Kind::Function)),
                "native Compare key event must select the next recorded row");
            assert_alignment_visible(cx, &scroll, &format!("function_{at:02}"));
        }
        let below = scroll.offset().y;
        assert!(below < px(0.0), "keyboard selection must reveal an alignment below the headers");
        for at in (0..17).rev() {
            cx.simulate_keystrokes("up");
            draw(cx); draw(cx);
            assert_eq!(state.read_with(cx, |state, _| state.selected.clone()), Some((format!("function_{at:02}").into(), Kind::Function)));
            assert_alignment_visible(cx, &scroll, &format!("function_{at:02}"));
        }
        assert!(scroll.offset().y > below, "walking back upward must reveal an earlier row from a pre-scrolled viewport");
    }
    #[test]
    fn overload_label_agrees_with_its_count() {
        assert_eq!(more_declarations(1), "1 more matching declaration");
        assert_eq!(more_declarations(2), "2 more matching declarations");
    }
    #[test]
    fn package_headers_wrap_only_when_text_scaled_columns_would_be_too_narrow() {
        assert_eq!(header_columns(437.0, 3, 16.0), 3);
        assert_eq!(header_columns(400.0, 4, 16.0), 2);
        assert_eq!(header_columns(255.0, 3, 16.0), 1);
        assert_eq!(header_columns(760.0, 4, 16.0), 4);
        assert_eq!(header_columns(392.0, 3, 17.0), 2, "measured gap, not a fixed token, decides the fit");
    }

    #[gpui::test]
    fn mounted_two_package_headers_share_a_row_when_space_allows(cx: &mut TestAppContext) {
        cx.update(|cx| { gpui_component::init(cx); set_facet(Facet { reduced_motion: true, ..Facet::default() }, cx); crate::probe::enable(cx); });
        let scroll = ScrollHandle::new();
        let candidate = |name: &str| Candidate { key: name.to_owned().into(), name: name.to_owned().into(), version: None,
            description: None, facts: vec![], operations: Some(vec![]), complete: true, coverage: None };
        let model = Arc::new(Model::new(vec![candidate("first-package"), candidate("second-package")]));
        let actions = Actions { active: true, scroll: scroll.clone(), open_package: Rc::new(|_, _, _| {}), open_symbol: Rc::new(|_, _, _| {}),
            open_code: Rc::new(|_, _, _| {}), remove_package: Rc::new(|_, _, _| {}) };
        let (_, cx) = cx.add_window_view(|_, cx| {
            let state = cx.new(|cx| State::new(scroll.clone(), cx));
            MountedCompare { state, scroll, model, actions }
        });
        draw(cx);
        let ledger = cx.update(|_, cx| crate::probe::take(cx));
        let name = |wanted: &str| ledger.texts.iter().find(|text| text.content == wanted).expect("header name painted");
        let first = name("first-package");
        let second = name("second-package");
        assert!((first.bounds.y - second.bounds.y).abs() < 1.0, "headers unexpectedly wrapped: {first:?} {second:?}");
    }
    #[test]
    fn every_projected_kind_has_its_recorded_silhouette_family() {
        for kind in [Kind::Function, Kind::Method, Kind::Constructor, Kind::Macro] {
            assert_eq!(family(kind), Family::Callables, "{kind:?}");
        }
        for kind in [Kind::Struct, Kind::Class, Kind::Enum, Kind::Union, Kind::Type, Kind::Trait, Kind::Interface] {
            assert_eq!(family(kind), Family::Types, "{kind:?}");
        }
        assert_eq!(family(Kind::Module), Family::Modules);
        for kind in [Kind::Field, Kind::Variable, Kind::Import] {
            assert_eq!(family(kind), Family::Other, "{kind:?}");
        }
    }

    #[test]
    fn hidden_selection_is_remembered_without_inspecting_an_invisible_row() {
        let old = Alignment { name: "from_str".into(), kind: Kind::Function, slots: vec![vec![0]], evidence: vec![CellEvidence::Present] };
        let type_ = Alignment { name: "Value".into(), kind: Kind::Struct, slots: vec![vec![0]], evidence: vec![CellEvidence::Present] };
        let remembered = (old.name.clone(), old.kind);
        assert_eq!(chosen_visible(&[&type_], Some(&remembered)).map(|row| row.name.as_ref()), Some("Value"));
        assert_eq!(chosen_visible(&[&type_, &old], Some(&remembered)).map(|row| row.name.as_ref()), Some("from_str"));
        assert_eq!(chosen_visible(&[], Some(&remembered)), None);
    }

    #[test]
    fn paging_inspects_a_row_on_the_current_page_and_restores_a_remembered_choice() {
        let rows = (0..=ROW_PAGE).map(|at| Alignment {
            name: format!("name-{at}").into(), kind: Kind::Function,
            slots: vec![vec![0]], evidence: vec![CellEvidence::Present],
        }).collect::<Vec<_>>();
        let visible = rows.iter().collect::<Vec<_>>();
        let remembered = (rows[0].name.clone(), Kind::Function);
        let (first, first_len) = page_span(visible.len(), 0);
        assert_eq!((first, first_len), (0, ROW_PAGE));
        assert_eq!(chosen_visible(&visible[first..first + first_len], Some(&remembered)).map(|row| row.name.as_ref()), Some("name-0"));
        let (next, next_len) = page_span(visible.len(), 1);
        assert_eq!((next, next_len), (ROW_PAGE, 1));
        assert_eq!(chosen_visible(&visible[next..next + next_len], Some(&remembered)).map(|row| row.name.as_ref()), Some("name-18"));
        let (back, back_len) = page_span(visible.len(), 0);
        assert_eq!(chosen_visible(&visible[back..back + back_len], Some(&remembered)).map(|row| row.name.as_ref()), Some("name-0"));
        assert_eq!(page_span(visible.len(), usize::MAX), (ROW_PAGE, 1), "an excessive page state clamps safely");
        assert_eq!(page_span(0, 8), (0, 0));
    }
    fn candidate(name: &str, kinds: &[Kind], complete: bool) -> Candidate {
        Candidate { key: name.to_owned().into(), name: name.to_owned().into(), version: None, description: None, facts: vec![],
            operations: Some(kinds.iter().enumerate().map(|(at, kind)| Operation { path: None, answer: Answer {
                key: format!("{name}::{at}").into(), name: "Value".into(), kind: *kind, reason: "indexed".into(), summary: None, pipe: None, signature: None,
                context: None,
            }}).collect()), complete, coverage: None }
    }
    #[test]
    fn alignment_is_kind_aware_and_retains_same_named_declarations() {
        let a = candidate("one", &[Kind::Struct, Kind::Struct, Kind::Function], true);
        let b = candidate("two", &[Kind::Struct, Kind::Macro], true);
        let rows = align(&[a, b]);
        let value = rows.iter().find(|row| row.kind == Kind::Struct).unwrap();
        assert_eq!(value.slots, vec![vec![0, 1], vec![0]]);
        assert_eq!(value.present(), 2);
        let macro_ = rows.iter().find(|row| row.kind == Kind::Macro).unwrap();
        assert_eq!(macro_.slots, vec![vec![], vec![1]]);
        assert!(!rows.iter().any(|row| row.kind == Kind::Macro && row.present() == 2));
    }

    #[test]
    fn a_partial_or_unread_outline_is_never_an_absent_capability() {
        let a = candidate("one", &[Kind::Function], true);
        let partial = candidate("partial", &[], false);
        let mut unread = candidate("unread", &[], false); unread.operations = None;
        let rows = align(&[a.clone(), partial, unread]);
        assert_eq!(rows[0].evidence, [CellEvidence::Present, CellEvidence::Partial, CellEvidence::Unread]);
        assert!(!rows[0].differs());
        let complete = candidate("complete", &[], true);
        let rows = align(&[a, complete]);
        assert_eq!(rows[0].evidence, [CellEvidence::Present, CellEvidence::Absent]);
        assert!(rows[0].differs());
    }
}
