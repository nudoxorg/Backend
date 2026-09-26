//! The pins column: pinned peeks, calm (a small head, then mark + name +
//! where per pin). It exists only while something is pinned and the window
//! has 1900 effective px (the shell decides); the rows come from the float
//! layer's pins.

use super::float::Pin;
use super::focus::{Act, Target, Targets};
use super::kit::{kind_of, symbol_route, text};
use super::region::{Links, Region, RegionCore};
use crate::model::pages::PageKey;
use crate::navigation::Intent;
use crate::runtime::store::DataStore;
use facet::icons::{self, Icon, IconSize, KindSize};
use facet::motion::Motion;
use facet::tokens::ty;
use facet::{ActiveFacet as _, Space};
use gpui::{
    ClickEvent, Context, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, div, px,
};
use std::rc::Rc;

/// The pins region.
pub(crate) struct Pins {
    core: RegionCore,
    links: Links,
    pub(crate) targets: Targets,
    motion: Motion,
    pins: Vec<Pin>,
}

impl Pins {
    pub(crate) fn new(links: Links, store: &DataStore) -> Self {
        Self {
            core: RegionCore::new(store, &[]),
            links,
            targets: Targets::default(),
            motion: Motion::new(),
            pins: Vec::new(),
        }
    }

    pub(crate) const fn renders(&self) -> u64 {
        self.core.renders()
    }

    /// Replaces the pins; re-renders only when they changed.
    pub(crate) fn set_pins(&mut self, pins: &[Pin], cx: &mut Context<Self>) {
        if self.pins != pins {
            self.pins = pins.to_vec();
            let keys = self.pins.iter().map(|pin| PageKey::Symbol(pin.symbol.clone())).collect::<Vec<_>>();
            let store = self.links.store.clone();
            self.core.watch_keys(store.read(cx), keys);
            cx.notify();
        }
    }
}

impl Region for Pins {
    fn core(&mut self) -> &mut RegionCore {
        &mut self.core
    }
}

impl Render for Pins {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.core.rendered();
        self.targets.begin();
        let measure = self.core.measure(cx);
        let facet = cx.facet();
        let palette = facet.palette();
        let mut column = div()
            .size_full()
            .flex()
            .flex_col()
            .gap(measure.space(Space::Roomy))
            .p(measure.space(Space::Gutter))
            .border_l_1()
            .border_color(palette.line1.hsla())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(measure.space(Space::Snug))
                    .child(icons::ui(Icon::Pin, IconSize::S12, palette.ink3).size(measure.icon(12.0)))
                    .child(text(ty::SMALL, &measure, palette.ink3).child("Pinned")),
            );
        let store = self.links.store.clone();
        for pin in &self.pins {
            let kind = store
                .read(cx)
                .symbol(&pin.symbol)
                .loaded_value()
                .map_or(facet::icons::Kind::Unknown, |page| kind_of(page.identity.kind));
            let id: SharedString = format!("pin-{}", pin.symbol).into();
            let links = self.links.clone();
            let route = pin.symbol.package().and_then(|package| symbol_route(package.as_str(), &pin.symbol));
            let act: Act = Rc::new(move |_, cx| {
                if let Some(route) = route.clone() {
                    links.dispatch(Intent::Navigate(route), cx);
                }
            });
            self.targets.push(Target {
                id: id.clone(),
                label: pin.name.clone(),
                act: Rc::clone(&act),
                peek: Some(PageKey::Symbol(pin.symbol.clone())),
                source: Some(pin.symbol.clone()),
            });
            column = column.child(
                self.targets.track(
                    id.clone(),
                    div()
                        .id(id)
                        .flex()
                        .items_start()
                        .gap(measure.space(Space::Roomy))
                        .py(measure.space(Space::Snug))
                        .child(super::kit::kind_mark(kind, KindSize::Md, &measure, palette))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .min_w(px(0.0))
                                .child(text(ty::MONO_ROW, &measure, palette.ink0).child(pin.name.clone()))
                                .child(text(ty::MONO_SMALL, &measure, palette.ink3).child(pin.place.clone())),
                        )
                        .on_click(move |_: &ClickEvent, window, cx| act(window, cx)),
                ),
            );
        }
        div().relative().size_full().child(column).child(self.targets.glow(&self.motion, &measure))
    }
}
