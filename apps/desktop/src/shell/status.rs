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
use gpui::{App, AppContext as _, Context, ElementId, InteractiveElement, IntoElement, ParentElement, Pixels, Render, SharedString, StatefulInteractiveElement, Styled, Window, div, px};

/// The foot keeps a readable sample; its native status and disclosure retain
/// the exact full message. This bound also owns the shell's height calculation.
const NOTICE_LINES: usize = 3;

struct Feedback {
    message: String,
    lines: Vec<String>,
    role: TypeRole,
    clipped: bool,
}

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
    let feedback = feedback(snapshot, focus, notice, width, cx);
    (feedback.lines, feedback.role)
}

fn feedback(snapshot: &AppSnapshot, focus: Option<&crate::runtime::graph_focus::GraphFocus>, notice: Option<&crate::runtime::graph_focus::Notice>, width: Pixels, cx: &App) -> Feedback {
    let notice = notice.filter(|notice| notice.active(snapshot));
    let message = feedback_message(snapshot, focus, notice);
    let measure = Measure::new(width, &cx.facet());
    let role = measure.role(ty::MONO_SMALL);
    let gap = measure.space(Space::Roomy);
    let retry = notice.is_some_and(|notice| notice.retry.is_some());
    let retry_room = if retry { button_room("Try again", &measure, cx) + gap } else { px(0.0) };
    let room = (width - gap * 2.0 - retry_room).max(px(1.0));
    let mut lines = wrap_identifier(&message, &role, room, cx);
    let clipped = lines.len() > NOTICE_LINES;
    if clipped {
        let room = (room - button_room("Details", &measure, cx) - gap).max(px(1.0));
        lines = wrap_identifier(&message, &role, room, cx);
        lines.truncate(NOTICE_LINES);
        if let Some(last) = lines.last_mut() {
            while !last.is_empty() && text_width(&format!("{last}…"), &role, cx) > room * 0.98 { last.pop(); }
            last.push('…');
        }
    }
    Feedback { message, lines, role, clipped }
}

fn feedback_message(snapshot: &AppSnapshot, focus: Option<&crate::runtime::graph_focus::GraphFocus>, notice: Option<&crate::runtime::graph_focus::Notice>) -> String {
    notice.filter(|notice| notice.active(snapshot)).map_or_else(
        || focus.filter(|focus| focus.active(snapshot)).map_or_else(|| jump::address_parts(snapshot).full(), |focus| focus.status()),
        |notice| notice.message.to_string(),
    )
}

fn button_room(label: &str, measure: &Measure, cx: &App) -> Pixels {
    facet::controls::button::label_width(label, facet::Control::Small, measure, cx)
}

/// The shell and rendered foot consume this same bounded presentation. Held
/// marks get a separate line, preserving recovery controls at narrow widths.
pub(crate) fn feedback_height(snapshot: &AppSnapshot, focus: Option<&crate::runtime::graph_focus::GraphFocus>, notice: Option<&crate::runtime::graph_focus::Notice>, width: Pixels, cards: usize, one_line: f32, cx: &App) -> f32 {
    let (lines, role) = feedback_lines(snapshot, focus, notice, width, cx);
    height(lines.len(), &role, one_line) + if cards == 0 { 0.0 } else { one_line }
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

/// Feedback owns its own row; the held marks never reduce its readable room.
pub(crate) fn line_room(width: Pixels, _reader_left: Pixels, _cards: usize, _scale: f32) -> Pixels {
    width
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
    /// A disclosure is scoped to the current full message and visible visit.
    details: Option<String>,
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
            details: None,
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

    fn observe(&mut self, event: &crate::runtime::store::StoreEvent, _store: &DataStore) {
        if event.is_branch(Branch::Route) || event.is_branch(Branch::Root) || event.is_branch(Branch::Overlay) {
            self.details = None;
        }
    }
}

impl Render for Status {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.core.rendered();
        let measure = self.core.measure(cx);
        let palette = cx.facet().palette();
        let store = self.links.store.read(cx);
        let snapshot = store.snapshot();
        let foot = div().relative().size_full().flex().flex_col().justify_center().border_t_1().border_color(palette.line1.hsla());
        if let Some(opening) = &self.opening {
            return foot.pl((self.reader_left + px(20.0 * measure.scale())).min(self.core.width() / 3.0))
                .child(super::kit::text(ty::MONO_SMALL, &measure, palette.ink1).role(gpui::Role::Status).aria_label(opening.clone()).child(opening.clone()));
        }
        let (focus, notice) = (store.graph_focus().cloned(), store.notice().cloned());
        let speaks = graph_speaks(&snapshot, focus.as_ref(), notice.as_ref());
        let hand = snapshot.session().hand.clone();
        if !speaks || !hand.is_empty() {
            let view = crate::runtime::hand::hand_view_for(&hand, &snapshot, cx);
            let left = marks_left(self.core.width(), self.reader_left, measure.scale())
                .min((self.core.width() - px((14.0 + 23.0 * view.cards.len() as f32 + 16.0) * measure.scale()) - measure.space(Space::Roomy)).max(px(0.0)));
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
                    .min_w_0()
                    .items_baseline()
                    .gap(px(5.0 * measure.scale()))
                    .child(super::kit::text(ty::MONO_SMALL, &measure, palette.ink1).min_w_0().truncate().role(gpui::Role::Label).aria_label(name.clone()).child(name))
                    .child(super::kit::text(ty::CAPTION, &measure, palette.ink3).child("in hand"))
            });
            let mut row = div()
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap(measure.space(Space::Roomy))
                    .children(self.marks.render(&view, &self.links, &measure, palette, window, cx))
                    .children(words);
            if let Some(status) = &view.status {
                row = row.child(super::kit::text(ty::CAPTION, &measure, palette.ink2).min_w_0().flex_1().truncate().role(gpui::Role::Status).aria_label(status.to_string()).child(status.to_string()));
            }
            let foot = foot.child(row.pl(left));
            if !speaks { return foot; }
            return foot.child(self.render_feedback(&snapshot, focus.as_ref(), notice.as_ref(), &measure, window, cx));
        }
        foot.child(self.render_feedback(&snapshot, focus.as_ref(), notice.as_ref(), &measure, window, cx))
    }
}

impl Status {
    fn render_feedback(&mut self, snapshot: &AppSnapshot, focus: Option<&crate::runtime::graph_focus::GraphFocus>, notice: Option<&crate::runtime::graph_focus::Notice>, measure: &Measure, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let feedback = feedback(snapshot, focus, notice, self.core.width(), cx);
        let palette = cx.facet().palette();
        if self.details.as_deref() != Some(&feedback.message) { self.details = None; }
        let details_focus = window.use_keyed_state("status-details-focus", cx, |_, cx| cx.focus_handle().tab_stop(true)).read(cx).clone();
        let close_focus = window.use_keyed_state("status-details-close-focus", cx, |_, cx| cx.focus_handle().tab_stop(true)).read(cx).clone();
        let retry = retry_button(notice, snapshot, &self.links, measure, cx);
        let mut row = div().flex().items_center().gap(measure.space(Space::Roomy)).px(measure.space(Space::Roomy))
            .child(div().id("status-message").role(gpui::Role::Status).aria_label(feedback.message.clone())
                .min_w_0().flex_1().flex().flex_col().overflow_hidden()
                .children(said_lines(feedback.lines, feedback.role, palette.ink3.hsla())))
            .children(retry);
        if feedback.clipped {
            let message = feedback.message.clone();
            let route = snapshot.route().clone();
            let authority = snapshot.key().authority();
            let owner = cx.entity().downgrade();
            let open_focus = close_focus.clone();
            row = row.child(facet::controls::button("status-details", "Details", measure)
                .aria_label("Show status details").focus_handle(details_focus.clone())
                .size(facet::Control::Small).ghost().on_click(move |window, cx| {
                    if let Some(status) = owner.upgrade() {
                        status.update(cx, |status, cx| {
                            let current = status.links.snapshot(cx);
                            let store = status.links.store.read(cx);
                            let same_message = feedback_message(&current, store.graph_focus(), store.notice()) == message;
                            let background = status.links.shell.upgrade().is_some_and(|shell| shell.read(cx).transients() == (false, false, false));
                            if background && same_message && current.route() == &route && current.key().authority() == authority && current.overlay().is_none() {
                                status.details = Some(message.clone());
                                open_focus.focus(window, cx);
                                cx.notify();
                            }
                        });
                    }
                }));
        }
        if self.details.is_some() {
            let owner = cx.entity().downgrade();
            let return_focus = details_focus.clone();
            let close = facet::controls::button("status-details-close", "Close details", measure)
                .focus_handle(close_focus).size(facet::Control::Small).ghost()
                .on_click(move |window, cx| {
                    if let Some(status) = owner.upgrade() { status.update(cx, |status, cx| {
                        status.details = None;
                        return_focus.focus(window, cx);
                        cx.notify();
                    }); }
                });
            let return_focus = details_focus.clone();
            row = row.child(div().id("status-details-panel").role(gpui::Role::Dialog).aria_label("Status details")
                .absolute().bottom(gpui::relative(1.0)).left(measure.space(Space::Roomy)).right(measure.space(Space::Roomy))
                .flex().flex_col().gap(measure.space(Space::Base)).p(measure.space(Space::Roomy))
                .bg(palette.plate2).border_1().border_color(palette.line2.hsla())
                .on_action(cx.listener(move |status, _: &super::keys::Escape, window, cx| {
                    status.details = None; return_focus.focus(window, cx); cx.notify(); cx.stop_propagation();
                }))
                .child(div().id("status-details-scroll").max_h(window.viewport_size().height * 0.4).overflow_y_scroll()
                    .child(super::kit::text(ty::SMALL, measure, palette.ink1)
                        .role(gpui::Role::Label).aria_label(feedback.message.clone()).keyed("status-full-message").child(feedback.message)))
                .child(close));
        }
        row.into_any_element()
    }
}

/// "Try again" beside a notice that has something to retry (the index could
/// not start: asking for the page again starts the owner again).
fn retry_button(
    notice: Option<&crate::runtime::graph_focus::Notice>,
    snapshot: &AppSnapshot,
    links: &Links,
    measure: &Measure,
    cx: &App,
) -> Option<gpui::AnyElement> {
    let notice = notice.filter(|notice| notice.active(snapshot))?;
    let key = notice.retry.clone()?;
    let captured = notice.clone();
    let store = links.store.read(cx);
    let owner_retry = store.current_owner_retry();
    let serving = store.current_owner_attachment();
    let enabled = owner_retry.is_some() || serving.is_some();
    let links = links.clone();
    Some(
        facet::controls::button("status-retry", "Try again", measure)
            .size(facet::Control::Small)
            .ghost()
            .disabled(!enabled)
            .on_click(move |_, cx| {
                if !links.shell.upgrade().is_some_and(|shell| shell.read(cx).transients() == (false, false, false)) { return; }
                links.store.update(cx, |store, cx| {
                    if !store.notice().is_some_and(|current| current == &captured && current.active(&store.snapshot())) { return; }
                    if let Some(token) = &owner_retry { let _ = store.retry_owner_at(token, key.clone(), cx); }
                    else if serving.as_ref().is_some_and(|attachment| store.admits_owner_attachment(attachment)) { store.retry(key.clone(), cx); }
                });
            })
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
    use crate::navigation::{Intent, OrbitRoute, Route};
    use crate::model::AppearancePreference;
    use gpui::TestAppContext;

    fn native_tree(rig: &mut crate::shell::tests::Rig) -> serde_json::Value {
        rig.cx.update(|window, _| window.set_a11y_forced(true));
        rig.repaint();
        let json = rig.cx.update(|window, _| window.debug_a11y_tree_json()).expect("native status tree");
        serde_json::from_str(&json).expect("native tree JSON")
    }

    fn long_notice(rig: &mut crate::shell::tests::Rig) -> String {
        let message = "The local index could not answer this request. The connection ended before the package read completed; try again after the connection is restored. ".repeat(12);
        rig.graph.store.update(rig.cx, |store, cx| {
            let snapshot = store.snapshot();
            store.set_notice(Some(crate::runtime::graph_focus::Notice {
                visit: snapshot.route().clone(), root: snapshot.key(), message: message.clone().into(), retry: Some(crate::model::pages::PageKey::Orbit),
            }), cx);
        });
        rig.settle();
        message
    }

    /// Real paint bounds plus the committed AccessKit tree, not the fitter's
    /// output alone. Run on each actual window width and saved text size.
    #[gpui::test]
    fn status_notice_reserves_native_recovery_room_across_width_text_and_theme(cx: &mut TestAppContext) {
        let mut rig = crate::shell::tests::rig(cx, Some(Route::Orbit(OrbitRoute::Home)), 1440.0, 900.0);
        let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
        for appearance in [AppearancePreference::Abyss, AppearancePreference::Glacier] {
            rig.go(Intent::SetAppearance(appearance));
            for percent in [100_u16, 150, 200] {
                rig.go(Intent::ZoomTo { display: display.clone(), percent });
                for width in [360.0, 480.0, 663.0, 1440.0] {
                    rig.cx.simulate_resize(gpui::size(px(width), px(900.0)));
                    rig.settle();
                    let message = long_notice(&mut rig);
                    let tree = native_tree(&mut rig);
                    let nodes = tree["nodes"].as_object().expect("native nodes");
                    assert!(nodes.values().any(|node| node["aria"]["role"].as_str() == Some("Status") && node["aria"]["label"].as_str() == Some(message.as_str())), "the full message must survive painted truncation");
                    for label in ["Try again", "Show status details"] {
                        let node = nodes.values().find(|node| node["aria"]["label"].as_str() == Some(label)).expect("native recovery/disclosure control");
                        assert_eq!(node["aria"]["role"].as_str(), Some("Button"));
                        assert!(node["aria"]["on_action"].as_array().is_some_and(|actions| actions.iter().any(|action| action.as_str() == Some("Click"))));
                    }
                    let retry = rig.cx.debug_bounds("status-retry").expect("painted retry");
                    let details = rig.cx.debug_bounds("status-details").expect("painted disclosure");
                    assert!(retry.right() <= details.left() + px(0.5), "recovery controls overlap at {width}px/{percent}%");
                    assert!(details.right() <= px(width) && retry.left() >= px(0.0));
                    rig.cx.update(|_, cx| { let _ = facet::probe::take(cx); });
                    let ledger = crate::shell::anatomy_tests::painted(&mut rig);
                    let lines: Vec<_> = ledger.texts.iter().filter(|text| text.key.starts_with("address:")).collect();
                    assert_eq!(lines.len(), NOTICE_LINES);
                    for line in lines {
                        assert!(!line.clipped_without_ellipsis() && !line.clipped_vertically(), "{width}px/{percent}%: {line:?}");
                        assert!(line.bounds.x + line.bounds.width <= f32::from(retry.left()) + 0.5, "message overlaps retry at {width}px/{percent}%");
                        assert!(line.bounds.y + line.bounds.height <= 900.5, "message leaves the native window");
                    }
                }
            }
        }
    }

    #[gpui::test]
    fn native_status_details_expand_full_message_and_escape_returns_focus(cx: &mut TestAppContext) {
        let mut rig = crate::shell::tests::rig(cx, Some(Route::Orbit(OrbitRoute::Home)), 663.0, 900.0);
        let message = long_notice(&mut rig);
        native_tree(&mut rig);
        let details = rig.cx.debug_bounds("status-details").expect("native details button");
        rig.cx.simulate_click(details.center(), gpui::Modifiers::default());
        rig.settle();
        let tree = native_tree(&mut rig);
        let focus = tree["accesskit_focus"].as_str().expect("focus id");
        assert_eq!(tree["nodes"][focus]["aria"]["label"].as_str(), Some("Close details"));
        assert!(tree["nodes"].as_object().expect("nodes").values().any(|node| node["aria"]["role"].as_str() == Some("Label") && node["aria"]["label"].as_str() == Some(message.as_str())));
        let panel = rig.cx.debug_bounds("status-details-panel").expect("painted expansion");
        assert!(panel.left() >= px(0.0) && panel.right() <= px(663.0) && panel.top() >= px(0.0));
        rig.keys("escape");
        let tree = native_tree(&mut rig);
        let focus = tree["accesskit_focus"].as_str().expect("return focus id");
        assert_eq!(tree["nodes"][focus]["aria"]["label"].as_str(), Some("Show status details"));
        assert!(rig.cx.debug_bounds("status-details-panel").is_none());
    }

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
