//! Find: a query, packages that expose the matching declarations, and an
//! inspector that reveals a callable as inputs and exits before showing code.
//! The search and qualification are producer evidence; no popularity score or
//! project-use count is manufactured here.

use super::acquire::{AddActions, Adding, Offer, add_control_enabled};
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
use gpui::{AnyElement, App, AppContext as _, Context, ElementId, Entity, Focusable as _, InteractiveElement, IntoElement, ParentElement,
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
    /// The reply recorded both a source path and a line for opening Code.
    pub source_available: bool,
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
    /// A registry release behind the candidate, which can be added to the
    /// library (or already is).
    pub offer: Option<Offer>,
}

/// Find's immutable reading. Rows retain producer order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Model {
    pub query: SharedString,
    pub candidates: Vec<Candidate>,
    /// Matches whose package could not be addressed; recorded facts remain readable,
    /// but the shell must explain why their page/code routes are unavailable.
    pub loose: Vec<Answer>,
    pub coverage: Vec<SharedString>,
    /// A first page with an owner-issued continuation has more than these rows.
    pub more_answers: bool,
    pub loading: bool,
}

/// The shell admits a reading only at its exact current producer authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadAdmission {
    Current,
    Retained(SharedString),
    Failed(SharedString),
}

/// Editing and reading share one rule for disclosure and action eligibility.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Admission {
    Blank,
    Invalid(SharedString),
    Current,
    Retained(SharedString),
    Failed(SharedString),
}

impl Admission {
    fn allows_actions(&self) -> bool { matches!(self, Self::Blank | Self::Current) }
    fn note(&self) -> Option<SharedString> {
        match self { Self::Invalid(reason) | Self::Retained(reason) | Self::Failed(reason) => Some(reason.clone()), _ => None }
    }
}

/// Validation belongs to the shell's typed query constructor, not this view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueryInput {
    Blank,
    Valid,
    Invalid(SharedString),
}

fn admit(text: &str, input: QueryInput, loaded_query: &str, read: &ReadAdmission) -> Admission {
    let blank = match input { QueryInput::Invalid(reason) => return Admission::Invalid(reason), QueryInput::Blank => true, QueryInput::Valid => false };
    if let ReadAdmission::Failed(reason) = read { return Admission::Failed(reason.clone()); }
    if let ReadAdmission::Retained(reason) = read { return Admission::Retained(reason.clone()); }
    if text.trim() != loaded_query.trim() {
        return Admission::Retained("Previous results; waiting for the current query. Result actions are unavailable.".into());
    }
    if blank { Admission::Blank } else { Admission::Current }
}

/// An indexed declaration may lack an addressable package; this is not an action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Routability {
    Available,
    Unavailable(SharedString),
}

impl Routability {
    fn available(&self) -> bool { matches!(self, Self::Available) }
    fn reason(&self) -> Option<SharedString> { match self { Self::Unavailable(reason) => Some(reason.clone()), Self::Available => None } }
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
    /// Restores this exact query handle after Ask releases the same visit.
    pub return_focus: Rc<dyn Fn(gpui::FocusHandle, &mut Window, &mut App) -> super::library::ReturnDisposition>,
    pub query_input: Rc<dyn Fn(&str) -> QueryInput>,
    pub refine: Rc<dyn Fn(SharedString, &mut App)>,
    /// Retries the exact failed current route; absent when the owner cannot serve it.
    pub retry: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
    pub symbol_routability: Rc<dyn Fn(&SharedString) -> Routability>,
    pub open_symbol: Rc<dyn Fn(SharedString, &mut Window, &mut App)>,
    pub open_code: Rc<dyn Fn(SharedString, &mut Window, &mut App)>,
    pub open_package: Rc<dyn Fn(SharedString, &mut Window, &mut App)>,
    pub compare: Rc<dyn Fn(Vec<SharedString>, &mut Window, &mut App)>,
    /// Adding an offered release to the library; `None`: nothing is offered.
    pub acquire: Option<AddActions>,
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
    active: bool,
    read_admission: ReadAdmission,
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
    initial_focus_pending: bool,
    pending: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl State {
    fn result_admission(&self, cx: &App) -> Admission {
        if !self.active { return Admission::Retained("This Find page is departing. Its controls are unavailable.".into()); }
        let text = self.input.read(cx).value();
        let loaded = self.loaded_query.as_ref().map_or("", |query| query.as_ref());
        admit(&text, (self.actions.query_input)(&text), loaded, &self.read_admission)
    }

    fn new(query: SharedString, actions: Actions, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let refine = Rc::clone(&actions.refine);
        let input = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("A package, an item, or a word in its docs");
            input.set_value(query.clone(), window, cx);
            input
        });
        let subscription = cx.subscribe_in(&input, window, |state: &mut Self, input, event: &InputEvent, window, cx| {
            if !state.active { return; }
            if matches!(event, InputEvent::Change) {
                let text = input.read(cx).value();
                state.pending = None;
                state.generation = state.generation.wrapping_add(1);
                let generation = state.generation;
                if matches!((state.actions.query_input)(&text), QueryInput::Invalid(_)) { cx.notify(); return; }
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
                if matches!((state.actions.query_input)(&text), QueryInput::Invalid(_)) { cx.notify(); return; }
                if !reading_matches(state.loaded_query.as_ref(), &text) {
                    if text.trim() == state.route_query.as_ref().trim() && !matches!(state.read_admission, ReadAdmission::Current) {
                        if let Some(reason) = state.result_admission(cx).note() { action_notice(&reason, window, cx); }
                        return;
                    }
                    state.pending = None;
                    state.generation = state.generation.wrapping_add(1);
                    state.submitted = Some(text.trim().to_owned().into());
                    (state.refine)(text, cx);
                    return;
                }
                if !matches!(state.read_admission, ReadAdmission::Current) {
                    if let Some(reason) = state.result_admission(cx).note() { action_notice(&reason, window, cx); }
                    return;
                }
                if let Some(selected) = state.selected.clone() {
                    match selected {
                        Selection::Package(key) => (state.actions.open_package)(key, window, cx),
                        Selection::Answer(key) => {
                            match (state.actions.symbol_routability)(&key) {
                                Routability::Available => (state.actions.open_symbol)(key, window, cx),
                                Routability::Unavailable(reason) => action_notice(&reason, window, cx),
                            }
                        }
                    }
                }
            }
        });
        let held = Held(actions.initial_held.clone());
        let reveal = KeyboardReveal::new(actions.scroll.clone());
        Self { active: true, read_admission: ReadAdmission::Current, input, route_query: query, refine, actions, keyboard: vec![], loaded_query: None, snapshot: None, submitted: None, generation: 0, selected: None, reveal, held,
            all_packages: false, all_answers: false, source: false, initial_focus_pending: true, pending: None, _subscriptions: vec![subscription] }
    }
    /// A release added to the library changes its address (the offer's
    /// package URL becomes the tree the owner indexed): the selection follows
    /// the release, so the person keeps looking at what they added.
    fn follow_added_release(&mut self, model: &Model) {
        let (Some(Selection::Package(key)), Some(previous)) = (&self.selected, &self.snapshot) else { return };
        if model.candidates.iter().any(|candidate| &candidate.key == key) { return; }
        let Some(release) = previous.candidates.iter().find(|candidate| &candidate.key == key).and_then(|candidate| candidate.offer.as_ref()).map(|offer| offer.release.clone()) else { return };
        if let Some(moved) = model.candidates.iter().find(|candidate| candidate.offer.as_ref().is_some_and(|offer| offer.release == release)) {
            self.selected = Some(Selection::Package(moved.key.clone()));
        }
    }

    fn accept(&mut self, model: &Arc<Model>, actions: &Actions, window: &mut Window, cx: &mut Context<Self>) {
        self.refine = Rc::clone(&actions.refine);
        self.actions = actions.clone();
        self.loaded_query = (!model.loading && matches!(self.read_admission, ReadAdmission::Current)).then(|| model.query.clone());
        if !model.loading {
            if matches!(self.read_admission, ReadAdmission::Current) { self.follow_added_release(model); }
            self.snapshot = Some(Arc::clone(model));
        }
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

fn result_action_ready(state: &Entity<State>, window: &mut Window, cx: &mut App) -> bool {
    let admission = state.read(cx).result_admission(cx);
    if admission.allows_actions() { return true; }
    let reason = admission.note().unwrap_or_else(|| "Wait for the current Find reading.".into());
    action_notice(&reason, window, cx);
    false
}

fn action_notice(reason: &str, window: &mut Window, cx: &mut App) {
    crate::overlay::toast::show(crate::overlay::toast::Toast::new(reason).voice(crate::tokens::Voice::Coral), window, cx);
}

/// Compose a native Find folio.
#[must_use]
pub fn find(id: impl Into<ElementId>, model: Arc<Model>, actions: Actions, measure: &Measure) -> Find {
    Find { id: id.into(), model, actions, admission: None, active: true, measure: *measure, #[cfg(test)] test_state: None }
}

#[derive(IntoElement)]
pub struct Find { admission: Option<ReadAdmission>, active: bool, id: ElementId, model: Arc<Model>, actions: Actions, measure: Measure, #[cfg(test)] test_state: Option<Entity<State>> }

impl Find {
    /// Supplies the live resource's authority/activity/terminal admission.
    #[must_use]
    pub fn admission(mut self, admission: ReadAdmission) -> Self { self.admission = Some(admission); self }

    /// Only the settled reader owns input and exposes controls to native clients.
    #[must_use]
    pub fn active(mut self, active: bool) -> Self { self.active = active; self }
}

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
        let read_admission = self.admission.unwrap_or_else(|| if self.model.loading {
            ReadAdmission::Retained("Waiting for the current query; result actions are unavailable.".into())
        } else { ReadAdmission::Current });
        state.update(cx, |state, _| {
            state.read_admission = read_admission.clone();
            state.active = self.active;
            if !self.active { state.pending = None; state.generation = state.generation.wrapping_add(1); }
        });
        if self.active { state.update(cx, |state, cx| {
            // A retained/departing Find may mount for its pixels while Ask or
            // a different route owns the keyboard. Only its first active
            // paint transfers native focus to the query.
            if state.initial_focus_pending {
                state.initial_focus_pending = false;
                state.input.clone().update(cx, |input, cx| input.focus(window, cx));
            } else {
                let query = state.input.read(cx).focus_handle(cx);
                let restore = Rc::clone(&self.actions.return_focus);
                // A Reader is still rendering this element. Transfer only
                // after its frame returns; the host rechecks the exact visit.
                window.defer(cx, move |window, cx| { restore(query, window, cx); });
            }
            state.accept(&self.model, &self.actions, window, cx);
        }); }
        // A query admission keeps the last immutable reading in place. Its
        // evidence remains inspectable, but navigation waits for the new read.
        let model = state.read(cx).snapshot.clone().unwrap_or_else(|| Arc::clone(&self.model));
        let empty = state.read(cx).input.read(cx).value().trim().is_empty();
        let input_text = state.read(cx).input.read(cx).value();
        let admission = if self.active {
            admit(&input_text, (self.actions.query_input)(&input_text), &model.query, &read_admission)
        } else {
            Admission::Retained("This Find page is departing. Its controls are unavailable.".into())
        };
        let updating = !admission.allows_actions();
        // A retained reading can still be inspected locally while its query
        // remains the text in the field. Source actions keep their own gate.
        let inspectable = self.active && input_text.trim() == model.query.as_ref().trim();
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
            if !empty && inspectable && state.selected.as_ref().is_none_or(|selection| !keyboard.contains(selection)) {
                state.selected = model.candidates.first().map(|candidate| candidate.answers.first()
                    .map_or_else(|| Selection::Package(candidate.key.clone()), |answer| Selection::Answer(answer.key.clone())))
                    .or_else(|| model.loose.first().map(|answer| Selection::Answer(answer.key.clone())));
            }
            state.keyboard = if !inspectable || empty && (!self.model.query.is_empty() || self.model.loading) { vec![] } else { choices(state) };
        });
        let input = state.read(cx).input.clone();
        crate::controls::field::sync_text_engine(cx);
        let hero = div().flex().flex_col().gap(m.space(Space::Base))
            .child(words(child(&self.id, "eyebrow"), "FIND", ty::CAPTION, p.ink1, &m))
            .child(div().flex().items_center().gap(m.space(Space::Roomy)).py(m.space(Space::Base))
                .border_b_1().border_color(p.peri.base.hsla())
                .child(ui(Icon::Search, IconSize::S24, p.peri.base))
                .child(Input::new(&input).id(child(&self.id, "query")).aria_label("Find query").aria_description("Search package names and indexed declarations. Enter opens the selected current result; Up and Down move through results.").disabled(!self.active).appearance(false).bordered(false).focus_bordered(false)
                    .set(ty::TITLE, &m).px(px(0.0)).flex_1().min_w_0()))
            .child(words(child(&self.id, "hint"), "Search package names and indexed declarations. Select an item to inspect its recorded shape.", ty::CAPTION, p.ink2, &m));
        let keyboard_state = state.clone();
        let mut page = div().id(self.id.clone()).role(gpui::Role::Group).aria_label("Find").flex().flex_col().w(m.width()).gap(m.space(Space::Wide)).child(hero)
            .capture_key_down(move |event, _, cx| {
                let delta = match event.keystroke.key.as_str() { "down" => 1_isize, "up" => -1, _ => return };
                keyboard_state.update(cx, |state, cx| {
                    if !state.active { return; }
                    let at = state.selected.as_ref().and_then(|selected| state.keyboard.iter().position(|choice| choice == selected)).unwrap_or(0);
                    let next = at.saturating_add_signed(delta).min(state.keyboard.len().saturating_sub(1));
                    let selected = state.keyboard.get(next).cloned();
                    if state.selected != selected { state.selected = selected; state.reveal.request(); }
                    cx.notify();
                });
                cx.stop_propagation();
            });
        if let Some(note) = admission.note() {
            page = page.child(div().id(child(&self.id, "admission-status"))
                .role(gpui::Role::Status).aria_label(note.clone())
                .child(words(child(&self.id, "admission"), note, ty::CAPTION, p.ink2, &m)));
        }
        if matches!(admission, Admission::Failed(_)) && let Some(retry) = &self.actions.retry {
            let retry = retry.clone();
            page = page.child(button(child(&self.id, "retry"), "Retry current Find query", &m).primary().disabled(!self.active)
                .on_click(move |window, cx| retry(window, cx)));
        }
        if !state.read(cx).held.packages().is_empty() {
            page = page.child(held_tray(&child(&self.id, "held-tray"), &state, &self.actions, admission.allows_actions(), &m, cx));
        }
        if empty {
            let mut examples = div().flex().flex_wrap().items_center().gap(m.space(Space::Base))
                .child(words(child(&self.id, "examples-label"), "TRY", ty::CAPTION, p.ink2, &m));
            for (at, query) in ["from_str", "toml", "Deserialize"].into_iter().enumerate() {
                let chosen = state.clone();
                examples = examples.child(button(child(&self.id, format!("example-{at}")), query, &m).ghost().size(Control::Small).icon(Icon::Spark).disabled(!self.active)
                    .on_click(move |window, cx| chosen.update(cx, |state, cx| {
                        if !state.active { return; }
                        state.pending = None;
                        state.generation = state.generation.wrapping_add(1);
                        state.submitted = Some(query.into());
                        state.input.update(cx, |input, cx| input.set_value(query, window, cx));
                        (state.refine)(query.into(), cx);
                    })));
            }
            page = page.child(examples);
            if !self.model.query.is_empty() || self.model.loading || self.model.candidates.is_empty() {
                for (at, coverage) in self.model.coverage.iter().enumerate() {
                    page = page.child(words(child(&self.id, format!("coverage-{at}")), coverage.clone(), ty::CAPTION, p.ink2, &m));
                }
                return page.into_any_element();
            }
            page = page.child(words(child(&self.id, "home-packages"), "Packages known here", ty::HEAD, p.ink1, &m));
        }
        if updating {
            if state.read(cx).snapshot.is_none() { return page.into_any_element(); }
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
        let mut list = div().id(child(&self.id, "results")).role(gpui::Role::Group).aria_label("Search results")
            .aria_description(admission.note().unwrap_or_else(|| "Current query results.".into()))
            .flex().flex_col().w(list_m.width()).gap(m.space(Space::Base));
        let limit = if state.read(cx).all_packages { model.candidates.len() } else { 8 };
        for candidate in model.candidates.iter().take(limit) {
            let active = resolved.is_some_and(|(current, _)| current.key == candidate.key);
            let group = candidate_view(&child(&self.id, format!("candidate-{}", candidate.key)), candidate, active, resolved.and_then(|(_, answer)| answer), &state, &self.actions, !updating, inspectable, &list_m, cx);
            list = list.child(flow.item(candidate.key.clone(), group));
            if active && !wide {
                if let Some((candidate, answer)) = resolved {
                    list = list.child(inspector(&child(&self.id, format!("inspect-{}", answer.map_or(&candidate.key, |answer| &answer.key))), candidate, answer, &state, &self.actions, !updating, &inspect_m, cx));
                }
            }
        }
        if model.candidates.len() > limit {
            let count = model.candidates.len() - limit;
            let all = state.clone();
            list = list.child(button(child(&self.id, "more-packages"), format!("Explore {count} more packages"), &list_m).ghost().disabled(!inspectable).icon(Icon::Layers)
                .on_click(move |_, cx| all.update(cx, |state, cx| { if !state.active { return; } state.all_packages = true; cx.notify(); })));
        }
        for answer in &model.loose {
            let open = Rc::clone(&self.actions.open_symbol);
            let key = answer.key.clone();
            let action_state = state.clone();
            let route = (self.actions.symbol_routability)(&answer.key);
            if let Some(reason) = route.reason() {
                list = list.child(words(child(&self.id, format!("loose-{}-unavailable", answer.key)), reason, ty::CAPTION, p.ink2, &list_m));
            }
            let row = button(child(&self.id, format!("loose-{}", answer.key)), answer.name.clone(), &list_m).ghost().disabled(updating || !route.available()).on_click(move |window, cx| {
                if result_action_ready(&action_state, window, cx) { open(key.clone(), window, cx); }
            });
            list = list.child(if selected.as_ref() == Some(&Selection::Answer(answer.key.clone())) { state.read(cx).reveal.selected(row) } else { row.into_any_element() });
        }
        if let Some(answer) = loose_selected {
            let unqualified = Candidate { key: "".into(), name: "Declaration".into(), version: None, indexed: true, summary: None, facts: vec![], answers: vec![], offer: None };
            list = list.child(inspector(&child(&self.id, format!("loose-inspect-{}", answer.key)), &unqualified, Some(answer), &state, &self.actions, !updating, &list_m, cx));
        }
        if model.candidates.is_empty() && model.loose.is_empty() {
            list = list.child(words(child(&self.id, "empty"), "No recorded match yet. Try a package name or an item such as from_str.", ty::LEDE, p.ink2, &list_m));
        }
        let mut spread = div().flex().items_start().gap(m.space(Space::Section)).child(list);
        if wide && arrived && let Some((candidate, answer)) = resolved {
            spread = spread.child(div().w(inspect_m.width()).child(inspector(&child(&self.id, format!("inspect-{}", answer.map_or(&candidate.key, |answer| &answer.key))), candidate, answer, &state, &self.actions, !updating, &inspect_m, cx)));
        }
        page = page.child(spread);
        for (at, coverage) in model.coverage.iter().enumerate() {
            page = page.child(words(child(&self.id, format!("coverage-{at}")), coverage.clone(), ty::CAPTION, p.ink2, &m));
        }
        if model.more_answers {
            page = page.child(words(child(&self.id, "bounded"), "Showing the first page of declaration matches. Refine the query to narrow the answer.", ty::CAPTION, p.ink2, &m));
        }
        page.into_any_element()
    }
}

fn held_tray(id: &ElementId, state: &Entity<State>, actions: &Actions, enabled: bool, m: &Measure, cx: &mut App) -> AnyElement {
    let p = cx.palette();
    let held = state.read(cx).held.clone();
    let mut chips = div().flex().flex_wrap().items_center().gap(m.space(Space::Base));
    for package in held.packages() {
        let remove = state.clone();
        let key = package.key.clone();
        let label = package.label();
        chips = chips.child(div().flex().items_center().gap(m.space(Space::Tight))
            .child(crate::paint::gem(Kind::Package).size(16.0 * m.scale()))
            .child(words(child(id, format!("name-{}", package.key)), label.clone(), ty::MONO_SMALL, p.ink1, m))
            .child(button(child(id, format!("remove-{}", package.key)), format!("Remove {label}"), m).ghost().size(Control::Small).disabled(!state.read(cx).active)
                .on_click(move |_, cx| remove.update(cx, |state, cx| { if !state.active { return; } state.held.remove(&key); (state.actions.persist_held)(state.held.0.clone(), cx); cx.notify(); }))));
    }
    let compare = Rc::clone(&actions.compare);
    let chosen = held.packages().iter().map(|package| package.key.clone()).collect::<Vec<_>>();
    let compare_state = state.clone();
    div().flex().flex_col().gap(m.space(Space::Base))
        .child(div().flex().items_center().gap(m.space(Space::Base))
            .child(ui(Icon::Split, IconSize::S18, p.peri.base))
            .child(words(child(id, "heading"), "HELD FOR COMPARISON", ty::CAPTION, p.ink1, m)))
        .child(chips)
        .child(button(child(id, "compare"), held.action_label(), m).edge().disabled(!enabled || !held.can_compare())
            .on_click(move |window, cx| { if result_action_ready(&compare_state, window, cx) { compare(chosen.clone(), window, cx); } }))
        .into_any_element()
}

fn held_candidate_label(candidate: &Candidate) -> SharedString { HeldPackage::from_candidate(candidate).label() }

fn selection_row(id: ElementId, state: &Entity<State>, selection: Selection, name: String, selected: bool, enabled: bool) -> gpui::Stateful<gpui::Div> {
    let mut row = div().id(id).role(gpui::Role::Button).aria_label(name)
        .aria_selected(selected).aria_disabled(!enabled);
    if enabled {
        let choose: Rc<dyn Fn(&mut App)> = {
            let state = state.clone();
            Rc::new(move |cx| state.update(cx, |state, cx| {
                if !state.active { return; }
                state.selected = Some(selection.clone()); state.source = false; cx.notify();
            }))
        };
        let native_choose = choose.clone();
        let key_choose = choose.clone();
        row = row.focusable().tab_index(0).cursor_pointer()
            .on_a11y_action(gpui::AccessibleAction::Click, move |_, _, cx| native_choose(cx))
            .on_key_down(move |event, _, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") { key_choose(cx); cx.stop_propagation(); }
            })
            .on_click(move |_, _, cx| choose(cx));
    }
    row
}

fn candidate_view(id: &ElementId, candidate: &Candidate, active: bool, selected: Option<&Answer>, state: &Entity<State>, actions: &Actions, enabled: bool, inspectable: bool, m: &Measure, cx: &mut App) -> AnyElement {
    let p = cx.palette();
    let key = candidate.key.clone();
    let held = state.read(cx).held.contains(&key);
    let toggle = state.clone();
    let held_candidate = HeldPackage::from_candidate(candidate);
    // The name keeps a floor of its own: what does not fit beside it (the version, the button) wraps under it.
    let mut title = div().flex().flex_wrap().items_center().gap_x(m.space(Space::Roomy)).gap_y(m.space(Space::Tight))
        .child(crate::paint::gem(Kind::Package).size(22.0 * m.scale()))
        .child(div().min_w(px(96.0 * m.scale())).flex_1().child(words_ellipsis(child(id, "name"), candidate.name.clone(), ty::MONO_ROW, if active { p.ink0 } else { p.ink1 }, m)));
    if let Some(version) = &candidate.version { title = title.child(words(child(id, "version"), version.clone(), ty::MONO_SMALL, p.ink2, m)); }
    let hold = button(child(id, "hold"), if held { "Held" } else { "Compare" }, m).ghost().size(Control::Small).icon(if held { Icon::Pin } else { Icon::Split })
        .aria_label(format!("{} {}", if held { "Remove from comparison:" } else { "Hold for comparison:" }, held_candidate.label()))
        .disabled(!enabled || (!held && state.read(cx).held.packages().len() == 4))
        .on_click(move |window, cx| { if result_action_ready(&toggle, window, cx) {
            toggle.update(cx, |state, cx| { state.held.toggle(held_candidate.clone()); (state.actions.persist_held)(state.held.0.clone(), cx); cx.notify(); });
        } });
    let mut group = div().flex().flex_col().gap(m.space(Space::Tight));
    let row = selection_row(child(id, "select"), state, Selection::Package(key),
        format!("Inspect package {}", held_candidate_label(candidate)), active && selected.is_none(), inspectable)
        .px(m.space(Space::Base)).py(m.space(Space::Base))
        .bg(if active { p.tint.hsla() } else { gpui::transparent_black() }).hover(|style| style.bg(p.tint))
        .child(title);
    let row = div().flex().items_center().gap(m.space(Space::Base)).child(row.flex_1().min_w_0()).child(hold);
    group = group.child(if active && selected.is_none() { state.read(cx).reveal.selected(row) } else { row.into_any_element() });
    let shown = if active || candidate.answers.len() <= 2 { 4 } else { 1 };
    let shown = if state.read(cx).all_answers && active { candidate.answers.len() } else { shown };
    for (at, answer) in candidate.answers.iter().take(shown).enumerate() {
        let answer_selected = selected.is_some_and(|current| current.key == answer.key);
        let key = answer.key.clone();
        let open = Rc::clone(&actions.open_symbol);
        let open_key = key.clone();
        let action_state = state.clone();
        let can_open = enabled && (actions.symbol_routability)(&key).available();
        let line = selection_row(child(id, format!("answer-{}", answer.key)), state,
            Selection::Answer(key), format!("Inspect declaration {}{}", answer.name,
                answer.context.as_ref().map(|context| format!(", {context}")).unwrap_or_default()), answer_selected, inspectable).flex().items_start().gap(m.space(Space::Snug))
            .pl(m.space(Space::Wide)).pr(m.space(Space::Base)).py(m.space(Space::Tight)).cursor_pointer()
            .hover(|style| style.bg(p.tint))
            .child(kind_mark(answer.kind, KindSize::Sm, p))
            .child(div().flex().flex_col().flex_1().min_w_0().gap(m.space(Space::Hair))
                .child(words(child(id, format!("answer-{at}-name")), answer.name.clone(), ty::MONO_SMALL, if answer_selected { p.peri_hi.hsla() } else { p.ink1.hsla() }, m))
                .children(answer.context.as_ref().map(|context| words_ellipsis(child(id, format!("answer-{at}-context")), context.clone(), ty::CAPTION, p.ink2, m))));
        let line = div().flex().items_center().child(line.flex_1().min_w_0())
            .children(answer_selected.then(|| button(child(id, format!("open-answer-{}", answer.key)), "Open", m)
                .aria_label(format!("Explore declaration {}", answer.name)).ghost().size(Control::Small).disabled(!can_open)
                .on_click(move |window, cx| { if result_action_ready(&action_state, window, cx) { open(open_key.clone(), window, cx); } })));
        group = group.child(if answer_selected { state.read(cx).reveal.selected(line) } else { line.into_any_element() });
    }
    if active && candidate.answers.len() > shown {
        let more = state.clone();
        group = group.child(button(child(id, "more-answers"), format!("{} more matched declarations", candidate.answers.len() - shown), m).disabled(!inspectable).ghost().size(Control::Small)
            .on_click(move |_, cx| more.update(cx, |state, cx| { if !state.active { return; } state.all_answers = true; cx.notify(); })));
    }
    group.into_any_element()
}

fn inspector(id: &ElementId, candidate: &Candidate, answer: Option<&Answer>, state: &Entity<State>, actions: &Actions, enabled: bool, m: &Measure, cx: &mut App) -> AnyElement {
    let p = cx.palette();
    let mut detail = div().flex().flex_col().gap(m.space(Space::Base)).py(m.space(Space::Base));
    let care = match &candidate.offer {
        Some(offer) if offer.library.is_none() => match &offer.place {
            super::acquire::Place::Unpacked => "ON THIS MACHINE",
            super::acquire::Place::Archive => "ON THIS MACHINE · PACKED",
            super::acquire::Place::Download => "IN THE REGISTRY",
            super::acquire::Place::Ambiguous { .. } => "MULTIPLE LOCAL SOURCES",
            super::acquire::Place::UnverifiedArchive(_) => "UNVERIFIED ARCHIVE",
        },
        _ if candidate.indexed => "INDEXED HERE",
        _ => "CATALOG RECORD",
    };
    detail = detail.child(words(child(id, "care"), care, ty::CAPTION, p.ink2, m));
    let title = answer.map_or(&candidate.name, |answer| &answer.name);
    let kind = answer.map_or(Kind::Package, |answer| answer.kind);
    detail = detail.child(div().flex().items_center().gap(m.space(Space::Base))
        .child(crate::paint::gem(kind).size(24.0 * m.scale()))
        .child(words(child(id, "name"), title.clone(), ty::HEAD, p.ink0, m)));
    if let Some(summary) = answer.and_then(|answer| answer.summary.as_ref()).or(candidate.summary.as_ref()) {
        detail = detail.child(words(child(id, "summary"), summary.clone(), ty::LEDE, p.ink2, m));
    }
    if let Some(answer) = answer {
        let route = (actions.symbol_routability)(&answer.key);
        let enabled = enabled && route.available();
        if let Some(reason) = route.reason() {
            detail = detail.child(div().id(child(id, "unavailable-status"))
                .role(gpui::Role::Status).aria_label(reason.clone())
                .child(words(child(id, "unavailable"), reason, ty::CAPTION, p.ink2, m)));
        }
        if let Some(pipe) = &answer.pipe {
            detail = detail.child(facet_pipe(id, pipe.clone(), m));
        }
        detail = detail.child(words(child(id, "reason"), answer.reason.clone(), ty::CAPTION, p.ink2, m));
        let mut actions_row = div().flex().flex_wrap().items_center().gap(m.space(Space::Base));
        let open = Rc::clone(&actions.open_symbol);
        let key = answer.key.clone();
        let action_state = state.clone();
        actions_row = actions_row.child(button(child(id, "open"), "Explore declaration", m).primary().size(Control::Small).disabled(!enabled).icon(Icon::Trail)
            .on_click(move |window, cx| { if result_action_ready(&action_state, window, cx) { open(key.clone(), window, cx); } }));
        if answer.source_available {
            let source = Rc::clone(&actions.open_code);
            let key = answer.key.clone();
            let action_state = state.clone();
            actions_row = actions_row.child(button(child(id, "source-toggle"), "Code", m).aria_label(format!("Open code for {}", answer.name)).ghost().size(Control::Small).disabled(!enabled).icon(Icon::Peel)
                .on_click(move |window, cx| { if result_action_ready(&action_state, window, cx) { source(key.clone(), window, cx); } }));
        }
        if !candidate.key.is_empty() {
            let open = Rc::clone(&actions.open_package);
            let key = candidate.key.clone();
            let action_state = state.clone();
            actions_row = actions_row.child(button(child(id, "open-package"), "Package", m).aria_label(format!("Explore package {}", held_candidate_label(candidate))).ghost().size(Control::Small).disabled(!enabled).icon(Icon::Package)
                .on_click(move |window, cx| { if result_action_ready(&action_state, window, cx) { open(key.clone(), window, cx); } }));
        }
        detail = detail.child(actions_row);
    } else {
        for (at, (label, value)) in candidate.facts.iter().enumerate() {
            detail = detail.child(div().flex().flex_wrap().gap(m.space(Space::Roomy))
                .child(words(child(id, format!("fact-{at}-label")), label.clone(), ty::CAPTION, p.ink2, m))
                .child(words(child(id, format!("fact-{at}-value")), value.clone(), ty::SMALL, p.ink1, m)));
        }
    }
    // A release not in the library yet has no page to explore: it is added.
    // One added here keeps saying so (and opens) once the library has it.
    let offered = candidate.offer.as_ref().zip(actions.acquire.as_ref())
        .filter(|(offer, acquire)| offer.library.is_none() || (acquire.state)(&offer.release, cx) != Adding::Idle);
    if answer.is_none() && let Some((offer, acquire)) = offered {
        return detail.child(add_control_enabled(child(id, "acquire"), offer, acquire, m, enabled, cx)).into_any_element();
    }
    if answer.is_none() && !candidate.key.is_empty() {
        let open = Rc::clone(&actions.open_package);
        let key = candidate.key.clone();
        let action_state = state.clone();
        detail = detail.child(div().flex().child(button(child(id, "open-package"), "Explore package", m).primary().size(Control::Small).disabled(!enabled).icon(Icon::Package)
            .on_click(move |window, cx| { if result_action_ready(&action_state, window, cx) { open(key.clone(), window, cx); } })));
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
