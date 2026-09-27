//! The foot: the hand's marks at rest (D-Hand's Mark rung), from the reader
//! column's left edge; the graph's focus and notices while it has them. The
//! address left the foot (the jump bar says where you are; ⌘⇧C copies the
//! `nudox://` address; hovering the jump bar's plate shows it).

use super::region::{Links, Region, RegionCore};
use super::text_fit::{text_width, wrap_identifier};
use super::jump::{self, Address};
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
    (fit(&jump::address_parts(snapshot), &role, room, cx), role)
}

/// A graph focus has explicit fixture provenance, not an invented URL.
pub(crate) fn display_lines(snapshot: &AppSnapshot, focus: Option<&crate::runtime::graph_focus::GraphFocus>, width: Pixels, cx: &App) -> (Vec<String>, TypeRole) {
    let Some(focus) = focus.filter(|focus| focus.active(snapshot)) else { return address_lines(snapshot, width, cx); };
    let measure = Measure::new(width, &cx.facet());
    let role = measure.role(ty::MONO_SMALL);
    let room = (width - measure.space(Space::Roomy) * 2.0).max(px(1.0));
    (wrap_identifier(&focus.status(), &role, room, cx), role)
}

pub(crate) fn feedback_lines(snapshot: &AppSnapshot, focus: Option<&crate::runtime::graph_focus::GraphFocus>, notice: Option<&crate::runtime::graph_focus::GraphNotice>, width: Pixels, cx: &App) -> (Vec<String>, TypeRole) {
    if let Some(notice) = notice.filter(|notice| notice.active(snapshot)) {
        let measure = Measure::new(width, &cx.facet());
        let role = measure.role(ty::MONO_SMALL);
        let room = (width - measure.space(Space::Roomy) * 2.0).max(px(1.0));
        return (wrap_identifier(&format!("Graph · {}", notice.message), &role, room, cx), role);
    }
    display_lines(snapshot, focus, width, cx)
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

/// Whether the graph has something to say in the foot (its focus or a
/// notice), which then takes the foot.
pub(crate) fn graph_speaks(
    snapshot: &AppSnapshot,
    focus: Option<&crate::runtime::graph_focus::GraphFocus>,
    notice: Option<&crate::runtime::graph_focus::GraphNotice>,
) -> bool {
    notice.is_some_and(|notice| notice.active(snapshot)) || focus.is_some_and(|focus| focus.active(snapshot))
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
    /// Where the reader column starts (the hand's marks start 20 px in).
    reader_left: Pixels,
}

impl Status {
    pub(crate) fn new(links: Links, store: &DataStore) -> Self {
        Self {
            core: RegionCore::new(store, &[Branch::Route, Branch::Overlay, Branch::GraphFocus, Branch::Hand]),
            links,
            reader_left: px(0.0),
        }
    }

    /// Where the reader column starts, from the shell's frame.
    pub(crate) fn set_reader_left(&mut self, left: Pixels) {
        self.reader_left = left;
    }

    /// Where the reader column starts, as last set.
    pub(crate) const fn reader_left(&self) -> Pixels {
        self.reader_left
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
        let store = self.links.store.read(cx);
        let snapshot = store.snapshot();
        let foot = div().size_full().flex().flex_col().justify_center().border_t_1().border_color(palette.line1.hsla());
        if !graph_speaks(&snapshot, store.graph_focus(), store.graph_notice()) {
            let hand = snapshot.session().hand.clone();
            let view = crate::runtime::fixture_world::hand_view(&hand, cx);
            let left = (self.reader_left + px(20.0 * measure.scale())).min(self.core.width() / 3.0);
            return foot.pl(left).children(super::hand::marks(&view, &self.links, &measure, palette));
        }
        let (lines, role) = feedback_lines(&snapshot, store.graph_focus(), store.graph_notice(), self.core.width(), cx);
        let color = palette.ink3.hsla();
        foot
            .px(measure.space(Space::Roomy))
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
