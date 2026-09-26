//! The pins column: W-Float's pinned peeks, in the chrome's column frame.
//! It exists only while something is pinned and the window has 1900
//! effective px (the shell decides); its rows are the float layer's.

use super::focus::Targets;
use super::region::{Links, Region, RegionCore};
use crate::runtime::store::DataStore;
use facet::{ActiveFacet as _, Space};
use gpui::{Context, IntoElement, ParentElement, Render, Styled, Window, div};

/// The pins region.
pub(crate) struct Pins {
    core: RegionCore,
    pub(crate) targets: Targets,
}

impl Pins {
    pub(crate) fn new(_links: Links, store: &DataStore) -> Self {
        Self {
            core: RegionCore::new(store, &[]),
            targets: Targets::named("pins"),
        }
    }

    pub(crate) const fn renders(&self) -> u64 {
        self.core.renders()
    }
}

impl Region for Pins {
    fn core(&mut self) -> &mut RegionCore {
        &mut self.core
    }
}

impl Render for Pins {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.core.rendered();
        self.targets.begin();
        let measure = self.core.measure(cx);
        let palette = cx.facet().palette();
        let inner = measure.inset(measure.space(Space::Gutter));
        div()
            .relative()
            .size_full()
            .p(measure.space(Space::Gutter))
            .border_l_1()
            .border_color(palette.line1.hsla())
            .child(facet::overlay::float::pinned_column(&inner, window, cx))
            .child(self.targets.glow(&measure))
    }
}
