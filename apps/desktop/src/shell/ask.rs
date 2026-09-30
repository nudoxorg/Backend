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
use crate::model::pages::{MatchReason, PageKey, SearchQuery, SearchRow};
use crate::navigation::{BrowseRoute, Intent, OrbitRoute, Route};
use crate::runtime::store::StoreEvent;
use facet::icons::{self, KindSize};
use facet::tokens::ty;
use facet::{ActiveFacet as _, Measure, Space};
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, InteractiveElement,
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
            InputEvent::PressEnter { .. } => ask.choose(window, cx),
            InputEvent::Focus | InputEvent::Blur => {}
        });
        let store = links.store.clone();
        let landed = cx.subscribe(&store, |ask: &mut Self, _, event: &StoreEvent, cx| {
            if let (StoreEvent::Resource(PageKey::Search(query)), Some(mine)) = (event, &ask.query)
                && query == mine
            {
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

    /// Moves the selection and shows the row's place in the reader.
    pub(crate) fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let choices = self.choices(cx);
        if choices.is_empty() {
            return;
        }
        let next = if self.walked { self.selected.saturating_add_signed(delta).min(choices.len() - 1) } else { self.selected };
        self.selected = next;
        self.walked = true;
        self.scroll.scroll_to_item(self.selected);
        if let Some(route) = choices[next].route.clone() {
            self.links.dispatch(Intent::Preview(route), cx);
        }
        cx.notify();
    }

    /// ↵: keeps the place the walk is showing, or opens the chosen row.
    /// Dead end #14: a row with no place does not silently do nothing — the
    /// Notice says there is nowhere to go.
    pub(crate) fn choose(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let choices = self.choices(cx);
        let Some(choice) = choices.get(self.selected) else { return };
        let previewing = self.links.snapshot(cx).session().preview.is_some();
        match choice.route.clone() {
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
                let notice = crate::runtime::graph_focus::Notice {
                    visit: snapshot.route().clone(),
                    root: snapshot.key(),
                    message: format!("{} has no page yet", choice.name).into(),
                    retry: None,
                };
                self.links.store.update(cx, |store, cx| store.set_notice(Some(notice), cx));
            }
        }
    }

    /// The route for "every result, as a page" (⌘↵).
    pub(crate) fn all_results(&self) -> Option<Route> {
        self.query.clone().map(|query| Route::Orbit(OrbitRoute::Browse(BrowseRoute::Find(query))))
    }

    /// What the jump bar says at the query's end: how many, and where.
    pub(crate) fn count(&self, cx: &App) -> SharedString {
        if self.query.is_none() {
            return SharedString::default();
        }
        if self.searching(cx) {
            return "…".into();
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
        let store = self.links.store.read(cx);
        let results = store.search(query);
        let Some(page) = results.loaded_value() else { return Vec::new() };
        let mut rows: Vec<Choice> = page.rows.iter().map(|row| result_choice(row, &query.text, here.as_deref())).collect();
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
        shown
    }

    /// Whether a search for the current query is still on its way.
    fn searching(&self, cx: &App) -> bool {
        self.query
            .as_ref()
            .is_some_and(|query| self.pending.is_some() && !self.links.store.read(cx).search(query).is_loaded())
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

fn result_choice(row: &SearchRow, query: &str, here: Option<&str>) -> Choice {
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
    let route = package.and_then(|package| symbol_route(package.as_str(), &row.decl.coordinate));
    Choice {
        name: name.into(),
        matched,
        place: place.into(),
        reason: reason.map(SharedString::from),
        kind: kind_of(row.decl.kind),
        route,
        group,
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
        let searching = self.searching(cx);
        let mut list = div().id("ask-results").flex().flex_col().pt(measure.space(Space::Tight))
            .size_full().overflow_y_scroll().track_scroll(&self.scroll);
        if let Some(status) = self.query.as_ref().and_then(|query| {
            self.links
                .store
                .read(cx)
                .search(query)
                .loaded_value()
                .and_then(|page| page.coverage.semantic_search_status())
        }) {
            list = list.child(
                div()
                    .px(measure.space(Space::Gutter))
                    .py(measure.space(Space::Snug))
                    .child(text(ty::MONO_SMALL, &measure, palette.ink3).child(semantic_search_label(status))),
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
        if choices.is_empty() && let Some(query) = &self.query {
            // Never an empty plate: what the search is doing, or why it
            // could not answer.
            let terminal = self.links.store.read(cx).search(query).terminal().clone();
            let words: SharedString = match terminal {
                crate::core::ResourceTerminal::Fault(error) => format!("The index could not search: {}", error.message()).into(),
                crate::core::ResourceTerminal::Unavailable(_) => "The index does not search yet.".into(),
                crate::core::ResourceTerminal::Complete if searching => "Searching the library…".into(),
                crate::core::ResourceTerminal::Complete => "Nothing matches that yet.".into(),
            };
            list = list.child(div().id("ask-said").px(measure.space(Space::Gutter)).py(measure.space(Space::Roomy)).child(super::kit::quiet(words, &measure, palette)));
        }
        if let Some(route) = self.all_results().filter(|_| !choices.is_empty()) {
            let links = self.links.clone();
            list = list.child(
                div().id("ask-find-page").flex().flex_none().items_center().gap(measure.space(Space::Roomy))
                    .h(measure.row() + measure.space(Space::Snug)).px(measure.space(Space::Gutter)).mt(measure.space(Space::Tight))
                    .border_t_1().border_color(palette.line1.hsla())
                    .hover(|style| style.bg(palette.tint)).cursor_pointer()
                    .child(text(ty::SMALL, &measure, palette.ink2).child("every result, as a page"))
                    .on_click(move |_: &ClickEvent, _, cx| links.dispatch(Intent::Navigate(route.clone()), cx)),
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
        let mut row = div()
            .id(("ask-row", index))
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
        if on {
            row = row.bg(palette.plate2).child(
                div().absolute().left_0().top_0().bottom_0().w(px(2.0 * measure.scale())).bg(palette.peri.base.hsla()),
            );
        }
        if let Some(route) = choice.route.clone() {
            let links = self.links.clone();
            row = row.cursor_pointer().hover(|style| style.bg(palette.tint)).on_click(move |_: &ClickEvent, _, cx| {
                links.dispatch(Intent::Navigate(route.clone()), cx);
            });
        }
        row.into_any_element()
    }
}

fn group_head(words: String, count: usize, measure: &Measure, palette: &facet::Palette) -> AnyElement {
    div()
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
    use crate::model::pages::{DeclRef, Gap, GapReason, Known, MatchReason, PageValue, ReadFailure, SearchPage, SearchRow};
    use crate::runtime::reads::{PageReader, ReadContext, ReadPool, ReadRequest};
    use crate::shell::tests::{Fixture, page_route, rig_with_reads};
    use backend_library::DeclarationKind;
    use gpui::TestAppContext;
    use std::sync::Arc;

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
