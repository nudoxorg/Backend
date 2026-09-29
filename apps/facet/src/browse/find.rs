//! Find: a query, packages that expose the matching declarations, and an
//! inspector that reveals a callable as inputs and exits before showing code.
//! The search and qualification are producer evidence; no popularity score or
//! project-use count is manufactured here.

use super::view::{KeyboardReveal, child, words, words_ellipsis};
use crate::controls::button::button;
use crate::fluid::Modes;
use crate::icons::{Icon, IconSize, Kind, KindSize, kind_mark, ui};
use crate::measure::{Control, Measure, Set, Space};
use crate::motion::flow::Flow;
use crate::semantics::model::Pipe;
use crate::theme::ActiveFacet;
use crate::tokens::fluid::{FIND, FIND_INSPECTOR, Split};
use crate::tokens::ty;
use gpui::{AnyElement, App, AppContext as _, Context, ElementId, Entity, InteractiveElement, IntoElement, ParentElement,
    RenderOnce, ScrollHandle, SharedString, StatefulInteractiveElement, Styled, Subscription, Task, Window, div, px};
use gpui_component::input::{Input, InputEvent, InputState};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// One actual declaration matched by the index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    /// Exact symbol coordinate to open.
    pub key: SharedString,
    /// Its recorded name and kind.
    pub name: SharedString,
    pub kind: Kind,
    /// Package-relative source and line, when the index recorded them.
    /// Disambiguates same-named declarations without showing a code block.
    pub context: Option<SharedString>,
    /// Producer's matching reason, in words.
    pub reason: SharedString,
    /// Recorded documentation, with inline Markdown removed for this sentence.
    pub summary: Option<SharedString>,
    /// A display projection of the recorded callable, when understood.
    pub pipe: Option<Pipe>,
    /// Exact source signature, behind disclosure.
    pub signature: Option<SharedString>,
}

/// A package qualification, with declarations belonging to this exact package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub key: SharedString,
    pub name: SharedString,
    pub version: Option<SharedString>,
    pub indexed: bool,
    pub summary: Option<SharedString>,
    /// Actual known facts only; unavailable facts have a coverage note instead.
    pub facts: Vec<(SharedString, SharedString)>,
    pub answers: Vec<Answer>,
}

/// Find's immutable reading. Rows retain producer order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Model {
    pub query: SharedString,
    pub candidates: Vec<Candidate>,
    /// Matches whose package could not be addressed; still actionable symbols.
    pub loose: Vec<Answer>,
    pub coverage: Vec<SharedString>,
    /// A first page with an owner-issued continuation has more than these rows.
    pub more_answers: bool,
    pub loading: bool,
}

/// Typed actions supplied by the shell; the component never contacts the owner.
#[derive(Clone)]
pub struct Actions {
    /// Reader viewport used only after an explicit keyboard selection walk.
    pub scroll: ScrollHandle,
    /// A Reader-owned hand restored when Find remounts after Compare/Back.
    pub initial_held: Vec<HeldPackage>,
    /// Publish each edit to the Reader before navigation can unmount Find.
    pub persist_held: Rc<dyn Fn(Vec<HeldPackage>, &mut App)>,
    pub refine: Rc<dyn Fn(SharedString, &mut App)>,
    pub open_symbol: Rc<dyn Fn(SharedString, &mut Window, &mut App)>,
    pub open_code: Rc<dyn Fn(SharedString, &mut Window, &mut App)>,
    pub open_package: Rc<dyn Fn(SharedString, &mut Window, &mut App)>,
    pub compare: Rc<dyn Fn(Vec<SharedString>, &mut Window, &mut App)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Selection { Package(SharedString), Answer(SharedString) }

fn reading_matches(query: Option<&SharedString>, text: &str) -> bool {
    query.is_some_and(|query| query.as_ref().trim() == text.trim())
}

/// A held package keeps the display name and exact version seen when chosen.
/// Its address is retained only for the typed comparison route.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeldPackage {
    pub key: SharedString,
    pub name: SharedString,
    pub version: Option<SharedString>,
}

impl HeldPackage {
    fn from_candidate(candidate: &Candidate) -> Self {
        Self { key: candidate.key.clone(), name: candidate.name.clone(), version: candidate.version.clone() }
    }
    fn label(&self) -> SharedString {
        match &self.version { Some(version) => format!("{} {}", self.name, version).into(), None => self.name.clone() }
    }
}

/// The held package tray admits four distinct identities in order.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Held(Vec<HeldPackage>);

impl Held {
    /// Toggle one identity. A full tray leaves the existing choice intact.
    fn toggle(&mut self, package: HeldPackage) {
        if let Some(at) = self.0.iter().position(|held| held.key == package.key) { self.0.remove(at); }
        else if self.0.len() < 4 { self.0.push(package); }
    }
    /// Remove an address even if it no longer matches the current search.
    fn remove(&mut self, key: &SharedString) { self.0.retain(|held| &held.key != key); }
    fn contains(&self, key: &SharedString) -> bool { self.0.iter().any(|held| &held.key == key) }
    fn packages(&self) -> &[HeldPackage] { &self.0 }
    fn can_compare(&self) -> bool { (2..=4).contains(&self.0.len()) }
    fn action_label(&self) -> String {
        if self.0.len() == 1 { "Choose one more package".to_owned() }
        else { format!("Compare {} packages", self.0.len()) }
    }
}

struct State {
    input: Entity<InputState>,
    route_query: SharedString,
    refine: Rc<dyn Fn(SharedString, &mut App)>,
    actions: Actions,
    keyboard: Vec<Selection>,
    loaded_query: Option<SharedString>,
    snapshot: Option<Arc<Model>>,
    submitted: Option<SharedString>,
    generation: u64,
    selected: Option<Selection>,
    reveal: KeyboardReveal,
    held: Held,
    all_packages: bool,
    all_answers: bool,
    source: bool,
    pending: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl State {
    fn new(query: SharedString, actions: Actions, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let refine = Rc::clone(&actions.refine);
        let input = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("A package, an item, or a word in its docs");
            input.set_value(query.clone(), window, cx);
            input
        });
        let subscription = cx.subscribe_in(&input, window, |state: &mut Self, input, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::Change) {
                let text = input.read(cx).value();
                state.pending = None;
                state.generation = state.generation.wrapping_add(1);
                let generation = state.generation;
                if text.trim() == state.route_query.as_ref().trim() { cx.notify(); return; }
                {
                    state.pending = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(Duration::from_millis(110)).await;
                        let _ = this.update(cx, |state, cx| {
                            if state.generation != generation { return; }
                            state.submitted = Some(text.trim().to_owned().into());
                            (state.refine)(text, cx);
                        });
                    }));
                }
                cx.notify();
            } else if matches!(event, InputEvent::PressEnter { .. }) {
                let text = input.read(cx).value();
                if !reading_matches(state.loaded_query.as_ref(), &text) {
                    state.pending = None;
                    state.generation = state.generation.wrapping_add(1);
                    state.submitted = Some(text.trim().to_owned().into());
                    (state.refine)(text, cx);
                    return;
                }
                if let Some(selected) = state.selected.clone() {
                    match selected {
                        Selection::Package(key) => (state.actions.open_package)(key, window, cx),
                        Selection::Answer(key) => (state.actions.open_symbol)(key, window, cx),
                    }
                }
            }
        });
        let held = Held(actions.initial_held.clone());
        let reveal = KeyboardReveal::new(actions.scroll.clone());
        Self { input, route_query: query, refine, actions, keyboard: vec![], loaded_query: None, snapshot: None, submitted: None, generation: 0, selected: None, reveal, held,
            all_packages: false, all_answers: false, source: false, pending: None, _subscriptions: vec![subscription] }
    }
    fn accept(&mut self, model: &Arc<Model>, actions: &Actions, window: &mut Window, cx: &mut Context<Self>) {
        self.refine = Rc::clone(&actions.refine);
        self.actions = actions.clone();
        self.loaded_query = (!model.loading).then(|| model.query.clone());
        if !model.loading { self.snapshot = Some(Arc::clone(&model)); }
        if self.route_query != model.query {
            let local = self.submitted.as_ref() == Some(&model.query);
            self.route_query = model.query.clone();
            if model.query.is_empty() { self.selected = None; }
            if local { self.submitted = None; }
            else {
                // Back/Forward is a new editing session, even when the old
                // field was dirty. Drop its timer before resetting text.
                self.pending = None;
                self.generation = self.generation.wrapping_add(1);
                self.submitted = None;
                self.input.update(cx, |input, cx| input.set_value(model.query.clone(), window, cx));
            }
        }
    }

}

/// Compose a native Find folio.
#[must_use]
pub fn find(id: impl Into<ElementId>, model: Arc<Model>, actions: Actions, measure: &Measure) -> Find {
    Find { id: id.into(), model, actions, measure: *measure, #[cfg(test)] test_state: None }
}

#[derive(IntoElement)]
pub struct Find { id: ElementId, model: Arc<Model>, actions: Actions, measure: Measure, #[cfg(test)] test_state: Option<Entity<State>> }

impl RenderOnce for Find {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let m = self.measure;
        let p = cx.palette();
        let query = self.model.query.clone();
        let actions = self.actions.clone();
        #[cfg(test)]
        let state = self.test_state.unwrap_or_else(|| window.use_keyed_state(child(&self.id, "state"), cx, |window, cx| State::new(query, actions, window, cx)));
        #[cfg(not(test))]
        let state = window.use_keyed_state(child(&self.id, "state"), cx, |window, cx| State::new(query, actions, window, cx));
        state.update(cx, |state, cx| state.accept(&self.model, &self.actions, window, cx));
        // A query admission keeps the last immutable reading in place. Its
        // evidence remains inspectable, but navigation waits for the new read.
        let model = state.read(cx).snapshot.clone().unwrap_or_else(|| Arc::clone(&self.model));
        let empty = state.read(cx).input.read(cx).value().trim().is_empty();
        let updating = self.model.loading || model.query.as_ref().trim() != state.read(cx).input.read(cx).value().trim();
        state.update(cx, |state, _| {
            let shown_packages = if state.all_packages { model.candidates.len() } else { 8 };
            let choices = |state: &State| model.candidates.iter().take(shown_packages).flat_map(|candidate| {
                let active = state.selected.as_ref().is_some_and(|selected| match selected {
                    Selection::Package(key) => *key == candidate.key,
                    Selection::Answer(key) => candidate.answers.iter().any(|answer| answer.key == *key),
                });
                let shown = if active && state.all_answers { candidate.answers.len() } else if active || candidate.answers.len() <= 2 { 4 } else { 1 };
                std::iter::once(Selection::Package(candidate.key.clone())).chain(candidate.answers.iter().take(shown).map(|answer| Selection::Answer(answer.key.clone())))
            }).chain(model.loose.iter().map(|answer| Selection::Answer(answer.key.clone()))).collect::<Vec<_>>();
            let keyboard = choices(state);
            if !empty && !updating && state.selected.as_ref().is_none_or(|selection| !keyboard.contains(selection)) {
                state.selected = model.candidates.first().map(|candidate| candidate.answers.first()
                    .map_or_else(|| Selection::Package(candidate.key.clone()), |answer| Selection::Answer(answer.key.clone())))
                    .or_else(|| model.loose.first().map(|answer| Selection::Answer(answer.key.clone())));
            }
            state.keyboard = if empty && (!self.model.query.is_empty() || self.model.loading) { vec![] } else { choices(state) };
        });
        let input = state.read(cx).input.clone();
        crate::controls::field::sync_text_engine(cx);
        let hero = div().flex().flex_col().gap(m.space(Space::Base))
            .child(words(child(&self.id, "eyebrow"), "FIND", ty::CAPTION, p.ink1, &m))
            .child(div().flex().items_center().gap(m.space(Space::Roomy)).py(m.space(Space::Base))
                .border_b_1().border_color(p.peri.base.hsla())
                .child(ui(Icon::Search, IconSize::S24, p.peri.base))
                .child(Input::new(&input).appearance(false).bordered(false).focus_bordered(false)
                    .set(ty::TITLE, &m).px(px(0.0)).flex_1().min_w_0()))
            .child(words(child(&self.id, "hint"), "Search package names and indexed declarations. Select an item to inspect its recorded shape.", ty::CAPTION, p.ink2, &m));
        let keyboard_state = state.clone();
        let mut page = div().id(self.id.clone()).flex().flex_col().w(m.width()).gap(m.space(Space::Wide)).child(hero)
            .capture_key_down(move |event, _, cx| {
                let delta = match event.keystroke.key.as_str() { "down" => 1_isize, "up" => -1, _ => return };
                keyboard_state.update(cx, |state, cx| {
                    let at = state.selected.as_ref().and_then(|selected| state.keyboard.iter().position(|choice| choice == selected)).unwrap_or(0);
                    let next = at.saturating_add_signed(delta).min(state.keyboard.len().saturating_sub(1));
                    let selected = state.keyboard.get(next).cloned();
                    if state.selected != selected { state.selected = selected; state.reveal.request(); }
                    cx.notify();
                });
                cx.stop_propagation();
            });
        if !state.read(cx).held.packages().is_empty() {
            page = page.child(held_tray(&child(&self.id, "held-tray"), &state, &self.actions, &m, cx));
        }
        if empty {
            let mut examples = div().flex().flex_wrap().items_center().gap(m.space(Space::Base))
                .child(words(child(&self.id, "examples-label"), "TRY", ty::CAPTION, p.ink2, &m));
            for (at, query) in ["from_str", "toml", "Deserialize"].into_iter().enumerate() {
                let chosen = state.clone();
                examples = examples.child(button(child(&self.id, format!("example-{at}")), query, &m).ghost().size(Control::Small).icon(Icon::Spark)
                    .on_click(move |window, cx| chosen.update(cx, |state, cx| {
                        state.pending = None;
                        state.generation = state.generation.wrapping_add(1);
                        state.submitted = Some(query.into());
                        state.input.update(cx, |input, cx| input.set_value(query, window, cx));
                        (state.refine)(query.into(), cx);
                    })));
            }
            page = page.child(examples);
            if !self.model.query.is_empty() || self.model.loading || self.model.candidates.is_empty() { return page; }
            page = page.child(words(child(&self.id, "home-packages"), "Packages known here", ty::HEAD, p.ink1, &m));
        }
        if updating {
            page = page.child(words(child(&self.id, "loading"), "Updating results…", ty::CAPTION, p.ink2, &m));
            if state.read(cx).snapshot.is_none() { return page; }
        }
        let selected = state.read(cx).selected.clone();
        let loose_selected = selected.as_ref().and_then(|selected| match selected {
            Selection::Answer(key) => model.loose.iter().find(|answer| &answer.key == key), _ => None,
        });
        let resolved = selected.as_ref().and_then(|selected| match selected {
            Selection::Package(key) => model.candidates.iter().find(|candidate| &candidate.key == key).map(|candidate| (candidate, None)),
            Selection::Answer(key) => model.candidates.iter().find_map(|candidate| candidate.answers.iter().find(|answer| &answer.key == key).map(|answer| (candidate, Some(answer)))),
        }).or_else(|| if empty || loose_selected.is_some() { None } else { model.candidates.first().map(|candidate| (candidate, candidate.answers.first())) });
        let split = Modes::keyed(child(&self.id, "modes"), window, cx).settle(&FIND, m.fluid_room());
        let wide = split.mode == Split::Beside;
        // The inspector arrives once the rows have moved out of its column (they glide
        // from full width to the narrower list): text never crosses text.
        let motion = crate::motion::Motion::scoped(gpui::SharedString::from(format!("find-mode-{:?}", self.id)), cx);
        let arrived = split.progress(&motion, window, cx) >= 0.5;
        let (list_m, inspect_m) = if wide {
            let inspector = FIND_INSPECTOR.at(m.fluid_room());
            (m.within(m.width() - inspector - m.space(Space::Section)), m.within(inspector))
        } else { (m, m) };
        let flow = Flow::scoped(format!("find-{:?}", self.id), cx);
        flow.epoch((split.epoch, model.query.clone(), selected.clone().map(|selection| format!("{selection:?}"))));
        let mut list = div().flex().flex_col().w(list_m.width()).gap(m.space(Space::Base));
        let limit = if state.read(cx).all_packages { model.candidates.len() } else { 8 };
        for (at, candidate) in model.candidates.iter().take(limit).enumerate() {
            let active = resolved.is_some_and(|(current, _)| current.key == candidate.key);
            let group = candidate_view(&child(&self.id, format!("candidate-{at}")), candidate, active, resolved.and_then(|(_, answer)| answer), &state, &self.actions, !updating, &list_m, cx);
            list = list.child(flow.item(candidate.key.clone(), group));
            if active && !wide {
                if let Some((candidate, answer)) = resolved {
                    list = list.child(inspector(&child(&self.id, "inspect"), candidate, answer, &state, &self.actions, !updating, &inspect_m, cx));
                }
            }
        }
        if model.candidates.len() > limit {
            let count = model.candidates.len() - limit;
            let all = state.clone();
            list = list.child(button(child(&self.id, "more-packages"), format!("Explore {count} more packages"), &list_m).ghost().icon(Icon::Layers)
                .on_click(move |_, cx| all.update(cx, |state, cx| { state.all_packages = true; cx.notify(); })));
        }
        for (at, answer) in model.loose.iter().enumerate() {
            let open = Rc::clone(&self.actions.open_symbol);
            let key = answer.key.clone();
            let row = button(child(&self.id, format!("loose-{at}")), answer.name.clone(), &list_m).ghost().disabled(updating).on_click(move |window, cx| open(key.clone(), window, cx));
            list = list.child(if selected.as_ref() == Some(&Selection::Answer(answer.key.clone())) { state.read(cx).reveal.selected(row) } else { row.into_any_element() });
        }
        if let Some(answer) = loose_selected {
            let unqualified = Candidate { key: "".into(), name: "Declaration".into(), version: None, indexed: true, summary: None, facts: vec![], answers: vec![] };
            list = list.child(inspector(&child(&self.id, "loose-inspect"), &unqualified, Some(answer), &state, &self.actions, !updating, &list_m, cx));
        }
        if model.candidates.is_empty() && model.loose.is_empty() {
            list = list.child(words(child(&self.id, "empty"), "No recorded match yet. Try a package name or an item such as from_str.", ty::LEDE, p.ink2, &list_m));
        }
        let mut spread = div().flex().items_start().gap(m.space(Space::Section)).child(list);
        if wide && arrived && let Some((candidate, answer)) = resolved {
            spread = spread.child(div().w(inspect_m.width()).child(inspector(&child(&self.id, "inspect"), candidate, answer, &state, &self.actions, !updating, &inspect_m, cx)));
        }
        page = page.child(spread);
        for (at, coverage) in model.coverage.iter().enumerate() {
            page = page.child(words(child(&self.id, format!("coverage-{at}")), coverage.clone(), ty::CAPTION, p.ink2, &m));
        }
        if model.more_answers {
            page = page.child(words(child(&self.id, "bounded"), "Showing the first page of declaration matches. Refine the query to narrow the answer.", ty::CAPTION, p.ink2, &m));
        }
        page
    }
}

fn held_tray(id: &ElementId, state: &Entity<State>, actions: &Actions, m: &Measure, cx: &mut App) -> AnyElement {
    let p = cx.palette();
    let held = state.read(cx).held.clone();
    let mut chips = div().flex().flex_wrap().items_center().gap(m.space(Space::Base));
    for (at, package) in held.packages().iter().enumerate() {
        let remove = state.clone();
        let key = package.key.clone();
        let label = package.label();
        chips = chips.child(div().flex().items_center().gap(m.space(Space::Tight))
            .child(crate::paint::gem(Kind::Package).size(16.0 * m.scale()))
            .child(words(child(id, format!("name-{at}")), label.clone(), ty::MONO_SMALL, p.ink1, m))
            .child(button(child(id, format!("remove-{at}")), format!("Remove {label}"), m).ghost().size(Control::Small)
                .on_click(move |_, cx| remove.update(cx, |state, cx| { state.held.remove(&key); (state.actions.persist_held)(state.held.0.clone(), cx); cx.notify(); }))));
    }
    let compare = Rc::clone(&actions.compare);
    let chosen = held.packages().iter().map(|package| package.key.clone()).collect::<Vec<_>>();
    div().flex().flex_col().gap(m.space(Space::Base))
        .child(div().flex().items_center().gap(m.space(Space::Base))
            .child(ui(Icon::Split, IconSize::S18, p.peri.base))
            .child(words(child(id, "heading"), "HELD FOR COMPARISON", ty::CAPTION, p.ink1, m)))
        .child(chips)
        .child(button(child(id, "compare"), held.action_label(), m).edge().disabled(!held.can_compare())
            .on_click(move |window, cx| compare(chosen.clone(), window, cx)))
        .into_any_element()
}

fn candidate_view(id: &ElementId, candidate: &Candidate, active: bool, selected: Option<&Answer>, state: &Entity<State>, actions: &Actions, enabled: bool, m: &Measure, cx: &mut App) -> AnyElement {
    let p = cx.palette();
    let select = state.clone();
    let key = candidate.key.clone();
    let held = state.read(cx).held.contains(&key);
    let toggle = state.clone();
    let held_candidate = HeldPackage::from_candidate(candidate);
    // The name keeps a floor of its own: what does not fit beside it (the version, the button) wraps under it.
    let mut title = div().flex().flex_wrap().items_center().gap_x(m.space(Space::Roomy)).gap_y(m.space(Space::Tight))
        .child(crate::paint::gem(Kind::Package).size(22.0 * m.scale()))
        .child(div().min_w(px(96.0 * m.scale())).flex_1().child(words_ellipsis(child(id, "name"), candidate.name.clone(), ty::MONO_ROW, if active { p.ink0 } else { p.ink1 }, m)));
    if let Some(version) = &candidate.version { title = title.child(words(child(id, "version"), version.clone(), ty::MONO_SMALL, p.ink2, m)); }
    title = title.child(button(child(id, "hold"), if held { "Held" } else { "Compare" }, m).ghost().size(Control::Small).icon(if held { Icon::Pin } else { Icon::Split })
        .disabled(!held && state.read(cx).held.packages().len() == 4)
        .on_click(move |_, cx| toggle.update(cx, |state, cx| { state.held.toggle(held_candidate.clone()); (state.actions.persist_held)(state.held.0.clone(), cx); cx.notify(); })));
    let mut group = div().flex().flex_col().gap(m.space(Space::Tight));
    let row = div().id(child(id, "select")).px(m.space(Space::Base)).py(m.space(Space::Base)).cursor_pointer()
        .bg(if active { p.tint.hsla() } else { gpui::transparent_black() }).hover(|style| style.bg(p.tint))
        .child(title).on_click(move |_, _, cx| select.update(cx, |state, cx| { state.selected = Some(Selection::Package(key.clone())); state.source = false; cx.notify(); }));
    group = group.child(if active && selected.is_none() { state.read(cx).reveal.selected(row) } else { row.into_any_element() });
    let shown = if active || candidate.answers.len() <= 2 { 4 } else { 1 };
    let shown = if state.read(cx).all_answers && active { candidate.answers.len() } else { shown };
    for (at, answer) in candidate.answers.iter().take(shown).enumerate() {
        let answer_selected = selected.is_some_and(|current| current.key == answer.key);
        let select = state.clone();
        let key = answer.key.clone();
        let open = Rc::clone(&actions.open_symbol);
        let open_key = key.clone();
        let mut line = div().id(child(id, format!("answer-{at}"))).flex().items_start().gap(m.space(Space::Snug))
            .pl(m.space(Space::Wide)).pr(m.space(Space::Base)).py(m.space(Space::Tight)).cursor_pointer()
            .hover(|style| style.bg(p.tint))
            .child(kind_mark(answer.kind, KindSize::Sm, p))
            .child(div().flex().flex_col().flex_1().min_w_0().gap(m.space(Space::Hair))
                .child(words(child(id, format!("answer-{at}-name")), answer.name.clone(), ty::MONO_SMALL, if answer_selected { p.peri_hi.hsla() } else { p.ink1.hsla() }, m))
                .children(answer.context.as_ref().map(|context| words_ellipsis(child(id, format!("answer-{at}-context")), context.clone(), ty::CAPTION, p.ink2, m))));
        if answer_selected { line = line.child(button(child(id, "open-answer"), "Open", m).ghost().size(Control::Small).disabled(!enabled).on_click(move |window, cx| open(open_key.clone(), window, cx))); }
        let line = line.on_click(move |_, _, cx| select.update(cx, |state, cx| { state.selected = Some(Selection::Answer(key.clone())); state.source = false; cx.notify(); }));
        group = group.child(if answer_selected { state.read(cx).reveal.selected(line) } else { line.into_any_element() });
    }
    if active && candidate.answers.len() > shown {
        let more = state.clone();
        group = group.child(button(child(id, "more-answers"), format!("{} more matched declarations", candidate.answers.len() - shown), m).ghost().size(Control::Small)
            .on_click(move |_, cx| more.update(cx, |state, cx| { state.all_answers = true; cx.notify(); })));
    }
    group.into_any_element()
}

fn inspector(id: &ElementId, candidate: &Candidate, answer: Option<&Answer>, _state: &Entity<State>, actions: &Actions, enabled: bool, m: &Measure, cx: &mut App) -> AnyElement {
    let p = cx.palette();
    let mut detail = div().flex().flex_col().gap(m.space(Space::Base)).py(m.space(Space::Base));
    detail = detail.child(words(child(id, "care"), if candidate.indexed { "INDEXED HERE" } else { "CATALOG RECORD" }, ty::CAPTION, p.ink2, m));
    let title = answer.map_or(&candidate.name, |answer| &answer.name);
    let kind = answer.map_or(Kind::Package, |answer| answer.kind);
    detail = detail.child(div().flex().items_center().gap(m.space(Space::Base))
        .child(crate::paint::gem(kind).size(24.0 * m.scale()))
        .child(words(child(id, "name"), title.clone(), ty::HEAD, p.ink0, m)));
    if let Some(summary) = answer.and_then(|answer| answer.summary.as_ref()).or(candidate.summary.as_ref()) {
        detail = detail.child(words(child(id, "summary"), summary.clone(), ty::LEDE, p.ink2, m));
    }
    if let Some(answer) = answer {
        if let Some(pipe) = &answer.pipe {
            detail = detail.child(facet_pipe(id, pipe.clone(), m));
        }
        detail = detail.child(words(child(id, "reason"), answer.reason.clone(), ty::CAPTION, p.ink2, m));
        let mut actions_row = div().flex().flex_wrap().items_center().gap(m.space(Space::Base));
        let open = Rc::clone(&actions.open_symbol);
        let key = answer.key.clone();
        actions_row = actions_row.child(button(child(id, "open"), "Explore declaration", m).primary().size(Control::Small).disabled(!enabled).icon(Icon::Trail)
            .on_click(move |window, cx| open(key.clone(), window, cx)));
        if answer.signature.is_some() {
            let source = Rc::clone(&actions.open_code);
            let key = answer.key.clone();
            actions_row = actions_row.child(button(child(id, "source-toggle"), "Code", m).ghost().size(Control::Small).disabled(!enabled).icon(Icon::Peel)
                .on_click(move |window, cx| source(key.clone(), window, cx)));
        }
        if !candidate.key.is_empty() {
            let open = Rc::clone(&actions.open_package);
            let key = candidate.key.clone();
            actions_row = actions_row.child(button(child(id, "open-package"), "Package", m).ghost().size(Control::Small).disabled(!enabled).icon(Icon::Package)
                .on_click(move |window, cx| open(key.clone(), window, cx)));
        }
        detail = detail.child(actions_row);
    } else {
        for (at, (label, value)) in candidate.facts.iter().enumerate() {
            detail = detail.child(div().flex().flex_wrap().gap(m.space(Space::Roomy))
                .child(words(child(id, format!("fact-{at}-label")), label.clone(), ty::CAPTION, p.ink2, m))
                .child(words(child(id, format!("fact-{at}-value")), value.clone(), ty::SMALL, p.ink1, m)));
        }
    }
    if answer.is_none() && !candidate.key.is_empty() {
        let open = Rc::clone(&actions.open_package);
        let key = candidate.key.clone();
        detail = detail.child(div().flex().child(button(child(id, "open-package"), "Explore package", m).primary().size(Control::Small).disabled(!enabled).icon(Icon::Package)
            .on_click(move |window, cx| open(key.clone(), window, cx))));
    }
    detail.into_any_element()
}

fn facet_pipe(id: &ElementId, pipe: Pipe, m: &Measure) -> AnyElement {
    crate::anatomy::pipe(child(id, "action"), pipe, m, &crate::anatomy::Links::plain()).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn entering_a_new_query_never_opens_an_answer_from_the_previous_reading() {
        let loaded: SharedString = "from_str".into();
        assert!(reading_matches(Some(&loaded), "  from_str  "));
        for typed in ["toml", "from_st", ""] { assert!(!reading_matches(Some(&loaded), typed)); }
        assert!(!reading_matches(None, "from_str"));
        assert!(reading_matches(Some(&SharedString::default()), " "));
    }
    #[test]
    fn a_hand_is_ordered_unique_bounded_and_keeps_names_after_the_query_changes() {
        let packet = |key: &str, name: &str, version: Option<&str>| HeldPackage {
            key: key.into(), name: name.into(), version: version.map(Into::into),
        };
        let mut held = Held::default();
        let toml = packet("/Users/example/.cargo/registry/src/toml-0.8.23", "toml", Some("0.8.23"));
        let edit = packet("/Users/example/.cargo/registry/src/toml_edit-0.23.7", "toml_edit", Some("0.23.7"));
        held.toggle(toml.clone());
        assert_eq!(held.action_label(), "Choose one more package");
        held.toggle(edit.clone());
        assert_eq!(held.action_label(), "Compare 2 packages");
        held.toggle(packet("/catalog/basic-toml", "basic-toml", None));
        held.toggle(packet("/catalog/serde_json", "serde_json", None));
        held.toggle(packet("/catalog/fifth", "fifth", None));
        assert_eq!(held.packages().iter().map(|package| package.name.as_ref()).collect::<Vec<_>>(), ["toml", "toml_edit", "basic-toml", "serde_json"]);
        assert_eq!(held.packages()[0].label().as_ref(), "toml 0.8.23");
        assert!(!held.packages()[0].label().to_string().contains("/Users"));
        assert!(held.can_compare());
        // A new query can hold another package without supplying the old
        // candidate again; the saved name is still the one the person chose.
        held.toggle(edit);
        assert_eq!(held.packages()[0].label().as_ref(), "toml 0.8.23");
        assert_eq!(held.packages()[1].name.as_ref(), "basic-toml");
        held.remove(&toml.key);
        held.remove(&"/catalog/basic-toml".into());
        assert!(!held.can_compare());
    }
}

#[cfg(test)]
#[path = "find_tests.rs"]
mod window_tests;
