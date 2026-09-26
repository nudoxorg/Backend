//! The status bar region: the mono address of the current place, and
//! nothing else (calm targets). Transient states are said once, in place,
//! by the region they belong to.

use super::region::{Links, Region, RegionCore};
use super::text_fit::{text_width, wrap_identifier};
use super::thread::{self, Address};
use crate::model::AppSnapshot;
use crate::runtime::store::{Branch, DataStore};
use facet::tokens::{TypeRole, ty};
use facet::{ActiveFacet as _, Measure, Space, Typeset as _};
use gpui::{App, Context, ElementId, IntoElement, ParentElement, Pixels, Render, SharedString, Styled, Window, div, px};

/// The address for a status bar `width` wide, in the lines it is set in and
/// the role it is set in. Whole when it fits; otherwise its path gives way
/// from the left (`…/glyph/RelationLabel`) and the name stays whole; a name
/// wider than the bar wraps at identifier boundaries. Never a cut name.
///
/// The root sizes the bar from the same lines, in the same frame.
pub(crate) fn address_lines(snapshot: &AppSnapshot, width: Pixels, cx: &App) -> (Vec<String>, TypeRole) {
    let measure = Measure::new(width, &cx.facet());
    let role = measure.role(ty::MONO_SMALL);
    let room = (width - measure.space(Space::Roomy) * 2.0).max(px(1.0));
    (fit(&thread::address_parts(snapshot), &role, room, cx), role)
}

fn fit(address: &Address, role: &TypeRole, room: Pixels, cx: &App) -> Vec<String> {
    let fits = |line: &str| text_width(line, role, cx) <= room * 0.98;
    let full = address.full();
    if fits(&full) {
        return vec![full];
    }
    for keep in (0..address.path.len()).rev() {
        let cut = address.keeping(keep);
        if fits(&cut) {
            return vec![cut];
        }
    }
    wrap_identifier(&address.keeping(0), role, room, cx)
}

/// How tall the status bar is for `lines` of address in `role`, given its
/// one-line height.
pub(crate) fn height(lines: usize, role: &TypeRole, one_line: f32) -> f32 {
    one_line + role.line * lines.saturating_sub(1) as f32
}

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
        let (lines, role) = address_lines(&self.links.snapshot(cx), self.core.width(), cx);
        let color = palette.ink3.hsla();
        div()
            .size_full()
            .flex()
            .flex_col()
            .justify_center()
            .px(measure.space(Space::Roomy))
            .border_t_1()
            .border_color(palette.line1.hsla())
            .children(lines.into_iter().enumerate().map(move |(index, line)| {
                let words = SharedString::from(line);
                facet::probe::text(
                    ElementId::Name(format!("address:{index}:{words}").into()),
                    words.clone(),
                    role,
                    1.0,
                    facet::probe::TextOverflow::Clip,
                    div().whitespace_nowrap().typeset_at(role, 1.0).text_color(color).child(words),
                )
            }))
    }
}
