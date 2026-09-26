//! The titlebar region: the shelf toggle, the bead thread with the here
//! capsule (or the Ask field on Orbit), and the trail and inbox buttons.
//!
//! It degrades from its own measured width (`v4/shots/flow-*.png`): below
//! 1100 effective px older beads go, below 760 only "here" remains and
//! fills, below 520 the icon buttons go.

use super::focus::{Target, Targets};
use super::kit::{keycap, text};
use super::region::{Links, Region, RegionCore};
use super::thread::{self, Bead, Here, Mark};
use crate::model::AppSnapshot;
use crate::model::pages::PageKey;
use crate::navigation::{Intent, OrbitRoute, Route, View};
use crate::runtime::store::{Branch, DataStore};
use facet::icons::{self, Icon, IconSize, KindSize};
use facet::paint::{Bevel, Chamfer, cut};
use facet::tokens::ty;
use facet::{ActiveFacet as _, Measure, Palette, Space};
use gpui::{
    AnyElement, ClickEvent, Context, Hsla, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Window,
    WindowControlArea, div, px,
};
use std::rc::Rc;

/// The titlebar region.
pub(crate) struct Titlebar {
    core: RegionCore,
    links: Links,
    pub(crate) targets: Targets,
}

impl Titlebar {
    pub(crate) fn new(links: Links, store: &DataStore) -> Self {
        Self {
            core: RegionCore::new(store, &[Branch::Route, Branch::Overlay, Branch::Settings, Branch::GraphFocus]),
            links,
            targets: Targets::named("titlebar"),
        }
    }

    pub(crate) const fn renders(&self) -> u64 {
        self.core.renders()
    }
}

impl Region for Titlebar {
    fn core(&mut self) -> &mut RegionCore {
        &mut self.core
    }

    fn keys(&self, snapshot: &AppSnapshot) -> Vec<PageKey> {
        thread::thread_keys(snapshot.session())
    }

    fn urgency(&self) -> super::region::Urgency {
        super::region::Urgency::Warm
    }
}

/// How much of the thread the titlebar's width affords.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Reach {
    Full,
    Recent,
    HereOnly,
}

impl Render for Titlebar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.core.rendered();
        self.targets.begin();
        let measure = self.core.measure(cx);
        let facet = cx.facet();
        let palette = facet.palette();
        let keys = facet.reveal.keys;
        let effective = measure.effective();
        // `v4.css`: ≤ 1100 older beads go, ≤ 760 only "here", ≤ 520 no icons.
        let reach = if effective > 1100.0 {
            Reach::Full
        } else if effective > 760.0 {
            Reach::Recent
        } else {
            Reach::HereOnly
        };
        let icons_shown = effective > 520.0;
        let snapshot = self.links.snapshot(cx);
        let (mut thread, here) = {
            let store = self.links.store.read(cx);
            (thread::thread(snapshot.session(), store), thread::here(&snapshot, store))
        };
        match reach {
            Reach::Full => {}
            Reach::Recent => {
                let keep = thread.behind.len().saturating_sub(2);
                thread.behind.drain(..keep);
                thread.ahead.truncate(1);
            }
            Reach::HereOnly => {
                thread.behind.clear();
                thread.ahead.clear();
            }
        }
        let orbit = matches!(snapshot.route(), Route::Orbit(OrbitRoute::Home)) && snapshot.overlay().is_none();
        let shelf_on = snapshot.settings().shelf_open;
        let links = self.links.clone();

        let inset = if cfg!(target_os = "macos") { px(78.0) } else { measure.space(Space::Roomy) };
        let mut left = div().flex().flex_none().items_center().gap(measure.space(Space::Base)).pl(inset);
        if icons_shown {
            let id: SharedString = "tb-shelf".into();
            let toggle_links = links.clone();
            let act: super::focus::Act = Rc::new(move |_, cx| {
                toggle_links.shell(cx, |shell, cx| shell.toggle_shelf(cx));
            });
            self.targets.push(Target {
                id: id.clone(),
                label: "Toggle the shelf".into(),
                act: Rc::clone(&act),
                peek: None,
                source: None,
            });
            left = left.child(
                self.targets.track(
                    id.clone(),
                    facet::controls::icon_button(id, Icon::SideL, "Toggle the shelf", &measure)
                        .on(shelf_on)
                        .key("⌘\\")
                        .on_click(move |window, cx| act(window, cx)),
                ),
            );
        }

        // The altimeter's slot is the view switch (§8.4): Graph · Page · Code,
        // only the active view named.
        // (Below 760 only "here" remains, the switch included: G and ⌘. still work.)
        if let Some(active) = view_of(snapshot.route())
            && reach != Reach::HereOnly
        {
            left = left.child(self.view_switch(active, &measure, palette, keys));
        }

        let center = if orbit {
            self.ask_field(&measure, palette, keys, cx)
        } else {
            self.thread_row(&thread, &here, reach, &measure, palette, keys, cx)
        };

        let mut right = div().flex().flex_none().items_center().gap(measure.space(Space::Tight)).pr(measure.space(Space::Roomy));
        if icons_shown {
            for (id, icon, label, intent) in [
                ("tb-trail", Icon::Trail, "Trail", None),
                ("tb-inbox", Icon::Inbox, "Inbox", Some(Intent::OpenInbox)),
            ] {
                let target_links = links.clone();
                let act: super::focus::Act = Rc::new(move |_, cx| match &intent {
                    Some(intent) => target_links.dispatch(intent.clone(), cx),
                    None => target_links.shell(cx, |shell, cx| shell.open_ask(true, cx)),
                });
                self.targets.push(Target {
                    id: id.into(),
                    label: label.into(),
                    act: Rc::clone(&act),
                    peek: None,
                    source: None,
                });
                right = right.child(self.targets.track(
                    id,
                    facet::controls::icon_button(id, icon, label, &measure).on_click(move |window, cx| act(window, cx)),
                ));
            }
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
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .flex()
                    .justify_center()
                    .child(center),
            )
            .child(right)
            .child(glow)
    }
}

impl Titlebar {
    #[allow(clippy::too_many_arguments)]
    fn thread_row(
        &mut self,
        thread: &thread::Thread,
        here: &Here,
        reach: Reach,
        measure: &Measure,
        palette: &Palette,
        keys: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut row = div().flex().items_center().min_w(px(0.0)).gap(measure.space(Space::Snug));
        if reach == Reach::HereOnly {
            row = row.w_full();
        }
        let count = thread.behind.len();
        for (index, bead) in thread.behind.iter().enumerate() {
            let nearest = index + 1 == count;
            row = row.child(self.bead(bead, false, nearest && keys, measure, palette));
            row = row.child(strand(measure, if nearest { palette.mint.base.into() } else { palette.line2.into() }, false));
        }
        row = row.child(self.capsule(here, reach, measure, palette, keys, cx));
        for (index, bead) in thread.ahead.iter().enumerate() {
            row = row.child(strand(measure, palette.line3.into(), true));
            row = row.child(self.bead(bead, true, index == 0 && keys, measure, palette));
        }
        row.into_any_element()
    }

    fn bead(&mut self, bead: &Bead, ahead: bool, cap: bool, measure: &Measure, palette: &Palette) -> AnyElement {
        let id: SharedString = format!("bead{}", bead.steps).into();
        let links = self.links.clone();
        let steps = bead.steps;
        let act: super::focus::Act = Rc::new(move |_, cx| {
            let intent = if steps < 0 { Intent::Back } else { Intent::Forward };
            for _ in 0..steps.unsigned_abs() {
                links.dispatch(intent.clone(), cx);
            }
        });
        self.targets.push(Target {
            id: id.clone(),
            label: bead.label.clone(),
            act: Rc::clone(&act),
            peek: None,
            source: None,
        });
        let color: Hsla = match bead.mark {
            Mark::Kind(kind) if ahead => icons::Kind::hue(kind, palette),
            Mark::Kind(kind) => {
                let mut hue = icons::Kind::hue(kind, palette);
                hue.alpha *= 0.85;
                hue
            }
            Mark::Orbit | Mark::Place => palette.ink3.into(),
        };
        let side = measure.icon(12.0);
        let label = if steps < 0 { "⌘[" } else { "⌘]" };
        self.targets
            .track(
                id.clone(),
                div()
                    .id(id)
                    .relative()
                    .flex_none()
                    // The diamond is 12 px; its hit target is never under 24.
                    .size((side + measure.space(Space::Tight) * 2.0).max(px(24.0 * measure.scale())))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icons::ui(Icon::Diamond, IconSize::S12, color).size(side))
                    .on_click(move |_: &ClickEvent, window, cx| act(window, cx))
                    .children(keycap(cap, label, measure)),
            )
            .into_any_element()
    }

    fn capsule(
        &mut self,
        here: &Here,
        reach: Reach,
        measure: &Measure,
        palette: &Palette,
        keys: bool,
        _cx: &mut Context<Self>,
    ) -> AnyElement {
        let links = self.links.clone();
        let act: super::focus::Act = Rc::new(move |_, cx| {
            links.shell(cx, |shell, cx| shell.open_ask(false, cx));
        });
        self.targets.push(Target {
            id: "here".into(),
            label: here.name.clone(),
            act: Rc::clone(&act),
            peek: None,
            source: None,
        });
        let mark = match here.mark {
            Mark::Kind(kind) => super::kit::kind_mark(kind, KindSize::Sm, &measure, palette),
            Mark::Orbit => icons::ui(Icon::Orbit, IconSize::S14, palette.ink2).size(measure.icon(14.0)).into_any_element(),
            Mark::Place => icons::ui(Icon::Settings, IconSize::S14, palette.ink2).size(measure.icon(14.0)).into_any_element(),
        };
        let height = px(32.0 * measure.scale());
        let mut plate = cut()
            .chamfer(Chamfer::Float)
            .bevel(Bevel::Rest)
            .fill(palette.plate)
            .h(height)
            .min_w(px(0.0))
            .px(measure.space(Space::Roomy))
            .flex()
            .items_center()
            .gap(measure.space(Space::Base))
            .child(mark)
            .child(
                text(ty::MONO_ROW, measure, palette.ink0)
                    .font_weight(gpui::FontWeight(600.0))
                    .flex_shrink_0()
                    .when_narrow(reach == Reach::HereOnly)
                    .child(here.name.clone()),
            )
            .child(
                text(ty::SMALL, measure, palette.ink3)
                    .flex_1()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(here.path.clone()),
            )
            .child(icons::ui(Icon::Search, IconSize::S14, palette.ink4).size(measure.icon(14.0)));
        plate = match reach {
            Reach::HereOnly => plate.flex_1(),
            Reach::Full | Reach::Recent => plate.w(px(430.0 * measure.scale())).flex_shrink_0(),
        };
        self.targets
            .track(
                "here",
                div()
                    .id("here")
                    .relative()
                    .min_w(px(0.0))
                    .when_flex(reach == Reach::HereOnly)
                    .child(plate)
                    .on_click(move |_: &ClickEvent, window, cx| act(window, cx))
                    .children(keycap(keys, "⌘K", measure)),
            )
            .into_any_element()
    }

    fn ask_field(&mut self, measure: &Measure, palette: &Palette, keys: bool, _cx: &mut Context<Self>) -> AnyElement {
        let links = self.links.clone();
        let act: super::focus::Act = Rc::new(move |_, cx| {
            links.shell(cx, |shell, cx| shell.open_ask(false, cx));
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
}

/// The view a place shows, when it is a declaration or the world graph.
fn view_of(route: &Route) -> Option<View> {
    match route {
        Route::Symbol(route) => Some(route.view),
        Route::World => Some(View::Graph),
        Route::Orbit(_) | Route::Package(_) => None,
    }
}

impl Titlebar {
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
            let act: super::focus::Act = Rc::new(move |window, cx| {
                match view {
                    View::Page => window.dispatch_action(Box::new(super::keys::DepthPage), cx),
                    View::Code => window.dispatch_action(Box::new(super::keys::DepthCode), cx),
                    View::Graph => {
                        if !super::bodies::graph::is_graph(links.snapshot(cx).route()) {
                            links.dispatch(Intent::SetView(View::Graph), cx);
                        }
                    }
                }
            });
            self.targets.push(Target {
                id: id.clone(),
                label: name.into(),
                act: Rc::clone(&act),
                peek: None,
                source: None,
            });
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

fn strand(measure: &Measure, color: Hsla, dashed: bool) -> AnyElement {
    let width = px(14.0 * measure.scale());
    if dashed {
        div()
            .flex()
            .flex_none()
            .gap(px(2.0))
            .children((0..3).map(|_| div().w(px(3.0)).h(px(1.0)).bg(color)))
            .w(width)
            .into_any_element()
    } else {
        div().flex_none().w(width).h(px(1.0)).bg(color).into_any_element()
    }
}

/// A name that may shrink (with an ellipsis) when the capsule is all there is.
trait WhenNarrow: Styled + Sized {
    fn when_narrow(self, narrow: bool) -> Self {
        if narrow {
            self.flex_shrink(1.0).min_w(px(0.0)).overflow_hidden().whitespace_nowrap().text_ellipsis()
        } else {
            self
        }
    }
}

impl<E: Styled> WhenNarrow for E {}

/// `flex_1` only when asked, so the capsule fills only in "here only" mode.
trait WhenFlex: Styled + Sized {
    fn when_flex(self, flex: bool) -> Self {
        if flex { self.flex_1() } else { self }
    }
}

impl<E: Styled> WhenFlex for E {}

