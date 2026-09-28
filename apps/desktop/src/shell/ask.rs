//! Ask (⌘K): one field, one list, one reason per row, a short preview.
//!
//! Typing asks the store for a search through the read pool (latest wins:
//! a newer query supersedes the older one, whose rows never land). The list
//! re-renders only when its own query's page lands. With an empty field the
//! list waits for a question (the jump bar's back menu holds where you
//! have been).
//!
//! PLACEHOLDER frame: the dialog plate is composed from facet primitives
//! until `facet::overlay::float` exports the dialog; the field is
//! `gpui_component`'s input (IME) until the facet input lands.

use super::kit::{kind_of, symbol_route, text};
use super::region::Links;
use crate::model::pages::{MatchReason, PageKey, SearchQuery, SearchRow};
use crate::navigation::{BrowseRoute, Intent, OrbitRoute, Route};
use crate::runtime::store::StoreEvent;
use facet::icons::{self, Icon, IconSize, KindSize};
use facet::paint::{Bevel, Chamfer, cut};
use facet::tokens::ty;
use facet::{ActiveFacet as _, Measure, Space};
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Task, Window, ScrollHandle, div, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use std::time::Duration;

/// How long typing rests before the query is asked.
const SETTLE: Duration = Duration::from_millis(90);

/// One row of the list.
#[derive(Clone)]
struct Choice {
    name: SharedString,
    place: SharedString,
    reason: SharedString,
    kind: icons::Kind,
    route: Option<Route>,
}

/// The Ask surface.
pub(crate) struct Ask {
    links: Links,
    input: Entity<InputState>,
    query: Option<SearchQuery>,
    /// Trail mode: the list is history, not results.
    selected: usize,
    renders: u64,
    pending: Option<Task<()>>,
    scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl Ask {
    pub(crate) fn new(links: Links, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Ask anything, or find a package"));
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
            renders: 0,
            pending: None,
            scroll: ScrollHandle::new(),
            _subscriptions: vec![typed, landed],
        }
    }

    pub(crate) const fn renders(&self) -> u64 {
        self.renders
    }

    /// Opens fresh: empty field, focused.
    pub(crate) fn opened(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = 0;
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

    /// Moves the selection.
    pub(crate) fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.choices(cx).len();
        if count == 0 {
            return;
        }
        self.selected = self.selected.saturating_add_signed(delta).min(count - 1);
        if self.selected + 1 < count { self.scroll.scroll_to_item(self.selected); }
        cx.notify();
    }

    /// Opens the selected row. Dead end #14: a row with no place does not
    /// silently do nothing — the Notice says there is nowhere to go.
    pub(crate) fn choose(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let choices = self.choices(cx);
        let Some(choice) = choices.get(self.selected) else { return };
        match choice.route.clone() {
            Some(route) => self.links.dispatch(Intent::Navigate(route), cx),
            None => {
                // The Notice is a page-foot fixture (never drawn under an
                // overlay); closing Ask, exactly as a real navigation
                // would, is what makes it visible at all.
                self.links.dispatch(Intent::DismissOverlay, cx);
                let snapshot = self.links.snapshot(cx);
                let notice = crate::runtime::graph_focus::Notice {
                    visit: snapshot.route().clone(),
                    root: snapshot.key(),
                    message: format!("{} has no page yet", choice.name).into(),
                };
                self.links.store.update(cx, |store, cx| store.set_notice(Some(notice), cx));
            }
        }
    }

    fn choices(&self, cx: &App) -> Vec<Choice> {
        let Some(query) = &self.query else {
            return vec![Choice {
                name: "Find packages".into(),
                place: "Explore what your index knows".into(),
                reason: "open Find".into(),
                kind: icons::Kind::Package,
                route: Some(Route::Orbit(OrbitRoute::Browse(BrowseRoute::FindHome))),
            }];
        };
        let store = self.links.store.read(cx);
        let results = store.search(query);
        let mut choices: Vec<Choice> = results
            .loaded_value()
            .map(|page| page.rows.iter().take(8).map(result_choice).collect())
            .unwrap_or_default();
        choices.push(Choice {
            name: "All answers as a page".into(),
            place: query.text.to_string().into(),
            reason: "explore packages".into(),
            kind: icons::Kind::Package,
            route: Some(Route::Orbit(OrbitRoute::Browse(BrowseRoute::Find(query.clone())))),
        });
        choices
    }

    /// Whether a search for the current query is still on its way.
    fn searching(&self, cx: &App) -> bool {
        self.query
            .as_ref()
            .is_some_and(|query| self.pending.is_some() && !self.links.store.read(cx).search(query).is_loaded())
    }
}

fn result_choice(row: &SearchRow) -> Choice {
    let reason = match row.reason {
        MatchReason::ExactName => "exact name",
        MatchReason::Name => "in the name",
        MatchReason::Signature => "in the signature",
        MatchReason::Docs => "in the docs",
        MatchReason::Producer => "the index's pick",
    };
    let place = row.package.as_ref().map_or_else(
        || row.decl.path.as_deref().unwrap_or_default().to_owned(),
        |package| {
            let name = crate::model::pages::PackageRef::parse(package).map_or_else(|_| package.to_string(), |package| package.display_name().to_owned());
            match &row.decl.path {
                Some(path) => format!("{name} · {path}"),
                None => name,
            }
        },
    );
    let route = row
        .decl
        .coordinate
        .package()
        .and_then(|package| symbol_route(package.as_str(), &row.decl.coordinate));
    Choice {
        name: row.decl.name.to_string().into(),
        place: place.into(),
        reason: reason.into(),
        kind: kind_of(row.decl.kind),
        route,
    }
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
        self.renders = self.renders.saturating_add(1);
        let facet = cx.facet();
        let palette = facet.palette();
        let viewport = window.viewport_size();
        let width = (viewport.width - px(32.0)).min(px(680.0 * facet.text_scale)).max(px(0.0));
        let measure = Measure::new(width, &facet);
        let choices = self.choices(cx);
        let searching = self.searching(cx);
        let count: SharedString = if self.query.is_none() {
            "a name, a shape, or a question".into()
        } else if searching {
            "asking…".into()
        } else {
            format!("{} quick answers", choices.len().saturating_sub(1)).into()
        };
        let available = (viewport.height - px(72.0 * facet.text_scale) - px(16.0 * facet.text_scale)).max(px(0.0));
        // Reserve the input, status and the two-line page door before giving
        // the remaining viewport to the independently scrolling quick list.
        let list_height = (available - px(192.0 * facet.text_scale)).max(px(0.0)).min(px(420.0 * facet.text_scale));
        let quick = choices.len().saturating_sub(1);
        let mut list = div().id("ask-results").flex().flex_col().py(measure.space(Space::Tight))
            .min_h(px(0.0)).max_h(list_height).overflow_y_scroll().track_scroll(&self.scroll);
        for (index, choice) in choices.iter().enumerate().take(quick) {
            let on = index == self.selected;
            let has_place = choice.route.is_some();
            let mut row = div()
                .id(("ask-row", index))
                .flex()
                .items_center()
                .gap(measure.space(Space::Roomy))
                .px(measure.space(Space::Gutter))
                .py(measure.space(Space::Snug))
                .when_on(on, palette)
                .child(super::kit::kind_mark(choice.kind, KindSize::Sm, &measure, palette))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w(px(0.0))
                        .flex_1()
                        // Dead end #14/#15: a row with no place is drawn as
                        // text (kit's rule), never as a link that goes
                        // nowhere.
                        .child(text(ty::MONO_ROW, &measure, super::kit::link_ink(has_place, palette)).child(choice.name.clone()))
                        .child(
                            text(ty::MONO_SMALL, &measure, palette.ink3)
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(choice.place.clone()),
                        ),
                )
                .children((measure.effective() >= 440.0).then(|| text(ty::CAPTION, &measure, palette.ink3).flex_none().child(choice.reason.clone())));
            if let Some(route) = choice.route.clone() {
                let links = self.links.clone();
                row = row.hover(|style| style.bg(palette.tint)).on_click(move |_: &ClickEvent, _, cx| {
                    links.dispatch(Intent::Navigate(route.clone()), cx);
                });
            }
            list = list.child(row);
        }
        let footer = choices.last().map(|choice| {
            let links = self.links.clone();
            let route = choice.route.clone();
            div().id("ask-find-page").flex().flex_none().items_center().gap(measure.space(Space::Roomy))
                .px(measure.space(Space::Gutter)).py(measure.space(Space::Roomy))
                .when_on(self.selected == quick, palette).hover(|style| style.bg(palette.tint)).cursor_pointer()
                .child(super::kit::kind_mark(icons::Kind::Package, KindSize::Sm, &measure, palette))
                .child(div().flex_1().min_w_0().flex().flex_col()
                    .child(text(ty::ROW, &measure, palette.ink0).child("All answers as a page"))
                    .child(text(ty::CAPTION, &measure, palette.ink3).child(if measure.effective() < 440.0 { "Inspect and compare packages" } else { "Inspect packages and compare what they expose" })))
                .on_click(move |_, _, cx| { if let Some(route) = route.clone() { links.dispatch(Intent::Navigate(route), cx); } })
        });
        if choices.is_empty() && !searching {
            let words = if self.query.is_some() { "Nothing matches that yet." } else { "Nothing walked yet." };
            list = list.child(div().px(measure.space(Space::Gutter)).py(measure.space(Space::Roomy)).child(super::kit::quiet(words, &measure, palette)));
        }
        let field = Input::new(&self.input).appearance(false).bordered(false);
        let semantic_status = self.query.as_ref().and_then(|query| {
            self.links
                .store
                .read(cx)
                .search(query)
                .loaded_value()
                .and_then(|page| page.coverage.semantic_search_status())
        });
        let mut surface = cut()
            .chamfer(Chamfer::Lg)
            .bevel(Bevel::Peri)
            .fill(palette.glass)
            .floating()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(measure.space(Space::Roomy))
                    .px(measure.space(Space::Gutter))
                    .h(px(52.0 * facet.text_scale))
                    .border_b_1()
                    .border_color(palette.line1.hsla())
                    .child(icons::ui(Icon::Search, IconSize::S16, palette.ink2).size(measure.icon(16.0)))
                    .child(div().flex_1().min_w(px(0.0)).set_ui(&measure, palette).child(field))
                    .child(text(ty::MONO_SMALL, &measure, palette.ink3).flex_none().child(count)),
            );
        if let Some(status) = semantic_status {
            surface = surface.child(
                div()
                    .px(measure.space(Space::Gutter))
                    .py(measure.space(Space::Snug))
                    .child(text(ty::MONO_SMALL, &measure, palette.ink3).child(semantic_search_label(status))),
            );
        }
        surface = surface.child(div().max_h(px(420.0 * facet.text_scale)).overflow_hidden().child(list));
        div()
            .id("ask")
            .w(width)
            .child(surface.max_h(available).children(footer))
    }
}

trait AskStyle: Styled + Sized {
    fn when_on(self, on: bool, palette: &facet::Palette) -> Self {
        if on { self.bg(palette.plate2) } else { self }
    }

    fn set_ui(self, measure: &Measure, palette: &facet::Palette) -> Self {
        use facet::Set as _;
        self.set(ty::HEAD, measure).text_color(palette.ink0.hsla())
    }
}

impl<E: Styled> AskStyle for E {}

#[allow(dead_code)]
fn _any(_: AnyElement) {}

#[cfg(test)]
mod tests {
    use super::*;
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
        let ask = rig.shell.read_with(rig.cx, |shell, _| shell.ask_entity());
        rig.cx.update(|_, cx| ask.update(cx, |ask, cx| ask.typed("mystery".to_owned(), cx)));
        rig.frame(120);
        rig.settle();
        let route_before = rig.route();
        rig.cx.update(|window, cx| ask.update(cx, |ask, cx| ask.choose(window, cx)));
        assert_eq!(rig.route(), route_before, "a row with no place does not move the page");
        let (ask_open, _, _) = rig.shell.read_with(rig.cx, |shell, _| shell.transients());
        assert!(!ask_open, "Ask closes so the Notice (a page-foot fixture) is visible");
        let message = rig.graph.store.read_with(rig.cx, |store, _| store.notice().map(|notice| notice.message.to_string()));
        assert_eq!(message.as_deref(), Some("Mystery has no page yet"));
    }
}
