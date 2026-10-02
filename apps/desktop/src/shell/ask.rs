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

use super::kit::{kind_of, symbol_route, text};
use super::region::Links;
use crate::core::{ReadHoldReason, Resource, ResourceAdmission, ResourceTerminal, VersionedRoot, admit_resource};
use crate::model::pages::{MatchReason, PageKey, SearchPage, SearchQuery, SearchRow};
use crate::navigation::{BrowseRoute, Intent, OrbitRoute, Overlay, Route};
use crate::runtime::store::{Branch, StoreEvent};
use facet::icons::{self, KindSize};
use facet::tokens::ty;
use facet::{ActiveFacet as _, Measure, Space};
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, FocusHandle, Focusable as _, InteractiveElement, KeyDownEvent, Role,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Task, Window, ScrollHandle, div, px,
};
use gpui_component::input::{InputEvent, InputState};
use std::time::Duration;

/// How long typing rests before the query is asked.
const SETTLE: Duration = Duration::from_millis(90);

/// Rows per group before the rest fold into "all results".
const PER_GROUP: usize = 8;

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

/// The query surface.
pub(crate) struct Ask {
    links: Links,
    input: Entity<InputState>,
    query: Option<SearchQuery>,
    selected: usize,
    /// Whether ↑ ↓ have walked the rows since the last keystroke: ↵ then
    /// keeps the shown place instead of opening the first row.
    walked: bool,
    renders: u64,
    pending: Option<Task<()>>,
    scroll: ScrollHandle,
    /// Native focus belongs to the typed destination, not its current row.
    row_focus: Vec<(Route, usize, FocusHandle)>,
    all_focus: FocusHandle,
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
            let search_changed = matches!((event, &ask.query),
                (StoreEvent::Resource(PageKey::Search(query)), Some(mine)) if query == mine);
            if search_changed || event.is_branch(Branch::Root) {
                cx.notify();
            }
        });
        Self {
            links,
            input,
            query: None,
            selected: 0,
            walked: false,
            renders: 0,
            pending: None,
            scroll: ScrollHandle::new(),
            row_focus: Vec::new(),
            all_focus: cx.focus_handle().tab_stop(true),
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

    /// Opens fresh: empty field, focused.
    pub(crate) fn opened(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = 0;
        self.walked = false;
        self.query = None;
        self.pending = None;
        self.input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    fn typed(&mut self, text: String, cx: &mut Context<Self>) {
        self.selected = 0;
        let query = SearchQuery::new(&text, SearchQuery::DEFAULT_LIMIT).ok();
        if query == self.query {
            return;
        }
        // A new query shows where you were again until you walk its rows.
        if self.walked || self.links.snapshot(cx).session().preview.is_some() {
            self.links.dispatch(Intent::EndPreview, cx);
        }
        self.walked = false;
        self.query = query.clone();
        cx.notify();
        let Some(query) = query else {
            self.pending = None;
            return;
        };
        let store = self.links.store.clone();
        // Latest wins: a newer keystroke drops this timer, and the store
        // supersedes an older query's read.
        self.pending = Some(cx.spawn(async move |_, cx| {
            cx.background_executor().timer(SETTLE).await;
            let _ = cx.update(|cx| {
                store.update(cx, |store, cx| store.ensure(PageKey::Search(query), cx));
            });
        }));
    }

    /// The modal's editor and mounted links are its native keyboard owners.
    pub(crate) fn owns_focus(&self, window: &Window, cx: &App) -> bool {
        self.input.read(cx).focus_handle(cx).is_focused(window)
            || self.row_focus.iter().any(|(_, _, handle)| handle.is_focused(window))
            || self.all_focus.is_focused(window)
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
        self.scroll.scroll_to_item(self.selected);
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
            self.scroll.scroll_to_item(index);
        } else if next == 0 {
            self.selected = 0;
            self.walked = false;
        }
        cx.notify();
    }

    /// Returns true when a focused destination vanished from the mounted plate.
    fn sync_row_focus(&mut self, choices: &[Choice], all_mounted: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let lost_focus = self.row_focus.iter().any(|(route, occurrence, handle)| {
            handle.is_focused(window)
                && !choices.iter().any(|choice| choice.route.as_ref() == Some(route) && choice.route_occurrence == *occurrence)
        }) || (!all_mounted && self.all_focus.is_focused(window));
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
            let editor = self.input.read(cx).focus_handle(cx);
            editor.focus(window, cx);
            self.selected = 0;
            self.walked = false;
            cx.notify();
        }
        lost_focus
    }

    /// ↵: keeps the place the walk is showing, or opens the chosen row.
    /// Dead end #14: a row with no place does not silently do nothing — the
    /// Notice says there is nowhere to go.
    pub(crate) fn choose(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let choices = self.choices(cx);
        let Some(choice) = choices.get(self.selected) else {
            if self.query.is_some() {
                let message = self.read_status(cx).0.unwrap_or_else(|| "No result matches this query.".into());
                search_notice(&self.links, message, cx);
            }
            return;
        };
        let previewing = self.links.snapshot(cx).session().preview.is_some();
        match choice.route.clone() {
            Some(route) if !self.query.as_ref().is_some_and(|query| current_row_route(&self.links, query, &route, cx)) => {
                stale_search_notice(&self.links, cx);
            }
            Some(route) if previewing && self.walked && self.links.snapshot(cx).route() == &route => {
                self.links.dispatch(Intent::CommitPreview, cx);
                self.links.dispatch(Intent::DismissOverlay, cx);
            }
            Some(route) => self.links.dispatch(Intent::Navigate(route), cx),
            None => {
                // The Notice is a page-foot fixture (never drawn under an
                // overlay); closing the query, exactly as a real navigation
                // would, is what makes it visible at all.
                self.links.dispatch(Intent::DismissOverlay, cx);
                let snapshot = self.links.snapshot(cx);
                let words = choice.unavailable.clone().unwrap_or_else(|| format!("{} has no page yet", choice.name).into());
                let notice = crate::runtime::graph_focus::Notice {
                    visit: snapshot.route().clone(),
                    root: snapshot.key(),
                    message: std::sync::Arc::<str>::from(words.as_ref()),
                    retry: None,
                };
                self.links.store.update(cx, |store, cx| store.set_notice(Some(notice), cx));
            }
        }
    }

    /// ⌘↵ opens Find only while this exact query has a current served page.
    fn choose_all(&self, cx: &mut Context<Self>) {
        if self.choices(cx).is_empty() { return; }
        if let (Some(query), Some(route)) = (&self.query, self.all_results(cx)) {
            follow_all(&self.links, &self.input, query, &route, cx);
        }
    }

    /// The route for "every result, as a page" (⌘↵).
    pub(crate) fn all_results(&self, cx: &App) -> Option<Route> {
        let query = self.query.clone()?;
        let (results, root, serving) = self.search_resource(cx)?;
        admit_resource(&results, root, serving).current_value()
            .is_some_and(|page| page.query.as_ref() == query.text.as_ref())
            .then(|| Route::Orbit(OrbitRoute::Browse(BrowseRoute::Find(query))))
    }

    /// What the jump bar says at the query's end: how many, and where.
    pub(crate) fn count(&self, cx: &App) -> SharedString {
        if self.query.is_none() {
            return SharedString::default();
        }
        if let Some((results, root, serving)) = self.search_resource(cx) {
            match admit_resource(&results, root, serving) {
                ResourceAdmission::Current(page) if self.query.as_ref().is_some_and(|query| page.query.as_ref() != query.text.as_ref()) =>
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
        self.query.is_some()
    }

    fn choices(&self, cx: &App) -> Vec<Choice> {
        let Some(query) = &self.query else { return Vec::new() };
        let snapshot = self.links.snapshot(cx);
        let here = route_package(snapshot.committed_route()).map(str::to_owned);
        let Some((results, root, serving)) = self.search_resource(cx) else { return Vec::new() };
        let admission = admit_resource(&results, root, serving);
        let (page, unavailable) = match admission {
            ResourceAdmission::Current(page) => (page, None),
            ResourceAdmission::Retained { value, reason } =>
                (value, Some(format!("Earlier result; {}; cannot open until verified", read_hold_words(reason)).into())),
            ResourceAdmission::Failed { retained: Some(value), .. } =>
                (value, Some("Search failed; earlier result cannot be opened".into())),
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
        let query = self.query.as_ref()?;
        let store = self.links.store.read(cx);
        Some((store.search(query), store.snapshot().key(), store.owner_serving()))
    }

    /// The same admission controls what the plate says and which links it
    /// offers. Older rows may remain visible, but cannot masquerade as live.
    fn read_status(&self, cx: &App) -> (Option<SharedString>, Option<backend_library::SemanticSearchStatus>) {
        let Some((results, root, serving)) = self.search_resource(cx) else { return (None, None) };
        match admit_resource(&results, root, serving) {
            ResourceAdmission::Current(page) if self.query.as_ref().is_some_and(|query| page.query.as_ref() != query.text.as_ref()) =>
                (Some("Search replied for a different query; links unavailable.".into()), None),
            ResourceAdmission::Current(page) => (None, page.coverage.semantic_search_status()),
            ResourceAdmission::Retained { reason, .. } => {
                (Some(format!("Earlier search results · {} · links unavailable", read_hold_words(reason)).into()), None)
            }
            ResourceAdmission::Pending(reason) => {
                (Some(match reason {
                    ReadHoldReason::OwnerUnavailable => "Search is waiting for the index owner.".into(),
                    ReadHoldReason::AuthorityChanged => "Search is waiting for the current index.".into(),
                    ReadHoldReason::Reading | ReadHoldReason::NotReady => "Searching the library…".into(),
                }), None)
            }
            ResourceAdmission::Failed { terminal, retained } => {
                let detail = match terminal {
                    ResourceTerminal::Fault(error) => format!("Search failed: {}", error.message()),
                    ResourceTerminal::Unavailable(_) => "The index does not provide search.".to_owned(),
                    ResourceTerminal::Complete | ResourceTerminal::Partial => "Search is unavailable.".to_owned(),
                };
                (Some(format!("{detail}{}", if retained.is_some() { " Earlier results are shown without links." } else { "" }).into()), None)
            }
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

/// The package a route reads, when it reads one.
fn route_package(route: &Route) -> Option<&str> {
    match route {
        Route::Package(route) => Some(route.package.as_str()),
        Route::Symbol(route) => Some(route.package.as_str()),
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
    let package = row.decl.coordinate.package()?;
    (row.package.as_deref() == Some(package.as_str())).then(|| symbol_route(package.as_str(), &row.decl.coordinate)).flatten()
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

fn search_notice(links: &Links, message: SharedString, cx: &mut App) {
    links.dispatch(Intent::DismissOverlay, cx);
    let snapshot = links.snapshot(cx);
    let notice = crate::runtime::graph_focus::Notice {
        visit: snapshot.route().clone(),
        root: snapshot.key(),
        message: std::sync::Arc::<str>::from(message.as_ref()),
        retry: None,
    };
    links.store.update(cx, |store, cx| store.set_notice(Some(notice), cx));
}

fn stale_search_notice(links: &Links, cx: &mut App) {
    search_notice(links, "That search result is no longer verified by the current index. Search again.".into(), cx);
}

fn follow_row(links: &Links, input: &Entity<InputState>, query: &SearchQuery, route: &Route, cx: &mut App) {
    if links.snapshot(cx).overlay() != Some(Overlay::CommandPalette) || !current_input_query(input, query, cx) {
        return;
    }
    if current_row_route(links, query, route, cx) {
        links.dispatch(Intent::Navigate(route.clone()), cx);
    } else {
        stale_search_notice(links, cx);
    }
}

fn current_row_route(links: &Links, query: &SearchQuery, route: &Route, cx: &App) -> bool {
    current_search(links, query, cx, |page| page.rows.iter().take(usize::from(query.limit))
        .any(|row| row_route(row).as_ref() == Some(route)))
}

fn follow_all(links: &Links, input: &Entity<InputState>, query: &SearchQuery, route: &Route, cx: &mut App) {
    if links.snapshot(cx).overlay() != Some(Overlay::CommandPalette) || !current_input_query(input, query, cx) {
        return;
    }
    if current_search(links, query, cx, |_| true) {
        links.dispatch(Intent::Navigate(route.clone()), cx);
    } else {
        stale_search_notice(links, cx);
    }
}

/// Where `query` sits in `name`, ignoring case: a whole-word query first,
/// else its first run of letters (a question's words don't underline).
fn matched_range(name: &str, query: &str) -> Option<std::ops::Range<usize>> {
    let needle = query.trim();
    if needle.is_empty() || needle.contains(char::is_whitespace) {
        return None;
    }
    let lower = name.to_lowercase();
    // Byte offsets agree only when lowercasing kept every length (ASCII
    // names, the common case); otherwise nothing is underlined.
    if lower.len() != name.len() {
        return None;
    }
    let at = lower.find(&needle.to_lowercase())?;
    Some(at..at + needle.len())
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
        let mut list = div().id("ask-results").role(Role::List).aria_label("Search results")
            .flex().flex_col().pt(measure.space(Space::Tight))
            .size_full().overflow_y_scroll().track_scroll(&self.scroll);
        if let Some(words) = read_status.clone() {
            list = list.child(
                div().id("ask-read-status").role(Role::Status).aria_label(words.clone())
                    .px(measure.space(Space::Gutter)).py(measure.space(Space::Snug))
                    .child(text(ty::MONO_SMALL, &measure, palette.ink3).child(words)),
            );
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
            list = list.child(self.row(index, choice, &measure, palette));
        }
        if choices.is_empty() && self.query.is_some() && read_status.is_none() {
            // Never an empty plate: what the search is doing, or why it
            // could not answer.
            let words: SharedString = "Nothing matches that yet.".into();
            list = list.child(div().id("ask-said").px(measure.space(Space::Gutter)).py(measure.space(Space::Roomy)).child(super::kit::quiet(words, &measure, palette)));
        }
        if let Some(route) = all_results {
            let links = self.links.clone();
            let input = self.input.clone();
            let query = self.query.clone().expect("an all-results route has a query");
            list = list.child(
                div().id("ask-find-page").flex().flex_none().items_center().gap(measure.space(Space::Roomy))
                    .role(Role::Link).aria_label("Open every search result as a page")
                    .track_focus(&self.all_focus).tab_stop(true)
                    .h(measure.row() + measure.space(Space::Snug)).px(measure.space(Space::Gutter)).mt(measure.space(Space::Tight))
                    .border_t_1().border_color(palette.line1.hsla())
                    .hover(|style| style.bg(palette.tint)).focus_visible(|style| style.bg(palette.tint)).cursor_pointer()
                    .child(text(ty::SMALL, &measure, palette.ink2).child("every result, as a page"))
                    .on_click({
                        let route = route.clone();
                        let query = query.clone();
                        let links = links.clone();
                        let input = input.clone();
                        move |_: &ClickEvent, _, cx| follow_all(&links, &input, &query, &route, cx)
                    })
                    .on_key_down(move |event: &KeyDownEvent, _, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space")
                            && !event.keystroke.modifiers.modified() && !event.is_held
                        {
                            follow_all(&links, &input, &query, &route, cx);
                            cx.stop_propagation();
                        }
                    }),
            );
        }
        div()
            .id("ask")
            .size_full()
            .bg(palette.g2)
            .border_r_1()
            .border_color(palette.line2.hsla())
            .child(list)
            .into_any_element()
    }

    fn row(&self, index: usize, choice: &Choice, measure: &Measure, palette: &facet::Palette) -> AnyElement {
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
            let query = self.query.clone().expect("a result route has a query");
            let handle = self.row_focus.iter().find(|(known, occurrence, _)| known == &route && *occurrence == choice.route_occurrence)
                .map(|(_, _, handle)| handle).expect("every mounted route has a focus handle");
            row = row.track_focus(handle).tab_stop(true).focus_visible(|style| style.bg(palette.tint))
                .cursor_pointer().hover(|style| style.bg(palette.tint))
                .on_click({
                    let route = route.clone();
                    let query = query.clone();
                    let links = links.clone();
                    let input = input.clone();
                    move |_: &ClickEvent, _, cx| follow_row(&links, &input, &query, &route, cx)
                })
                .on_key_down(move |event: &KeyDownEvent, _, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space")
                        && !event.keystroke.modifiers.modified() && !event.is_held
                    {
                        follow_row(&links, &input, &query, &route, cx);
                        cx.stop_propagation();
                    }
                });
        }
        row.into_any_element()
    }
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
    use super::{Choice, Group, step_index};
    use crate::core::{LocalProjectId, VersionedRoot};
    use crate::model::{AppSnapshot, SessionState};
    use crate::model::pages::{DeclRef, Gap, GapReason, Known, MatchReason, PageValue, ReadFailure, SearchPage, SearchRow};
    use crate::runtime::actor::EngineActor;
    use crate::runtime::reads::{PageReader, ReadContext, ReadPool, ReadRequest};
    use crate::runtime::{DesktopRuntime, UiEntityGraph};
    use crate::shell::root::Shell;
    use crate::shell::tests::{Fixture, RootOnly, page_route, rig_with_reads};
    use backend_library::DeclarationKind;
    use gpui::{AppContext as _, Focusable as _, TestAppContext, VisualTestContext};
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

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
        cx.update(|cx| {
            gpui_component::init(cx);
            let _ = facet::fonts::install(cx);
            crate::shell::bodies::graph::install_test_fixture(cx);
        });
        let mut snapshot = AppSnapshot::empty(VersionedRoot::synthetic(
            backend_library::view_state_root(&[("shell".to_owned(), "ask-tab".to_owned())]), 4,
        ));
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
        visual.update(|window, _| window.activate_window());
        let draw = |visual: &mut VisualTestContext| {
            visual.update(|window, cx| window.draw(cx).clear(cx));
            visual.run_until_parked();
        };
        draw(visual);
        visual.simulate_keystrokes("cmd-k");
        draw(visual);
        let ask = shell.read_with(visual, |shell, _| shell.ask_entity());
        let input = ask.read_with(visual, |ask, _| ask.input().clone());
        visual.update(|window, cx| input.update(cx, |input, cx| input.replace_all("RelationLabel", window, cx)));
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            visual.executor().advance_clock(Duration::from_millis(200));
            draw(visual);
            if ask.read_with(visual, |ask, cx| !ask.choices(cx).is_empty()) { break; }
            assert!(Instant::now() < deadline, "the fixture's served Ask result never mounted");
            std::thread::sleep(Duration::from_millis(2));
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
    /// ⏎, and the Notice says why instead of doing nothing silently.
    #[gpui::test]
    fn a_row_with_no_place_speaks_through_the_notice_on_enter(cx: &mut TestAppContext) {
        let pool = ReadPool::start(2, |_| NoPlaceSearch).expect("pool");
        let mut rig = rig_with_reads(cx, Some(page_route("RelationLabel")), 1440.0, 900.0, pool);
        rig.keys("cmd-k");
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
        rig.cx.update(|_, cx| ask.update(cx, |ask, cx| ask.typed("mystery".to_owned(), cx)));
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
        rig.cx.update(|window, cx| ask.update(cx, |ask, cx| ask.choose(window, cx)));
        assert_eq!(rig.route(), route_before, "a row with no place does not move the page");
        let (ask_open, _, _) = rig.shell.read_with(rig.cx, |shell, _| shell.transients());
        assert!(!ask_open, "Ask closes so the Notice (a page-foot fixture) is visible");
        let message = rig.graph.store.read_with(rig.cx, |store, _| store.notice().map(|notice| notice.message.to_string()));
        assert_eq!(message.as_deref(), Some("Mystery has no page yet"));
    }
}
