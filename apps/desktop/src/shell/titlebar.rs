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
use crate::core::VersionedRoot;
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
    AnyElement, App, ClickEvent, Context, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement, Pixels, Render, SharedString,
    StatefulInteractiveElement, Styled, Task, Window, WindowControlArea, div, px,
};
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

/// How long a press on back waits before it lists the last places.
const LONG_PRESS: Duration = Duration::from_millis(400);

/// A titlebar action's exact visit. Observation counters are UI metadata;
/// the producer authority and the route/overlay are the action boundary.
#[derive(Clone)]
struct JumpVisit {
    route: Route,
    subject: jump::JumpSubject,
    graph_selection: crate::runtime::graph_focus::GraphViewEligibility,
    attachment: Option<crate::runtime::store::OwnerAttachment>,
    overlay: Option<Overlay>,
    root: VersionedRoot,
}

impl JumpVisit {
    fn at(snapshot: &AppSnapshot, store: &DataStore) -> Self {
        Self {
            route: snapshot.route().clone(),
            subject: jump::bar_subject(snapshot, store),
            graph_selection: store.graph_view_eligibility(),
            attachment: store.current_owner_attachment(),
            overlay: snapshot.overlay(),
            root: snapshot.key(),
        }
    }

    fn current(&self, links: &Links, cx: &App) -> bool {
        let store = links.store.read(cx);
        let snapshot = store.snapshot();
        snapshot.route() == &self.route
            && snapshot.overlay() == self.overlay
            && snapshot.key().same_authority(self.root)
            && jump::bar_subject(&snapshot, store) == self.subject
            && store.graph_view_eligibility() == self.graph_selection
            && store.current_owner_attachment() == self.attachment
    }
}

#[derive(Clone)]
enum JumpAction {
    Ask,
    Back { visit: JumpVisit },
    Forward { visit: JumpVisit },
    Navigate { visit: JumpVisit, index: usize, route: Route },
    Siblings { visit: JumpVisit, index: usize },
    View { visit: JumpVisit, view: View },
}

impl JumpAction {
    fn run(&self, links: &Links, targets: &Targets, window: &mut Window, cx: &mut App) {
        match self {
            Self::Ask => links.shell(cx, |shell, cx| shell.open_ask(cx)),
            Self::Back { visit } if visit.current(links, cx) => {
                // Back can retire its own auxiliary-page button. Dispatch
                // through the live Shell's existing top-layer boundary.
                links.shell(cx, |shell, cx| shell.take_zone(super::focus::Zone::Titlebar, window, cx));
                window.dispatch_action(Box::new(super::keys::Back), cx);
            },
            Self::Forward { visit } if visit.current(links, cx) && links.snapshot(cx).overlay().is_none()
                && !links.snapshot(cx).session().forward.is_empty() => links.dispatch(Intent::Forward, cx),
            Self::Navigate { visit, index, route } if visit.current(links, cx) => {
                let still_here = {
                    let store = links.store.read(cx);
                    let snapshot = store.snapshot();
                    jump::bar_segments(&snapshot, store).get(*index).is_some_and(|segment| {
                        !segment.quiet && segment.route.as_ref() == Some(route)
                    })
                };
                if still_here { links.dispatch(Intent::Navigate(route.clone()), cx); }
            }
            Self::Siblings { visit, index } if visit.current(links, cx) => {
                siblings_menu(links, targets, *index, visit.clone(), window, cx);
            }
            Self::View { visit, view }
                if visit.current(links, cx)
                    && visit.subject.page_route().is_some()
                    && links.snapshot(cx).overlay().is_none()
                    && links.snapshot(cx).page_overlay().is_none()
                    && links.store.read(cx).graph_view_eligibility().allows(*view) => match view {
                View::Page => window.dispatch_action(Box::new(super::keys::DepthPage), cx),
                View::Code => window.dispatch_action(Box::new(super::keys::DepthCode), cx),
                View::Graph => {
                    if !super::bodies::graph::is_graph(links.snapshot(cx).route()) {
                        window.dispatch_action(Box::new(super::keys::Graph), cx);
                    }
                }
            },
            _ => {}
        }
    }
}

fn activate_key(event: &KeyDownEvent, action: &super::focus::Act, window: &mut Window, cx: &mut App) {
    if !event.keystroke.modifiers.modified()
        && matches!(event.keystroke.key.as_str(), "enter" | "space")
        && !event.is_held
    {
        action(window, cx);
        cx.stop_propagation();
    }
}

fn jump_act(action: JumpAction, links: &Links, targets: &Targets) -> super::focus::Act {
    let links = links.clone();
    let targets = targets.clone();
    Rc::new(move |window, cx| action.run(&links, &targets, window, cx))
}

fn segment_action(visit: &JumpVisit, index: usize, segment: &Segment, store: &DataStore, current: bool) -> Option<JumpAction> {
    if segment.quiet { return None; }
    let route = visit.subject.page_route()?;
    if index > 0 && !jump::siblings(route, index, store).is_empty() {
        return Some(JumpAction::Siblings { visit: visit.clone(), index });
    }
    if current { return None; }
    segment.route.clone().map(|route| JumpAction::Navigate { visit: visit.clone(), index, route })
}

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
        crate::runtime::store::RouteDependencies::chrome_keys(snapshot.route())
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
            && snapshot.overlay().is_none()
            && snapshot.page_overlay().is_none()
        {
            let fade = if bar.from.is_some_and(|from| from < Bar::Full) { arriving } else { 1.0 };
            left = left.child(div().opacity(fade).child(self.view_switch(active, &measure, palette, keys, cx)));
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
        Route::CargoSource(file) => file.target.path().as_str().to_owned(),
        Route::Orbit(_) => "Orbit".to_owned(),
        Route::World => "Graph".to_owned(),
    }
}

/// Opens the back menu under `anchor`: the last ten places, nearest first.
fn back_menu(links: &Links, anchor: gpui::Bounds<gpui::Pixels>, window: &mut Window, cx: &mut App) {
    // On an auxiliary page Back closes that page; its retained content
    // history is not advertised as the auxiliary subject's history menu.
    if jump::bar_subject(&links.snapshot(cx), links.store.read(cx)).page_route().is_none() { return; }
    let visit = JumpVisit::at(&links.snapshot(cx), links.store.read(cx));
    let back = links.snapshot(cx).session().back.to_vec();
    let places: Vec<Route> = back.into_iter().take(10).collect();
    if places.is_empty() {
        return;
    }
    let items = places.iter().map(|route| MenuItem::new(place_words(route))).collect();
    focus_menu_owner(links, window, cx);
    let links = links.clone();
    let steps = places.len();
    let expected = places;
    let menu = Menu::new(items, move |index, _, cx| {
        if !visit.current(&links, cx) || !links.snapshot(cx).session().back.iter().take(steps).eq(expected.iter()) {
            return;
        }
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
        let visit = JumpVisit::at(snapshot, self.links.store.read(cx));
        let can_back = snapshot.overlay().is_some() || !session.back.is_empty()
            || self.links.shell.upgrade().is_some_and(|shell| shell.read(cx).shelf_input_owner(true));
        if can_back {
            let act = jump_act(JumpAction::Back { visit: visit.clone() }, &self.links, &self.targets);
            let key_act = Rc::clone(&act);
            let click_act = Rc::clone(&act);
            let menu_links = self.links.clone();
            let targets = self.targets.clone();
            let long = Rc::clone(&self.long);
            let press_links = self.links.clone();
            let press_targets = self.targets.clone();
            let weak = cx.weak_entity();
            let release = cx.weak_entity();
            self.targets.push(Target { id: "jump-back".into(), label: "Back".into(), act, peek: None, source: None });
            let right_links = menu_links.clone();
            let right_targets = targets.clone();
            bar = bar.child(self.targets.track("jump-back",
                div()
                    .id("jump-back")
                    .role(gpui::Role::Button)
                    .aria_label("Back")
                    .focusable()
                    .relative().flex().items_center().justify_center().size(hit_side(measure))
                    .cursor_pointer()
                    .child(text(ty::ROW, measure, palette.ink2).child("‹"))
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
                    .on_click(move |event: &ClickEvent, window, cx| {
                        if long.take() {
                            return;
                        }
                        if !matches!(event, ClickEvent::Keyboard(_)) { click_act(window, cx); }
                    })
                    .on_key_down(move |event, window, cx| activate_key(event, &key_act, window, cx))
                    .children(keycap(keys, "⌘[", measure)),
            ));
        } else {
            bar = bar.child(div()
                .id("jump-back")
                .role(gpui::Role::Button)
                .aria_label("Back")
                .aria_disabled(true)
                .flex().items_center().justify_center().size(hit_side(measure))
                .child(text(ty::ROW, measure, palette.ink3).child("‹")));
        }
        if snapshot.overlay().is_none() && !session.forward.is_empty() {
            let act = jump_act(JumpAction::Forward { visit: visit.clone() }, &self.links, &self.targets);
            let key_act = Rc::clone(&act);
            let click_act = Rc::clone(&act);
            self.targets.push(Target { id: "jump-forward".into(), label: "Forward".into(), act: Rc::clone(&act), peek: None, source: None });
            bar = bar.child(
                self.targets.track(
                    "jump-forward",
                    div()
                        .id("jump-forward")
                        .role(gpui::Role::Button)
                        .aria_label("Forward")
                        .focusable()
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(hit_side(measure))
                        .cursor_pointer()
                        .child(text(ty::ROW, measure, palette.ink2).child("›"))
                        .on_click(move |event: &ClickEvent, window, cx| {
                            if !matches!(event, ClickEvent::Keyboard(_)) { click_act(window, cx); }
                        })
                        .on_key_down(move |event, window, cx| activate_key(event, &key_act, window, cx)),
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
        let visit = JumpVisit::at(snapshot, self.links.store.read(cx));
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
        let action = segments.get(lead).and_then(|segment| {
            segment_action(&visit, lead, segment, self.links.store.read(cx), true)
        });
        let actionable = action.is_some();
        let mut here_name = div()
            .id(last_id.clone())
            .flex().items_center().h(hit_side(measure))
            .gap(measure.space(Space::Snug)).min_w(px(0.0))
            .child(mark).child(name);
        if let Some(action) = action {
            let act = jump_act(action, &self.links, &self.targets);
            let click_act = Rc::clone(&act);
            let key_act = Rc::clone(&act);
            self.targets.push(Target { id: last_id.clone(), label: format!("Show {} siblings", here.name).into(), act, peek: None, source: None });
            here_name = here_name
                .role(gpui::Role::Button)
                .aria_label(format!("Show {} siblings", here.name))
                .focusable().cursor_pointer()
                .on_click(move |event: &ClickEvent, window, cx| {
                    if !matches!(event, ClickEvent::Keyboard(_)) { click_act(window, cx); }
                })
                .on_key_down(move |event, window, cx| activate_key(event, &key_act, window, cx));
        }
        let here_name = if actionable {
            self.targets.track(last_id.clone(), here_name).into_any_element()
        } else {
            here_name.into_any_element()
        };
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
        let ask = jump_act(JumpAction::Ask, &self.links, &self.targets);
        let click_ask = Rc::clone(&ask);
        let key_ask = Rc::clone(&ask);
        self.targets.push(Target { id: "here".into(), label: "Ask anything, or find a package".into(), act: ask, peek: None, source: None });
        plate = plate.child(
            self.targets.track("here", div()
                .id("here")
                .role(gpui::Role::Button)
                .aria_label("Ask anything, or find a package")
                .focusable()
                .cursor_pointer()
                .child(icons::ui(Icon::Search, IconSize::S14, palette.ink4).size(measure.icon(14.0)))
                .on_click(move |event: &ClickEvent, window, cx| {
                    if !matches!(event, ClickEvent::Keyboard(_)) { click_ask(window, cx); }
                })
                .on_key_down(move |event, window, cx| activate_key(event, &key_ask, window, cx))),
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
        bar = bar.child(
                div()
                    .id("jump-plate")
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
        );
        bar.into_any_element()
    }

    /// One segment before the name: its siblings, or its page when it has
    /// none to list (the package).
    fn segment(&mut self, id: SharedString, index: usize, segment: &Segment, measure: &Measure, palette: &Palette, cx: &App) -> AnyElement {
        let snapshot = self.links.snapshot(cx);
        let store = self.links.store.read(cx);
        let visit = JumpVisit::at(&snapshot, store);
        let action = segment_action(&visit, index, segment, store, false);
        let Some(action) = action else {
            return text(ty::SMALL, measure, palette.ink3).flex_none().whitespace_nowrap().child(segment.name.clone()).into_any_element();
        };
        let navigates = matches!(&action, JumpAction::Navigate { .. });
        let role = if navigates { gpui::Role::Link } else { gpui::Role::Button };
        let label = if navigates { format!("Open {}", segment.name) } else { format!("Show {} siblings", segment.name) };
        let act = jump_act(action, &self.links, &self.targets);
        let click_act = Rc::clone(&act);
        let key_act = Rc::clone(&act);
        self.targets.push(Target { id: id.clone(), label: label.clone().into(), act, peek: None, source: None });
        // At least 24 × 24 to hit (gui-plan.md:213), grown by padding that
        // a matching negative margin takes back: the plate looks the same.
        let side = hit_side(measure);
        let pad = ((side - super::text_fit::text_width(&segment.name, &measure.role(ty::SMALL), cx)) / 2.0).max(px(0.0));
        self.targets
            .track(
                id.clone(),
                div()
                    .id(id)
                    .role(role)
                    .aria_label(label)
                    .focusable()
                    .cursor_pointer()
                    .flex_none()
                    .h(side)
                    .flex()
                    .items_center()
                    .px(pad)
                    .mx(-pad)
                    .child(text(ty::SMALL, measure, palette.ink2).whitespace_nowrap().child(segment.name.clone()))
                    .on_click(move |event: &ClickEvent, window, cx| {
                        if !matches!(event, ClickEvent::Keyboard(_)) { click_act(window, cx); }
                    })
                    .on_key_down(move |event, window, cx| activate_key(event, &key_act, window, cx)),
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
                            .aria_label("Ask anything, or find a package")
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
        let act = jump_act(JumpAction::Ask, &self.links, &self.targets);
        let click_act = Rc::clone(&act);
        let key_act = Rc::clone(&act);
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
                    .role(gpui::Role::Button)
                    .aria_label("Ask anything, or find a package")
                    .focusable()
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
                    .on_click(move |event: &ClickEvent, window, cx| {
                        if !matches!(event, ClickEvent::Keyboard(_)) { click_act(window, cx); }
                    })
                    .on_key_down(move |event, window, cx| activate_key(event, &key_act, window, cx))
                    .children(keycap(keys, "⌘K", measure)),
            )
            .into_any_element()
    }

    fn view_switch(&mut self, active: View, measure: &Measure, palette: &Palette, keys: bool, cx: &App) -> AnyElement {
        let mut row = div().flex().items_center().gap(measure.space(Space::Hair));
        let snapshot = self.links.snapshot(cx);
        let (visit, eligibility) = {
            let store = self.links.store.read(cx);
            (JumpVisit::at(&snapshot, store), store.graph_view_eligibility())
        };
        for view in [View::Graph, View::Page, View::Code] {
            let on = view == active;
            let available = eligibility.allows(view);
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
            let cap = match view {
                View::Graph => super::keys::cap(super::keys::Command::Graph),
                View::Page | View::Code => super::keys::cap(super::keys::Command::CodePage),
            };
            let ink = if on { palette.ink0 } else if available { palette.ink3 } else { palette.ink4 };
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
                .child(icons::ui(icon, IconSize::S14, ink).size(measure.icon(14.0)));
            if on {
                face = face.child(text(ty::DEPTH, measure, palette.ink0).child(name));
                row = row.child(face.role(gpui::Role::Label).aria_label(format!("{name} view selected")));
                continue;
            }
            if !available {
                row = row.child(face.role(gpui::Role::Label).aria_label(format!("{name} unavailable: {}", eligibility.guidance().unwrap_or_default())));
                continue;
            }
            let act = jump_act(JumpAction::View { visit: visit.clone(), view }, &self.links, &self.targets);
            let click_act = Rc::clone(&act);
            let key_act = Rc::clone(&act);
            self.targets.push(Target { id: id.clone(), label: format!("Show {name} view").into(), act, peek: None, source: None });
            face = face
                .role(gpui::Role::Button)
                .aria_label(format!("Show {name} view"))
                .focusable().cursor_pointer()
                .on_click(move |event: &ClickEvent, window, cx| {
                    if !matches!(event, ClickEvent::Keyboard(_)) { click_act(window, cx); }
                })
                .on_key_down(move |event, window, cx| activate_key(event, &key_act, window, cx));
            if keys { face = face.children(keycap(true, cap, measure)); }
            row = row.child(self.targets.track(id, face));
        }
        if let Some(guidance) = eligibility.brief_guidance() {
            row = row.child(text(ty::CAPTION, measure, palette.ink3).child(guidance));
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

/// A popup can outlive the titlebar leaf that opened it (for example when
/// Settings covers a declaration). Its existing native return handle must
/// therefore belong to the persistent titlebar dispatch owner, not that leaf.
fn focus_menu_owner(links: &Links, window: &mut Window, cx: &mut App) {
    links.shell(cx, |shell, cx| shell.take_zone(super::focus::Zone::Titlebar, window, cx));
}

/// Opens the siblings of segment `index` under it: the outline level it
/// sits at; choosing one opens its page.
fn siblings_menu(links: &Links, targets: &Targets, index: usize, visit: JumpVisit, window: &mut Window, cx: &mut App) {
    let Some(anchor) = targets.bounds_of(&format!("jump-seg-{index}")).or_else(|| targets.bounds_of("here")) else {
        return;
    };
    if !visit.current(links, cx) { return; }
    let Some(route) = visit.subject.page_route() else { return; };
    let siblings = jump::siblings(route, index, links.store.read(cx));
    if siblings.is_empty() {
        return;
    }
    open_siblings(links, anchor, index, siblings, false, visit, window, cx);
}

/// The siblings menu: the real entries, then the test-only modules folded
/// into one "tests" row (choosing it reopens the menu with them unfolded).
fn open_siblings(
    links: &Links,
    anchor: gpui::Bounds<gpui::Pixels>,
    index: usize,
    siblings: jump::Siblings,
    unfolded: bool,
    visit: JumpVisit,
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
    focus_menu_owner(links, window, cx);
    let links = links.clone();
    let menu = Menu::new(items, move |choice, window, cx| {
        if !visit.current(&links, cx) { return; }
        let Some(route) = visit.subject.page_route() else { return; };
        let current = jump::siblings(route, index, links.store.read(cx));
        if current.is_empty() { return; }
        if folded && choice == siblings.real.len() {
            let (links, visit) = (links.clone(), visit.clone());
            window.defer(cx, move |window, cx| {
                if visit.current(&links, cx) {
                    let Some(route) = visit.subject.page_route() else { return; };
                    let fresh = jump::siblings(route, index, links.store.read(cx));
                    if !fresh.is_empty() { open_siblings(&links, anchor, index, fresh, true, visit, window, cx); }
                }
            });
            return;
        }
        let shown = if unfolded { current.real.iter().chain(current.tests.iter()).nth(choice) } else { current.real.get(choice) };
        let original = if unfolded { siblings.real.iter().chain(siblings.tests.iter()).nth(choice) } else { siblings.real.get(choice) };
        if let Some(route) = shown.and_then(|sibling| sibling.route.clone()).filter(|route| original.and_then(|item| item.route.as_ref()) == Some(route)) {
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
        Route::CargoSource(_) | Route::Orbit(_) | Route::Package(_) => None,
    }
}


#[cfg(test)]
mod auxiliary_subject_tests {
    use super::*;
    use crate::shell::anatomy_tests::painted;
    use crate::shell::tests::{Rig, page_route, rig};
    use gpui::{Modifiers, TestAppContext, point};

    fn native(rig: &mut Rig) {
        let shell = rig.shell.clone();
        rig.cx.update(|window, cx| {
            window.replace_root(cx, |window, cx| gpui_component::Root::new(shell, window, cx).bordered(false));
            window.set_a11y_forced(true);
            facet::probe::enable(cx);
        });
        rig.settle();
    }
    fn click_target(rig: &mut Rig, key: &str, right: bool) {
        let ledger = painted(rig);
        let target = ledger.targets.iter().find(|target| target.key == key).expect("painted target");
        let at = point(px(target.bounds.x + target.bounds.width / 2.0), px(target.bounds.y + target.bounds.height / 2.0));
        if right {
            rig.cx.simulate_mouse_down(at, MouseButton::Right, Modifiers::none());
            rig.cx.simulate_mouse_up(at, MouseButton::Right, Modifiers::none());
        } else { rig.cx.simulate_click(at, Modifiers::none()); }
        rig.settle();
    }
    fn overlay(rig: &mut Rig) -> Option<Overlay> {
        rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().overlay())
    }
    fn assert_auxiliary_capsule(rig: &mut Rig, label: &str) {
        let ledger = painted(rig);
        let name = ledger.texts.iter().find(|text| text.key == "graph-test-here-name").expect("actual titlebar name");
        assert_eq!(name.content, label);
        assert!(!ledger.targets.iter().any(|target| target.key.starts_with("jump-seg-") || target.key.starts_with("view-")),
            "auxiliary subject has no retained declaration action targets");
        let at = point(px(name.bounds.x + name.bounds.width / 2.0), px(name.bounds.y + name.bounds.height / 2.0));
        let json = rig.cx.update(|window, _| window.debug_a11y_tree_json()).expect("forced native titlebar tree");
        let tree: serde_json::Value = serde_json::from_str(&json).expect("native tree JSON");
        assert!(!tree["nodes"].as_object().expect("native nodes").values().any(|node|
            node["aria"]["label"].as_str() == Some(format!("Show {label} siblings").as_str())),
            "accessibility cannot advertise a declaration action under an auxiliary name");
        rig.cx.simulate_click(at, Modifiers::none()); rig.settle();
        assert!(!rig.cx.update(|window, cx| menu_open(window, cx)), "clicking the name cannot open retained declaration siblings");
    }

    #[gpui::test]
    fn native_settings_and_inbox_capsules_do_not_borrow_declaration_siblings(cx: &mut TestAppContext) {
        let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
        native(&mut rig);
        let route = rig.route();
        rig.keys("cmd-,");
        let settings = overlay(&mut rig);
        assert!(matches!(settings, Some(Overlay::Settings(_))));
        assert_auxiliary_capsule(&mut rig, "Settings");
        assert_eq!(overlay(&mut rig), settings);
        assert_eq!(rig.route(), route);
        rig.keys("escape");
        click_target(&mut rig, "tb-inbox", false);
        assert_eq!(overlay(&mut rig), Some(Overlay::Inbox));
        assert_auxiliary_capsule(&mut rig, "Inbox");
        assert_eq!(overlay(&mut rig), Some(Overlay::Inbox));
        assert_eq!(rig.route(), route);
    }

    #[gpui::test]
    fn native_popup_escape_consumes_only_the_menu_above_settings(cx: &mut TestAppContext) {
        let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
        native(&mut rig);
        click_target(&mut rig, "jump-seg-1", false);
        assert!(rig.cx.update(|window, cx| menu_open(window, cx)));
        let route = rig.route();
        rig.keys("cmd-,");
        let settings = overlay(&mut rig);
        assert!(matches!(settings, Some(Overlay::Settings(_))));
        assert!(rig.cx.update(|window, cx| menu_open(window, cx)), "the menu remains the top input owner");
        rig.keys("escape");
        assert!(!rig.cx.update(|window, cx| menu_open(window, cx)));
        assert_eq!(overlay(&mut rig), settings, "one native Escape must not also dismiss its underlay");
        assert_eq!(rig.route(), route);
        rig.keys("cmd-k");
        assert_eq!(overlay(&mut rig), Some(Overlay::CommandPalette), "popup focus return keeps native dispatch alive");
        rig.keys("escape");
        assert_eq!(overlay(&mut rig), settings);
    }

    #[gpui::test]
    fn native_menu_enter_cannot_commit_a_declaration_action_after_settings_covers_it(cx: &mut TestAppContext) {
        for history in [false, true] {
            let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
            native(&mut rig);
            if history {
                rig.go(Intent::Navigate(page_route("KindGlyph")));
                click_target(&mut rig, "jump-back", true);
            } else { click_target(&mut rig, "jump-seg-1", false); }
            assert!(rig.cx.update(|window, cx| menu_open(window, cx)));
            let route = rig.route();
            rig.keys("cmd-,");
            let settings = overlay(&mut rig);
            assert!(matches!(settings, Some(Overlay::Settings(_))));
            rig.keys("enter");
            assert_eq!(overlay(&mut rig), settings, "a covered subject cannot admit a captured menu action");
            assert_eq!(rig.route(), route);
            rig.keys("cmd-k");
            assert_eq!(overlay(&mut rig), Some(Overlay::CommandPalette));
        }
    }

    #[gpui::test]
    fn native_back_closes_settings_without_consuming_content_history_and_keeps_keyboard_alive(cx: &mut TestAppContext) {
        let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
        native(&mut rig);
        let history = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().back.clone());
        let route = rig.route();
        rig.keys("cmd-,");
        click_target(&mut rig, "jump-back", false);
        assert_eq!(overlay(&mut rig), None);
        assert_eq!(rig.route(), route);
        assert_eq!(rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().back.clone()), history,
            "closing Settings cannot consume content history");
        rig.keys("cmd-k");
        assert_eq!(overlay(&mut rig), Some(Overlay::CommandPalette));
    }
}
