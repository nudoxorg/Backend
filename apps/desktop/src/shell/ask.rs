//! The query (⌘K, /): the jump bar is the path at rest and the query when
//! you type; the results are a plate over the shelf's column, and walking
//! them shows each place in the reader for real.
//!
//! Typing asks the store for a search through the read pool (latest wins:
//! a newer query supersedes the older one, whose rows never land). ↑ ↓ walk
//! the rows and **preview** each one (`Intent::Preview`: no history); ↵
//! keeps the shown place (`CommitPreview`), or opens the chosen row when
//! nothing was walked; Esc and Back put the place you were on back. Rows are
//! grouped by where they are: in the package you are reading first, then
//! everywhere. The part of each name the query matched is underlined in
//! periwinkle; a row that matched somewhere else says where ("docs").
//!
//! The field itself is drawn by the jump bar ([`Ask::input`]); this entity
//! draws the plate. The field is `gpui_component`'s input (IME) until the
//! facet input lands.

use super::kit::{kind_of, search_result_route, text};
use super::region::Links;
use crate::core::{ReadHoldReason, Resource, ResourceAdmission, ResourceTerminal, VersionedRoot, admit_resource};
use crate::model::pages::{KeyError, MatchReason, PageKey, SearchPage, SearchQuery, SearchRow};
use crate::navigation::{BrowseRoute, Intent, OrbitRoute, Overlay, Route};
use crate::runtime::store::{Branch, StoreEvent};
use facet::icons::{self, KindSize};
use facet::tokens::ty;
use facet::{ActiveFacet as _, Measure, Space};
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, Focusable as _, InteractiveElement, Role,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Task, Window, ScrollHandle, div, px,
};
use gpui_component::input::{InputEvent, InputState};
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

/// How long typing rests before the query is asked.
const SETTLE: Duration = Duration::from_millis(90);

/// Rows per group before the rest fold into "all results".
const PER_GROUP: usize = 8;

/// Keep a pasted query bounded before it becomes a page key or a read.
const MAX_QUERY_CHARS: usize = 256;

/// The editor's draft is the sole source of truth for the plate and routes.
#[derive(Clone, Debug, Eq, PartialEq)]
enum QueryDraft {
    Blank,
    Valid(SearchQuery),
    Rejected(QueryReject),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QueryReject {
    TooLong,
    Invalid(KeyError),
}

impl QueryDraft {
    fn parse(text: &str) -> Self {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Self::Blank;
        }
        if trimmed.chars().nth(MAX_QUERY_CHARS).is_some() {
            return Self::Rejected(QueryReject::TooLong);
        }
        match SearchQuery::new(trimmed, SearchQuery::DEFAULT_LIMIT) {
            Ok(query) => Self::Valid(query),
            Err(error) => Self::Rejected(QueryReject::Invalid(error)),
        }
    }

    const fn query(&self) -> Option<&SearchQuery> {
        match self {
            Self::Valid(query) => Some(query),
            Self::Blank | Self::Rejected(_) => None,
        }
    }

    const fn status(&self) -> Option<&'static str> {
        match self {
            Self::Rejected(QueryReject::TooLong) => Some("Search query is too long (maximum 256 characters)."),
            Self::Rejected(QueryReject::Invalid(KeyError::ControlCharacter)) => Some("Search query contains a control character."),
            Self::Rejected(QueryReject::Invalid(KeyError::Package | KeyError::Empty)) => Some("Search query cannot be used."),
            Self::Blank | Self::Valid(_) => None,
        }
    }
}

/// Where a row is, relative to the place you are reading.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Group {
    /// In the package you are reading.
    Here,
    /// Anywhere else.
    Everywhere,
}

/// One row of the list.
#[derive(Clone)]
struct Choice {
    name: SharedString,
    /// The byte range of `name` the query matched, when it matched there.
    matched: Option<std::ops::Range<usize>>,
    place: SharedString,
    /// Said only when the name does not show why the row matched.
    reason: Option<SharedString>,
    kind: icons::Kind,
    route: Option<Route>,
    /// Repeated matches for one exact destination remain separate stops.
    route_occurrence: usize,
    unavailable: Option<SharedString>,
    group: Group,
}

/// Result identity is distinct from the scroll container's child index:
/// the semantic status and group headings occupy child slots but are not choices.
#[derive(Clone)]
enum ResultScrollTarget {
    Page { route: Route, occurrence: usize },
    Note { index: usize, name: SharedString, group: Group },
    All,
}
impl ResultScrollTarget {
    fn choice(index: usize, choice: &Choice) -> Self {
        choice.route.as_ref().map_or_else(
            || Self::Note { index, name: choice.name.clone(), group: choice.group },
            |route| Self::Page { route: route.clone(), occurrence: choice.route_occurrence },
        )
    }
    fn matches(&self, index: usize, choice: &Choice) -> bool {
        match self {
            Self::Page { route, occurrence } => choice.route.as_ref() == Some(route) && choice.route_occurrence == *occurrence,
            Self::Note { index: known, name, group } => *known == index && choice.route.is_none() && choice.name == *name && choice.group == *group,
            Self::All => false,
        }
    }
}

struct ResultScrollRequest {
    target: ResultScrollTarget,
    revision: u64,
    root: VersionedRoot,
    attachment: Option<crate::runtime::store::OwnerAttachment>,
}

#[derive(Clone, Copy)]
struct ScrollChildIndex(usize);

/// The mapping is built from the same admitted choices/statuses that paint
/// this frame. It has at most sixteen choice entries and one final action.
struct ResultScrollMap {
    choices: Vec<ScrollChildIndex>,
    all: Option<ScrollChildIndex>,
}
impl ResultScrollMap {
    fn new(choices: &[Choice], semantic_status: bool, all: bool) -> Self {
        let mut child = usize::from(semantic_status);
        let mut group = None;
        let mut indices = Vec::with_capacity(choices.len());
        for choice in choices {
            if group != Some(choice.group) { group = Some(choice.group); child += 1; }
            indices.push(ScrollChildIndex(child));
            child += 1;
        }
        Self { choices: indices, all: all.then_some(ScrollChildIndex(child)) }
    }
    fn resolve(&self, target: &ResultScrollTarget, choices: &[Choice]) -> Option<ScrollChildIndex> {
        if matches!(target, ResultScrollTarget::All) { return self.all; }
        choices.iter().enumerate().find(|(index, choice)| target.matches(*index, choice))
            .and_then(|(index, _)| self.choices.get(index).copied())
    }
}

/// A submission refusal belongs to the current draft, not navigation.
#[derive(Clone)]
enum SubmitRefusal { NoMatch, Unavailable(SharedString), NoDestination(SharedString) }
impl SubmitRefusal {
    fn words(&self) -> SharedString {
        match self { Self::NoMatch => "No result matches this query. Try a declaration name or different words.".into(),
            Self::Unavailable(words) | Self::NoDestination(words) => words.clone() }
    }
}

/// Only an admitted destination can commit navigation. Refusals stay with
/// the exact draft; events from earlier drafts or covered editors are inert.
enum SubmissionOutcome {
    Navigate(Route),
    CommitPreview,
    Refused(SubmitRefusal),
    Superseded,
}

#[derive(Clone)]
enum SearchPressDestination {
    Row(Route),
    AllResults,
}

/// Every native search control captures its painted draft and destination.
#[derive(Clone)]
struct SearchPressTarget {
    query: SearchQuery,
    revision: u64,
    destination: SearchPressDestination,
}

/// Feedback for one native pointer press cannot activate a retired Link.
struct PressedResult {
    target: SearchPressTarget,
    root: VersionedRoot,
    attachment: Option<crate::runtime::store::OwnerAttachment>,
    revoked: bool,
}

/// The query surface.
pub(crate) struct Ask {
    links: Links,
    input: Entity<InputState>,
    draft: QueryDraft,
    /// Only a draft whose typing debounce finished may renew a visible read.
    query_settled: bool,
    refusal: Option<SubmitRefusal>,
    pressed_result: Option<PressedResult>,
    /// Invalidates callbacks painted for an earlier editor draft, even when
    /// a later draft happens to reuse the same query text.
    revision: Rc<Cell<u64>>,
    selected: usize,
    /// Whether ↑ ↓ have walked the rows since the last keystroke: ↵ then
    /// keeps the shown place instead of opening the first row.
    walked: bool,
    renders: u64,
    pending: Option<Task<()>>,
    scroll: ScrollHandle,
    /// One pending keyboard request; resolved only by the current painted list.
    scroll_request: Option<ResultScrollRequest>,
    /// Native focus belongs to the typed destination, not its current row.
    row_focus: Vec<(Route, usize, FocusHandle)>,
    all_focus: FocusHandle,
    preparation_focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl Ask {
    pub(crate) fn new(links: Links, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find a name, or ask"));
        let typed = cx.subscribe_in(&input, window, |ask: &mut Self, input, event: &InputEvent, window, cx| match event {
            InputEvent::Change => {
                let text = input.read(cx).value().to_string();
                ask.typed(text, cx);
            }
            InputEvent::PressEnter { secondary: true, shift: false } => ask.choose_all(cx),
            InputEvent::PressEnter { .. } => ask.choose(window, cx),
            InputEvent::Focus | InputEvent::Blur => {}
        });
        let store = links.store.clone();
        let landed = cx.subscribe(&store, |ask: &mut Self, _, event: &StoreEvent, cx| {
            let search_changed = matches!((event, ask.draft.query()),
                (StoreEvent::Resource(PageKey::Search(query)), Some(mine)) if query == mine);
            if search_changed || event.is_branch(Branch::Root) || event.is_branch(Branch::GraphFocus)
                || event.is_branch(Branch::Owner) || event.is_branch(Branch::Route) {
                ask.mark_revoked_press(cx);
                if search_changed || event.is_branch(Branch::Owner) || event.is_branch(Branch::Root) || event.is_branch(Branch::Route) {
                    ask.ensure_current_query(cx);
                }
                cx.notify();
            }
            if event.is_branch(Branch::Overlay)
                && ask.links.snapshot(cx).overlay() != Some(Overlay::CommandPalette) {
                ask.pressed_result = None;
            }
        });
        Self {
            links,
            input,
            draft: QueryDraft::Blank,
            query_settled: false,
            refusal: None,
            pressed_result: None,
            revision: Rc::new(Cell::new(0)),
            selected: 0,
            walked: false,
            renders: 0,
            pending: None,
            scroll: ScrollHandle::new(),
            scroll_request: None,
            row_focus: Vec::new(),
            all_focus: cx.focus_handle().tab_stop(true),
            preparation_focus: cx.focus_handle().tab_stop(true),
            _subscriptions: vec![typed, landed],
        }
    }

    pub(crate) const fn renders(&self) -> u64 {
        self.renders
    }

    /// The field, for the jump bar to draw in its place.
    pub(crate) const fn input(&self) -> &Entity<InputState> {
        &self.input
    }

    /// Opens fresh. The Shell's current-view keyboard claim focuses the
    /// mounted field, so a covered or inactive window cannot steal focus.
    pub(crate) fn opened(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = 0;
        self.walked = false;
        self.reset_result_scroll();
        self.refusal = None;
        self.pressed_result = None;
        self.draft = QueryDraft::Blank;
        self.query_settled = false;
        self.revision.set(self.revision.get().wrapping_add(1));
        self.pending = None;
        self.links.store.update(cx, |store, cx| store.observe_ask_query(None, cx));
        self.input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        cx.notify();
    }

    fn typed(&mut self, text: String, cx: &mut Context<Self>) {
        self.refusal = None;
        self.pressed_result = None;
        let draft = QueryDraft::parse(&text);
        if draft == self.draft {
            return;
        }
        self.reset_result_scroll();
        let plate_changed = matches!(&self.draft, QueryDraft::Blank) != matches!(&draft, QueryDraft::Blank);
        self.selected = 0;
        // A new query shows where you were again until you walk its rows.
        if self.walked || self.links.snapshot(cx).session().preview.is_some() {
            self.links.dispatch(Intent::EndPreview, cx);
        }
        self.walked = false;
        self.draft = draft;
        self.query_settled = false;
        self.revision.set(self.revision.get().wrapping_add(1));
        self.pending = None;
        self.links.store.update(cx, |store, cx| store.observe_ask_query(None, cx));
        cx.notify();
        if plate_changed {
            self.links.shell(cx, |_, cx| cx.notify());
        }
        let Some(_) = self.draft.query() else {
            return;
        };
        let revision = self.revision.get();
        // Latest wins: a newer keystroke drops this timer, and the store
        // supersedes an older query's read.
        self.pending = Some(cx.spawn(async move |ask, cx| {
            cx.background_executor().timer(SETTLE).await;
            let _ = ask.update(cx, |ask, cx| {
                if ask.revision.get() != revision { return; }
                ask.query_settled = true;
                ask.ensure_current_query(cx);
            });
        }));
    }

    /// Ask is outside the route-focused key set. Its current visible draft
    /// must hold while Starting and renew after owner or root replacement.
    /// The store keeps repeated start/landing notifications idempotent.
    fn ensure_current_query(&self, cx: &mut Context<Self>) {
        if !self.query_settled
            || self.links.snapshot(cx).overlay() != Some(Overlay::CommandPalette) { return; }
        let Some(query) = self.draft.query().cloned() else { return; };
        if !current_input_query(&self.input, &query, cx) { return; }
        self.links.store.update(cx, |store, cx| { store.observe_ask_query(Some(query), cx); });
    }

    /// The modal's editor and mounted links are its native keyboard owners.
    pub(crate) fn owns_focus(&self, window: &Window, cx: &App) -> bool {
        self.input.read(cx).focus_handle(cx).is_focused(window)
            || self.row_focus.iter().any(|(_, _, handle)| handle.is_focused(window))
            || self.all_focus.is_focused(window)
            || self.preparation_focus.is_focused(window)
    }

    /// Moves the selection and shows the row's place in the reader.
    pub(crate) fn step(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let choices = self.choices(cx);
        let focused = self.row_focus.iter().find(|(_, _, handle)| handle.is_focused(window))
            .map(|(route, occurrence, _)| (route.clone(), *occurrence));
        let from_link = focused.is_some() || self.all_focus.is_focused(window);
        let all_mounted = !choices.is_empty() && self.all_results(cx).is_some();
        if self.sync_row_focus(&choices, all_mounted, window, cx) {
            return;
        }
        if choices.is_empty() {
            if self.walked && self.links.snapshot(cx).session().preview.is_some() {
                self.links.dispatch(Intent::EndPreview, cx);
                self.walked = false;
            }
            return;
        }
        let next = step_index(&choices, focused.as_ref().map(|(route, occurrence)| (route, *occurrence)),
            self.selected, self.walked, delta);
        self.selected = next;
        self.walked = true;
        self.request_result_scroll(ResultScrollTarget::choice(next, &choices[next]), cx);
        if from_link {
            let focus = choices[next].route.as_ref().and_then(|route| {
                self.row_focus.iter().find(|(known, occurrence, _)| {
                    known == route && *occurrence == choices[next].route_occurrence
                }).map(|(_, _, handle)| handle.clone())
            }).unwrap_or_else(|| self.input.read(cx).focus_handle(cx));
            focus.focus(window, cx);
        }
        if let Some(route) = choices[next].route.clone() {
            self.links.dispatch(Intent::Preview(route), cx);
        } else if self.links.snapshot(cx).session().preview.is_some() {
            self.links.dispatch(Intent::EndPreview, cx);
        }
        cx.notify();
    }

    /// Tab walks only Ask's editor and its currently mounted destinations.
    /// The veiled shelf is never an intermediate keyboard stop.
    pub(crate) fn focus_next(&mut self, backwards: bool, window: &mut Window, cx: &mut Context<Self>) {
        let choices = self.choices(cx);
        let all_mounted = !choices.is_empty() && self.all_results(cx).is_some();
        if self.sync_row_focus(&choices, all_mounted, window, cx) {
            // The index can change while a result owns native focus. Its
            // former handle is no longer a stop; leave focus in the editor.
            return;
        }
        let mut targets = Vec::with_capacity(choices.len() + 2);
        targets.push((None, self.input.read(cx).focus_handle(cx)));
        if self.preparation_check_available(cx) {
            targets.push((None, self.preparation_focus.clone()));
        }
        for (index, choice) in choices.iter().enumerate() {
            if let Some((_, _, handle)) = self.row_focus.iter().find(|(route, occurrence, _)| {
                choice.route.as_ref() == Some(route) && choice.route_occurrence == *occurrence
            }) {
                targets.push((Some(index), handle.clone()));
            }
        }
        if all_mounted {
            targets.push((None, self.all_focus.clone()));
        }
        let current = targets.iter().position(|(_, handle)| handle.is_focused(window));
        let next = match (current, backwards) {
            (Some(index), true) => (index + targets.len() - 1) % targets.len(),
            (Some(index), false) => (index + 1) % targets.len(),
            (None, true) => targets.len() - 1,
            (None, false) => 0,
        };
        targets[next].1.focus(window, cx);
        if let Some(index) = targets[next].0 {
            self.selected = index;
            self.walked = true;
            self.request_result_scroll(ResultScrollTarget::choice(index, &choices[index]), cx);
        } else if next == 0 {
            self.selected = 0;
            self.walked = false;
            if let Some(choice) = choices.first() { self.request_result_scroll(ResultScrollTarget::choice(0, choice), cx); }
        } else if all_mounted && targets[next].1 == self.all_focus {
            self.request_result_scroll(ResultScrollTarget::All, cx);
        }
        cx.notify();
    }

    fn reset_result_scroll(&mut self) {
        // A new handle also retires GPUI's deferred active-item request and
        // old child geometry; setting offset alone cannot retire those.
        self.scroll = ScrollHandle::new();
        self.scroll_request = None;
    }

    fn request_result_scroll(&mut self, target: ResultScrollTarget, cx: &App) {
        let store = self.links.store.read(cx);
        self.scroll_request = Some(ResultScrollRequest { target, revision: self.revision.get(),
            root: store.snapshot().key(), attachment: store.current_owner_attachment() });
    }

    fn apply_result_scroll(&mut self, choices: &[Choice], map: &ResultScrollMap, cx: &App) {
        let Some(request) = self.scroll_request.take() else { return; };
        let store = self.links.store.read(cx);
        if request.revision != self.revision.get() || request.root != store.snapshot().key()
            || request.attachment != store.current_owner_attachment()
            || store.snapshot().overlay() != Some(Overlay::CommandPalette) { return; }
        if let Some(child) = map.resolve(&request.target, choices) { self.scroll.scroll_to_item(child.0); }
    }

    /// Returns true when a focused destination vanished from the mounted plate.
    fn sync_row_focus(&mut self, choices: &[Choice], all_mounted: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let lost_destination = self.row_focus.iter().any(|(route, occurrence, handle)| {
            handle.is_focused(window)
                && !choices.iter().any(|choice| choice.route.as_ref() == Some(route) && choice.route_occurrence == *occurrence)
        }) || (!all_mounted && self.all_focus.is_focused(window));
        let lost_focus = lost_destination || (self.preparation_focus.is_focused(window) && !self.preparation_check_available(cx));
        self.row_focus.retain(|(route, occurrence, _)| {
            choices.iter().any(|choice| choice.route.as_ref() == Some(route) && choice.route_occurrence == *occurrence)
        });
        for choice in choices {
            if let Some(route) = &choice.route
                && !self.row_focus.iter().any(|(known, occurrence, _)| known == route && *occurrence == choice.route_occurrence)
            {
                self.row_focus.push((route.clone(), choice.route_occurrence, cx.focus_handle().tab_stop(true)));
            }
        }
        debug_assert!(self.row_focus.len() <= PER_GROUP * 2);
        if lost_focus {
            self.scroll_request = None;
            let editor = self.input.read(cx).focus_handle(cx);
            editor.focus(window, cx);
            if lost_destination {
                self.selected = 0;
                self.walked = false;
            }
            cx.notify();
        }
        lost_focus
    }

    fn preparation_check_available(&self, cx: &App) -> bool {
        self.draft.query().is_some_and(|query|
            self.links.store.read(cx).preparation_token(&PageKey::Search(query.clone())).is_some())
    }

    /// ↵: keeps the place the walk is showing, or opens the chosen row.
    /// A refused destination stays in this query and explains why it cannot
    /// navigate; only a verified destination commits the current visit.
    pub(crate) fn choose(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.links.snapshot(cx).overlay() != Some(Overlay::CommandPalette)
            || self.draft.query().is_some_and(|query| !current_input_query(&self.input, query, cx)) {
            return;
        }
        let choices = self.choices(cx);
        let outcome = match choices.get(self.selected) {
            None => self.empty_submission(cx),
            Some(choice) => match choice.route.clone() {
                Some(route) => {
                    let admitted = self.draft.query().is_some_and(|query| current_row_route(&self.links, query, &route, cx));
                    if !admitted { stale_submission() }
                    else if self.links.snapshot(cx).session().preview.is_some() && self.walked && self.links.snapshot(cx).route() == &route {
                        SubmissionOutcome::CommitPreview
                    } else { SubmissionOutcome::Navigate(route) }
                }
                None => SubmissionOutcome::Refused(SubmitRefusal::NoDestination(choice.unavailable.clone()
                    .unwrap_or_else(|| format!("{} has no page yet", choice.name).into()))),
            },
        };
        self.complete_submission(outcome, cx);
    }

    fn empty_submission(&self, cx: &App) -> SubmissionOutcome {
        if self.draft.query().is_none() { return SubmissionOutcome::Superseded; }
        SubmissionOutcome::Refused(self.read_status(cx).0.map_or(SubmitRefusal::NoMatch, SubmitRefusal::Unavailable))
    }

    fn complete_submission(&mut self, outcome: SubmissionOutcome, cx: &mut Context<Self>) {
        self.pressed_result = None;
        match outcome {
            SubmissionOutcome::Navigate(route) => self.links.dispatch(Intent::Navigate(route), cx),
            SubmissionOutcome::CommitPreview => {
                self.links.dispatch(Intent::CommitPreview, cx);
                self.links.dispatch(Intent::DismissOverlay, cx);
            }
            SubmissionOutcome::Refused(reason) => { self.refusal = Some(reason); cx.notify(); }
            SubmissionOutcome::Superseded => {}
        }
    }

    fn capture_native_press(&mut self, target: &SearchPressTarget, cx: &App) {
        if target.revision == self.revision.get()
            && self.links.snapshot(cx).overlay() == Some(Overlay::CommandPalette)
            && current_input_query(&self.input, &target.query, cx)
            && current_press_target(&self.links, target, cx)
        {
            let store = self.links.store.read(cx);
            self.pressed_result = Some(PressedResult {
                target: target.clone(),
                root: store.snapshot().key(),
                attachment: store.current_owner_attachment(),
                revoked: false,
            });
        }
    }

    /// A native Link may be unmounted between press and release. Keep its
    /// cancellation intact, but explain the lost authority in the current Ask.
    fn mark_revoked_press(&mut self, cx: &App) {
        let Some(pressed) = self.pressed_result.as_ref() else {
            return;
        };
        if pressed.target.revision != self.revision.get()
            || self.draft.query() != Some(&pressed.target.query)
            || self.links.snapshot(cx).overlay() != Some(Overlay::CommandPalette)
            || !current_input_query(&self.input, &pressed.target.query, cx)
        {
            self.pressed_result = None;
            return;
        }
        let store = self.links.store.read(cx);
        let revoked = pressed.attachment != store.current_owner_attachment()
            || !pressed.root.same_authority(store.snapshot().key())
            || !current_press_target(&self.links, &pressed.target, cx);
        if revoked && let Some(pressed) = self.pressed_result.as_mut() {
            pressed.revoked = true;
        }
    }

    fn refuse_revoked_press(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.mark_revoked_press(cx);
        if self.pressed_result.as_ref().is_some_and(|pressed| pressed.revoked) {
            self.complete_submission(stale_submission(), cx);
            self.input.read(cx).focus_handle(cx).focus(window, cx);
            return true;
        }
        false
    }

    fn owner_failure_words(&self, cx: &App) -> Option<SharedString> {
        self.links.store.read(cx).owner_fault().map(|fault| {
            format!("The index owner is unavailable: {fault}").into()
        })
    }

    /// ⌘↵ opens Find only while this exact query has a current served page.
    fn choose_all(&mut self, cx: &mut Context<Self>) {
        if self.links.snapshot(cx).overlay() != Some(Overlay::CommandPalette) { return; }
        let outcome = match (self.draft.query(), self.all_results(cx)) {
            (Some(query), Some(route)) if !self.choices(cx).is_empty() =>
                follow_all(&self.links, &self.input, query, &route, self.revision.get(), &self.revision, cx),
            _ => self.empty_submission(cx),
        };
        self.complete_submission(outcome, cx);
    }

    /// The Find route for the current search results (⌘↵).
    pub(crate) fn all_results(&self, cx: &App) -> Option<Route> {
        let query = self.draft.query()?.clone();
        let (results, root, serving) = self.search_resource(cx)?;
        admit_resource(&results, root, serving).current_value()
            .is_some_and(|page| page.query.as_ref() == query.text.as_ref())
            .then(|| Route::Orbit(OrbitRoute::Browse(BrowseRoute::Find(query))))
    }

    /// What the jump bar says at the query's end: how many, and where.
    pub(crate) fn count(&self, cx: &App) -> SharedString {
        if let Some(status) = self.draft.status() { return status.into(); }
        if self.draft.query().is_none() { return SharedString::default(); }
        if let Some((results, root, serving)) = self.search_resource(cx) {
            match admit_resource(&results, root, serving) {
                ResourceAdmission::Current(page) if self.draft.query().is_some_and(|query| page.query.as_ref() != query.text.as_ref()) =>
                    return "search unavailable".into(),
                ResourceAdmission::Current(_) => {}
                ResourceAdmission::Retained { .. } => return "earlier results".into(),
                ResourceAdmission::Pending(_) => return "…".into(),
                ResourceAdmission::Failed { .. } => return "search unavailable".into(),
            }
        }
        let choices = self.choices(cx);
        let here = choices.iter().filter(|choice| choice.group == Group::Here).count();
        let everywhere = choices.len() - here;
        match (here, everywhere) {
            (0, 0) => "nothing".into(),
            (0, n) => format!("{n} found").into(),
            (h, 0) => format!("{h} here").into(),
            (h, n) => format!("{h} here · {n} elsewhere").into(),
        }
    }

    /// Whether the plate has anything to answer: a query was asked.
    pub(crate) const fn shows(&self) -> bool {
        !matches!(&self.draft, QueryDraft::Blank)
    }

    fn choices(&self, cx: &App) -> Vec<Choice> {
        let Some(query) = self.draft.query() else { return Vec::new() };
        let snapshot = self.links.snapshot(cx);
        let here = route_package(snapshot.committed_route()).map(str::to_owned);
        let Some((results, root, serving)) = self.search_resource(cx) else { return Vec::new() };
        let admission = admit_resource(&results, root, serving);
        let (page, unavailable) = match admission {
            ResourceAdmission::Current(page) => (page, None),
            ResourceAdmission::Retained { value, reason } => {
                let detail = if reason == ReadHoldReason::OwnerUnavailable {
                    self.owner_failure_words(cx)
                } else { None };
                (value, Some(detail.unwrap_or_else(|| format!(
                    "Earlier result; {}; cannot open until verified", read_hold_words(reason)).into())))
            }
            ResourceAdmission::Failed { retained: Some(value), terminal } =>
                (value, Some(failed_search_words(terminal, true))),
            ResourceAdmission::Pending(_) | ResourceAdmission::Failed { retained: None, .. } => return Vec::new(),
        };
        if page.query.as_ref() != query.text.as_ref() { return Vec::new(); }
        // Ask is the first-page preview. Find may append thousands of later
        // rows to this shared resource; they must not be rebuilt per frame.
        let mut rows: Vec<Choice> = page.rows.iter().take(usize::from(query.limit))
            .map(|row| result_choice(row, &query.text, here.as_deref(), unavailable.clone())).collect();
        // Stable: the producer's order within each group.
        rows.sort_by_key(|choice| choice.group == Group::Everywhere);
        let mut shown = Vec::with_capacity(rows.len());
        let (mut in_here, mut elsewhere) = (0, 0);
        for row in rows {
            let count = if row.group == Group::Here { &mut in_here } else { &mut elsewhere };
            if *count < PER_GROUP {
                *count += 1;
                shown.push(row);
            }
        }
        for index in 0..shown.len() {
            if let Some(route) = shown[index].route.clone() {
                shown[index].route_occurrence = shown[..index].iter().filter(|earlier| earlier.route.as_ref() == Some(&route)).count();
            }
        }
        shown
    }

    /// Reads the live resource and its exact owner authority together.
    fn search_resource(&self, cx: &App) -> Option<(Resource<SearchPage>, VersionedRoot, bool)> {
        let query = self.draft.query()?;
        let store = self.links.store.read(cx);
        Some((store.search(query), store.snapshot().key(), store.owner_serving()))
    }

    /// The same admission controls what the plate says and which links it
    /// offers. Older rows may remain visible, but cannot masquerade as live.
    fn read_status(&self, cx: &App) -> (Option<SharedString>, Option<backend_library::SemanticSearchStatus>) {
        if let Some(status) = self.draft.status() { return (Some(status.into()), None); }
        let Some((results, root, serving)) = self.search_resource(cx) else { return (None, None) };
        if serving && let Some(preparation) = results.query_preparation()
            && preparation.basis.matches(root.root()) {
            return (Some(format!("{}{}", preparation.words(),
                if results.loaded_value().is_some() { " Earlier results are read-only." } else { "" }).into()), None);
        }
        match admit_resource(&results, root, serving) {
            ResourceAdmission::Current(page) if self.draft.query().is_some_and(|query| page.query.as_ref() != query.text.as_ref()) =>
                (Some("Search replied for a different query; links unavailable.".into()), None),
            ResourceAdmission::Current(page) => (None, page.coverage.semantic_search_status()),
            ResourceAdmission::Retained { reason, .. } => {
                let detail = if reason == ReadHoldReason::OwnerUnavailable {
                    self.owner_failure_words(cx)
                } else { None };
                (Some(detail.unwrap_or_else(|| format!(
                    "Earlier search results · {} · links unavailable", read_hold_words(reason)).into())), None)
            }
            ResourceAdmission::Pending(reason) => {
                (Some(match reason {
                    ReadHoldReason::OwnerUnavailable => "Search is waiting for the index owner.".into(),
                    ReadHoldReason::AuthorityChanged => "Search is waiting for the current index.".into(),
                    ReadHoldReason::Reading | ReadHoldReason::NotReady => "Searching the library…".into(),
                }), None)
            }
            ResourceAdmission::Failed { terminal, retained } =>
                (Some(failed_search_words(terminal, retained.is_some())), None),
        }
    }
}

/// A focused Link keeps its exact destination if rows reorder between frames.
fn step_index(choices: &[Choice], focused: Option<(&Route, usize)>, selected: usize, walked: bool, delta: isize) -> usize {
    let origin = focused.and_then(|(route, occurrence)| choices.iter().position(|choice| {
        choice.route.as_ref() == Some(route) && choice.route_occurrence == occurrence
    })).or_else(|| walked.then_some(selected));
    origin.map_or(selected.min(choices.len() - 1), |index| index.saturating_add_signed(delta).min(choices.len() - 1))
}

fn read_hold_words(reason: ReadHoldReason) -> &'static str {
    match reason {
        ReadHoldReason::OwnerUnavailable => "index owner unavailable",
        ReadHoldReason::AuthorityChanged => "index changed",
        ReadHoldReason::Reading => "search still running",
        ReadHoldReason::NotReady => "search not ready",
    }
}

/// A retained row and the read status disclose the same typed terminal;
/// submitting that row must not replace the actionable failure detail.
fn failed_search_words(terminal: &ResourceTerminal, retained: bool) -> SharedString {
    let detail = match terminal {
        ResourceTerminal::Fault(error) => format!("Search failed: {}", error.message()),
        ResourceTerminal::Unavailable(_) => "The index does not provide search.".to_owned(),
        ResourceTerminal::Complete | ResourceTerminal::Partial => "Search is unavailable.".to_owned(),
    };
    format!("{detail}{}", if retained { " Earlier results are shown without links." } else { "" }).into()
}

/// The package a route reads, when it reads one.
fn route_package(route: &Route) -> Option<&str> {
    match route {
        Route::Package(route) => Some(route.package.as_str()),
        Route::Symbol(route) => Some(route.package.as_str()),
        Route::CargoSource(route) => Some(route.package.as_str()),
        Route::Orbit(_) | Route::World => None,
    }
}

fn result_choice(row: &SearchRow, query: &str, here: Option<&str>, hold: Option<SharedString>) -> Choice {
    let name = row.decl.name.to_string();
    let matched = matched_range(&name, query);
    let reason = match row.reason {
        MatchReason::ExactName | MatchReason::Name if matched.is_some() => None,
        MatchReason::ExactName | MatchReason::Name => None,
        MatchReason::Signature => Some("signature"),
        MatchReason::Docs => Some("docs"),
        MatchReason::Producer => None,
    };
    let package = row.decl.coordinate.package();
    let group = match (package.as_ref(), here) {
        (Some(package), Some(here)) if package.as_str() == here => Group::Here,
        _ => Group::Everywhere,
    };
    let place = row.package.as_ref().map_or_else(
        || row.decl.path.as_deref().unwrap_or_default().to_owned(),
        |package| {
            let name = crate::model::pages::PackageRef::parse(package).map_or_else(|_| package.to_string(), |package| package.display_name().to_owned());
            match (&row.decl.path, group) {
                // In the package you are reading, the package goes without saying.
                (Some(path), Group::Here) => path.clone().to_string(),
                (Some(path), Group::Everywhere) => format!("{name} · {path}"),
                (None, _) => name,
            }
        },
    );
    let route = hold.is_none().then(|| row_route(row)).flatten();
    let unavailable = if route.is_none() {
        Some(hold.unwrap_or_else(|| {
            if package.is_none() {
                format!("{} has no page yet", name).into()
            } else {
                "This result has no verified page".into()
            }
        }))
    } else { None };
    Choice {
        name: name.into(),
        matched,
        place: place.into(),
        reason: reason.map(SharedString::from),
        kind: kind_of(row.decl.kind),
        route,
        route_occurrence: 0,
        unavailable,
        group,
    }
}

/// The producer's package claim and coordinate must name the same exact
/// destination before a live row may advertise navigation.
fn row_route(row: &SearchRow) -> Option<Route> {
    search_result_route(row, crate::navigation::View::Page)
}

/// Recheck the live owner at the input event, not only when the Link painted.
/// A replacement owner may arrive between those two UI turns.
fn current_search(links: &Links, query: &SearchQuery, cx: &App, accepts: impl FnOnce(&SearchPage) -> bool) -> bool {
    let store = links.store.read(cx);
    let results = store.search(query);
    admit_resource(&results, store.snapshot().key(), store.owner_serving())
        .current_value().is_some_and(|page| page.query.as_ref() == query.text.as_ref() && accepts(page))
}

/// A painted Link cannot outlive the editor query that produced it.
fn current_input_query(input: &Entity<InputState>, query: &SearchQuery, cx: &App) -> bool {
    SearchQuery::new(&input.read(cx).value().to_string(), SearchQuery::DEFAULT_LIMIT)
        .ok().as_ref() == Some(query)
}

fn stale_submission() -> SubmissionOutcome {
    SubmissionOutcome::Refused(SubmitRefusal::Unavailable(
        "That search result is no longer verified by the current index. Search again.".into()))
}

fn follow_row(links: &Links, input: &Entity<InputState>, query: &SearchQuery, route: &Route,
    revision: u64, current_revision: &Cell<u64>, cx: &App) -> SubmissionOutcome {
    if current_revision.get() != revision || links.snapshot(cx).overlay() != Some(Overlay::CommandPalette)
        || !current_input_query(input, query, cx) {
        return SubmissionOutcome::Superseded;
    }
    if current_row_route(links, query, route, cx) { SubmissionOutcome::Navigate(route.clone()) }
    else { stale_submission() }
}

fn current_row_route(links: &Links, query: &SearchQuery, route: &Route, cx: &App) -> bool {
    current_search(links, query, cx, |page| page.rows.iter().take(usize::from(query.limit))
        .any(|row| row_route(row).as_ref() == Some(route)))
}

fn current_press_target(links: &Links, target: &SearchPressTarget, cx: &App) -> bool {
    match &target.destination {
        SearchPressDestination::Row(route) => current_row_route(links, &target.query, route, cx),
        SearchPressDestination::AllResults => current_search(links, &target.query, cx, |_| true),
    }
}

fn follow_all(links: &Links, input: &Entity<InputState>, query: &SearchQuery, route: &Route,
    revision: u64, current_revision: &Cell<u64>, cx: &App) -> SubmissionOutcome {
    if current_revision.get() != revision || links.snapshot(cx).overlay() != Some(Overlay::CommandPalette)
        || !current_input_query(input, query, cx) {
        return SubmissionOutcome::Superseded;
    }
    if current_search(links, query, cx, |_| true) { SubmissionOutcome::Navigate(route.clone()) }
    else { stale_submission() }
}

/// Where `query` sits in `name`, ignoring case: a whole-word query first,
/// else its first run of letters (a question's words don't underline).
fn matched_range(name: &str, query: &str) -> Option<std::ops::Range<usize>> {
    let needle = query.trim();
    if needle.is_empty() || needle.contains(char::is_whitespace) {
        return None;
    }
    if name.is_ascii() && needle.is_ascii() {
        let folded_needle = needle.to_ascii_lowercase();
        let at = name.to_ascii_lowercase().find(folded_needle.as_str())?;
        return Some(at..at + needle.len());
    }

    // Fold each source char once, recording only boundaries that also end
    // whole original chars. A fold such as İ -> i + ◌̇ has interior UTF-8
    // boundaries that must never become an original-name slice offset.
    let folded_needle: String = needle.chars().flat_map(char::to_lowercase).collect();
    if folded_needle.is_empty() { return None; }
    let mut folded = String::with_capacity(name.len());
    let mut original_at = vec![Some(0)];
    for (start, character) in name.char_indices() {
        folded.extend(character.to_lowercase());
        original_at.resize(folded.len() + 1, None);
        original_at[folded.len()] = Some(start + character.len_utf8());
    }

    // KMP visits every folded byte at most twice, including overlapping
    // matches. Skipping an interior match must not hide a later whole-char
    // match ("İi" searched for "i" is one example).
    let pattern = folded_needle.as_bytes();
    let mut fallback = vec![0; pattern.len()];
    let mut matched = 0;
    for at in 1..pattern.len() {
        while matched > 0 && pattern[at] != pattern[matched] {
            matched = fallback[matched - 1];
        }
        if pattern[at] == pattern[matched] { matched += 1; }
        fallback[at] = matched;
    }
    matched = 0;
    for (at, byte) in folded.bytes().enumerate() {
        while matched > 0 && byte != pattern[matched] {
            matched = fallback[matched - 1];
        }
        if byte == pattern[matched] { matched += 1; }
        if matched == pattern.len() {
            let end = at + 1;
            let start = end - pattern.len();
            if let (Some(from), Some(to)) = (original_at[start], original_at[end]) {
                return Some(from..to);
            }
            matched = fallback[matched - 1];
        }
    }
    None
}

fn semantic_search_label(status: backend_library::SemanticSearchStatus) -> String {
    use backend_library::{SemanticSearchReason, SemanticSearchStatus};
    let reason = |reason| match reason {
        SemanticSearchReason::Unconfigured => "unconfigured",
        SemanticSearchReason::InvalidConfiguration => "invalid configuration",
        SemanticSearchReason::ProviderUnavailable => "provider unavailable",
        SemanticSearchReason::NoActiveProjection => "no active projection",
        SemanticSearchReason::StaleProjection => "stale projection",
        SemanticSearchReason::ModelUnavailable => "embedding model unavailable",
    };
    match status {
        SemanticSearchStatus::Available => "semantic search available".to_owned(),
        SemanticSearchStatus::Unavailable { reason: why } => {
            format!("semantic search unavailable · {}", reason(why))
        }
        SemanticSearchStatus::Stale { reason: why } => {
            format!("semantic search stale · {}", reason(why))
        }
    }
}

impl Render for Ask {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Ask's words are its own region: its plate covers the shelf's (and
        // part of the page's), whose words under it are not on screen and
        // are neither its neighbours nor linted (the harness reads a
        // dialog's region from its stack key, `ask-…`).
        facet::probe::region("ask", || self.draw(window, cx))
    }
}

impl Ask {
    fn draw(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        self.renders = self.renders.saturating_add(1);
        let facet = cx.facet();
        let palette = facet.palette();
        let viewport = window.viewport_size();
        let measure = Measure::new(viewport.width, &facet);
        let choices = self.choices(cx);
        let all_results = self.all_results(cx).filter(|_| !choices.is_empty());
        self.sync_row_focus(&choices, all_results.is_some(), window, cx);
        let (read_status, semantic_status) = self.read_status(cx);
        let read_status = self.refusal.as_ref().map(SubmitRefusal::words).or(read_status);
        let preparation_check = self.draft.query().and_then(|query| {
            let key = PageKey::Search(query.clone());
            self.links.store.read(cx).preparation_token(&key).map(|token| (query.clone(), key, token))
        });
        let scroll_map = ResultScrollMap::new(&choices, semantic_status.is_some(), all_results.is_some());
        self.apply_result_scroll(&choices, &scroll_map, cx);
        let mut list = div().id("ask-results").role(Role::List).aria_label("Search results")
            .flex().flex_col().pt(measure.space(Space::Tight))
            .flex_1().min_h_0().overflow_y_scroll().track_scroll(&self.scroll);
        if let Some((query, key, token)) = preparation_check {
            let weak = cx.weak_entity();
            let revision = self.revision.get();
            let control = div().id("ask-check-preparation")
                .role(Role::Button).aria_label("Check again")
                .border_2().border_color(if self.preparation_focus.is_focused(window) { palette.peri.base.hsla() } else { palette.line2.hsla() })
                .px(measure.space(Space::Gutter)).py(measure.space(Space::Snug))
                .child(text(ty::MONO_SMALL, &measure, palette.ink1).child("Check again"));
            list = list.child(facet::controls::button::native_button(control, &self.preparation_focus,
                move |_, cx| {
                    let _ = weak.update(cx, |ask, cx| {
                        if ask.revision.get() != revision
                            || ask.links.snapshot(cx).overlay() != Some(Overlay::CommandPalette)
                            || ask.draft.query() != Some(&query)
                            || !current_input_query(&ask.input, &query, cx) { return; }
                        ask.links.store.update(cx, |store, cx| store.check_preparation(key.clone(), &token, cx));
                    });
                }));
        }
        if let Some(status) = semantic_status {
            let status_words = semantic_search_label(status);
            list = list.child(
                div()
                    .id("ask-search-status")
                    .role(Role::Status)
                    .aria_label(status_words.clone())
                    .px(measure.space(Space::Gutter))
                    .py(measure.space(Space::Snug))
                    .child(text(ty::MONO_SMALL, &measure, palette.ink3).child(status_words)),
            );
        }
        let mut group = None;
        for (index, choice) in choices.iter().enumerate() {
            if group != Some(choice.group) {
                group = Some(choice.group);
                let (words, count) = match choice.group {
                    Group::Here => (
                        route_package(self.links.snapshot(cx).committed_route())
                            .and_then(|package| crate::model::pages::PackageRef::parse(package).ok())
                            .map_or_else(|| "here".to_owned(), |package| format!("in {}", package.display_name())),
                        choices.iter().filter(|c| c.group == Group::Here).count(),
                    ),
                    Group::Everywhere => ("everywhere".to_owned(), choices.iter().filter(|c| c.group == Group::Everywhere).count()),
                };
                list = list.child(group_head(words, count, &measure, palette));
            }
            list = list.child(self.row(index, choice, &measure, palette, cx));
        }
        if choices.is_empty() && self.draft.query().is_some() && read_status.is_none() {
            // Never an empty plate: what the search is doing, or why it
            // could not answer.
            let words: SharedString = "Nothing matches that yet.".into();
            list = list.child(div().id("ask-said").px(measure.space(Space::Gutter)).py(measure.space(Space::Roomy)).child(super::kit::quiet(words, &measure, palette)));
        }
        if let Some(route) = all_results {
            let links = self.links.clone();
            let input = self.input.clone();
            let query = self.draft.query().cloned().expect("an all-results route has a query");
            let revision = self.revision.get();
            let current_revision = self.revision.clone();
            list = list.child(
                native_search_control(div().id("ask-find-page").flex().flex_none().items_center().gap(measure.space(Space::Roomy))
                    .role(Role::Link).aria_label("Open search results")
                    .h(measure.row() + measure.space(Space::Snug)).px(measure.space(Space::Gutter)).mt(measure.space(Space::Tight))
                    .border_t_1().border_color(palette.line1.hsla())
                    .hover(|style| style.bg(palette.tint)).focus_visible(|style| style.bg(palette.tint)).cursor_pointer()
                    .child(text(ty::SMALL, &measure, palette.ink2).child("Open search results")),
                    &self.all_focus, cx.weak_entity(),
                    SearchPressTarget {
                        query: query.clone(),
                        revision,
                        destination: SearchPressDestination::AllResults,
                    }, move |cx| {
                        follow_all(&links, &input, &query, &route, revision, &current_revision, cx)
                    }),
            );
        }
        div()
            .id("ask")
            .on_mouse_up(gpui::MouseButton::Left, cx.listener(|ask, _, window, cx| {
                ask.refuse_revoked_press(window, cx);
                ask.pressed_result = None;
            }))
            .on_mouse_up_out(gpui::MouseButton::Left, cx.listener(|ask, _, _, _| {
                ask.pressed_result = None;
            }))
            .size_full()
            .flex().flex_col()
            .bg(palette.g2)
            .border_r_1()
            .border_color(palette.line2.hsla())
            // A refusal belongs to this query, but must not move the selected
            // result under the pointer. Reserve one scaled status line from
            // the first results frame; long words retain their full native name.
            .child(div().id("ask-status-slot").flex_none()
                .h(px(measure.role(ty::MONO_SMALL).line) + measure.space(Space::Snug) * 2.0)
                .px(measure.space(Space::Gutter)).py(measure.space(Space::Snug))
                .overflow_hidden()
                .children(read_status.map(|words| {
                    div().id("ask-read-status").role(Role::Status).aria_label(words.clone())
                        .w_full().overflow_hidden().whitespace_nowrap().text_ellipsis()
                        .child(text(ty::MONO_SMALL, &measure, palette.ink3).child(words))
                })))
            .child(list)
            .into_any_element()
    }

    fn row(&self, index: usize, choice: &Choice, measure: &Measure, palette: &facet::Palette, cx: &Context<Self>) -> AnyElement {
        let on = index == self.selected && (self.walked || index == 0);
        let has_place = choice.route.is_some();
        let ink = if on { palette.ink0.hsla() } else { super::kit::link_ink(has_place, palette) };
        let name = &choice.name;
        let mut words = div().flex().items_baseline().min_w(px(0.0)).flex_none();
        match choice.matched.clone() {
            Some(range) => {
                let (before, hit, after) = (&name[..range.start], &name[range.clone()], &name[range.end..]);
                if !before.is_empty() {
                    words = words.child(text(ty::MONO_ROW, measure, ink).child(before.to_owned()));
                }
                words = words.child(
                    text(ty::MONO_ROW, measure, palette.ink0)
                        .border_b(px(1.5 * measure.scale()))
                        .border_color(palette.peri.base.hsla())
                        .child(hit.to_owned()),
                );
                if !after.is_empty() {
                    words = words.child(text(ty::MONO_ROW, measure, ink).child(after.to_owned()));
                }
            }
            None => words = words.child(text(ty::MONO_ROW, measure, ink).child(name.clone())),
        }
        let id: SharedString = choice.route.as_ref().map_or_else(
            || format!("ask-unavailable-{index}"),
            |route| format!("ask-row-{route:?}-{}", choice.route_occurrence),
        ).into();
        let mut row = div()
            .id(id)
            .debug_selector(|| "ask-result-row".to_owned())
            .relative()
            .flex()
            .items_center()
            .gap(measure.space(Space::Roomy))
            .h(measure.row() + measure.space(Space::Snug))
            .px(measure.space(Space::Gutter))
            .child(super::kit::kind_mark(choice.kind, KindSize::Sm, measure, palette))
            .child(words)
            .children(choice.reason.clone().map(|reason| text(ty::MONO_SMALL, measure, palette.ink3).flex_none().child(reason)))
            .child(
                text(ty::MONO_SMALL, measure, palette.ink3)
                    .flex_1()
                    .min_w(px(0.0))
                    .text_right()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(choice.place.clone()),
            );
        let place: &str = choice.place.as_ref();
        let accessible_name = if place.is_empty() {
            format!("Result {}: Open {}", index + 1, choice.name)
        } else {
            format!("Result {}: Open {}, {}", index + 1, choice.name, place)
        };
        row = row.role(if has_place { Role::Link } else { Role::Label })
            .aria_label(if has_place { accessible_name } else {
                format!("Result {}: {}{}, {}", index + 1, choice.name,
                    if place.is_empty() { String::new() } else { format!(", {place}") },
                    choice.unavailable.as_ref().map_or("page unavailable", |words| words.as_ref()))
            });
        if on {
            row = row.bg(palette.plate2).child(
                div().absolute().left_0().top_0().bottom_0().w(px(2.0 * measure.scale())).bg(palette.peri.base.hsla()),
            );
        }
        if let Some(route) = choice.route.clone() {
            let links = self.links.clone();
            let input = self.input.clone();
            let query = self.draft.query().cloned().expect("a result route has a query");
            let revision = self.revision.get();
            let current_revision = self.revision.clone();
            let handle = self.row_focus.iter().find(|(known, occurrence, _)| known == &route && *occurrence == choice.route_occurrence)
                .map(|(_, _, handle)| handle).expect("every mounted route has a focus handle");
            row = native_search_control(row.focus_visible(|style| style.bg(palette.tint))
                .cursor_pointer().hover(|style| style.bg(palette.tint)), handle, cx.weak_entity(),
                SearchPressTarget {
                    query: query.clone(),
                    revision,
                    destination: SearchPressDestination::Row(route.clone()),
                }, move |cx| {
                    follow_row(&links, &input, &query, &route, revision, &current_revision, cx)
                });
        } else {
            let weak = cx.weak_entity();
            let revision = self.revision.get();
            let input = self.input.clone();
            let query = self.draft.query().cloned();
            let refusal = choice.unavailable.clone().unwrap_or_else(|| format!("{} has no page yet", choice.name).into());
            row = row.on_click(move |_, _, cx| {
                let _ = weak.update(cx, |ask, cx| {
                    if ask.revision.get() == revision && ask.links.snapshot(cx).overlay() == Some(Overlay::CommandPalette)
                        && query.as_ref().is_some_and(|query| current_input_query(&input, query, cx)) {
                        ask.complete_submission(SubmissionOutcome::Refused(SubmitRefusal::NoDestination(refusal.clone())), cx);
                    }
                });
            });
        }
        row.into_any_element()
    }
}

fn native_search_control(
    element: gpui::Stateful<gpui::Div>,
    focus: &FocusHandle,
    owner: gpui::WeakEntity<Ask>,
    press: SearchPressTarget,
    admission: impl Fn(&App) -> SubmissionOutcome + 'static,
) -> gpui::Stateful<gpui::Div> {
    let press_owner = owner.clone();
    let element = element.on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| {
        let _ = press_owner.update(cx, |ask, cx| ask.capture_native_press(&press, cx));
    });
    facet::controls::button::native_button_with_event(element, focus, move |event, window, cx| {
        let _ = owner.update(cx, |ask, cx| {
            if matches!(event, gpui::ClickEvent::Mouse(_)) {
                // Child clicks run before Ask's mouse-up listener. Consume the
                // original press receipt before a replacement owner's same-route
                // result can admit navigation and erase its sticky revocation.
                if ask.refuse_revoked_press(window, cx) {
                    return;
                }
            }
            let outcome = admission(cx);
            ask.complete_submission(outcome, cx);
        });
    })
}

fn group_head(words: String, count: usize, measure: &Measure, palette: &facet::Palette) -> AnyElement {
    div()
        .id(SharedString::from(format!("ask-group-{words}")))
        .role(Role::Heading)
        .aria_label(format!("{words}, {count} results"))
        .flex()
        .items_center()
        .gap(measure.space(Space::Base))
        .h(measure.row())
        .px(measure.space(Space::Gutter))
        .mt(measure.space(Space::Tight))
        .child(text(ty::LABEL, measure, palette.ink3).flex_none().child(words))
        .child(div().flex_1().h(px(1.0)).bg(palette.line1.hsla()))
        .child(text(ty::MONO_SMALL, measure, palette.ink3).flex_none().child(count.to_string()))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{Choice, Group, QueryDraft, QueryReject, SubmitRefusal, current_input_query, matched_range, step_index};
    use gpui_component::input::InputEvent;
    use crate::core::{LocalProjectId, VersionedRoot};
    use crate::model::{AppSnapshot, SessionState};
    use crate::model::pages::{DeclRef, Gap, GapReason, Known, MatchReason, PageValue, ReadFailure, SearchPage, SearchRow};
    use crate::runtime::actor::EngineActor;
    use crate::runtime::reads::{PageReader, ReadContext, ReadPool, ReadRequest};
    use crate::runtime::{DesktopRuntime, UiEntityGraph};
    use crate::shell::root::Shell;
    use crate::shell::tests::{Fixture, Rig, RootOnly, page_route, rig_with_reads};
    use backend_library::DeclarationKind;
    use gpui::{AppContext as _, Focusable as _, Modifiers, TestAppContext, VisualTestContext, point, px};
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[test]
    fn query_draft_distinguishes_blank_invalid_and_valid_unicode() {
        assert_eq!(QueryDraft::parse(" \u{2003} "), QueryDraft::Blank);
        let controlled = QueryDraft::parse("a\nthing");
        assert_eq!(controlled, QueryDraft::Rejected(QueryReject::Invalid(
            crate::model::pages::KeyError::ControlCharacter,
        )));
        assert_eq!(controlled.status(), Some("Search query contains a control character."));
        let at_limit = "🧭".repeat(256);
        let admitted = QueryDraft::parse(&at_limit);
        assert_eq!(admitted.query().map(|query| query.text.as_ref()),
            Some(at_limit.as_str()));
        assert_eq!(QueryDraft::parse(&"🧭".repeat(257)), QueryDraft::Rejected(QueryReject::TooLong));
        assert!(QueryDraft::parse(&"🧭".repeat(257)).status().is_some_and(|status| status.len() < 80));
    }

    #[test]
    fn matched_name_ranges_are_original_utf8_boundaries() {
        for (name, query, expected) in [
            ("RelationLabel", "label", Some(8..13)),
            ("AİẞZ", "ß", Some(3..6)),
            ("İtem", "i\u{307}", Some(0..2)),
            ("İtem", "i", None),
            ("İi", "i", Some(2..3)),
            ("aéx", "É", Some(1..3)),
            ("AİẞZ", "i\u{307}ß", Some(1..6)),
        ] {
            let actual = matched_range(name, query);
            assert_eq!(actual, expected, "{name:?} / {query:?}");
            if let Some(range) = actual {
                assert!(name.is_char_boundary(range.start) && name.is_char_boundary(range.end));
                let _ = &name[range];
            }
        }
        let long_name = format!("{}ß", "İ".repeat(4096));
        assert_eq!(matched_range(&long_name, "ß"), Some(8192..8194));
    }

    #[gpui::test]
    fn invalid_draft_retracts_preview_and_old_callbacks_even_after_same_query_returns(cx: &mut TestAppContext) {
        let mut rig = rig_with_reads(cx, Some(page_route("RelationDirection")), 1440.0, 900.0,
            ReadPool::start(2, |_| Fixture).expect("fixture pool"));
        let committed = rig.route();
        rig.keys("secondary-k");
        let ask = rig.shell.read_with(rig.cx, |shell, _| shell.ask_entity());
        let input = ask.read_with(rig.cx, |ask, _| ask.input().clone());
        rig.cx.update(|window, cx| input.update(cx, |input, cx| input.replace_all("RelationLabel", window, cx)));
        rig.frame(120);
        rig.settle();
        let (links, query, route, all_route, old_revision, current_revision) = ask.read_with(rig.cx, |ask, cx| {
            let choice = ask.choices(cx).into_iter().next().expect("a current routed result");
            (ask.links.clone(), ask.draft.query().cloned().expect("valid query"),
                choice.route.expect("verified route"), ask.all_results(cx).expect("served Find route"),
                ask.revision.get(), ask.revision.clone())
        });
        rig.cx.update(|window, cx| ask.update(cx, |ask, cx| ask.step(1, window, cx)));
        rig.settle();
        assert_ne!(rig.route(), committed, "walking previews a different page");

        let oversized = "🧭".repeat(257);
        rig.cx.update(|window, cx| input.update(cx, |input, cx| input.replace_all(&oversized, window, cx)));
        rig.settle();
        assert_eq!(rig.route(), committed, "rejection ends the prior preview");
        assert!(ask.read_with(rig.cx, |ask, cx| {
            matches!(&ask.draft, QueryDraft::Rejected(QueryReject::TooLong))
                && ask.pending.is_none() && ask.shows() && ask.choices(cx).is_empty()
                && ask.all_results(cx).is_none()
                && ask.read_status(cx).0.as_deref() == Some("Search query is too long (maximum 256 characters).")
        }));
        rig.cx.update(|_, cx| facet::probe::enable(cx));
        rig.repaint();
        let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
        assert!(ledger.texts.iter().any(|text| text.region.as_deref() == Some("ask")
            && text.content == "Search query is too long (maximum 256 characters)."));
        rig.cx.update(|window, cx| ask.update(cx, |ask, cx| ask.choose(window, cx)));
        rig.cx.update(|_, cx| {
            let outcome = super::follow_row(&links, &input, &query, &route, old_revision, &current_revision, cx);
            ask.update(cx, |ask, cx| ask.complete_submission(outcome, cx));
        });
        rig.cx.update(|_, cx| {
            let outcome = super::follow_all(&links, &input, &query, &all_route, old_revision, &current_revision, cx);
            ask.update(cx, |ask, cx| ask.complete_submission(outcome, cx));
        });
        assert_eq!(rig.route(), committed, "rejected input and a retained row cannot navigate");
        assert_eq!(rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().overlay()),
            Some(crate::navigation::Overlay::CommandPalette), "rejection stays in the editor");

        rig.cx.update(|window, cx| input.update(cx, |input, cx| input.replace_all("RelationLabel", window, cx)));
        rig.frame(120);
        rig.settle();
        assert!(!ask.read_with(rig.cx, |ask, cx| ask.choices(cx).is_empty()));
        rig.cx.update(|_, cx| {
            let outcome = super::follow_row(&links, &input, &query, &route, old_revision, &current_revision, cx);
            ask.update(cx, |ask, cx| ask.complete_submission(outcome, cx));
        });
        rig.cx.update(|_, cx| {
            let outcome = super::follow_all(&links, &input, &query, &all_route, old_revision, &current_revision, cx);
            ask.update(cx, |ask, cx| ask.complete_submission(outcome, cx));
        });
        assert_eq!(rig.route(), committed, "a callback from the old draft stays inert after the same query returns");
        assert_eq!(rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().overlay()),
            Some(crate::navigation::Overlay::CommandPalette));
    }

    #[gpui::test]
    fn rapid_blank_invalid_valid_typing_only_requests_the_current_query(cx: &mut TestAppContext) {
        let mut rig = rig_with_reads(cx, None, 1440.0, 900.0,
            ReadPool::start(2, |_| Fixture).expect("fixture pool"));
        rig.keys("secondary-k");
        let ask = rig.shell.read_with(rig.cx, |shell, _| shell.ask_entity());
        let input = ask.read_with(rig.cx, |ask, _| ask.input().clone());
        let oversized = "🧭".repeat(257);
        for value in ["  ", "NotIssued", oversized.as_str(), "RelationLabel"] {
            rig.cx.update(|window, cx| input.update(cx, |input, cx| input.replace_all(value, window, cx)));
        }
        assert_eq!(ask.read_with(rig.cx, |ask, _| ask.draft.query().map(|query| query.text.to_string())),
            Some("RelationLabel".to_owned()));
        rig.frame(120);
        rig.settle();
        let never_issued = crate::model::pages::SearchQuery::new("NotIssued", 50).expect("query");
        assert_eq!(rig.graph.store.read_with(rig.cx, |store, _| store.search(&never_issued).activity()),
            crate::core::Activity::NotYet, "superseded debounce never submitted a read");
        assert!(!ask.read_with(rig.cx, |ask, cx| ask.choices(cx).is_empty()));
    }

    #[test]
    fn arrow_origin_follows_focused_route_after_results_reorder() {
        let a = page_route("A");
        let b = page_route("B");
        let c = page_route("C");
        let choice = |route, occurrence| Choice {
            name: "result".into(),
            matched: None,
            place: "fixture".into(),
            reason: None,
            kind: facet::icons::Kind::Enum,
            route: Some(route),
            route_occurrence: occurrence,
            unavailable: None,
            group: Group::Everywhere,
        };
        // B held native focus at index 1 in the preceding frame; its current
        // position is 0, so Down must land on A rather than skipping to C.
        let reordered = [choice(b.clone(), 0), choice(a.clone(), 0), choice(c, 0)];
        assert_eq!(step_index(&reordered, Some((&b, 0)), 1, true, 1), 1);
        // Repeated exact routes are separate stops, identified by occurrence.
        let repeated = [choice(a.clone(), 1), choice(b, 0), choice(a.clone(), 0)];
        assert_eq!(step_index(&repeated, Some((&a, 0)), 0, true, -1), 1);
        assert_eq!(step_index(&reordered, None, 0, false, 1), 0, "Down from the editor previews the first result");
    }

    /// Presses actual GPUI actions through the component window root, whose
    /// general Tab action used to send Ask's editor into the Inbox button.
    #[gpui::test]
    fn component_root_tab_cycles_editor_link_and_all_results_inside_ask(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let root = VersionedRoot::synthetic(
            backend_library::view_state_root(&[("shell".to_owned(), "ask-tab".to_owned())]), 4,
        );
        cx.update(|cx| {
            gpui_component::init(cx);
            let _ = facet::fonts::install(cx);
            crate::shell::bodies::graph::install_test_fixture(root, cx);
        });
        let mut snapshot = AppSnapshot::empty(root);
        let folder = std::env::temp_dir().join(format!("nudox-ask-tab-{}", std::process::id()));
        std::fs::create_dir_all(&folder).expect("fixture root");
        let mut workspace = snapshot.workspace().clone();
        workspace.host = LocalProjectId::from_path(&folder).ok();
        snapshot = snapshot.with_workspace(workspace).with_session(SessionState::default());
        let actor = EngineActor::start(RootOnly, 8).expect("owner actor");
        let runtime = DesktopRuntime::new(snapshot, actor);
        let pool = ReadPool::start(2, |_| Fixture).expect("fixture reader");
        let graph = cx.update(|cx| UiEntityGraph::install_with_reads(cx, runtime, None, Some(pool)));
        let window_graph = UiEntityGraph { root: graph.root.clone(), store: graph.store.clone() };
        let shell_slot = Rc::new(RefCell::new(None));
        let capture_shell = shell_slot.clone();
        let window = cx.update(|cx| {
            cx.bind_keys(crate::shell::keys::bindings());
            cx.open_window(gpui::WindowOptions {
                window_bounds: Some(gpui::WindowBounds::Windowed(gpui::Bounds::new(
                    gpui::point(gpui::px(0.0), gpui::px(0.0)),
                    gpui::size(gpui::px(1440.0), gpui::px(900.0)),
                ))),
                ..gpui::WindowOptions::default()
            }, move |window, cx| {
                let shell = cx.new(|cx| Shell::new(&window_graph, window, cx));
                capture_shell.borrow_mut().replace(shell.clone());
                cx.new(|cx| gpui_component::Root::new(shell, window, cx).bordered(false))
            }).expect("component-rooted shell window")
        });
        let shell = shell_slot.borrow_mut().take().expect("mounted shell");
        let visual = VisualTestContext::from_window(window.into(), cx).into_mut();
        visual.update(|window, cx| {
            window.activate_window();
            window.set_a11y_forced(true);
            facet::probe::enable(cx);
        });
        let draw = |visual: &mut VisualTestContext| {
            visual.update(|window, cx| {
                window.simulate_reconciled_next_frame(cx);
                window.refresh();
                window.draw(cx).clear(cx);
            });
            visual.run_until_parked();
        };
        draw(visual);
        visual.simulate_keystrokes("secondary-k");
        draw(visual);
        let ask = shell.read_with(visual, |shell, _| shell.ask_entity());
        let input = ask.read_with(visual, |ask, _| ask.input().clone());
        visual.update(|window, cx| input.update(cx, |input, cx| input.replace_all("RelationLabel", window, cx)));
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            visual.executor().advance_clock(Duration::from_millis(16));
            draw(visual);
            let result_label = ask.read_with(visual, |ask, cx| {
                ask.choices(cx).first().filter(|choice| choice.route.is_some()).map(|choice| {
                    if choice.place.is_empty() {
                        format!("Result 1: Open {}", choice.name)
                    } else {
                        format!("Result 1: Open {}, {}", choice.name, choice.place)
                    }
                })
            });
            let ledger = visual.update(|_, cx| facet::probe::take(cx));
            let phase = ledger.stacks.iter().rev().flat_map(|stack| stack.entries.iter())
                .find(|entry| entry.key == "ask-plate").map(|entry| entry.phase);
            let tree = visual.update(|window, _| {
                let json = window.debug_a11y_tree_json().expect("forced native Ask tree");
                serde_json::from_str::<serde_json::Value>(&json).expect("native tree JSON")
            });
            let actionable_link = |label: &str| {
                tree["nodes"].as_object().expect("native nodes").values().any(|node| {
                    node["aria"]["label"].as_str() == Some(label)
                        && node["aria"]["role"].as_str() == Some("Link")
                        && node["aria"]["on_action"].as_array().is_some_and(|actions| {
                            actions.iter().any(|action| action.as_str() == Some("Click"))
                        })
                })
            };
            let result_live = result_label.as_deref().is_some_and(|label| actionable_link(label));
            let all_live = actionable_link("Open search results");
            if phase == Some(facet::probe::StackPhase::Open) && result_live && all_live { break; }
            assert!(Instant::now() < deadline,
                "Ask never exposed its settled native stops: phase={phase:?}, result={result_label:?}, result_live={result_live}, all_live={all_live}");
            std::thread::yield_now();
        }
        assert_eq!(ask.read_with(visual, |ask, cx| ask.choices(cx).len()), 1, "fixture result is mounted");
        let focused = |visual: &mut VisualTestContext| visual.update(|window, cx| {
            let ask = ask.read(cx);
            if ask.input.read(cx).focus_handle(cx).is_focused(window) { "editor" }
            else if ask.row_focus.iter().any(|(_, _, handle)| handle.is_focused(window)) { "link" }
            else if ask.all_focus.is_focused(window) { "all" }
            else { "outside" }
        });
        assert_eq!(focused(visual), "editor");
        for (stroke, expected) in [
            ("tab", "link"), ("shift-tab", "editor"), ("shift-tab", "all"),
            ("tab", "editor"), ("tab", "link"), ("tab", "all"),
        ] {
            visual.simulate_keystrokes(stroke);
            draw(visual);
            assert_eq!(focused(visual), expected, "{stroke} stays in Ask's mounted stops");
            assert_eq!(graph.store.read_with(visual, |store, _| store.snapshot().overlay()),
                Some(crate::navigation::Overlay::CommandPalette));
        }
    }

    /// The jump-bar button is the real pointer entry, unlike the ⌘K tests.
    /// Verify the editor owns the mounted text handler before asking the
    /// platform to dispatch a character, including when the owner is absent.
    #[gpui::test]
    fn native_ask_button_gives_the_first_character_to_its_editor(cx: &mut TestAppContext) {
        let mut rig = rig_with_reads(cx, Some(page_route("RelationLabel")), 1440.0, 900.0,
            ReadPool::start(1, |_| Fixture).expect("search reader"));
        let shell = rig.shell.clone();
        rig.cx.update(|window, cx| {
            window.replace_root(cx, |window, cx| gpui_component::Root::new(shell, window, cx).bordered(false));
            window.set_a11y_forced(true);
            facet::probe::enable(cx);
        });
        rig.settle();

        // Keep the Hand's raw key session open under Ask. The
        // first character tests native text delivery; Backspace tests that
        // the covered Hand cannot let go of the saved card instead.
        rig.keys("secondary-d");
        let held = |rig: &mut Rig| rig.graph.store.read_with(rig.cx, |store, _|
            store.snapshot().session().hand.held().len());
        assert_eq!(held(&mut rig), 1, "fixture page is held");
        rig.keys("h");
        assert!(rig.shell.read_with(rig.cx, |shell, cx| shell.chrome_words(cx))
            .contains(&("hand", "open".to_owned())), "Hand is open behind Ask");

        let open_from_button = |rig: &mut Rig| {
            let ledger = crate::shell::anatomy_tests::painted(rig);
            let button = ledger.targets.iter().find(|target| target.key == "here")
                .expect("painted titlebar Ask button");
            assert!(button.state.clickable && button.paint_clip.is_some());
            let at = point(px(button.bounds.x + button.bounds.width / 2.0),
                px(button.bounds.y + button.bounds.height / 2.0));
            rig.cx.simulate_mouse_move(at, None, Modifiers::none());
            rig.draw();
            rig.cx.simulate_click(at, Modifiers::none());
            rig.settle();
            let ask = rig.shell.read_with(rig.cx, |shell, _| shell.ask_entity());
            let input = ask.read_with(rig.cx, |ask, _| ask.input().clone());
            assert_eq!(rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().overlay()),
                Some(crate::navigation::Overlay::CommandPalette));
            assert!(rig.cx.update(|window, cx| input.read(cx).focus_handle(cx).is_focused(window)),
                "the mounted Ask editor owns native focus after the button click");
            let tree: serde_json::Value = rig.cx.update(|window, _| serde_json::from_str(
                &window.debug_a11y_tree_json().expect("forced Ask native tree"))
                .expect("native tree JSON"));
            let focused = tree["accesskit_focus"].as_str().expect("focused native Ask node");
            assert_eq!(tree["nodes"][focused]["aria"]["role"].as_str(), Some("TextInput"));
            assert_eq!(tree["nodes"][focused]["aria"]["label"].as_str(),
                Some("Ask anything, or find a package"));
            let shell = rig.shell.clone();
            let keyboard = rig.cx.update(|window, cx| shell.read(cx).keyboard_diagnostic(window, cx));
            assert_eq!(keyboard.focus_owner, "ask-editor");
            assert!(keyboard.target_mounted && keyboard.input_handler_present,
                "the rendered editor must install its native text handler: {keyboard:?}");
            assert!(!keyboard.pending_claim, "one post-paint keyboard handoff completed");
            (ask, input)
        };

        let (ask, input) = open_from_button(&mut rig);
        assert!(rig.shell.read_with(rig.cx, |shell, cx| shell.chrome_words(cx))
            .contains(&("hand", "open".to_owned())), "Ask covers an open Hand");
        rig.keys("f");
        assert_eq!(input.read_with(rig.cx, |input, _| input.value().to_string()), "f",
            "one platform key reaches the editor without a second click");
        assert!(matches!(ask.read_with(rig.cx, |ask, _| ask.draft.clone()), QueryDraft::Valid(_)));
        assert!(!rig.shell.read_with(rig.cx, |shell, _| shell.transients()).2,
            "plain f inside Ask cannot start body hints");
        rig.keys("backspace");
        assert_eq!(input.read_with(rig.cx, |input, _| input.value().to_string()), "",
            "Backspace edits the focused Ask input while Hand is open");
        assert_eq!(held(&mut rig), 1, "Backspace cannot let go of a covered Hand card");

        rig.keys("escape");
        rig.graph.store.update(rig.cx, |store, cx| store.owner_failed(
            &crate::runtime::owner::OwnerFault::Lost("fixture owner unavailable".into()), cx));
        rig.settle();
        assert!(rig.shell.read_with(rig.cx, |shell, cx| shell.chrome_words(cx))
            .contains(&("hand", "open".to_owned())), "owner loss leaves the local Hand session open");
        rig.keys("f");
        assert!(rig.shell.read_with(rig.cx, |shell, _| shell.transients()).2,
            "an uncovered page first owns its hint session");
        let (ask, input) = open_from_button(&mut rig);
        assert!(!rig.shell.read_with(rig.cx, |shell, _| shell.transients()).2,
            "opening Ask retires the previous page's hints");
        rig.keys("r");
        assert_eq!(input.read_with(rig.cx, |input, _| input.value().to_string()), "r",
            "owner loss cannot prevent local Ask typing or let old hints eat it");
        assert!(matches!(ask.read_with(rig.cx, |ask, _| ask.draft.clone()), QueryDraft::Valid(_)));
        rig.keys("backspace");
        assert_eq!(input.read_with(rig.cx, |input, _| input.value().to_string()), "",
            "offline Backspace still belongs to the native editor");
        assert_eq!(held(&mut rig), 1, "owner loss cannot revive the covered Hand keyboard session");
    }

    #[test]
    fn search_destination_requires_the_exact_producer_package_claim() {
        let decl = DeclRef::from_label(&crate::shell::tests::coordinate("RelationLabel"), None,
            Some(DeclarationKind::Enum), None).expect("fixture declaration");
        let mut row = SearchRow {
            rank: 0,
            package: Some(Arc::from(crate::shell::tests::PACKAGE)),
            decl,
            score: Known::Unknown(Gap::new(GapReason::NotServed, "")),
            signature: Known::Unknown(Gap::new(GapReason::NotServed, "")),
            snippet: None,
            reason: MatchReason::ExactName,
        };
        let exact = super::row_route(&row).expect("same-package producer claim admits the destination");
        assert!(matches!(exact, crate::navigation::Route::Symbol(_)));
        row.package = Some(Arc::from("/fixture/runtime"));
        assert!(super::row_route(&row).is_none(), "a foreign package claim must never advertise a link");
        row.package = None;
        assert!(super::row_route(&row).is_none(), "a missing claim is not authority either");
        row.decl = DeclRef::from_label("external semantic target",
            Some(backend_library::symbol_key("external-target")), None, None)
            .expect("relation endpoint display label");
        row.package = Some(Arc::from("external semantic target"));
        assert!(super::row_route(&row).is_none(),
            "a Symbol row with a root-looking display label is not a package page");
    }

    #[test]
    fn observed_local_package_root_search_row_opens_its_package_page() {
        // This is the coordinate shape the real base16ct owner published:
        // the package root itself is a search row, with no `::` declaration
        // tail. A Symbol route for it is necessarily unreadable.
        let root = "/private/tmp/nudox-gui-user-audit-20261003/real-source-base16ct-1.0.0";
        let basis = backend_library::Basis::new(
            backend_library::view_state_root(&[]), backend_library::object_version(b"root-result"),
        );
        let producer = backend_library::Row::new(
            backend_library::RowId::Package(backend_library::package_key(root)), basis, root,
        );
        let page = crate::runtime::page_mapping::search_rows(
            "base16ct", &[producer], &[backend_library::Coverage::Complete], None, 0,
        );
        let row = page.rows.first().expect("producer package row survives lowering");
        assert_eq!(row.decl.coordinate.as_str(), root);
        assert_eq!(row.package.as_deref(), Some(root));
        let destination = super::row_route(row).expect("exact package root has a page");
        assert!(matches!(destination, crate::navigation::Route::Package(ref route) if route.package.as_str() == root));

        let module = format!("{root}::src/lib.rs");
        let module = crate::model::pages::SymbolRef::new(&module).expect("module coordinate");
        let package = module.package().expect("typed package root");
        let destination = crate::shell::kit::indexed_result_route(&package, &module, crate::navigation::View::Page)
            .expect("module belongs to its exact package");
        assert!(matches!(destination, crate::navigation::Route::Symbol(_)));
        assert!(crate::shell::kit::indexed_result_route(&package, &module, crate::navigation::View::Code).is_some());
        assert!(crate::shell::kit::indexed_result_route(&package, &row.decl.coordinate, crate::navigation::View::Code).is_none(),
            "a package root has no declaration Code view");
    }

    /// A search fixture whose one row has no package: the coordinate names
    /// a bare declaration the index cannot place (dead end #14).
    struct NoPlaceSearch;
    impl PageReader for NoPlaceSearch {
        fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
            if let ReadRequest::Search(query) | ReadRequest::SearchMore { query, .. } = request {
                // A label with an empty project part (`::Mystery`) is the
                // one shape whose `coordinate.package()` is genuinely
                // `None`: a bare name (no `::`) is itself admitted as a
                // local project reference, so it is NOT enough on its own.
                let decl = DeclRef::from_label("::Mystery", None, Some(DeclarationKind::Struct), None).expect("decl");
                debug_assert!(decl.coordinate.package().is_none(), "the fixture's row must have no place");
                return Ok(PageValue::Search(SearchPage {
                    query: Arc::clone(&query.text),
                    rows: Arc::from([SearchRow {
                        rank: 0,
                        decl,
                        package: None,
                        score: Known::Unknown(Gap::new(GapReason::NotServed, "")),
                        signature: Known::Unknown(Gap::new(GapReason::NotServed, "")),
                        snippet: None,
                        reason: MatchReason::ExactName,
                    }]),
                    coverage: backend_present::CoverageLine::new(&[], None),
                    next: None,
                }));
            }
            Fixture.read(request, context)
        }
    }

    /// Dead end #14: an Ask row with no place does not move the page on
    /// ⏎, and the active query says why while preserving its draft.
    #[gpui::test]
    fn a_row_with_no_place_keeps_its_draft_and_speaks_inside_ask_on_enter(cx: &mut TestAppContext) {
        let pool = ReadPool::start(2, |_| NoPlaceSearch).expect("pool");
        let mut rig = rig_with_reads(cx, Some(page_route("RelationLabel")), 1440.0, 900.0, pool);
        rig.keys("secondary-k");
        // Open with nothing typed, Ask is its field: no plate is drawn, and
        // none is said to be (the page under the veil is what shows).
        let dialogs = |ledger: &facet::probe::Ledger| -> Vec<String> {
            ledger.stacks.iter().flat_map(|stack| &stack.entries).filter(|entry| entry.kind == "dialog").map(|entry| entry.key.clone()).collect()
        };
        rig.cx.update(|_, cx| facet::probe::enable(cx));
        rig.repaint();
        let empty = rig.cx.update(|_, cx| facet::probe::take(cx));
        assert_eq!(dialogs(&empty), ["ask-field"], "no query, no plate");
        let ask = rig.shell.read_with(rig.cx, |shell, _| shell.ask_entity());
        let input = ask.read_with(rig.cx, |ask, _| ask.input().clone());
        rig.cx.update(|window, cx| input.update(cx, |input, cx| input.replace_all("mystery", window, cx)));
        rig.frame(120);
        rig.settle();
        // Ask's words are its own region (its plate hides the shelf's words
        // under it: the harness neither reads nor lints those).
        rig.repaint();
        let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
        assert_eq!(dialogs(&ledger), ["ask-field", "ask-plate"], "a query draws the plate");
        let mystery = ledger.texts.iter().find(|text| text.content == "Mystery").unwrap_or_else(|| panic!("the row is painted: {:?}", ledger.texts.iter().map(|t| &t.content).collect::<Vec<_>>()));
        assert_eq!(mystery.region.as_deref(), Some("ask"), "Ask's row is in Ask's region");
        let route_before = rig.route();
        let observed_ask = ask.clone();
        let input_sequence = Rc::new(RefCell::new(Vec::new()));
        let observed_sequence = Rc::clone(&input_sequence);
        let _input_events = rig.cx.update(|_, cx| cx.subscribe(&input, move |input, event: &InputEvent, cx| {
            let event = match event {
                InputEvent::Change => "change".to_owned(),
                InputEvent::PressEnter { secondary, shift } => format!("enter secondary={secondary} shift={shift}"),
                InputEvent::Focus => "focus".to_owned(),
                InputEvent::Blur => "blur".to_owned(),
            };
            observed_sequence.borrow_mut().push(event.clone());
            eprintln!("ASK-DISPATCH input-event={event} value={:?} refusal={:?}", input.read(cx).value(), observed_ask.read(cx).refusal.as_ref().map(SubmitRefusal::words));
        }));
        let _key_events = rig.cx.update(|_, cx| cx.observe_keystrokes(|event, _, _| {
            if event.keystroke.key == "enter" {
                eprintln!("ASK-DISPATCH action={:?} contexts={:?}", event.action.as_ref().map(|action| action.name()), event.context_stack);
            }
        }));
        let admission = rig.cx.update(|window, cx| (
            input.read(cx).focus_handle(cx).is_focused(window),
            window.is_action_available(&gpui_component::input::Enter { secondary: false, shift: false }, cx),
            ask.read(cx).links.snapshot(cx).overlay(),
            ask.read(cx).draft.query().is_some_and(|query| current_input_query(&input, query, cx)),
        ));
        eprintln!("ASK-DISPATCH before-enter focused/action-available/overlay/exact-query={admission:?}");
        rig.keys("enter");
        eprintln!("ASK-DISPATCH after-enter refusal={:?}", ask.read_with(rig.cx, |ask, _| ask.refusal.as_ref().map(SubmitRefusal::words)));
        assert_eq!(input_sequence.borrow().as_slice(), ["enter secondary=false shift=false"],
            "one Enter submits once without a second unchanged editor Change");
        assert_eq!(rig.route(), route_before, "a row with no place does not move the page");
        let (ask_open, _, _) = rig.shell.read_with(rig.cx, |shell, _| shell.transients());
        assert!(ask_open, "a refused destination preserves the active query");
        assert_eq!(input.read_with(rig.cx, |input, _| input.value().to_string()), "mystery");
        rig.repaint();
        let refused = rig.cx.update(|_, cx| facet::probe::take(cx));
        assert!(refused.texts.iter().any(|text| text.region.as_deref() == Some("ask")
            && text.content == "Mystery has no page yet"), "the refusal is actually painted in the retained query");
    }

    #[gpui::test]
    fn refused_enter_keeps_selected_native_result_bounds_at_both_text_sizes_and_after_resize(cx: &mut TestAppContext) {
        for percent in [100, 200] {
            let mut rig = rig_with_reads(cx, Some(page_route("RelationLabel")), 1440.0, 900.0,
                ReadPool::start(2, |_| NoPlaceSearch).expect("no-destination reader"));
            let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
            rig.go(crate::navigation::Intent::ZoomTo { display, percent });
            native_ask(&mut rig);
            // Native AX supplies the row geometry. The capture probe forces
            // every region to render per frame and would bypass the product
            // caching whose Reader isolation this control measures.
            rig.cx.update(|_, cx| facet::probe::disable(cx));
            rig.cx.simulate_input("mystery");
            rig.settle();
            let ask = rig.shell.read_with(rig.cx, |shell, _| shell.ask_entity());
            let input = ask.read_with(rig.cx, |ask, _| ask.input().clone());
            let reader = rig.shell.read_with(rig.cx, |shell, _| shell.reader_entity());
            for width in [1440.0, 720.0, 1080.0] {
                // Resize the mounted query, then edit through the real focused
                // editor so each Enter starts with no submission refusal.
                rig.cx.simulate_resize(gpui::size(px(width), px(900.0)));
                rig.settle();
                rig.keys("backspace");
                rig.cx.simulate_input("y");
                rig.settle();
                assert!(ask.read_with(rig.cx, |ask, _| ask.refusal.is_none()));
                assert_eq!(ask.read_with(rig.cx, |ask, _| ask.selected), 0);
                let native_row = |rig: &mut Rig| {
                    let tree: serde_json::Value = rig.cx.update(|window, _| serde_json::from_str(
                        &window.debug_a11y_tree_json().expect("committed native result tree")).expect("native JSON"));
                    tree["nodes"].as_object().expect("native nodes").values()
                        .find(|node| node["element_id"] == "Name(\"ask-unavailable-0\")")
                        .expect("selected Mystery row is actually mounted")["bounds"].clone()
                };
                let before = native_row(&mut rig);
                let route = rig.route();
                let pages = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_pages(cx));
                let reader_renders = rig.counts().reader;
                rig.keys("enter");
                let after = native_row(&mut rig);
                assert_eq!(after, before,
                    "failed Enter must not move the selected native result at {percent}%/{width}px");
                assert_eq!(rig.route(), route, "refusal does not navigate the Reader");
                assert_eq!(rig.shell.read_with(rig.cx, |shell, _| shell.reader_entity()), reader,
                    "refusal retains the mounted Reader entity");
                assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.reader_pages(cx)), pages);
                assert_eq!(rig.counts().reader, reader_renders, "refusal only redraws Ask");
                assert_eq!(input.read_with(rig.cx, |input, _| input.value().to_string()), "mystery");
                assert_refusal(&mut rig, "mystery", "Mystery has no page yet");
                eprintln!("native refused Enter geometry: percent={percent} width={width} before={before} after={after} reader_renders={reader_renders}");
            }
        }
    }

    #[gpui::test]
    fn native_refused_enter_reader_frame_diagnostic(cx: &mut TestAppContext) {
        for percent in [100, 200] {
            for width in [1440.0, 720.0, 1080.0] {
                for submit in [false, true] {
                    let mut rig = rig_with_reads(cx, Some(page_route("RelationLabel")), 1440.0, 900.0,
                        ReadPool::start(2, |_| NoPlaceSearch).expect("no-destination reader"));
                    let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
                    rig.go(crate::navigation::Intent::ZoomTo { display, percent });
                    native_ask(&mut rig);
                    rig.cx.simulate_input("mystery");
                    rig.settle();
                    rig.cx.simulate_resize(gpui::size(px(width), px(900.0)));
                    rig.settle();
                    rig.keys("backspace");
                    rig.cx.simulate_input("y");
                    rig.settle();
                    let ask = rig.shell.read_with(rig.cx, |shell, _| shell.ask_entity());
                    let reader = rig.shell.read_with(rig.cx, |shell, _| shell.reader_entity());
                    let input = ask.read_with(rig.cx, |ask, _| ask.input().clone());
                    let route = rig.route();
                    let pages = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_pages(cx));
                    let row = |rig: &mut Rig| {
                        let tree: serde_json::Value = rig.cx.update(|window, _| serde_json::from_str(
                            &window.debug_a11y_tree_json().expect("committed native tree")).expect("native JSON"));
                        tree["nodes"].as_object().expect("native nodes").values()
                            .find(|node| node["element_id"] == "Name(\"ask-unavailable-0\")")
                            .expect("mounted Mystery row")["bounds"].clone()
                    };
                    let before = row(&mut rig);
                    let started = rig.cx.executor().now();
                    let log = |rig: &mut Rig, stage: &str| {
                        let counts = rig.counts();
                        let motion = rig.shell.read_with(rig.cx, |shell, cx| shell.diagnostic_cover_motion(cx));
                        let reader_state = reader.read_with(rig.cx, |reader, _| (
                            reader.diagnostic_background_presentation(), reader.native_motion_settled(), reader.native_input_allowed()));
                        let frame = rig.cx.update(|window, _| window.a11y_frame_number());
                        eprintln!("ASK-READER-DIAGNOSTIC percent={percent} width={width} submit={submit} stage={stage} elapsed={:?} counts={counts:?} frame={frame} motion={motion:?} reader={reader_state:?}",
                            rig.cx.executor().now().duration_since(started));
                    };
                    log(&mut rig, "before");
                    if submit {
                        rig.cx.simulate_keystrokes("enter");
                        rig.cx.simulate_event(gpui::KeyUpEvent {
                            keystroke: gpui::Keystroke::parse("enter").expect("native Enter"),
                        });
                    }
                    log(&mut rig, "synchronous");
                    rig.cx.run_until_parked();
                    rig.draw();
                    log(&mut rig, "parked-and-drawn");
                    // Both branches advance the same ordinary native frame
                    // machinery; only one dispatches the actual Enter gesture.
                    rig.settle();
                    log(&mut rig, "settled");
                    assert_eq!(row(&mut rig), before, "diagnostic advancement preserves selected row geometry");
                    assert_eq!(rig.route(), route);
                    assert_eq!(rig.shell.read_with(rig.cx, |shell, _| shell.reader_entity()), reader);
                    assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.reader_pages(cx)), pages);
                    assert_eq!(input.read_with(rig.cx, |input, _| input.value().to_string()), "mystery");
                    if submit { assert_refusal(&mut rig, "mystery", "Mystery has no page yet"); }
                    else { assert!(ask.read_with(rig.cx, |ask, _| ask.refusal.is_none())); }
                }
            }
        }
    }

    struct FailedSearch;
    impl PageReader for FailedSearch {
        fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
            if matches!(request, ReadRequest::Search(_)) {
                Err(ReadFailure::Fault(crate::core::ErrorValue::new(crate::core::FaultCode::Transport,
                    "fixture search owner disconnected")))
            } else { Fixture.read(request, context) }
        }
    }

    #[gpui::test]
    fn real_enter_keeps_failed_search_draft_focus_and_refusal_visible(cx: &mut TestAppContext) {
        let mut rig = rig_with_reads(cx, None, 1440.0, 900.0,
            ReadPool::start(1, |_| FailedSearch).expect("fixture pool"));
        rig.cx.update(|_, cx| facet::probe::enable(cx));
        rig.keys("secondary-k");
        rig.cx.simulate_input("RelationLabel");
        rig.settle();
        let route = rig.route();
        rig.keys("enter");
        assert_eq!(rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().overlay()),
            Some(crate::navigation::Overlay::CommandPalette));
        assert_eq!(rig.route(), route);
        let ask = rig.shell.read_with(rig.cx, |shell, _| shell.ask_entity());
        let input = ask.read_with(rig.cx, |ask, _| ask.input().clone());
        assert_eq!(input.read_with(rig.cx, |input, _| input.value().to_string()), "RelationLabel");
        assert!(rig.cx.update(|window, cx| input.read(cx).focus_handle(cx).is_focused(window)));
        rig.repaint();
        let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
        assert!(ledger.texts.iter().any(|text| text.region.as_deref() == Some("ask")
            && text.content.contains("fixture search owner disconnected")), "the refusal is painted inside Ask");
        rig.cx.simulate_input("X"); rig.settle();
        assert!(ask.read_with(rig.cx, |ask, _| ask.refusal.is_none()), "editing retires the old submission refusal");
    }


    struct EmptySearch;
    impl PageReader for EmptySearch {
        fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
            if let ReadRequest::Search(query) | ReadRequest::SearchMore { query, .. } = request {
                return Ok(PageValue::Search(SearchPage {
                    query: Arc::clone(&query.text), rows: Arc::from([]),
                    coverage: backend_present::CoverageLine::new(&[], Some(0)).with_semantic_search_status(backend_library::SemanticSearchStatus::Unavailable { reason: backend_library::SemanticSearchReason::Unconfigured }), next: None,
                }));
            }
            Fixture.read(request, context)
        }
    }

    fn native_ask(rig: &mut crate::shell::tests::Rig) {
        let shell = rig.shell.clone();
        rig.cx.update(|window, cx| {
            window.replace_root(cx, |window, cx| gpui_component::Root::new(shell, window, cx).bordered(false));
            window.set_a11y_forced(true);
            facet::probe::enable(cx);
        });
        rig.settle();
        rig.keys("secondary-k");
    }

    fn assert_refusal(rig: &mut crate::shell::tests::Rig, draft: &str, words: &str) {
        assert_eq!(rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().overlay()), Some(crate::navigation::Overlay::CommandPalette));
        let ask = rig.shell.read_with(rig.cx, |shell, _| shell.ask_entity());
        let input = ask.read_with(rig.cx, |ask, _| ask.input().clone());
        assert_eq!(input.read_with(rig.cx, |input, _| input.value().to_string()), draft);
        let expected_label = ask.read_with(rig.cx, |ask, _| ask.refusal.as_ref()
            .expect("native submission retained its refusal").words());
        rig.repaint();
        let json = rig.cx.update(|window, _| window.debug_a11y_tree_json()).expect("forced native Ask tree");
        let tree: serde_json::Value = serde_json::from_str(&json).expect("native tree JSON");
        let native_status = tree["nodes"].as_object().expect("native nodes").values().any(|node|
            node["element_id"] == "Name(\"ask-read-status\")"
                && node["aria"]["role"].as_str() == Some("Status")
                && node["aria"]["label"].as_str().is_some_and(|label| label.contains(words) && label == &*expected_label)
                && node["bounds"]["width"].as_f64().is_some_and(|width| width > 0.0)
                && node["bounds"]["height"].as_f64().is_some_and(|height| height > 0.0));
        if !native_status {
            let state = ask.read_with(rig.cx, |ask, cx| (
                ask.refusal.as_ref().map(SubmitRefusal::words), ask.read_status(cx).0,
                ask.owner_failure_words(cx), ask.search_resource(cx)));
            eprintln!("ASK-REFUSAL-AX-DIAGNOSTIC expected={words:?} state={state:?} tree={json}");
        }
        assert!(native_status, "the active query exposes actionable refusal words to native accessibility");
        assert!(rig.cx.update(|window, cx| input.read(cx).focus_handle(cx).is_focused(window)), "the retained editor keeps native focus");
    }

    #[gpui::test]
    fn actual_empty_search_enter_and_all_results_keep_exact_draft_until_escape(cx: &mut TestAppContext) {
        let mut rig = rig_with_reads(cx, Some(page_route("RelationLabel")), 1440.0, 900.0,
            ReadPool::start(1, |_| EmptySearch).expect("empty search reader"));
        native_ask(&mut rig);
        let route = rig.route();
        let draft = "How does compilation work";
        rig.cx.simulate_input(draft);
        rig.settle();
        let ask = rig.shell.read_with(rig.cx, |shell, _| shell.ask_entity());
        assert!(ask.read_with(rig.cx, |ask, cx| {
            let (resource, root, serving) = ask.search_resource(cx).expect("actual search resource");
            crate::core::admit_resource(&resource, root, serving).current_value().is_some_and(|page| page.rows.is_empty())
        }), "the actual worker delivered an admitted empty page");
        for key in ["enter", "secondary-enter"] {
            rig.keys(key);
            assert_eq!(rig.route(), route);
            assert_refusal(&mut rig, draft, "Try a declaration name or different words");
        }
        rig.keys("escape");
        assert_eq!(rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().overlay()), None, "intentional Escape still cancels");
        rig.keys("secondary-k");
        let input = ask.read_with(rig.cx, |ask, _| ask.input().clone());
        assert!(input.read_with(rig.cx, |input, _| input.value().is_empty()), "only a new query visit clears the old draft");
    }

    #[gpui::test]
    fn actual_enter_denies_a_nonserving_owner_without_losing_the_draft(cx: &mut TestAppContext) {
        let mut rig = rig_with_reads(cx, Some(page_route("RelationLabel")), 1440.0, 900.0,
            ReadPool::start(1, |_| Fixture).expect("search reader"));
        native_ask(&mut rig);
        rig.cx.simulate_input("RelationLabel");
        rig.settle();
        let route = rig.route();
        rig.graph.store.update(rig.cx, |store, cx| store.owner_failed(&crate::runtime::owner::OwnerFault::Lost("index attachment retired".into()), cx));
        rig.settle();
        rig.keys("enter");
        assert_eq!(rig.route(), route);
        assert_refusal(&mut rig, "RelationLabel", "index attachment retired");
    }

    // These tests renew an owner at its already published root. RootOnly
    // intentionally advances authority for other shell tests, so it cannot
    // isolate this same-root owner contract.
    struct StableAskRoot;
    impl crate::runtime::actor::EngineClient for StableAskRoot {
        fn execute(&mut self, request: &crate::runtime::actor::EngineRequest)
            -> Result<crate::runtime::actor::EngineDto, crate::runtime::actor::EngineFault> {
            match request {
                crate::runtime::actor::EngineRequest::Root { request, basis, .. } => {
                    Ok(crate::runtime::actor::EngineDto::Root {
                        request: *request, basis: *basis, key: *basis,
                        revision: basis.revision(), delta: None, project: None, catalog: None,
                    })
                }
                _ => Err(crate::runtime::actor::EngineFault::Cancelled),
            }
        }
    }

    struct CountAskReads(Arc<std::sync::atomic::AtomicUsize>);
    impl PageReader for CountAskReads {
        fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
            if matches!(request, ReadRequest::Search(_)) {
                self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            Fixture.read(request, context)
        }
    }

    fn native_result_labels(rig: &mut Rig) -> Vec<String> {
        let tree: serde_json::Value = rig.cx.update(|window, _| serde_json::from_str(
            &window.debug_a11y_tree_json().expect("committed native Ask tree")).expect("native Ask JSON"));
        let mut labels = tree["nodes"].as_object().expect("native nodes").values()
            .filter(|node| node["aria"]["role"] == "Link")
            .filter_map(|node| node["aria"]["label"].as_str())
            .filter(|label| label.starts_with("Result "))
            .map(str::to_owned).collect::<Vec<_>>();
        labels.sort();
        labels
    }

    #[gpui::test]
    fn visible_native_ask_query_renews_once_after_owner_recovery_but_hidden_query_stays_lazy(cx: &mut TestAppContext) {
        use crate::model::ServiceMode;
        use crate::runtime::owner::{OwnerGate, OwnerState};
        use crate::runtime::store::DataStore;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let initial = VersionedRoot::synthetic(backend_library::view_state_root(
            &[("shell".to_owned(), "tests".to_owned())]), 4);
        let gate = OwnerGate::ready(initial, ServiceMode::Attached);
        let reads = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&reads);
        let mut rig = crate::shell::tests::rig_with_engine_gate(cx,
            Some(page_route("RelationLabel")), 1440.0, 900.0,
            ReadPool::start(1, move |_| CountAskReads(Arc::clone(&counter))).expect("Ask reader"),
            StableAskRoot, Some(gate.clone()));
        native_ask(&mut rig);
        rig.cx.update(|window, cx| { facet::probe::disable(cx); cx.set_global(gpui::TextTrace); window.set_a11y_forced(true); });
        rig.cx.simulate_input("RelationLabel");
        rig.settle();
        let ask = rig.shell.read_with(rig.cx, |shell, _| shell.ask_entity());
        let input = ask.read_with(rig.cx, |ask, _| ask.input().clone());
        let query = ask.read_with(rig.cx, |ask, _| ask.draft.query().expect("current native draft").clone());
        let key = crate::model::pages::PageKey::Search(query.clone());
        let route = rig.route();
        let root = rig.graph.store.read_with(rig.cx, |store, _| {
            assert!(!store.focused().contains(&key), "the mounted Ask query is outside route-focused reads");
            assert!(!store.observation_revoked(&key));
            store.snapshot().key()
        });
        let labels = native_result_labels(&mut rig);
        assert!(!labels.is_empty(), "the initial native result is actionable");
        let count = reads.load(Ordering::SeqCst);
        assert_eq!(count, 1, "typing admits exactly one actual Search read");
        let renders = ask.read_with(rig.cx, |ask, _| ask.renders());
        let frame = rig.cx.update(|window, _| window.a11y_frame_number());
        rig.cx.run_until_parked();
        assert_eq!(ask.read_with(rig.cx, |ask, _| ask.renders()), renders, "idle does not render Ask");
        assert_eq!(rig.cx.update(|window, _| window.a11y_frame_number()), frame, "idle does not draw a frame");
        gate.publish(OwnerState::Starting);
        rig.graph.store.update(rig.cx, DataStore::owner_starting);
        rig.cx.run_until_parked();
        assert!(rig.graph.store.read_with(rig.cx, |store, _| store.observation_revoked(&key)));
        assert_eq!(reads.load(Ordering::SeqCst), count, "Starting holds the visible query without dispatching it");
        gate.publish(OwnerState::Ready { key: root, mode: ServiceMode::Attached });
        rig.graph.store.update(rig.cx, DataStore::owner_ready);
        let deadline = Instant::now() + rig.patience;
        loop {
            rig.cx.run_until_parked();
            if rig.graph.store.read_with(rig.cx, |store, _| store.pool_activity().is_idle()) { break; }
            assert!(Instant::now() < deadline, "owner recovery did not finish its native reads");
            rig.cx.executor().advance_clock(Duration::from_millis(10));
            std::thread::sleep(Duration::from_millis(2));
        }
        eprintln!("native Ask owner recovery: initial_search_reads={count} final_search_reads={} resource={:?}",
            reads.load(Ordering::SeqCst), rig.graph.store.read_with(rig.cx, |store, _| store.search(&query)));
        assert_eq!(reads.load(Ordering::SeqCst), count + 1, "the visible current draft renews exactly once without another keystroke");
        assert!(ask.read_with(rig.cx, |ask, cx| ask.read_status(cx).0.is_none()), "the renewed result is current");
        assert_eq!(native_result_labels(&mut rig), labels, "the exact native result actions return without a test repaint");
        assert!(ask.read_with(rig.cx, |ask, _| ask.renders()) > renders);
        assert!(rig.cx.update(|window, _| window.a11y_frame_number()) > frame);
        assert_eq!(input.read_with(rig.cx, |input, _| input.value().to_string()), "RelationLabel");
        assert_eq!(rig.route(), route);
        assert_eq!(rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key()), root);
        rig.keys("escape");
        assert_ne!(rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().overlay()), Some(crate::navigation::Overlay::CommandPalette));
        let hidden = reads.load(Ordering::SeqCst);
        gate.publish(OwnerState::Starting);
        rig.graph.store.update(rig.cx, DataStore::owner_starting);
        rig.cx.run_until_parked();
        gate.publish(OwnerState::Ready { key: root, mode: ServiceMode::Attached });
        rig.graph.store.update(rig.cx, DataStore::owner_ready);
        rig.cx.run_until_parked();
        assert_eq!(reads.load(Ordering::SeqCst), hidden, "the closed Ask draft causes no hidden eager Search");
    }

    #[gpui::test]
    fn native_owner_recovery_preserves_the_current_cached_query_debounce(cx: &mut TestAppContext) {
        use crate::model::ServiceMode;
        use crate::runtime::owner::{OwnerGate, OwnerState};
        use crate::runtime::store::DataStore;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let initial = VersionedRoot::synthetic(backend_library::view_state_root(
            &[("shell".to_owned(), "tests".to_owned())]), 4);
        let gate = OwnerGate::ready(initial, ServiceMode::Attached);
        let reads = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&reads);
        let mut rig = crate::shell::tests::rig_with_engine_gate(cx,
            Some(page_route("RelationLabel")), 1440.0, 900.0,
            ReadPool::start(1, move |_| CountAskReads(Arc::clone(&counter))).expect("Ask reader"),
            StableAskRoot, Some(gate.clone()));
        native_ask(&mut rig);
        rig.cx.update(|window, cx| { facet::probe::disable(cx); window.set_a11y_forced(true); });
        rig.cx.simulate_input("RelationLabel");
        rig.settle();
        let ask = rig.shell.read_with(rig.cx, |shell, _| shell.ask_entity());
        let input = ask.read_with(rig.cx, |ask, _| ask.input().clone());
        let labels = native_result_labels(&mut rig);
        assert!(!labels.is_empty());
        let count = reads.load(Ordering::SeqCst);
        assert_eq!(count, 1);
        // Return to an already cached query, but do not let this new draft's
        // debounce expire. Its resource activity alone cannot admit it.
        for text in ["NotIssued", "RelationLabel"] {
            rig.cx.update(|window, cx| input.update(cx, |input, cx| input.replace_all(text, window, cx)));
        }
        let root = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
        gate.publish(OwnerState::Starting);
        rig.graph.store.update(rig.cx, DataStore::owner_starting);
        rig.cx.run_until_parked();
        gate.publish(OwnerState::Ready { key: root, mode: ServiceMode::Attached });
        rig.graph.store.update(rig.cx, DataStore::owner_ready);
        rig.cx.run_until_parked();
        assert_eq!(reads.load(Ordering::SeqCst), count, "owner renewal cannot bypass the current draft's debounce");
        rig.cx.executor().advance_clock(Duration::from_millis(89));
        rig.cx.run_until_parked();
        assert_eq!(reads.load(Ordering::SeqCst), count, "the cached query still waits for its full debounce");
        rig.cx.executor().advance_clock(Duration::from_millis(1));
        let deadline = Instant::now() + rig.patience;
        loop {
            rig.cx.run_until_parked();
            if rig.graph.store.read_with(rig.cx, |store, _| store.pool_activity().is_idle()) { break; }
            assert!(Instant::now() < deadline, "the debounced native query never completed");
            rig.cx.executor().advance_clock(Duration::from_millis(10));
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(reads.load(Ordering::SeqCst), count + 1, "the settled current draft admits exactly one replacement read");
        assert_eq!(native_result_labels(&mut rig), labels, "the exact native result actions return without a test repaint");
        let superseded = crate::model::pages::SearchQuery::new("NotIssued", 50).expect("superseded query");
        assert_eq!(rig.graph.store.read_with(rig.cx, |store, _| store.search(&superseded).activity()),
            crate::core::Activity::NotYet, "the superseded draft never reads");
        assert_eq!(input.read_with(rig.cx, |input, _| input.value().to_string()), "RelationLabel");
        assert_eq!(rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key()), root);
        eprintln!("native Ask debounce across owner recovery: initial_search_reads={count} final_search_reads={}", reads.load(Ordering::SeqCst));
    }

    struct OwnerAskView {
        ask: gpui::Entity<super::Ask>,
        input: gpui::Entity<gpui_component::input::InputState>,
    }

    impl gpui::Render for OwnerAskView {
        fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
            use gpui::{ParentElement as _, Styled as _};
            gpui::div().size_full().flex().flex_col()
                .child(gpui_component::input::Input::new(&self.input))
                .child(self.ask.clone())
        }
    }

    #[gpui::test]
    fn passive_owner_only_renewal_repaints_native_ask_without_moving_search_stamp(cx: &mut TestAppContext) {
        use crate::model::ServiceMode;
        use crate::model::pages::PageKey;
        use crate::runtime::owner::{OwnerGate, OwnerState};
        use crate::runtime::store::{Branch, DataStore, StoreEvent};
        let mut rig = rig_with_reads(cx, Some(page_route("RelationLabel")), 1440.0, 900.0,
            ReadPool::start(1, |_| Fixture).expect("shell reader"));
        rig.keys("secondary-k");
        let snapshot = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot());
        let root = snapshot.key();
        let gate = OwnerGate::ready(root, ServiceMode::Attached);
        // Exercise Ask's real StoreEvent boundary independently of the Shell
        // and owner watcher's other notifications, which can mask this wake.
        let (store, ask, input) = rig.cx.update(|window, cx| {
            let store = DataStore::install_with_owner(cx, snapshot,
                Some(ReadPool::start(1, |_| Fixture).expect("Ask reader")), Some(gate.clone()), None);
            let links = super::Links { root: rig.graph.root.downgrade(), store: store.clone(),
                shell: rig.shell.downgrade(), reader: Rc::new(RefCell::new(None)) };
            let ask = cx.new(|cx| super::Ask::new(links, window, cx));
            let input = ask.read(cx).input().clone();
            window.replace_root(cx, |window, cx| {
                let view = cx.new(|_| OwnerAskView { ask: ask.clone(), input: input.clone() });
                gpui_component::Root::new(view, window, cx).bordered(false)
            });
            window.set_a11y_forced(true);
            cx.set_global(gpui::TextTrace);
            facet::probe::disable(cx);
            input.read(cx).focus_handle(cx).focus(window, cx);
            (store, ask, input)
        });
        rig.draw();
        // Cache this exact query before typing a new draft for it. That
        // draft's debounce is still pending at the owner boundary, so the
        // query must not renew and only Branch::Owner can repaint its status.
        let cached = crate::model::pages::SearchQuery::new("RelationLabel", 50).expect("cached query");
        store.update(rig.cx, |store, cx| { store.ensure(PageKey::Search(cached.clone()), cx); });
        let deadline = Instant::now() + rig.patience;
        while store.read_with(rig.cx, |store, _| store.search(&cached).loaded_value().is_none()) {
            rig.cx.run_until_parked();
            assert!(Instant::now() < deadline, "native Ask's initial read never completed");
            rig.cx.executor().advance_clock(Duration::from_millis(10));
            std::thread::sleep(Duration::from_millis(2));
        }
        rig.cx.simulate_input("RelationLabel");
        rig.cx.run_until_parked();
        assert!(!ask.read_with(rig.cx, |ask, _| ask.query_settled), "the current draft still owns its typing debounce");
        assert!(ask.read_with(rig.cx, |ask, cx| ask.read_status(cx).0.is_none() && !ask.choices(cx).is_empty()));
        rig.repaint();
        let bounds = rig.cx.debug_bounds("ask-result-row").expect("actual native result row");
        rig.cx.simulate_mouse_down(bounds.center(), gpui::MouseButton::Left, Modifiers::none());
        assert!(ask.read_with(rig.cx, |ask, _| ask.pressed_result.is_some()));
        gate.publish(OwnerState::Starting);
        store.update(rig.cx, DataStore::owner_starting);
        rig.cx.run_until_parked();
        assert!(ask.read_with(rig.cx, |ask, _| ask.pressed_result.as_ref().is_some_and(|press| press.revoked)));
        let status_before = ask.read_with(rig.cx, |ask, cx| ask.read_status(cx).0.expect("retained unavailable status"));
        let query = ask.read_with(rig.cx, |ask, _| ask.draft.query().expect("current draft").clone());
        let key = PageKey::Search(query);
        let stamp = store.read_with(rig.cx, |store, _| store.stamp(&key));
        let renders = ask.read_with(rig.cx, |ask, _| ask.renders());
        let frame = rig.cx.update(|window, _| window.a11y_frame_number());
        rig.cx.run_until_parked();
        assert_eq!(ask.read_with(rig.cx, |ask, _| ask.renders()), renders, "idle observation does not render Ask");
        assert_eq!(rig.cx.update(|window, _| window.a11y_frame_number()), frame, "idle observation does not draw a frame");
        let events = Rc::new(RefCell::new(Vec::new()));
        let observed = Rc::clone(&events);
        let _events = rig.cx.update(|_, cx| cx.subscribe(&store, move |_, event: &StoreEvent, _| {
            observed.borrow_mut().push(event.clone());
        }));
        gate.publish(OwnerState::Ready { key: root, mode: ServiceMode::Attached });
        store.update(rig.cx, DataStore::owner_ready);
        // Pump only ordinary events. No settle/draw/refresh/notify of Ask.
        rig.cx.run_until_parked();
        let status_after = ask.read_with(rig.cx, |ask, cx| ask.read_status(cx).0.expect("revoked read remains held"));
        assert_ne!(status_before, status_after, "owner admission changes the exact retained status");
        assert_eq!(store.read_with(rig.cx, |store, _| store.stamp(&key)), stamp, "renewal did not move the Search slot");
        assert_eq!(store.read_with(rig.cx, |store, _| store.snapshot().key()), root);
        assert!(events.borrow().iter().any(|event| event.is_branch(Branch::Owner)));
        assert!(!events.borrow().iter().any(|event| event.is_branch(Branch::Root)
            || event.is_branch(Branch::GraphFocus)
            || matches!(event, StoreEvent::Resource(changed) if changed == &key)), "Ask has only its independent owner wake");
        assert!(ask.read_with(rig.cx, |ask, _| ask.renders()) > renders, "owner renewal passively renders Ask");
        assert!(rig.cx.update(|window, _| window.a11y_frame_number()) > frame, "the owner wake commits a native frame");
        let tree: serde_json::Value = rig.cx.update(|window, _| serde_json::from_str(
            &window.debug_a11y_tree_json().expect("committed native Ask tree")).expect("native Ask JSON"));
        assert!(tree["nodes"].as_object().expect("native nodes").values().any(|node|
            node["aria"]["role"] == "Status" && node["aria"]["label"] == status_after.as_ref()),
            "the exact current status reaches native accessibility without a test repaint");
        assert!(rig.cx.update(|window, _| window.painted_texts().iter()
            .any(|text| text.text.as_ref() == status_after.as_ref())),
            "the exact current status is painted without a test repaint");
        eprintln!("native Ask owner-only wake: status_before={status_before:?} status_after={status_after:?} search_stamp={stamp:?} renders_before={renders} renders_after={} frame_before={frame} frame_after={} events={:?}",
            ask.read_with(rig.cx, |ask, _| ask.renders()),
            rig.cx.update(|window, _| window.a11y_frame_number()), events.borrow());
        assert!(ask.read_with(rig.cx, |ask, _| ask.refusal.is_none()), "owner publication is not a submission");
        assert_eq!(input.read_with(rig.cx, |input, _| input.value().to_string()), "RelationLabel");
        rig.cx.simulate_mouse_up(point(px(1400.0), px(10.0)), gpui::MouseButton::Left, Modifiers::none());
        assert!(ask.read_with(rig.cx, |ask, _| ask.pressed_result.is_none()));
        assert!(ask.read_with(rig.cx, |ask, _| ask.refusal.is_none()), "release outside still cancels the revoked press");
    }

    #[gpui::test]
    fn actual_pointer_release_on_a_postpaint_stale_row_refuses_inside_ask(cx: &mut TestAppContext) {
        let mut rig = rig_with_reads(cx, Some(page_route("RelationLabel")), 1440.0, 900.0,
            ReadPool::start(1, |_| Fixture).expect("search reader"));
        native_ask(&mut rig);
        rig.cx.simulate_input("RelationLabel");
        rig.settle();
        let route = rig.route();
        let ask = rig.shell.read_with(rig.cx, |shell, _| shell.ask_entity());
        assert_eq!(ask.read_with(rig.cx, |ask, cx| ask.choices(cx).len()), 1, "one actual painted fixture row");
        let bounds = rig.cx.debug_bounds("ask-result-row").expect("actual painted row bounds");
        rig.cx.simulate_event(gpui::MouseDownEvent { position: bounds.center(), modifiers: gpui::Modifiers::none(), button: gpui::MouseButton::Left, click_count: 1, first_mouse: false });
        // Retire producer authority between the painted press and release;
        // retain the actual old dispatch frame until its native callback fires.
        rig.graph.store.update(rig.cx, |store, cx| store.owner_failed(&crate::runtime::owner::OwnerFault::Lost("postpaint attachment retired".into()), cx));
        rig.cx.simulate_event(gpui::MouseUpEvent { position: bounds.center(), modifiers: gpui::Modifiers::none(), button: gpui::MouseButton::Left, click_count: 1 });
        rig.settle();
        assert_eq!(rig.route(), route);
        assert_refusal(&mut rig, "RelationLabel", "no longer verified by the current index");
    }


    #[gpui::test]
    fn a_revoked_native_ask_press_cannot_borrow_a_fresh_same_route_before_repaint(
        cx: &mut TestAppContext,
    ) {
        revoked_native_search_press_before_repaint(cx, false);
    }

    #[gpui::test]
    fn a_revoked_native_all_results_press_cannot_borrow_a_fresh_query_before_repaint(
        cx: &mut TestAppContext,
    ) {
        revoked_native_search_press_before_repaint(cx, true);
    }

    fn revoked_native_search_press_before_repaint(cx: &mut TestAppContext, all_results: bool) {
        use crate::model::ServiceMode;
        use crate::model::pages::PageKey;
        use crate::runtime::owner::{OwnerFault, OwnerGate, OwnerState};
        use crate::runtime::store::DataStore;

        let initial = VersionedRoot::synthetic(
            backend_library::view_state_root(&[("shell".to_owned(), "tests".to_owned())]),
            4,
        );
        let gate = OwnerGate::ready(initial, ServiceMode::Attached);
        let mut rig = crate::shell::tests::rig_with_engine_gate(
            cx,
            Some(page_route("RelationDirection")),
            1440.0,
            900.0,
            ReadPool::start(1, |_| Fixture).expect("Ask reader"),
            StableAskRoot,
            Some(gate.clone()),
        );
        native_ask(&mut rig);
        rig.cx.simulate_input("RelationLabel");
        rig.settle();
        let route = rig.route();
        let ask = rig.shell.read_with(rig.cx, |shell, _| shell.ask_entity());
        let (query, destination, input) = ask.read_with(rig.cx, |ask, cx| {
            (
                ask.draft.query().expect("current query").clone(),
                if all_results {
                    ask.all_results(cx).expect("painted all-results route")
                } else {
                    ask.choices(cx)
                        .into_iter()
                        .next()
                        .expect("painted row")
                        .route
                        .expect("row route")
                },
                ask.input().clone(),
            )
        });
        assert_ne!(
            route, destination,
            "a borrowed press would visibly navigate"
        );
        let root = rig
            .graph
            .store
            .read_with(rig.cx, |store, _| store.snapshot().key());
        let former = rig.graph.store.read_with(rig.cx, |store, _| {
            store
                .current_owner_attachment()
                .expect("painted owner attachment")
        });
        let bounds = if all_results {
            crate::shell::tests::native_bounds_id(
                &mut rig,
                "ask-find-page",
                "Link",
                "Open search results",
                true,
            )
            .expect("committed native all-results control")
        } else {
            rig.cx
                .debug_bounds("ask-result-row")
                .expect("painted row bounds")
        };
        rig.cx.simulate_event(gpui::MouseDownEvent {
            position: bounds.center(),
            modifiers: Modifiers::none(),
            button: gpui::MouseButton::Left,
            click_count: 1,
            first_mouse: false,
        });
        assert!(ask.read_with(rig.cx, |ask, _| ask.pressed_result.is_some()));
        let store = rig.graph.store.clone();
        let patience = rig.patience;
        // GPUI test-support eagerly draws dirty windows when an outer App
        // update finishes. Keep publication and the actual native release in
        // one update so the old dispatch frame remains mounted throughout.
        rig.cx.update(|window, cx| {
            use gpui::InputEvent as _;

            let frame = window.a11y_frame_number();
            let fault = OwnerFault::Lost("pressed owner retired".into());
            gate.publish(OwnerState::Failed(fault.clone()));
            store.update(cx, |store, cx| store.owner_failed(&fault, cx));
            gate.publish(OwnerState::Ready {
                key: root,
                mode: ServiceMode::Attached,
            });
            store.update(cx, DataStore::owner_ready);
            store.update(cx, |store, cx| {
                store.ensure(PageKey::Search(query.clone()), cx);
            });

            // Drain the ordinary read worker, preserving all pending native
            // notifications until after mouse-up has used the old listeners.
            let deadline = Instant::now() + patience;
            loop {
                store.update(cx, |store, cx| store.drain(cx));
                let current = ask.read(cx);
                let replacement_is_current = if all_results {
                    current.all_results(cx).as_ref() == Some(&destination)
                } else {
                    super::current_row_route(&current.links, &query, &destination, cx)
                };
                if replacement_is_current {
                    break;
                }
                assert!(Instant::now() < deadline, "replacement Search did not land");
                std::thread::sleep(Duration::from_millis(2));
            }
            let current = store.read(cx);
            assert_eq!(current.snapshot().key(), root);
            assert!(current.current_owner_attachment().is_some());
            assert!(!current.admits_owner_attachment(&former));
            assert_eq!(
                window.a11y_frame_number(),
                frame,
                "the old dispatch frame must remain mounted through replacement publication"
            );
            window.dispatch_event(
                gpui::MouseUpEvent {
                    position: bounds.center(),
                    modifiers: Modifiers::none(),
                    button: gpui::MouseButton::Left,
                    click_count: 1,
                }
                .to_platform_input(),
                cx,
            );
            assert_eq!(window.a11y_frame_number(), frame);
            assert_eq!(
                store.read(cx).snapshot().route(),
                &route,
                "the revoked pointer cannot borrow the replacement result"
            );
            assert!(
                input.read(cx).focus_handle(cx).is_focused(window),
                "the refused native release restores editor focus before any repaint"
            );
        });
        assert_eq!(rig.route(), route);
        assert!(ask.read_with(rig.cx, |ask, _| ask.pressed_result.is_none()));
        assert_refusal(
            &mut rig,
            "RelationLabel",
            "no longer verified by the current index",
        );

        // The refusal retires this pointer gesture, not the fresh result.
        // A new keyboard submission still uses current admitted Search data.
        rig.keys(if all_results {
            "secondary-enter"
        } else {
            "enter"
        });
        assert_eq!(rig.route(), destination);
    }

    #[gpui::test]
    fn native_pointer_on_current_all_results_opens_search_results(cx: &mut TestAppContext) {
        let mut rig = rig_with_reads(
            cx,
            Some(page_route("RelationDirection")),
            1440.0,
            900.0,
            ReadPool::start(1, |_| Fixture).expect("search reader"),
        );
        native_ask(&mut rig);
        rig.cx.simulate_input("RelationLabel");
        rig.settle();
        let ask = rig.shell.read_with(rig.cx, |shell, _| shell.ask_entity());
        let destination = ask.read_with(rig.cx, |ask, cx| {
            ask.all_results(cx).expect("current Find route")
        });
        assert_ne!(rig.route(), destination);
        let bounds = crate::shell::tests::native_bounds_id(
            &mut rig,
            "ask-find-page",
            "Link",
            "Open search results",
            true,
        )
        .expect("committed native all-results control");
        rig.cx.simulate_event(gpui::MouseDownEvent {
            position: bounds.center(),
            modifiers: Modifiers::none(),
            button: gpui::MouseButton::Left,
            click_count: 1,
            first_mouse: false,
        });
        assert!(ask.read_with(rig.cx, |ask, _| ask.pressed_result.is_some()));
        rig.cx.simulate_event(gpui::MouseUpEvent {
            position: bounds.center(),
            modifiers: Modifiers::none(),
            button: gpui::MouseButton::Left,
            click_count: 1,
        });
        rig.settle();
        assert_eq!(rig.route(), destination);
        assert!(ask.read_with(rig.cx, |ask, _| ask.pressed_result.is_none()));
    }

    #[gpui::test]
    fn a_cancelled_native_ask_press_does_not_refuse_a_later_owner_loss(cx: &mut TestAppContext) {
        let mut rig = rig_with_reads(cx, Some(page_route("RelationLabel")), 1440.0, 900.0,
            ReadPool::start(1, |_| Fixture).expect("search reader"));
        native_ask(&mut rig);
        rig.cx.simulate_input("RelationLabel");
        rig.settle();
        let route = rig.route();
        let ask = rig.shell.read_with(rig.cx, |shell, _| shell.ask_entity());
        let bounds = rig.cx.debug_bounds("ask-result-row").expect("painted row bounds");
        rig.cx.simulate_event(gpui::MouseDownEvent { position: bounds.center(), modifiers: gpui::Modifiers::none(), button: gpui::MouseButton::Left, click_count: 1, first_mouse: false });
        assert!(ask.read_with(rig.cx, |ask, _| ask.pressed_result.is_some()));
        rig.cx.simulate_event(gpui::MouseUpEvent { position: gpui::point(gpui::px(1400.0), gpui::px(800.0)), modifiers: gpui::Modifiers::none(), button: gpui::MouseButton::Left, click_count: 1 });
        rig.settle();
        assert!(ask.read_with(rig.cx, |ask, _| ask.pressed_result.is_none()),
            "release outside Ask cancels only its feedback record");
        rig.graph.store.update(rig.cx, |store, cx| store.owner_failed(&crate::runtime::owner::OwnerFault::Lost("later owner loss".into()), cx));
        rig.settle();
        assert_eq!(rig.route(), route);
        assert!(ask.read_with(rig.cx, |ask, _| ask.refusal.is_none()),
            "an earlier cancelled gesture cannot become a new submission");
        assert!(ask.read_with(rig.cx, |ask, cx| ask.read_status(cx).0
            .is_some_and(|words| words.contains("later owner loss"))),
            "the current owner failure is still disclosed");
    }

    #[gpui::test]
    fn owner_loss_during_a_native_ask_press_still_allows_release_outside_to_cancel(cx: &mut TestAppContext) {
        let mut rig = rig_with_reads(cx, Some(page_route("RelationLabel")), 1440.0, 900.0,
            ReadPool::start(1, |_| Fixture).expect("search reader"));
        native_ask(&mut rig);
        rig.cx.simulate_input("RelationLabel");
        rig.settle();
        let route = rig.route();
        let ask = rig.shell.read_with(rig.cx, |shell, _| shell.ask_entity());
        let bounds = rig.cx.debug_bounds("ask-result-row").expect("painted row bounds");
        rig.cx.simulate_event(gpui::MouseDownEvent { position: bounds.center(), modifiers: gpui::Modifiers::none(), button: gpui::MouseButton::Left, click_count: 1, first_mouse: false });
        assert!(ask.read_with(rig.cx, |ask, _| ask.pressed_result.is_some()));
        rig.graph.store.update(rig.cx, |store, cx| store.owner_failed(&crate::runtime::owner::OwnerFault::Lost("owner lost during press".into()), cx));
        rig.settle();
        assert!(ask.read_with(rig.cx, |ask, _| ask.refusal.is_none()),
            "owner revocation alone is not a submission");
        assert!(ask.read_with(rig.cx, |ask, cx| ask.read_status(cx).0
            .is_some_and(|words| words.contains("owner lost during press"))),
            "the current failure is disclosed while the press remains cancellable");
        rig.cx.simulate_event(gpui::MouseUpEvent { position: gpui::point(gpui::px(1400.0), gpui::px(800.0)), modifiers: gpui::Modifiers::none(), button: gpui::MouseButton::Left, click_count: 1 });
        rig.settle();
        assert_eq!(rig.route(), route);
        assert!(ask.read_with(rig.cx, |ask, _| ask.pressed_result.is_none()));
        assert!(ask.read_with(rig.cx, |ask, _| ask.refusal.is_none()),
            "release outside cancels a revoked press without a submission refusal");
    }

    #[gpui::test]
    fn actual_pointer_on_a_nonaddressable_result_keeps_local_refusal(cx: &mut TestAppContext) {
        let mut rig = rig_with_reads(cx, Some(page_route("RelationLabel")), 1440.0, 900.0,
            ReadPool::start(1, |_| NoPlaceSearch).expect("nonaddressable search reader"));
        native_ask(&mut rig);
        rig.cx.simulate_input("mystery");
        rig.settle();
        let route = rig.route();
        let bounds = rig.cx.debug_bounds("ask-result-row").expect("painted nonaddressable result");
        rig.cx.simulate_click(bounds.center(), gpui::Modifiers::none());
        rig.settle();
        assert_eq!(rig.route(), route);
        assert_refusal(&mut rig, "mystery", "Mystery has no page yet");
    }


    /// Sixteen mounted choices, two package groups, a semantic status, and
    /// one nonaddressable row that can produce an additional refusal status.
    struct GroupedSearch;
    impl PageReader for GroupedSearch {
        fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
            if let ReadRequest::Search(query) | ReadRequest::SearchMore { query, .. } = request {
                let mut rows = Vec::new();
                let many = query.text.as_ref() != "short";
                let count = if many { 16 } else { 1 };
                for index in 0..count {
                    let package = if index < 8 { crate::shell::tests::PACKAGE } else { "/fixture/elsewhere" };
                    let name = if many { format!("Choice{index:02}") } else { "FreshFirst".to_owned() };
                    let nonaddressable = index == 8;
                    let coordinate = if nonaddressable { "::UnplacedChoice".to_owned() } else { format!("{package}::glyph.rs:138::{name}") };
                    rows.push(SearchRow {
                        rank: index,
                        decl: DeclRef::from_label(&coordinate, None, Some(DeclarationKind::Struct), None).expect("typed result coordinate"),
                        package: (!nonaddressable).then(|| Arc::from(package)),
                        score: Known::Unknown(Gap::new(GapReason::NotServed, "")),
                        signature: Known::Unknown(Gap::new(GapReason::NotServed, "")),
                        snippet: None, reason: MatchReason::ExactName,
                    });
                }
                return Ok(PageValue::Search(SearchPage {
                    query: Arc::clone(&query.text), rows: rows.into(),
                    coverage: backend_present::CoverageLine::new(&[], Some(count as u64)).with_semantic_search_status(
                        backend_library::SemanticSearchStatus::Unavailable { reason: backend_library::SemanticSearchReason::Unconfigured }),
                    next: None,
                }));
            }
            Fixture.read(request, context)
        }
    }

    /// Visibility is read from the actual native node bounds and actual
    /// scroll viewport, not from the result-to-child mapping under test.
    fn assert_native_result_visible(rig: &mut crate::shell::tests::Rig, prefix: &str) {
        rig.repaint();
        let ask = rig.shell.read_with(rig.cx, |shell, _| shell.ask_entity());
        let viewport = ask.read_with(rig.cx, |ask, _| ask.scroll.bounds());
        let json = rig.cx.update(|window, _| window.debug_a11y_tree_json()).expect("native result tree");
        let tree: serde_json::Value = serde_json::from_str(&json).expect("native tree JSON");
        let node = tree["nodes"].as_object().expect("nodes").values().find(|node|
            node["aria"]["label"].as_str().is_some_and(|label| label.starts_with(prefix))).expect("selected native result node");
        let top = node["bounds"]["y"].as_f64().expect("native row top");
        let height = node["bounds"]["height"].as_f64().expect("native row height");
        assert!(top >= f64::from(f32::from(viewport.top())) - 0.5
            && top + height <= f64::from(f32::from(viewport.bottom())) + 0.5,
            "selected row must fit the painted scroll viewport: top={top}, height={height}, viewport={viewport:?}");
    }

    #[gpui::test]
    fn native_arrows_scroll_selected_rows_past_statuses_and_both_group_headers(cx: &mut TestAppContext) {
        let mut rig = rig_with_reads(cx, Some(page_route("RelationLabel")), 1440.0, 360.0,
            ReadPool::start(1, |_| GroupedSearch).expect("grouped search reader"));
        native_ask(&mut rig);
        rig.cx.simulate_input("many");
        rig.settle();
        let ask = rig.shell.read_with(rig.cx, |shell, _| shell.ask_entity());
        assert_eq!(ask.read_with(rig.cx, |ask, cx| ask.choices(cx).len()), 16);
        for _ in 0..9 { rig.keys("down"); }
        assert_eq!(ask.read_with(rig.cx, |ask, _| ask.selected), 8);
        rig.native_press("enter");
        rig.settle();
        assert_refusal(&mut rig, "many", "UnplacedChoice has no page yet");
        for _ in 0..7 { rig.keys("down"); }
        assert_eq!(ask.read_with(rig.cx, |ask, _| ask.selected), 15);
        assert_native_result_visible(&mut rig, "Result 16:");
        assert!(ask.read_with(rig.cx, |ask, _| ask.scroll.offset().y < gpui::px(0.0)), "actual mounted results overflow and scroll");
        for _ in 0..15 { rig.keys("up"); }
        assert_native_result_visible(&mut rig, "Result 1:");
    }

    #[gpui::test]
    fn new_native_query_and_fresh_visit_retire_old_scroll_and_pending_item(cx: &mut TestAppContext) {
        let mut rig = rig_with_reads(cx, Some(page_route("RelationLabel")), 1440.0, 360.0,
            ReadPool::start(1, |_| GroupedSearch).expect("grouped search reader"));
        native_ask(&mut rig);
        rig.cx.simulate_input("many"); rig.settle();
        for _ in 0..16 { rig.keys("down"); }
        let ask = rig.shell.read_with(rig.cx, |shell, _| shell.ask_entity());
        assert!(ask.read_with(rig.cx, |ask, _| ask.scroll.offset().y < gpui::px(0.0)));
        let covered_offset = ask.read_with(rig.cx, |ask, _| ask.scroll.offset());
        rig.keys("secondary-o"); rig.keys("escape");
        assert_eq!(ask.read_with(rig.cx, |ask, _| ask.scroll.offset()), covered_offset, "covering and uncovering the same Ask visit preserves its scroll");
        rig.keys("secondary-a"); rig.cx.simulate_input("short"); rig.settle();
        assert_eq!(ask.read_with(rig.cx, |ask, _| ask.scroll.offset().y), gpui::px(0.0));
        assert_native_result_visible(&mut rig, "Result 1:");
        rig.keys("secondary-a"); rig.cx.simulate_input("many"); rig.settle();
        for _ in 0..16 { rig.keys("down"); }
        assert!(ask.read_with(rig.cx, |ask, _| ask.scroll.offset().y < gpui::px(0.0)));
        rig.keys("escape"); rig.keys("secondary-k");
        rig.cx.simulate_input("many"); rig.settle();
        assert_eq!(ask.read_with(rig.cx, |ask, _| ask.scroll.offset().y), gpui::px(0.0));
        assert_native_result_visible(&mut rig, "Result 1:");
    }

    #[gpui::test]
    fn native_tab_reveals_the_final_all_results_action(cx: &mut TestAppContext) {
        let mut rig = rig_with_reads(cx, Some(page_route("RelationLabel")), 1440.0, 360.0,
            ReadPool::start(1, |_| GroupedSearch).expect("grouped search reader"));
        native_ask(&mut rig);
        rig.cx.simulate_input("many"); rig.settle();
        // Fifteen addressable rows; the nonaddressable row is not a stop.
        for _ in 0..16 { rig.keys("tab"); }
        let ask = rig.shell.read_with(rig.cx, |shell, _| shell.ask_entity());
        assert!(rig.cx.update(|window, cx| ask.read(cx).all_focus.is_focused(window)));
        assert_native_result_visible(&mut rig, "Open search results");
        rig.keys("shift-tab");
        assert_native_result_visible(&mut rig, "Result 16:");
    }

}
