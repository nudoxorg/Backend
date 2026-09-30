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
use std::time::{Duration, Instant};
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

pub(crate) fn feedback_lines(snapshot: &AppSnapshot, focus: Option<&crate::runtime::graph_focus::GraphFocus>, notice: Option<&crate::runtime::graph_focus::Notice>, width: Pixels, cx: &App) -> (Vec<String>, TypeRole) {
    if let Some(notice) = notice.filter(|notice| notice.active(snapshot)) {
        let measure = Measure::new(width, &cx.facet());
        let role = measure.role(ty::MONO_SMALL);
        let room = (width - measure.space(Space::Roomy) * 2.0).max(px(1.0));
        return (wrap_identifier(&notice.message, &role, room, cx), role);
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
    notice: Option<&crate::runtime::graph_focus::Notice>,
) -> bool {
    notice.is_some_and(|notice| notice.active(snapshot)) || focus.is_some_and(|focus| focus.active(snapshot))
}

/// Where the hand's marks start: 20 px into the reader column, never past
/// a third of the bar.
pub(crate) fn marks_left(width: Pixels, reader_left: Pixels, scale: f32) -> Pixels {
    (reader_left + px(20.0 * scale)).min(width / 3.0)
}

/// The width the graph's line is set in: the whole bar with an empty hand;
/// beside the marks (the chevron and up to five stones) otherwise, so the
/// hand stays in the foot while the graph speaks.
pub(crate) fn line_room(width: Pixels, reader_left: Pixels, cards: usize, scale: f32) -> Pixels {
    if cards == 0 {
        return width;
    }
    #[allow(clippy::cast_precision_loss)]
    let marks = px((14.0 + 23.0 * cards as f32 + 16.0) * scale);
    (width - marks_left(width, reader_left, scale) - marks).max(px(160.0 * scale))
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
    /// The first-card whisper this foot is drawing, if any.
    whisper: Option<Whisper>,
    /// The hand at rest.
    marks: super::hand::Marks,
    opening: Option<SharedString>,
}

/// How long the first-card whisper stays.
const WHISPER: Duration = Duration::from_millis(2_400);

/// The first card ever held, as the foot first drew it: which, when (the
/// motion clock, virtual under the harness), and where it is in its time.
struct Whisper {
    held: crate::model::hand::Held,
    since: Instant,
    phase: WhisperPhase,
}

/// Where a whisper is: heard until its timer fires, then spent. The timer
/// ENDS it: the foot never asks a clock again whether the whisper is over
/// (a clock that disagrees with the timer would start it again, for ever).
enum WhisperPhase {
    /// Showing; the timer redraws the foot with the whisper spent.
    Heard(gpui::Task<()>),
    /// Over: it is not said again.
    Spent,
}

impl Whisper {
    /// How much of its time is left `now` (the motion clock): `None` once it
    /// has had it all.
    fn left(&self, now: Instant) -> Option<Duration> {
        whisper_left(self.since, now)
    }
}

/// How much of a whisper begun at `since` is left at `now`.
fn whisper_left(since: Instant, now: Instant) -> Option<Duration> {
    WHISPER.checked_sub(now.saturating_duration_since(since))
}

impl Status {
    pub(crate) fn new(links: Links, store: &DataStore) -> Self {
        Self {
            core: RegionCore::new(store, &[Branch::Route, Branch::Overlay, Branch::GraphFocus, Branch::Hand]),
            links,
            reader_left: px(0.0),
            whisper: None,
            marks: super::hand::Marks::default(),
            opening: None,
        }
    }

    /// Where the reader column starts, from the shell's frame.
    pub(crate) fn set_reader_left(&mut self, left: Pixels) {
        self.reader_left = left;
    }

    pub(crate) fn set_opening(&mut self, opening: Option<SharedString>, cx: &mut Context<Self>) {
        if self.opening != opening {
            self.opening = opening;
            cx.notify();
        }
    }

    /// How many marks the foot draws (tests).
    #[cfg(test)]
    pub(crate) fn marks_drawn(&self) -> usize {
        self.marks.drawn()
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.core.rendered();
        let measure = self.core.measure(cx);
        let palette = cx.facet().palette();
        let store = self.links.store.read(cx);
        let snapshot = store.snapshot();
        let foot = div().size_full().flex().flex_col().justify_center().border_t_1().border_color(palette.line1.hsla());
        if let Some(opening) = &self.opening {
            return foot.pl((self.reader_left + px(20.0 * measure.scale())).min(self.core.width() / 3.0))
                .child(super::kit::text(ty::MONO_SMALL, &measure, palette.ink1).child(opening.clone()));
        }
        let (focus, notice) = (store.graph_focus().cloned(), store.notice().cloned());
        let speaks = graph_speaks(&snapshot, focus.as_ref(), notice.as_ref());
        let retry = retry_button(notice.as_ref(), &snapshot, &self.links, &measure);
        let hand = snapshot.session().hand.clone();
        if !speaks || !hand.is_empty() {
            let view = crate::runtime::hand::hand_view_for(&hand, &snapshot, cx);
            let left = marks_left(self.core.width(), self.reader_left, measure.scale());
            // The first card ever held: "Value *in hand*", once per install.
            // Timed on the motion clock (virtual under the harness), from
            // the frame that first drew it.
            let now = facet::motion::now(cx);
            let whisper = snapshot.session().whisper.clone().and_then(|held| {
                if !self.whisper.as_ref().is_some_and(|seen| seen.held.same(&held)) {
                    // Heard from the frame that first drew it, for its time.
                    let timer = cx.spawn(async move |status, cx| {
                        cx.background_executor().timer(WHISPER).await;
                        let _ = status.update(cx, |status, cx| {
                            if let Some(seen) = status.whisper.as_mut() {
                                seen.phase = WhisperPhase::Spent;
                            }
                            cx.notify();
                        });
                    });
                    self.whisper = Some(Whisper { held: held.clone(), since: now, phase: WhisperPhase::Heard(timer) });
                }
                let seen = self.whisper.as_mut()?;
                match seen.phase {
                    WhisperPhase::Heard(_) if seen.left(now).is_some() => Some(held),
                    WhisperPhase::Heard(_) | WhisperPhase::Spent => {
                        seen.phase = WhisperPhase::Spent;
                        None
                    }
                }
            });
            let words = whisper.map(|held| {
                let name = view
                    .cards
                    .iter()
                    .find(|card| card.held.same(&held))
                    .map_or_else(|| SharedString::default(), |card| card.name.clone());
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(5.0 * measure.scale()))
                    .child(super::kit::text(ty::MONO_SMALL, &measure, palette.ink1).child(name))
                    .child(super::kit::text(ty::CAPTION, &measure, palette.ink3).child("in hand"))
            });
            // The graph's line sits to the right of the marks, on their row.
            let said = speaks.then(|| {
                let room = line_room(self.core.width(), self.reader_left, hand.held().len(), measure.scale());
                let (lines, role) = feedback_lines(&snapshot, focus.as_ref(), notice.as_ref(), room, cx);
                div().flex().flex_col().min_w(px(0.0)).children(said_lines(lines, role, palette.ink3.hsla()))
            });
            let mut row = div()
                    .flex()
                    .items_center()
                    .gap(measure.space(Space::Roomy))
                    .children(self.marks.render(&view, &self.links, &measure, palette, window, cx))
                    .children(words)
                    .children(said)
                    .children(retry);
            if let Some(status) = &view.status {
                row = row.child(super::kit::text(ty::CAPTION, &measure, palette.ink2).child(status.to_string()));
            }
            return foot.pl(left).child(row);
        }
        let (lines, role) = feedback_lines(&snapshot, focus.as_ref(), notice.as_ref(), self.core.width(), cx);
        if retry.is_some() {
            return foot.px(measure.space(Space::Roomy)).child(
                div()
                    .flex()
                    .items_center()
                    .gap(measure.space(Space::Roomy))
                    .child(div().flex().flex_col().min_w(px(0.0)).children(said_lines(lines, role, palette.ink3.hsla())))
                    .children(retry),
            );
        }
        foot.px(measure.space(Space::Roomy)).children(said_lines(lines, role, palette.ink3.hsla()))
    }
}

/// "Try again" beside a notice that has something to retry (the index could
/// not start: asking for the page again starts the owner again).
fn retry_button(
    notice: Option<&crate::runtime::graph_focus::Notice>,
    snapshot: &AppSnapshot,
    links: &Links,
    measure: &Measure,
) -> Option<gpui::AnyElement> {
    let notice = notice.filter(|notice| notice.active(snapshot))?;
    let key = notice.retry.clone()?;
    let links = links.clone();
    Some(
        facet::controls::button("status-retry", "Try again", measure)
            .size(facet::Control::Small)
            .ghost()
            .on_click(move |_, cx| links.retry(key.clone(), cx))
            .into_any_element(),
    )
}

/// The graph's line(s), each published to the probe as `address:{n}:…`.
fn said_lines(lines: Vec<String>, role: facet::tokens::TypeRole, color: gpui::Hsla) -> impl Iterator<Item = gpui::AnyElement> {
    lines.into_iter().enumerate().map(move |(index, line)| {
                let words = SharedString::from(line);
                facet::probe::text(
                    ElementId::Name(format!("address:{index}:{words}").into()),
                    words.clone(),
                    role,
                    1.0,
                    facet::probe::TextOverflow::Clip,
                    div().whitespace_nowrap().typeset_at(role, 1.0).text_color(color).child(words),
                )
                .into_any_element()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A whisper has its 2.4 s and no more; whether it is over is decided by
    /// its own time and its timer, never by a clock asked again and again.
    #[test]
    fn a_whisper_has_two_and_four_tenths_seconds() {
        let since = Instant::now();
        assert_eq!(whisper_left(since, since), Some(WHISPER));
        assert_eq!(whisper_left(since, since + Duration::from_millis(1_000)), Some(Duration::from_millis(1_400)));
        assert_eq!(whisper_left(since, since + WHISPER), Some(Duration::ZERO), "the last instant is still its own");
        assert_eq!(whisper_left(since, since + WHISPER + Duration::from_millis(1)), None);
        let earlier = since.checked_sub(Duration::from_secs(1)).unwrap_or(since);
        assert_eq!(whisper_left(since, earlier), Some(WHISPER), "a clock that steps back does not stretch it");
    }
}
