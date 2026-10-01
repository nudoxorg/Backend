//! The inbox: followed releases, calm. The local service publishes no
//! release feed yet, so the room says so once and stays empty.

use super::{Ctx, Leaf};
use crate::shell::kit::{quiet, text};
use facet::tokens::ty;
use facet::Space;
use gpui::{ParentElement, Styled, div};

pub(super) fn body(ctx: &mut Ctx<'_>) -> Vec<Leaf> {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let title = ctx.say("Inbox");
    let line = ctx.say("Nothing followed yet. Releases you follow arrive here once the local service publishes a release feed.");
    vec![Leaf::new(
        div()
            .flex()
            .flex_col()
            .gap(measure.space(Space::Roomy))
            .child(text(ty::DISPLAY, &measure, palette.ink0).child(title))
            .child(quiet(line, &measure, palette)),
    )]
}
