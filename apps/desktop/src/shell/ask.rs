//! Ask (⌘K): one field, one list, one reason per row, a short preview.
//!
//! Typing asks the store for a search through the read pool (latest wins:
//! a newer query supersedes the older one, whose rows never land). The list
//! re-renders only when its own query's page lands. With an empty field the
//! list is the trail you walked, nearest first.
//!
//! PLACEHOLDER frame: the dialog plate is composed from facet primitives
//! until `facet::overlay::float` exports the dialog; the field is
//! `gpui_component`'s input (IME) until the facet input lands.

use super::kit::{kind_of, symbol_route, text};
use super::region::Links;
use crate::model::pages::{MatchReason, PageKey, SearchQuery, SearchRow};
use crate::navigation::{Intent, Route};
use crate::runtime::store::StoreEvent;
use facet::icons::{self, Icon, IconSize, KindSize};
use facet::paint::{Bevel, Chamfer, cut};
use facet::tokens::ty;
use facet::{ActiveFacet as _, Measure, Space};
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, FocusHandle, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Task, Window, div, px,
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
    kind: facet::icons::Kind,
    route: Option<Route>,
}

/// The Ask surface.
pub(crate) struct Ask {
    links: Links,
    input: Entity<InputState>,
    query: Option<SearchQuery>,
    /// Trail mode: the list is history, not results.
    trail: bool,
    selected: usize,
    renders: u64,
    pending: Option<Task<()>>,
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
            trail: false,
            selected: 0,
            renders: 0,
            pending: None,
            _subscriptions: vec![typed, landed],
        }
    }

    pub(crate) const fn renders(&self) -> u64 {
        self.renders
    }

    /// Opens fresh: empty field, focused; `trail` lists history.
    pub(crate) fn opened(&mut self, trail: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.trail = trail;
        self.selected = 0;
        self.query = None;
        self.pending = None;
        self.input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    /// Types `text` into the field (the harness and tests drive this).
    pub(crate) fn type_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.set_value(text.to_owned(), window, cx));
        self.typed(text.to_owned(), cx);
    }

    fn typed(&mut self, text: String, cx: &mut Context<Self>) {
        self.selected = 0;
        let query = SearchQuery::new(&text, SearchQuery::DEFAULT_LIMIT).ok();
        if query == self.query {
            return;
        }
        self.trail = false;
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
        cx.notify();
    }

    /// Opens the selected row.
    pub(crate) fn choose(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let choices = self.choices(cx);
        if let Some(route) = choices.get(self.selected).and_then(|choice| choice.route.clone()) {
            self.links.dispatch(Intent::Navigate(route), cx);
        }
    }

    fn choices(&self, cx: &App) -> Vec<Choice> {
        if self.trail || self.query.is_none() {
            return self.trail_choices(cx);
        }
        let Some(query) = &self.query else {
            return Vec::new();
        };
        let store = self.links.store.read(cx);
        let results = store.search(query);
        results
            .loaded_value()
            .map(|page| page.rows.iter().map(result_choice).collect())
            .unwrap_or_default()
    }

    fn trail_choices(&self, cx: &App) -> Vec<Choice> {
        let snapshot = self.links.snapshot(cx);
        snapshot
            .session()
            .back
            .to_vec()
            .into_iter()
            .filter_map(|route| {
                let (name, place) = match &route {
                    Route::Symbol(symbol) => {
                        let identity = backend_present::Identity::parse(symbol.id.as_str());
                        (identity.name().to_owned(), identity.to_string())
                    }
                    Route::Package(package) => (package.package.as_str().to_owned(), "package".to_owned()),
                    Route::Orbit(_) | Route::World => return None,
                };
                Some(Choice {
                    name: name.into(),
                    place: place.into(),
                    reason: "a trail you walked".into(),
                    kind: facet::icons::Kind::Unknown,
                    route: Some(route),
                })
            })
            .take(12)
            .collect()
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
        let count: SharedString = if self.trail || self.query.is_none() {
            "your trail".into()
        } else if searching {
            "asking…".into()
        } else {
            format!("{} results", choices.len()).into()
        };
        let mut list = div().flex().flex_col().py(measure.space(Space::Tight));
        for (index, choice) in choices.iter().enumerate().take(40) {
            let on = index == self.selected;
            let route = choice.route.clone();
            let links = self.links.clone();
            list = list.child(
                div()
                    .id(("ask-row", index))
                    .flex()
                    .items_center()
                    .gap(measure.space(Space::Roomy))
                    .px(measure.space(Space::Gutter))
                    .py(measure.space(Space::Snug))
                    .when_on(on, palette)
                    .hover(|style| style.bg(palette.tint))
                    .child(super::kit::kind_mark(choice.kind, KindSize::Sm, &measure, palette))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w(px(0.0))
                            .flex_1()
                            .child(text(ty::MONO_ROW, &measure, palette.ink0).child(choice.name.clone()))
                            .child(
                                text(ty::MONO_SMALL, &measure, palette.ink3)
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(choice.place.clone()),
                            ),
                    )
                    .child(text(ty::CAPTION, &measure, palette.ink3).flex_none().child(choice.reason.clone()))
                    .on_click(move |_: &ClickEvent, _, cx| {
                        if let Some(route) = route.clone() {
                            links.dispatch(Intent::Navigate(route), cx);
                        }
                    }),
            );
        }
        if choices.is_empty() && !searching {
            let words = if self.query.is_some() { "Nothing matches that yet." } else { "Nothing walked yet." };
            list = list.child(div().px(measure.space(Space::Gutter)).py(measure.space(Space::Roomy)).child(super::kit::quiet(words, &measure, palette)));
        }
        let field = Input::new(&self.input).appearance(false).bordered(false);
        div()
            .id("ask")
            .w(width)
            .child(
                cut()
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
                    )
                    .child(div().max_h(px(420.0 * facet.text_scale)).overflow_hidden().child(list)),
            )
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
