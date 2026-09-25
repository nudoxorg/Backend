//! The Graph view: an empty region until the lead's graph prototype lands.
//!
//! It already carries the one thing the transition needs: the centred
//! declaration's mark under [`shared_id`](crate::shell::kit::shared_id), the
//! same id the Page view's hero gem carries, so W-Flow's shared-element
//! morph can match gem ↔ node. With nothing selected (the whole world) the
//! region is empty.

use super::{Ctx, Leaf, Pages};
use crate::navigation::Route;
use crate::runtime::store::route_symbol;
use crate::shell::kit::{kind_of, quiet, shared_id};
use facet::Space;
use gpui::{InteractiveElement as _, IntoElement, ParentElement, Styled, div, px};

pub(super) fn body(route: &Route, store: &Pages, ctx: &mut Ctx<'_>) -> Vec<Leaf> {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let centre = route_symbol(route).map(|symbol| {
        let kind = store
            .symbol(&symbol)
            .loaded_value()
            .map_or(facet::icons::Kind::Unknown, |page| kind_of(page.identity.kind));
        let name = ctx.say(symbol.identity().name().to_owned());
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(measure.space(Space::Base))
            .child(
                {
                    let key = crate::shell::kit::shared_key(&symbol);
                    div()
                        .id(shared_id(&symbol))
                        .debug_selector(move || key)
                        .child(facet::paint::gem(kind).size(40.0 * measure.scale()))
                },
            )
            .child(crate::shell::kit::text(facet::tokens::ty::MONO_ROW, &measure, palette.ink1).child(name))
    });
    let words = ctx.say(if centre.is_some() {
        "The graph around this declaration is being drawn."
    } else {
        "The graph of everything is being drawn."
    });
    vec![Leaf::new(
        div()
            .min_h(px(420.0 * measure.scale()))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(measure.space(Space::Wide))
            .children(centre)
            .child(quiet(words, &measure, palette))
            .into_any_element(),
    )]
}
