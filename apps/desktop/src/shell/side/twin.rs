//! Twins (`v6/cohesion/COHESION.md`, "The sidebar: primitives", 6). Hovering
//! a sidebar row lights its twin in the reader (the same declaration's row
//! or chip), and hovering in the reader lights the sidebar row (Figma's
//! layer <-> canvas hover).
//!
//! One global says which declaration is hovered ([`Lit`]). Every target a
//! region registers with a `source` is a declaration, so any of them, in any
//! region, can be a twin; none of the pages needs to know. A target that
//! is hovered publishes its `source` (`Targets::track`), and the shell draws
//! a ring on every other target that has the same one, above the regions
//! (which stay cached: a hover moving down a list re-renders nothing but the
//! ring layer).

use crate::model::pages::SymbolRef;
use facet::{ActiveFacet as _, Palette};
use gpui::{AnyElement, App, Bounds, Global, IntoElement, ParentElement, Pixels, Point, Styled, div};

/// The declaration hovered somewhere, if any.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Lit(Option<SymbolRef>);

impl Global for Lit {}

/// The declaration hovered somewhere.
pub(crate) fn lit(cx: &App) -> Option<SymbolRef> {
    cx.try_global::<Lit>().and_then(|lit| lit.0.clone())
}

/// Lights `symbol`'s twins (`None`: puts them out). Nothing is notified
/// unless what is lit changed.
pub(crate) fn light(symbol: Option<SymbolRef>, cx: &mut App) {
    if lit(cx) != symbol {
        cx.set_global(Lit(symbol));
    }
}

/// Puts the twins out when `symbol` is what is lit (the pointer left the
/// target that lit it, and has not gone to another that lights something).
pub(crate) fn put_out(symbol: &SymbolRef, cx: &mut App) {
    if lit(cx).as_ref() == Some(symbol) {
        light(None, cx);
    }
}

/// The ring layer: a ring on every twin of what is lit (`regions`: each
/// region's rectangle in window coordinates, and where its targets that are
/// twins of it are placed). A ring is clipped to its region: a target
/// scrolled out of its region keeps its last place, and must not draw a ring
/// over the chrome beside it. The target under the pointer wears its own
/// hover, not a ring.
pub(crate) fn rings(regions: &[(Bounds<Pixels>, Vec<Bounds<Pixels>>)], pointer: Point<Pixels>, cx: &App) -> Option<AnyElement> {
    lit(cx)?;
    let palette = cx.facet().palette();
    let mut layer = div().absolute().inset_0();
    let mut any = false;
    for (clip, placed) in regions {
        let mut region = div().absolute().left(clip.origin.x).top(clip.origin.y).w(clip.size.width).h(clip.size.height).overflow_hidden();
        let mut in_region = false;
        for bounds in placed {
            if bounds.contains(&pointer) || !clip.intersects(bounds) {
                continue;
            }
            in_region = true;
            region = region.child(ring(*bounds, clip.origin, palette));
        }
        if in_region {
            any = true;
            layer = layer.child(region);
        }
    }
    any.then(|| layer.into_any_element())
}

fn ring(bounds: Bounds<Pixels>, origin: Point<Pixels>, palette: &Palette) -> AnyElement {
    div()
        .absolute()
        .left(bounds.origin.x - origin.x)
        .top(bounds.origin.y - origin.y)
        .w(bounds.size.width)
        .h(bounds.size.height)
        .border_1()
        .border_color(palette.peri_hi.hsla())
        .into_any_element()
}
