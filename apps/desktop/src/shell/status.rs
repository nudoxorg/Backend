//! The status bar region: the mono address of the current place, and
//! nothing else (calm targets). Transient states are said once, in place,
//! by the region they belong to.

use super::kit::text;
use super::region::{Links, Region, RegionCore};
use super::thread;
use crate::runtime::store::{Branch, DataStore};
use facet::tokens::ty;
use facet::{ActiveFacet as _, Space};
use gpui::{Context, IntoElement, ParentElement, Render, Styled, Window, div, px};

/// The status bar region.
pub(crate) struct Status {
    core: RegionCore,
    links: Links,
}

impl Status {
    pub(crate) fn new(links: Links, store: &DataStore) -> Self {
        Self {
            core: RegionCore::new(store, &[Branch::Route, Branch::Overlay]),
            links,
        }
    }

    pub(crate) const fn renders(&self) -> u64 {
        self.core.renders()
    }
}

impl Region for Status {
    fn core(&mut self) -> &mut RegionCore {
        &mut self.core
    }
}

impl Render for Status {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.core.rendered();
        let measure = self.core.measure(cx);
        let palette = cx.facet().palette();
        let address = thread::address(&self.links.snapshot(cx));
        div()
            .size_full()
            .flex()
            .items_center()
            .px(measure.space(Space::Roomy))
            .border_t_1()
            .border_color(palette.line1.hsla())
            .child(
                text(ty::MONO_SMALL, &measure, palette.ink3)
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(address),
            )
    }
}
