//! The titlebar region: the shelf toggle and the view switch, then the jump
//! bar (D-Hand) — back and forward and where you are — or the Ask field on
//! Orbit, and the inbox button.
//!
//! Nothing about history shows at rest. ⌘[ / ⌘] walk it; the back chevron
//! goes back on a click and lists the last ten places on a long press
//! (400 ms) or a right click. Each segment of the plate (package › module ›
//! declaration) opens its siblings; hovering the plate shows the
//! `nudox://` address (⌘⇧C copies it).
//!
//! It degrades from its own measured room, in three modes
//! (`facet::tokens::fluid::BAR`): everything from 760 design px; from 560 the
//! plate drops the package segment and the view switch gives way to it; below
//! that the plate keeps only the name and the inbox button goes. The modes
//! hold through a hysteresis band, and what arrives or leaves glides to its
//! place (`Flow`) instead of jumping. The shelf toggle never goes: below 640
//! the shelf is not inline, and this button is the pointer's only way to open
//! it over the reader.

use super::focus::{Target, Targets};
use super::jump::{self, Here, Mark, Segment};
use super::kit::{keycap, text};
use super::region::{Links, Region, RegionCore};
use crate::model::AppSnapshot;
use crate::model::pages::PageKey;
use crate::navigation::{Intent, OrbitRoute, Overlay, Route, View};
use crate::runtime::store::{Branch, DataStore, route_package, route_symbol};
use facet::icons::{self, Icon, IconSize, KindSize};
use facet::motion::{Flow, Motion};
use facet::tokens::fluid::{BAR, Bar};
use facet::overlay::float::{self, FloatKind, FloatRequest, Side};
use facet::overlay::menu::{self, Menu, MenuItem};
use facet::paint::{Bevel, Chamfer, cut};
use facet::tokens::ty;
use facet::{ActiveFacet as _, Measure, Palette, Set as _, Space};
use gpui_component::input::{Input, InputState};
use gpui::{
    AnyElement, App, ClickEvent, Context, InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels, Render, SharedString,
    StatefulInteractiveElement, Styled, Task, Window, WindowControlArea, div, px,
};
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

/// How long a press on back waits before it lists the last places.
const LONG_PRESS: Duration = Duration::from_millis(400);

/// The titlebar region.
pub(crate) struct Titlebar {
    core: RegionCore,
    links: Links,
    pub(crate) targets: Targets,
    /// A press on back, waiting to become a long press.
    press: Option<Task<()>>,
    /// The press became a long press: its click is not a step back.
    long: Rc<Cell<bool>>,
    /// What the bar's modes move: the plate's segments and name glide to
    /// their new places when a control arrives or leaves (the controls at the
    /// window's edges follow the edge and need no flow of their own).
    flow: Flow,
    /// The bar's mode changes: what arrives fades in.
    motion: Motion,
    /// Ask's field (`Ask::input`): drawn in the bar's place while Ask is open.
    ask_input: Option<gpui::Entity<InputState>>,
}

impl Titlebar {
    pub(crate) fn new(links: Links, store: &DataStore) -> Self {
        Self {
            core: RegionCore::new(store, &[Branch::Route, Branch::Overlay, Branch::Settings, Branch::GraphFocus]),
            links,
            targets: Targets::named("titlebar"),
            press: None,
            long: Rc::new(Cell::new(false)),
            flow: Flow::new("titlebar"),
            motion: Motion::new(),
            ask_input: None,
        }
    }

    pub(crate) const fn renders(&self) -> u64 {
        self.core.renders()
    }

    /// Hands the bar Ask's field, which it draws while Ask is open.
    pub(crate) fn set_ask_input(&mut self, input: gpui::Entity<InputState>) {
        self.ask_input = Some(input);
    }
}

impl Region for Titlebar {
    fn core(&mut self) -> &mut RegionCore {
        &mut self.core
    }

    fn keys(&self, snapshot: &AppSnapshot) -> Vec<PageKey> {
        let route = snapshot.route();
        route_symbol(route).map(PageKey::Symbol).into_iter().chain(route_package(route).map(PageKey::Package)).collect()
    }

    fn urgency(&self) -> super::region::Urgency {
        super::region::Urgency::Warm
    }
}

impl Render for Titlebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.core.rendered();
        self.targets.begin();
        let measure = self.core.measure(cx);
        let facet = cx.facet();
        let palette = facet.palette();
        let keys = facet.reveal.keys;
        let bar = self.core.modes().settle(&BAR, measure.fluid_room());
        self.flow.epoch(bar.epoch);
        // What a change of mode brings in fades in as the rest glides to its
        // place; a settled bar (or reduced motion) is fully drawn.
        let arriving = bar.progress(&self.motion, window, cx);
        let inbox_shown = bar.mode >= Bar::Snug;
        let snapshot = self.links.snapshot(cx);
        let (here, segments) = {
            let store = self.links.store.read(cx);
            (jump::here(&snapshot, store), jump::bar_segments(&snapshot, store))
        };
        let orbit = matches!(snapshot.route(), Route::Orbit(OrbitRoute::Home)) && snapshot.overlay().is_none();
        let shelf_on = snapshot.settings().shelf_open;
        let links = self.links.clone();

        let inset = if cfg!(target_os = "macos") { px(78.0) } else { measure.space(Space::Roomy) };
        let mut left = div().flex().flex_none().items_center().gap(measure.space(Space::Base)).pl(inset);
        let id: SharedString = "tb-shelf".into();
        let toggle_links = links.clone();
        let act: super::focus::Act = Rc::new(move |_, cx| {
            toggle_links.shell(cx, |shell, cx| shell.toggle_shelf(cx));
        });
        self.targets.push(Target { id: id.clone(), label: "Toggle the shelf".into(), act: Rc::clone(&act), peek: None, source: None });
        left = left.child(
            self.targets.track(
                id.clone(),
                facet::controls::icon_button(id, Icon::SideL, "Toggle the shelf", &measure)
                    .on(shelf_on)
                    .key("⌘\\")
                    .on_click(move |window, cx| act(window, cx)),
            ),
        );
        // The altimeter's slot is the view switch (§8.4): Graph · Page · Code,
        // only the active view named. Below 760 it gives way to the bar.
        if let Some(active) = view_of(snapshot.route())
            && bar.mode == Bar::Full
        {
            let fade = if bar.from.is_some_and(|from| from < Bar::Full) { arriving } else { 1.0 };
            left = left.child(div().opacity(fade).child(self.view_switch(active, &measure, palette, keys)));
        }

        let asking = snapshot.overlay() == Some(Overlay::CommandPalette);
        let center = if let (true, Some(input)) = (asking, self.ask_input.clone()) {
            Self::ask_typing(&input, &measure, palette)
        } else if orbit {
            self.ask_field(&measure, palette, keys, cx)
        } else {
            self.jump_bar(&snapshot, &here, &segments, bar.mode, &measure, palette, keys, cx)
        };

        let mut right = div().flex().flex_none().items_center().gap(measure.space(Space::Tight)).pr(measure.space(Space::Roomy));
        if inbox_shown {
            let id = "tb-inbox";
            let target_links = links.clone();
            let act: super::focus::Act = Rc::new(move |_, cx| target_links.dispatch(Intent::OpenInbox, cx));
            self.targets.push(Target { id: id.into(), label: "Inbox".into(), act: Rc::clone(&act), peek: None, source: None });
            let fade = if bar.from.is_some_and(|from| from < Bar::Snug) { arriving } else { 1.0 };
            right = right.child(div().opacity(fade).child(self.targets.track(
                id,
                facet::controls::icon_button(id, Icon::Inbox, "Inbox", &measure).on_click(move |window, cx| act(window, cx)),
            )));
        }

        let glow = self.targets.glow(&measure);
        div()
            .id("titlebar")
            .relative()
            .size_full()
            .flex()
            .items_center()
            .gap(measure.space(Space::Roomy))
            .border_b_1()
            .border_color(palette.line1.hsla())
            .window_control_area(WindowControlArea::Drag)
            .on_mouse_down(MouseButton::Left, |event, window, _| {
                if event.click_count == 2 {
                    window.titlebar_double_click();
                }
            })
            .child(left)
            .child(div().flex_1().min_w(px(0.0)).flex().justify_center().child(center))
            .child(right)
            .child(glow)
    }
}

/// The words a back-menu row says for a place: its name, then its package.
fn place_words(route: &Route) -> String {
    match route {
        Route::Symbol(symbol) => {
            let name = backend_present::Identity::parse(symbol.id.as_str()).name().to_owned();
            let package = crate::model::pages::PackageRef::parse(symbol.package.as_str())
                .map_or_else(|_| symbol.package.as_str().to_owned(), |package| package.display_name().to_owned());
            format!("{name} · {package}")
        }
        Route::Package(package) => crate::model::pages::PackageRef::parse(package.package.as_str())
            .map_or_else(|_| package.package.as_str().to_owned(), |package| package.display_name().to_owned()),
        Route::Orbit(_) => "Orbit".to_owned(),
        Route::World => "Graph".to_owned(),
    }
}

/// Opens the back menu under `anchor`: the last ten places, nearest first.
fn back_menu(links: &Links, anchor: gpui::Bounds<gpui::Pixels>, window: &mut Window, cx: &mut App) {
    let back = links.snapshot(cx).session().back.to_vec();
    let places: Vec<Route> = back.into_iter().take(10).collect();
    if places.is_empty() {
        return;
    }
    let items = places.iter().map(|route| MenuItem::new(place_words(route))).collect();
    let links = links.clone();
    let steps = places.len();
    let menu = Menu::new(items, move |index, _, cx| {
        for _ in 0..=index.min(steps - 1) {
            links.dispatch(Intent::Back, cx);
        }
    });
    menu::open("jump-back-menu", anchor, Side::Below, menu, window, cx);
}

impl Titlebar {
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn jump_bar(
        &mut self,
        snapshot: &AppSnapshot,
        here: &Here,
        segments: &[Segment],
        mode: Bar,
        measure: &Measure,
        palette: &'static Palette,
        keys: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let scale = measure.scale();
        let height = px(32.0 * scale);
        let session = snapshot.session();
        // `flex_1`: without it this row sizes itself from its own content
        // (width: auto), and its one real child, `here` below, is itself
        // `flex_1().min_w(0)` — a 0%-basis, 0-floor item contributes ~0 to
        // that auto computation, so `bar` collapsed to nearly nothing and
        // handed `here`/`plate` almost no room to lay out in. `plate`'s
        // fixed-size children (each segment, each `›`) kept painting at
        // their own natural size regardless (nothing shrinks a `flex_none`
        // item below it), so the only child with no floor of its own — the
        // current name, `min_w(0)` for its own truncation — absorbed the
        // whole shortfall, down to a literal 0 px box (`ask_field`'s field
        // has no such wrapper and never collapses this way).
        let mut bar = div().flex().items_center().min_w(px(0.0)).flex_1().gap(measure.space(Space::Snug));

        // Back: a click steps back; a long press or a right click lists.
        let can_back = !session.back.is_empty();
        let back_links = self.links.clone();
        let menu_links = self.links.clone();
        let targets = self.targets.clone();
        let long = Rc::clone(&self.long);
        let press_links = self.links.clone();
        let press_targets = self.targets.clone();
        let weak = cx.weak_entity();
        let release = cx.weak_entity();
        let act: super::focus::Act = Rc::new(move |_, cx| back_links.dispatch(Intent::Back, cx));
        self.targets.push(Target { id: "jump-back".into(), label: "Back".into(), act: Rc::clone(&act), peek: None, source: None });
        let right_links = menu_links.clone();
        let right_targets = targets.clone();
        bar = bar.child(
            self.targets.track(
                "jump-back",
                div()
                    .id("jump-back")
                    .relative()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(hit_side(measure))
                    .cursor_pointer()
                    .child(text(ty::ROW, measure, if can_back { palette.ink2 } else { palette.ink3 }).child("‹"))
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        let links = press_links.clone();
                        let targets = press_targets.clone();
                        let window_handle = window.window_handle();
                        let _ = weak.update(cx, |bar, cx| {
                            bar.long.set(false);
                            let long = Rc::clone(&bar.long);
                            bar.press = Some(cx.spawn(async move |_, cx| {
                                cx.background_executor().timer(LONG_PRESS).await;
                                let _ = window_handle.update(cx, |_, window, cx| {
                                    long.set(true);
                                    if let Some(anchor) = targets.bounds_of("jump-back") {
                                        back_menu(&links, anchor, window, cx);
                                    }
                                });
                            }));
                        });
                    })
                    // A press that ends before it is long is a click: the
                    // wait ends with it.
                    .on_mouse_up(MouseButton::Left, move |_, _, cx| {
                        let _ = release.update(cx, |bar, _| bar.press = None);
                    })
                    .on_mouse_down(MouseButton::Right, move |_, window, cx| {
                        if let Some(anchor) = right_targets.bounds_of("jump-back") {
                            back_menu(&right_links, anchor, window, cx);
                        }
                    })
                    .on_click(move |_: &ClickEvent, window, cx| {
                        if long.take() {
                            return;
                        }
                        act(window, cx);
                    })
                    .children(keycap(keys, "⌘[", measure)),
            ),
        );
        if !session.forward.is_empty() {
            let forward_links = self.links.clone();
            let act: super::focus::Act = Rc::new(move |_, cx| forward_links.dispatch(Intent::Forward, cx));
            self.targets.push(Target { id: "jump-forward".into(), label: "Forward".into(), act: Rc::clone(&act), peek: None, source: None });
            bar = bar.child(
                self.targets.track(
                    "jump-forward",
                    div()
                        .id("jump-forward")
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(hit_side(measure))
                        .cursor_pointer()
                        .child(text(ty::ROW, measure, palette.ink2).child("›"))
                        .on_click(move |_: &ClickEvent, window, cx| act(window, cx)),
                ),
            );
        }

        // The plate: where you are.
        let mark = match here.mark {
            Mark::Kind(kind) => super::kit::kind_mark(kind, KindSize::Sm, measure, palette),
            Mark::Orbit => icons::ui(Icon::Orbit, IconSize::S14, palette.ink2).size(measure.icon(14.0)).into_any_element(),
            Mark::Place => icons::ui(Icon::Settings, IconSize::S14, palette.ink2).size(measure.icon(14.0)).into_any_element(),
        };
        let name = text(ty::MONO_ROW, measure, palette.ink0)
            .font_weight(gpui::FontWeight(600.0))
            .flex_shrink(1.0)
            .min_w(px(0.0))
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .child(here.name.clone());
        #[cfg(test)]
        let name = facet::probe::text(
            "graph-test-here-name",
            here.name.clone(),
            ty::MONO_ROW,
            measure.scale(),
            facet::probe::TextOverflow::Clip,
            name,
        );
        let mut plate = cut()
            .chamfer(Chamfer::Float)
            .bevel(Bevel::Rest)
            .fill(palette.plate)
            .h(height)
            .min_w(px(0.0))
            .flex_1()
            .max_w(px(520.0 * scale))
            .px(measure.space(Space::Roomy))
            .flex()
            .items_center()
            .gap(measure.space(Space::Snug));
        // Package › module segments before the name (the name is the last
        // segment); the narrow bar keeps the name and what is nearest it.
        let lead = segments.len().saturating_sub(1);
        let keep_from = match mode {
            Bar::Bare => lead,
            Bar::Snug => 1.min(lead),
            Bar::Full => 0,
        };
        for (index, segment) in segments.iter().enumerate().take(lead).skip(keep_from) {
            let id: SharedString = format!("jump-seg-{index}").into();
            // `ink4` reads 2.72:1 on the abyss ground — tokens.rs documents
            // it as "rules and inactive ticks only, never text", and this
            // separator is drawn as a text glyph. `ink3` ("quiet words",
            // already this file's tone for the plate's own quiet line) is
            // the nearest step up that clears 4.5:1 (6.12:1 here).
            let segment = self.segment(id, index, segment, measure, palette, cx);
            plate = plate
                .child(self.flow.item(SharedString::from(format!("tb-flow-segment-{index}")), segment))
                .child(self.flow.item(SharedString::from(format!("tb-flow-segment-{index}-sep")), text(ty::SMALL, measure, palette.ink3).child("›")));
        }
        let last_id: SharedString = format!("jump-seg-{lead}").into();
        let last_links = self.links.clone();
        let last_targets = self.targets.clone();
        let here_name = self.targets.track(
            last_id.clone(),
            div()
                .id(last_id.clone())
                .flex()
                .items_center()
                .h(hit_side(measure))
                .gap(measure.space(Space::Snug))
                .min_w(px(0.0))
                .cursor_pointer()
                .child(mark)
                .child(name)
                .on_click(move |_: &ClickEvent, window, cx| {
                    siblings_menu(&last_links, &last_targets, lead, window, cx);
                }),
        );
        plate = plate.child(self.flow.item("tb-flow-name", here_name));
        // Not a declaration: the place names itself (`registry`, `Appearance`).
        let quiet: Option<SharedString> = if segments.is_empty() || here.path.starts_with("viewing ") {
            Some(here.path.clone())
        } else {
            None
        };
        if let Some(quiet) = quiet.filter(|words| !words.is_empty()) {
            plate = plate.child(
                text(ty::SMALL, measure, palette.ink3)
                    .flex_1()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(quiet),
            );
        } else {
            plate = plate.child(div().flex_1());
        }
        let ask_links = self.links.clone();
        plate = plate.child(
            div()
                .id("jump-ask")
                .cursor_pointer()
                .child(icons::ui(Icon::Search, IconSize::S14, palette.ink4).size(measure.icon(14.0)))
                .on_click(move |_: &ClickEvent, _, cx| ask_links.shell(cx, |shell, cx| shell.open_ask(cx))),
        );
        let hover_targets = self.targets.clone();
        // A graph focus is fixture data, and the tip says so.
        let address = SharedString::from(
            self.links
                .store
                .read(cx)
                .graph_focus()
                .map_or_else(|| jump::address_parts(snapshot).full(), crate::runtime::graph_focus::GraphFocus::status),
        );
        let ask_act_links = self.links.clone();
        let act: super::focus::Act = Rc::new(move |_, cx| ask_act_links.shell(cx, |shell, cx| shell.open_ask(cx)));
        self.targets.push(Target { id: "here".into(), label: here.name.clone(), act, peek: None, source: None });
        bar = bar.child(
            self.targets.track(
                "here",
                div()
                    .id("here")
                    .relative()
                    .flex_1()
                    .min_w(px(0.0))
                    .flex()
                    .justify_center()
                    .child(plate)
                    .on_hover(move |hovered, window, cx| {
                        if *hovered
                            && let Some(anchor) = hover_targets.bounds_of("here")
                        {
                            let words = address.clone();
                            let request = FloatRequest::new("jump-address", anchor, FloatKind::Tip, move |measure, _, cx| {
                                text(ty::MONO_SMALL, measure, cx.facet().palette().ink2).child(words.clone()).into_any_element()
                            });
                            float::rest(request, window, cx);
                        } else {
                            float::leave(&"jump-address".into(), window, cx);
                        }
                    })
                    .children(keycap(keys, "⌘K", measure)),
            ),
        );
        bar.into_any_element()
    }

    /// One segment before the name: its siblings, or its page when it has
    /// none to list (the package).
    fn segment(&mut self, id: SharedString, index: usize, segment: &Segment, measure: &Measure, palette: &Palette, cx: &App) -> AnyElement {
        let links = self.links.clone();
        let targets = self.targets.clone();
        let route = segment.route.clone();
        let act: super::focus::Act = {
            let links = links.clone();
            let route = route.clone();
            Rc::new(move |_, cx| {
                if let Some(route) = route.clone() {
                    links.dispatch(Intent::Navigate(route), cx);
                }
            })
        };
        if segment.quiet {
            return text(ty::SMALL, measure, palette.ink3).flex_none().whitespace_nowrap().child(segment.name.clone()).into_any_element();
        }
        self.targets.push(Target { id: id.clone(), label: segment.name.clone(), act: Rc::clone(&act), peek: None, source: None });
        // At least 24 × 24 to hit (gui-plan.md:213), grown by padding that
        // a matching negative margin takes back: the plate looks the same.
        let side = hit_side(measure);
        let pad = ((side - super::text_fit::text_width(&segment.name, &measure.role(ty::SMALL), cx)) / 2.0).max(px(0.0));
        self.targets
            .track(
                id.clone(),
                div()
                    .id(id)
                    .cursor_pointer()
                    .flex_none()
                    .h(side)
                    .flex()
                    .items_center()
                    .px(pad)
                    .mx(-pad)
                    .child(text(ty::SMALL, measure, palette.ink2).whitespace_nowrap().child(segment.name.clone()))
                    .on_click(move |_: &ClickEvent, window, cx| {
                        if index == 0 {
                            act(window, cx);
                        } else {
                            siblings_menu(&links, &targets, index, window, cx);
                        }
                    }),
            )
            .into_any_element()
    }

    /// The bar while Ask is open: the query, live, on the plate the field
    /// rests on (the results are the plate over the shelf's column).
    fn ask_typing(input: &gpui::Entity<InputState>, measure: &Measure, palette: &Palette) -> AnyElement {
        let height = px(32.0 * measure.scale());
        div()
            .id("ask-typing")
            .relative()
            .min_w(px(0.0))
            .flex_1()
            .max_w(px(460.0 * measure.scale()))
            .child(
                cut()
                    .chamfer(Chamfer::Float)
                    .bevel(Bevel::Focus)
                    .fill(palette.plate)
                    .h(height)
                    .px(measure.space(Space::Roomy))
                    .flex()
                    .items_center()
                    .gap(measure.space(Space::Base))
                    .child(icons::ui(Icon::Search, IconSize::S14, palette.ink2).size(measure.icon(14.0)))
                    .child(
                        Input::new(input)
                            .appearance(false)
                            .bordered(false)
                            .focus_bordered(false)
                            .set(ty::ROW, measure)
                            .px(px(0.0))
                            .flex_1()
                            .min_w(px(0.0)),
                    ),
            )
            .into_any_element()
    }

    fn ask_field(&mut self, measure: &Measure, palette: &Palette, keys: bool, _cx: &mut Context<Self>) -> AnyElement {
        let links = self.links.clone();
        let act: super::focus::Act = Rc::new(move |_, cx| {
            links.shell(cx, |shell, cx| shell.open_ask(cx));
        });
        self.targets.push(Target {
            id: "ask".into(),
            label: "Ask anything, or find a package".into(),
            act: Rc::clone(&act),
            peek: None,
            source: None,
        });
        let height = px(32.0 * measure.scale());
        self.targets
            .track(
                "ask",
                div()
                    .id("ask")
                    .relative()
                    .min_w(px(0.0))
                    .flex_1()
                    .max_w(px(460.0 * measure.scale()))
                    .child(
                        cut()
                            .chamfer(Chamfer::Float)
                            .bevel(Bevel::Rest)
                            .fill(palette.plate)
                            .h(height)
                            .px(measure.space(Space::Roomy))
                            .flex()
                            .items_center()
                            .gap(measure.space(Space::Base))
                            .child(icons::ui(Icon::Search, IconSize::S14, palette.ink3).size(measure.icon(14.0)))
                            .child(
                                text(ty::ROW, measure, palette.ink3)
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child("Ask anything, or find a package"),
                            ),
                    )
                    .on_click(move |_: &ClickEvent, window, cx| act(window, cx))
                    .children(keycap(keys, "⌘K", measure)),
            )
            .into_any_element()
    }

    fn view_switch(&mut self, active: View, measure: &Measure, palette: &Palette, keys: bool) -> AnyElement {
        let mut row = div().flex().items_center().gap(measure.space(Space::Hair));
        for view in [View::Graph, View::Page, View::Code] {
            let on = view == active;
            let id: SharedString = format!("view-{}", view.as_str()).into();
            let name = match view {
                View::Graph => "Graph",
                View::Page => "Page",
                View::Code => "Code",
            };
            let icon = match view {
                View::Graph => Icon::Orbit,
                View::Page => Icon::Book,
                View::Code => Icon::File,
            };
            let links = self.links.clone();
            let act: super::focus::Act = Rc::new(move |window, cx| match view {
                View::Page => window.dispatch_action(Box::new(super::keys::DepthPage), cx),
                View::Code => window.dispatch_action(Box::new(super::keys::DepthCode), cx),
                View::Graph => {
                    if !super::bodies::graph::is_graph(links.snapshot(cx).route()) {
                        links.dispatch(Intent::SetView(View::Graph), cx);
                    }
                }
            });
            self.targets.push(Target { id: id.clone(), label: name.into(), act: Rc::clone(&act), peek: None, source: None });
            let cap = match view {
                View::Graph => super::keys::cap(super::keys::Command::Graph),
                View::Page | View::Code => super::keys::cap(super::keys::Command::CodePage),
            };
            let ink = if on { palette.ink0 } else { palette.ink3 };
            let mut face = div()
                .id(id.clone())
                .relative()
                .h(px(28.0 * measure.scale()))
                .min_w(px(24.0 * measure.scale()))
                .px(measure.space(Space::Snug))
                .flex()
                .items_center()
                .justify_center()
                .gap(measure.space(Space::Snug))
                .child(icons::ui(icon, IconSize::S14, ink).size(measure.icon(14.0)))
                .on_click(move |_: &ClickEvent, window, cx| act(window, cx));
            if on {
                face = face.child(text(ty::DEPTH, measure, palette.ink0).child(name));
            }
            if keys && !on {
                face = face.children(keycap(true, cap, measure));
            }
            row = row.child(self.targets.track(id, face));
        }
        row.into_any_element()
    }
}

/// The smallest hit area, 24 × 24 px at any text size (gui-plan.md:213).
fn hit_side(measure: &Measure) -> Pixels {
    px((24.0 * measure.scale()).max(24.0))
}

/// Whether a menu is open (the jump bar's back places and siblings, the
/// symbol page's package menu): it owns the plain keys.
pub(crate) fn menu_open(window: &Window, cx: &mut App) -> bool {
    facet::overlay::float::menu_open(window, cx)
}

/// Opens the siblings of segment `index` under it: the outline level it
/// sits at; choosing one opens its page.
fn siblings_menu(links: &Links, targets: &Targets, index: usize, window: &mut Window, cx: &mut App) {
    let Some(anchor) = targets.bounds_of(&format!("jump-seg-{index}")).or_else(|| targets.bounds_of("here")) else {
        return;
    };
    let route = jump::bar_route(&links.snapshot(cx), links.store.read(cx));
    let siblings = jump::siblings(&route, index, links.store.read(cx));
    if siblings.is_empty() {
        return;
    }
    open_siblings(links, anchor, index, siblings, false, window, cx);
}

/// The siblings menu: the real entries, then the test-only modules folded
/// into one "tests" row (choosing it reopens the menu with them unfolded).
fn open_siblings(
    links: &Links,
    anchor: gpui::Bounds<gpui::Pixels>,
    index: usize,
    siblings: jump::Siblings,
    unfolded: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let mut items: Vec<MenuItem> = siblings.real.iter().map(|sibling| MenuItem::new(sibling.name.clone())).collect();
    let folded = !siblings.tests.is_empty() && !unfolded;
    if !siblings.tests.is_empty()
        && let Some(last) = items.last_mut()
    {
        last.separator_after = true;
    }
    if folded {
        items.push(MenuItem::new("tests"));
    } else {
        items.extend(siblings.tests.iter().map(|sibling| MenuItem::new(sibling.name.clone())));
    }
    let links = links.clone();
    let menu = Menu::new(items, move |choice, window, cx| {
        if folded && choice == siblings.real.len() {
            let (links, siblings) = (links.clone(), siblings.clone());
            window.defer(cx, move |window, cx| open_siblings(&links, anchor, index, siblings, true, window, cx));
            return;
        }
        let chosen = siblings.real.iter().chain(siblings.tests.iter()).nth(choice);
        if let Some(route) = chosen.and_then(|sibling| sibling.route.clone()) {
            links.dispatch(Intent::Navigate(route), cx);
        }
    });
    // Unfolded, it is a new menu in the same place (the closing one keeps
    // its key until it has left).
    let key = if unfolded { format!("jump-siblings-{index}-tests") } else { format!("jump-siblings-{index}") };
    menu::open(key, anchor, Side::Below, menu, window, cx);
}

/// The view a place shows, when it is a declaration or the world graph.
fn view_of(route: &Route) -> Option<View> {
    match route {
        Route::Symbol(route) => Some(route.view),
        Route::World => Some(View::Graph),
        Route::Orbit(_) | Route::Package(_) => None,
    }
}
