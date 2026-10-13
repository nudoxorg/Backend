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
use std::rc::Rc;
use gpui::{App, AppContext as _, Context, ElementId, InteractiveElement, IntoElement, ParentElement, Pixels, Render, SharedString, StatefulInteractiveElement, Styled, Window, div, px};

/// The foot keeps a readable sample; its native status and disclosure retain
/// the exact full message. This bound also owns the shell's height calculation.
const NOTICE_LINES: usize = 3;

struct Feedback {
    message: String,
    lines: Vec<String>,
    role: TypeRole,
    clipped: bool,
    controls: Controls,
    extra_height: Pixels,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Controls { Inline, Below, Stacked }

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

pub(crate) fn feedback_lines(snapshot: &AppSnapshot, focus: Option<&crate::runtime::graph_focus::GraphFocus>, notice: Option<&crate::runtime::graph_focus::Notice>, width: Pixels, window: &Window, cx: &App) -> (Vec<String>, TypeRole) {
    let feedback = feedback(snapshot, focus, notice, width, window, cx);
    (feedback.lines, feedback.role)
}

fn feedback(snapshot: &AppSnapshot, focus: Option<&crate::runtime::graph_focus::GraphFocus>, notice: Option<&crate::runtime::graph_focus::Notice>, width: Pixels, window: &Window, cx: &App) -> Feedback {
    let notice = notice.filter(|notice| notice.active(snapshot));
    let message = feedback_message(snapshot, focus, notice);
    let measure = Measure::new(width, &cx.facet());
    let role = measure.role(ty::MONO_SMALL);
    let gap = measure.space(Space::Roomy);
    let retry = notice.is_some_and(|notice| notice.retry.is_some());
    let full_room = (width - gap * 2.0).max(px(1.0));
    let full_lines = wrap_identifier(&message, &role, full_room, cx);
    let clipped = full_lines.len() > NOTICE_LINES;
    let retry_width = if retry { button_room("Try again", &measure, window) } else { px(0.0) };
    let details_width = if clipped { button_room("Details", &measure, window) } else { px(0.0) };
    let control_gap = if retry && clipped { gap } else { px(0.0) };
    let controls_width = retry_width + details_width + control_gap;
    let inline_room = (full_room - controls_width - if controls_width > px(0.0) { gap } else { px(0.0) }).max(px(1.0));
    // One meaningful short phrase must fit beside the controls. If the full
    // width needs only three lines but an inline retry would force a fourth,
    // put the controls below instead of manufacturing a Details button.
    let readable_room = text_width("Index status", &role, cx).min(full_room / 2.0);
    let inline_fits = inline_room >= readable_room
        && (clipped || wrap_identifier(&message, &role, inline_room, cx).len() <= NOTICE_LINES);
    let controls = if inline_fits { Controls::Inline }
        else if controls_width <= full_room { Controls::Below }
        else { Controls::Stacked };
    let room = if controls == Controls::Inline { inline_room } else { full_room };
    let mut lines = wrap_identifier(&message, &role, room, cx);
    if clipped {
        lines.truncate(NOTICE_LINES);
        if let Some(last) = lines.last_mut() {
            while !last.is_empty() && text_width(&format!("{last}…"), &role, cx) > room * 0.98 { last.pop(); }
            last.push('…');
        }
    }
    let control_rows = match controls {
        Controls::Inline => 0,
        Controls::Below => 1,
        Controls::Stacked => usize::from(retry) + usize::from(clipped),
    };
    let extra_height = if control_rows == 0 { px(0.0) }
        else { measure.control(facet::Control::Small) * control_rows as f32 + gap * control_rows as f32 };
    Feedback { message, lines, role, clipped, controls, extra_height }
}

fn feedback_message(snapshot: &AppSnapshot, focus: Option<&crate::runtime::graph_focus::GraphFocus>, notice: Option<&crate::runtime::graph_focus::Notice>) -> String {
    notice.filter(|notice| notice.active(snapshot)).map_or_else(
        || focus.filter(|focus| focus.active(snapshot)).map_or_else(|| jump::address_parts(snapshot).full(), |focus| focus.status()),
        |notice| notice.message.to_string(),
    )
}

/// Disclosure belongs to the represented local message and reading visit;
/// it does not need a serving index owner or start another page read.
fn message_admission(links: &Links, snapshot: &AppSnapshot, message: String, cx: &App) -> facet::controls::button::ActivationAdmission {
    let links = links.clone();
    let visit = crate::runtime::content_visit::ContentVisit::capture(snapshot);
    let scope = links.shell.upgrade().map(|shell| shell.read(cx).local_activation_scope());
    std::rc::Rc::new(move |cx| {
        let store = links.store.read(cx);
        let current = store.snapshot();
        visit.current(&current) && current.overlay().is_none()
            && feedback_message(&current, store.graph_focus(), store.notice()) == message
            && links.shell.upgrade().is_some_and(|shell| {
                let shell = shell.read(cx);
                shell.transients() == (false, false, false)
                    && scope.is_some_and(|scope| shell.admits_local_activation_scope(scope, cx))
            })
    })
}

fn button_room(label: &str, measure: &Measure, window: &Window) -> Pixels {
    facet::controls::button::label_width(label, facet::Control::Small, measure, window)
}

/// The shell and rendered foot consume this same bounded presentation. Held
/// marks get a separate line, preserving recovery controls at narrow widths.
pub(crate) fn feedback_height(snapshot: &AppSnapshot, focus: Option<&crate::runtime::graph_focus::GraphFocus>, notice: Option<&crate::runtime::graph_focus::Notice>, width: Pixels, cards: usize, one_line: f32, window: &Window, cx: &App) -> f32 {
    let feedback = feedback(snapshot, focus, notice, width, window, cx);
    height(feedback.lines.len(), &feedback.role, one_line) + f32::from(feedback.extra_height)
        + if cards == 0 { 0.0 } else { one_line }
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
        let mut core = RegionCore::new(store, &[Branch::Route, Branch::Overlay, Branch::GraphFocus, Branch::Hand]);
        core.watch_reading_visit(store);
        Self {
            core,
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
        // Only a represented recovery page supplies a stamp dependency. This
        // watch renews its handlers without asking for another page read.
        self.core.watch_keys(store, store.notice().and_then(|notice| notice.retry.clone()));
        let snapshot = store.snapshot();
        let foot = div().relative().size_full().flex().flex_col().justify_center().border_t_1().border_color(palette.line1.hsla());
        if let Some(opening) = &self.opening {
            return foot.pl((self.reader_left + px(20.0 * measure.scale())).min(self.core.width() / 3.0))
                .child(super::kit::text(ty::MONO_SMALL, &measure, palette.ink1).keyed("status-opening").role(gpui::Role::Status).aria_label(opening.clone()).child(opening.clone()));
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
                    .child(super::kit::text(ty::MONO_SMALL, &measure, palette.ink1).keyed("status-whisper-name").min_w_0().truncate().role(gpui::Role::Label).aria_label(name.clone()).child(name))
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
                row = row.child(super::kit::text(ty::CAPTION, &measure, palette.ink2).keyed("status-hand-status").min_w_0().flex_1().truncate().role(gpui::Role::Status).aria_label(status.to_string()).child(status.to_string()));
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
        let feedback = feedback(snapshot, focus, notice, self.core.width(), window, cx);
        let palette = cx.facet().palette();
        if self.details.as_deref() != Some(&feedback.message) { self.details = None; }
        let details_focus = window.use_keyed_state("status-details-focus", cx, |_, cx| cx.focus_handle().tab_stop(true)).read(cx).clone();
        let close_focus = window.use_keyed_state("status-details-close-focus", cx, |_, cx| cx.focus_handle().tab_stop(true)).read(cx).clone();
        let retry = retry_button(notice, snapshot, &self.links, measure, cx);
        let disclosure = message_admission(&self.links, snapshot, feedback.message.clone(), cx);
        let mut message = div().id("status-message").role(gpui::Role::Status).aria_label(feedback.message.clone())
            .min_w_0().flex().flex_col().overflow_hidden()
            .children(said_lines(feedback.lines, feedback.role, palette.ink3.hsla()));
        message = if feedback.controls == Controls::Inline { message.flex_1() } else { message.w_full() };
        let details = if feedback.clipped {
            let message = feedback.message.clone();
            let admit = std::rc::Rc::clone(&disclosure);
            let owner = cx.entity().downgrade();
            let open_focus = close_focus.clone();
            Some(facet::controls::button("status-details", "Details", measure)
                .aria_label("Show status details").focus_handle(details_focus.clone())
                .size(facet::Control::Small).ghost().when_current(std::rc::Rc::clone(&admit))
                .on_click(move |window, cx| {
                    if !admit(cx) { return; }
                    if let Some(status) = owner.upgrade() {
                        status.update(cx, |status, cx| {
                            status.details = Some(message.clone());
                            open_focus.focus(window, cx);
                            cx.notify();
                        });
                    }
                }).into_any_element())
        } else { None };
        let gap = measure.space(Space::Roomy);
        let mut row = match feedback.controls {
            Controls::Inline => div().flex().items_center().gap(gap).px(gap)
                .child(message).children(retry).children(details),
            Controls::Below => div().flex().flex_col().gap(gap).px(gap)
                .child(message)
                .child(div().flex().items_center().gap(gap).children(retry).children(details)),
            Controls::Stacked => div().flex().flex_col().gap(gap).px(gap)
                .child(message)
                .children(retry.map(|button| div().flex().child(button)))
                .children(details.map(|button| div().flex().child(button))),
        };
        if self.details.is_some() {
            let owner = cx.entity().downgrade();
            let return_focus = details_focus.clone();
            let close_admit = std::rc::Rc::clone(&disclosure);
            let close = facet::controls::button("status-details-close", "Close details", measure)
                .focus_handle(close_focus).size(facet::Control::Small).ghost()
                .when_current(std::rc::Rc::clone(&close_admit))
                .on_click(move |window, cx| {
                    if !close_admit(cx) { return; }
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
                    if !disclosure(cx) { return; }
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

pub(super) fn retry_activation_name(expected: Option<&crate::runtime::store::PageRetryReceipt>) -> SharedString {
    expected.map(|receipt| receipt.activation_name("status-retry"))
        .unwrap_or_else(|| "status-retry:unadmitted".into())
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
    let enabled = store.current_owner_retry().is_some() || store.current_owner_attachment().is_some();
    let expected = store.capture_page_retry(&key);
    let activation_id = ElementId::Name(retry_activation_name(expected.as_ref()));
    let visit = snapshot.session().reading.current.id;
    let scope = links.shell.upgrade().map(|shell| shell.read(cx).local_activation_scope());
    let guarded_links = links.clone();
    let guarded_receipt = expected.clone();
    let admit: facet::controls::button::ActivationAdmission = Rc::new(move |cx| {
        if !enabled || !guarded_links.shell.upgrade().is_some_and(|shell| {
            let shell = shell.read(cx);
            shell.transients() == (false, false, false)
                && scope.is_some_and(|scope| shell.admits_local_activation_scope(scope, cx))
        }) { return false; }
        let store = guarded_links.store.read(cx);
        store.snapshot().session().reading.current.id == visit
            && store.notice().is_some_and(|current| current == &captured && current.active(&store.snapshot()))
            && guarded_receipt.as_ref().is_some_and(|expected| store.admits_page_retry(expected))
    });
    let links = links.clone();
    let action_admit = Rc::clone(&admit);
    Some(
        facet::controls::button("status-retry", "Try again", measure)
            .activation_id(activation_id)
            .size(facet::Control::Small)
            .ghost()
            .disabled(!enabled)
            .on_click(move |_, cx| {
                if !action_admit(cx) { return; }
                if let Some(expected) = &expected { links.retry_at(expected, cx); }
            })
            .when_current(admit)
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

    fn current_retry_name(rig: &mut crate::shell::tests::Rig) -> SharedString {
        rig.graph.store.read_with(&rig.cx, |store, _| {
            let key = store.notice().and_then(|notice| notice.retry.as_ref()).expect("represented recovery page");
            retry_activation_name(store.capture_page_retry(key).as_ref())
        })
    }

    fn gated_rig(cx: &mut TestAppContext, width: f32) -> (crate::shell::tests::Rig, crate::runtime::owner::OwnerGate) {
        let root = crate::core::VersionedRoot::synthetic(backend_library::view_state_root(&[("shell".to_owned(), "tests".to_owned())]), 4);
        let gate = crate::runtime::owner::OwnerGate::ready(root, crate::model::ServiceMode::Attached);
        let mut rig = crate::shell::tests::rig_with_engine_gate(cx, Some(Route::Orbit(OrbitRoute::Home)), width, 900.0,
            crate::runtime::reads::ReadPool::start(2, |_| crate::shell::tests::Fixture).expect("pool"), crate::shell::tests::RootOnly, Some(gate.clone()));
        (rig, gate)
    }

    fn long_notice(rig: &mut crate::shell::tests::Rig) -> String {
        rig.cx.update(|_, cx| facet::probe::enable(cx));
        let message = "The local index could not answer this request. The connection ended before the package read completed; try again after the connection is restored. ".repeat(12);
        rig.graph.store.update(&mut rig.cx, |store, cx| {
            let snapshot = store.snapshot();
            store.set_notice(Some(crate::runtime::graph_focus::Notice {
                visit: crate::runtime::content_visit::ContentVisit::capture(&snapshot), message: message.clone().into(), retry: Some(crate::model::pages::PageKey::Orbit),
            }), cx);
        });
        rig.settle();
        message
    }

    /// Real paint bounds plus the committed AccessKit tree, not the fitter's
    /// output alone. Run on each actual window width and saved text size.
    #[gpui::test]
    fn status_notice_reserves_native_recovery_room_across_width_text_and_theme(cx: &mut TestAppContext) {
        let (mut rig, _gate) = gated_rig(cx, 1440.0);
        let display = rig.shell.read_with(&rig.cx, |shell, _| shell.display_key());
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
                    let retry = crate::shell::tests::native_bounds(&mut rig, "Button", "Try again", true).expect("painted retry");
                    let details = crate::shell::tests::native_bounds(&mut rig, "Button", "Show status details", true).expect("painted disclosure");
                    assert!(retry.right() <= details.left() + px(0.5) || retry.bottom() <= details.top() + px(0.5),
                        "recovery controls overlap at {width}px/{percent}%");
                    assert!(details.right() <= px(width) && retry.left() >= px(0.0)
                        && details.bottom() <= px(900.0) && retry.bottom() <= px(900.0));
                    rig.cx.update(|_, cx| { let _ = facet::probe::take(cx); });
                    let ledger = crate::shell::anatomy_tests::painted(&mut rig);
                    let lines: Vec<_> = ledger.texts.iter().filter(|text| text.key.starts_with("address:")).collect();
                    assert_eq!(lines.len(), NOTICE_LINES);
                    for line in lines {
                        assert!(!line.clipped_without_ellipsis() && !line.clipped_vertically(), "{width}px/{percent}%: {line:?}");
                        assert!(line.bounds.x + line.bounds.width <= f32::from(retry.left()) + 0.5
                            || line.bounds.y + line.bounds.height <= f32::from(retry.top()) + 0.5,
                            "message overlaps retry at {width}px/{percent}%");
                        assert!(line.bounds.y + line.bounds.height <= 900.5, "message leaves the native window");
                    }
                }
            }
        }
    }

    #[gpui::test]
    fn native_status_details_expand_full_message_and_escape_returns_focus(cx: &mut TestAppContext) {
        let (mut rig, _gate) = gated_rig(cx, 663.0);
        let message = long_notice(&mut rig);
        native_tree(&mut rig);
        let details = crate::shell::tests::native_bounds(&mut rig, "Button", "Show status details", true).expect("native details button");
        rig.cx.simulate_click(details.center(), gpui::Modifiers::default());
        rig.settle();
        let tree = native_tree(&mut rig);
        let focus = tree["accesskit_focus"].as_str().expect("focus id");
        assert_eq!(tree["nodes"][focus]["aria"]["label"].as_str(), Some("Close details"));
        assert!(tree["nodes"].as_object().expect("nodes").values().any(|node| node["aria"]["role"].as_str() == Some("Label") && node["aria"]["label"].as_str() == Some(message.as_str())));
        let panel = crate::shell::tests::native_bounds(&mut rig, "Dialog", "Status details", false).expect("painted expansion");
        assert!(panel.left() >= px(0.0) && panel.right() <= px(663.0) && panel.top() >= px(0.0));
        rig.keys("escape");
        let tree = native_tree(&mut rig);
        let focus = tree["accesskit_focus"].as_str().expect("return focus id");
        assert_eq!(tree["nodes"][focus]["aria"]["label"].as_str(), Some("Show status details"));
        assert!(crate::shell::tests::native_bounds(&mut rig, "Dialog", "Status details", false).is_none());
    }

    #[gpui::test]
    fn native_retry_cannot_cross_an_identical_later_owner_failure(cx: &mut TestAppContext) {
        use crate::runtime::owner::OwnerState;
        let (mut rig, gate) = gated_rig(cx, 663.0);
        rig.cx.update(|_, cx| facet::probe::enable(cx));
        gate.publish(OwnerState::Failed("The fixture owner could not answer this read.".into()));
        rig.settle();
        native_tree(&mut rig);
        let retry_name = current_retry_name(&mut rig);
        let retry = crate::shell::tests::native_bounds_id(&mut rig, &retry_name, "Button", "Try again", true).expect("native failed-owner retry");
        rig.cx.simulate_event(gpui::MouseDownEvent { position: retry.center(), modifiers: gpui::Modifiers::default(), button: gpui::MouseButton::Left, click_count: 1, first_mouse: false });
        // Same words and same producer root are not the same retry capability.
        // Let the native pointer-up use its previously mounted listener.
        gate.publish(OwnerState::Starting);
        gate.publish(OwnerState::Failed("The fixture owner could not answer this read.".into()));
        rig.cx.simulate_event(gpui::MouseUpEvent { position: retry.center(), modifiers: gpui::Modifiers::default(), button: gpui::MouseButton::Left, click_count: 1 });
        assert!(matches!(gate.state(), OwnerState::Failed(_)), "an old native listener cannot restart a later failure");
        rig.settle();
        native_tree(&mut rig);
        let retry_name = current_retry_name(&mut rig);
        let retry = crate::shell::tests::native_bounds_id(&mut rig, &retry_name, "Button", "Try again", true).expect("fresh native retry");
        rig.cx.simulate_click(retry.center(), gpui::Modifiers::default());
        rig.draw();
        assert!(matches!(gate.state(), OwnerState::Starting), "a fresh native click consumes the current retry capability");
        gate.publish(OwnerState::Failed("No live producer can consume this retry.".into()));
        gate.disable_restart();
        rig.settle();
        let tree = native_tree(&mut rig);
        let retry_id = format!("{:?}", gpui::ElementId::Name(current_retry_name(&mut rig)));
        let node = tree["nodes"].as_object().expect("nodes").values().find(|node|
            node["element_id"].as_str() == Some(retry_id.as_str())
                && node["aria"]["role"].as_str() == Some("Button")
                && node["aria"]["label"].as_str() == Some("Try again")
        ).expect("disabled native retry button");
        assert_eq!(node["aria"]["disabled"].as_bool(), Some(true));
        assert!(!node["aria"]["on_action"].as_array().is_some_and(|actions| actions.iter().any(|action| action.as_str() == Some("Click"))));
        let retry_name = current_retry_name(&mut rig);
        let retry = crate::shell::tests::native_bounds_id(&mut rig, &retry_name, "Button", "Try again", false).expect("visible disabled retry");
        rig.cx.simulate_click(retry.center(), gpui::Modifiers::default());
        assert!(matches!(gate.state(), OwnerState::Failed(_)), "disabled native retry cannot invent a start");
    }

    #[gpui::test]
    fn retained_status_retry_cannot_replace_a_new_read_under_an_unchanged_notice(cx: &mut TestAppContext) {
        let (mut rig, _gate) = gated_rig(cx, 663.0);
        rig.go(Intent::SetMotion(crate::model::MotionPreference::Reduced));
        // The existing native notice fixture publishes the represented page
        // key through DataStore. The replacement below uses the actual pool.
        long_notice(&mut rig);
        native_tree(&mut rig);
        let (native, bounds) = rig.cx.update(|window, _| {
            let native = window.a11y_tree().expect("status AX").nodes.iter()
                .find(|(_, node)| node.role() == gpui::Role::Button && node.label() == Some("Try again")
                    && node.supports_action(gpui::AccessibleAction::Click)).map(|(id, _)| *id).expect("native status Retry");
            (native, window.a11y_node_bounds(native).expect("native status bounds"))
        });
        let key = crate::model::pages::PageKey::Orbit;
        let (notice, attachment, stamp, submitted, landed) = rig.graph.store.read_with(&rig.cx, |store, _| (
            store.notice().cloned(), store.current_owner_attachment(), store.stamp(&key), store.stats().submitted, store.stats().landed,
        ));
        let renders = rig.counts().status;
        let frame = rig.cx.update(|window, _| window.a11y_frame_number());
        let focused = rig.cx.update(|window, cx| {
            let original = window.focused(cx).expect("existing native focus");
            window.dispatch_event(gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                position: bounds.center(), modifiers: gpui::Modifiers::none(), button: gpui::MouseButton::Left,
                click_count: 1, first_mouse: false,
            }), cx);
            let focused = window.focused(cx);
            rig.graph.store.update(cx, |store, cx| store.retry(key.clone(), cx));
            assert_eq!(rig.graph.store.read(cx).notice().cloned(), notice);
            assert_eq!(rig.graph.store.read(cx).current_owner_attachment(), attachment);
            assert_ne!(rig.graph.store.read(cx).stamp(&key), stamp);
            window.dispatch_event(gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                position: bounds.center(), modifiers: gpui::Modifiers::none(), button: gpui::MouseButton::Left, click_count: 1,
            }), cx);
            for key in ["enter", "space"] {
                let keystroke = gpui::Keystroke::parse(key).expect("status Retry key");
                window.dispatch_event(gpui::PlatformInput::KeyDown(gpui::KeyDownEvent {
                    keystroke: keystroke.clone(), is_held: false, prefer_character_input: false,
                }), cx);
                window.dispatch_event(gpui::PlatformInput::KeyUp(gpui::KeyUpEvent { keystroke }), cx);
            }
            window.simulate_a11y_action(gpui::accesskit::ActionRequest {
                action: gpui::AccessibleAction::Click, target_tree: gpui::accesskit::TreeId::ROOT, target_node: native, data: None,
            }, cx);
            assert_eq!(rig.graph.store.read(cx).stats().submitted, submitted + 1, "unchanged notice and owner cannot admit an old page generation");
            assert_eq!(window.focused(cx), focused);
            original.focus(window, cx);
            window.dispatch_event(gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                position: bounds.center(), modifiers: gpui::Modifiers::none(), button: gpui::MouseButton::Left,
                click_count: 1, first_mouse: false,
            }), cx);
            window.dispatch_event(gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                position: bounds.center(), modifiers: gpui::Modifiers::none(), button: gpui::MouseButton::Left, click_count: 1,
            }), cx);
            assert_eq!(window.focused(cx), Some(original), "a stale new press cannot take focus before it is rejected");
            assert_eq!(window.a11y_frame_number(), frame);
            window.focused(cx)
        });
        assert_eq!(rig.graph.store.read_with(&rig.cx, |store, _| store.stats().submitted), submitted + 1);
        assert_eq!(rig.cx.update(|window, cx| window.focused(cx)), focused);
        assert_eq!(rig.cx.update(|window, _| window.a11y_frame_number()), frame);
        rig.settle();
        assert!(rig.graph.store.read_with(&rig.cx, |store, _| store.stats().landed > landed), "the replacement must actually land from its pool worker");
        assert!(rig.counts().status > renders, "the watched page stamp renews status without Back or a forced repaint");
        let (native, bounds) = rig.cx.update(|window, _| {
            let native = window.a11y_tree().expect("renewed status AX").nodes.iter()
                .find(|(_, node)| node.role() == gpui::Role::Button && node.label() == Some("Try again")
                    && node.supports_action(gpui::AccessibleAction::Click)).map(|(id, _)| *id).expect("passively renewed status Retry");
            (native, window.a11y_node_bounds(native).expect("renewed status bounds"))
        });
        assert!(rig.cx.update(|window, _| window.a11y_node_bounds(native).is_some()));
        let submitted = rig.graph.store.read_with(&rig.cx, |store, _| store.stats().submitted);
        rig.cx.simulate_click(bounds.center(), gpui::Modifiers::none());
        assert_eq!(rig.graph.store.read_with(&rig.cx, |store, _| store.stats().submitted), submitted + 1, "fresh current recovery remains available");
        rig.settle();
    }

    #[gpui::test]
    fn coalesced_same_route_return_retires_native_status_retry_before_redraw(cx: &mut TestAppContext) {
        let (mut rig, _gate) = gated_rig(cx, 663.0);
        rig.go(Intent::SetMotion(crate::model::MotionPreference::Reduced));
        long_notice(&mut rig);
        native_tree(&mut rig);
        let (native, bounds) = rig.cx.update(|window, _| {
            let native = window.a11y_tree().expect("status AX").nodes.iter()
                .find(|(_, node)| node.role() == gpui::Role::Button && node.label() == Some("Try again")
                    && node.supports_action(gpui::AccessibleAction::Click)).map(|(id, _)| *id).expect("native status Retry");
            (native, window.a11y_node_bounds(native).expect("native status bounds"))
        });
        let original = rig.graph.store.read_with(&rig.cx, |store, _| store.snapshot());
        let frame = rig.cx.update(|window, _| window.a11y_frame_number());
        let focused = rig.cx.update(|window, cx| {
            let focused = window.focused(cx);
            rig.graph.root.update(cx, |root, cx| {
                root.dispatch(Intent::Navigate(Route::World), cx);
                root.dispatch(Intent::Navigate(original.route().clone()), cx);
            });
            let latest = rig.graph.store.read(cx).snapshot();
            assert_eq!(latest.route(), original.route());
            assert!(latest.key().same_authority(original.key()));
            assert_ne!(latest.session().reading.current.id, original.session().reading.current.id);
            let submitted = rig.graph.store.read(cx).stats().submitted;
            window.dispatch_event(gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                position: bounds.center(), modifiers: gpui::Modifiers::none(), button: gpui::MouseButton::Left,
                click_count: 1, first_mouse: false,
            }), cx);
            window.dispatch_event(gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                position: bounds.center(), modifiers: gpui::Modifiers::none(), button: gpui::MouseButton::Left, click_count: 1,
            }), cx);
            for key in ["enter", "space"] {
                let keystroke = gpui::Keystroke::parse(key).expect("retired status key");
                window.dispatch_event(gpui::PlatformInput::KeyDown(gpui::KeyDownEvent {
                    keystroke: keystroke.clone(), is_held: false, prefer_character_input: false,
                }), cx);
                window.dispatch_event(gpui::PlatformInput::KeyUp(gpui::KeyUpEvent { keystroke }), cx);
            }
            window.simulate_a11y_action(gpui::accesskit::ActionRequest {
                action: gpui::AccessibleAction::Click, target_tree: gpui::accesskit::TreeId::ROOT, target_node: native, data: None,
            }, cx);
            assert_eq!(rig.graph.store.read(cx).stats().submitted, submitted);
            assert_eq!(window.focused(cx), focused);
            assert_eq!(window.a11y_frame_number(), frame);
            focused
        });
        assert_eq!(rig.route(), *original.route());
        assert_eq!(rig.cx.update(|window, cx| window.focused(cx)), focused);
        assert_eq!(rig.cx.update(|window, _| window.a11y_frame_number()), frame);
        rig.settle();
    }

    #[gpui::test]
    fn retained_native_status_disclosure_cannot_focus_or_close_after_message_or_visit_moves(cx: &mut TestAppContext) {
        for open in [false, true] {
            for movement in [0_u8, 1, 2] {
                let (mut rig, _gate) = gated_rig(cx, 663.0);
                long_notice(&mut rig);
                native_tree(&mut rig);
                if open {
                    let details = crate::shell::tests::native_bounds_id(&mut rig, "status-details", "Button", "Show status details", true).expect("Details");
                    rig.cx.simulate_click(details.center(), gpui::Modifiers::none());
                    rig.settle();
                }
                let label = if open { "Close details" } else { "Show status details" };
                let id = if open { "status-details-close" } else { "status-details" };
                let bounds = crate::shell::tests::native_bounds_id(&mut rig, id, "Button", label, true).expect("current native disclosure");
                let node = rig.cx.update(|window, _| window.a11y_tree().expect("committed disclosure tree").nodes.iter()
                    .find(|(_, node)| node.role() == gpui::Role::Button && node.label() == Some(label)
                        && node.supports_action(gpui::AccessibleAction::Click)).map(|(id, _)| *id).expect("disclosure Click"));
                let store = rig.graph.store.clone();
                let root = rig.graph.root.clone();
                let original = store.read_with(&rig.cx, |store, _| store.snapshot());
                let notice = store.read_with(&rig.cx, |store, _| store.notice().cloned().expect("represented notice"));
                let frame = rig.cx.update(|window, _| window.a11y_frame_number());
                let focus = rig.cx.update(|window, cx| {
                    let previous = window.focused(cx).expect("mounted focus");
                    window.dispatch_event(gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                        position: bounds.center(), modifiers: gpui::Modifiers::none(), button: gpui::MouseButton::Left,
                        click_count: 1, first_mouse: false,
                    }), cx);
                    let held = window.focused(cx);
                    match movement {
                        0 => store.update(cx, |store, cx| {
                            let mut next = notice.clone(); next.message = "A newer status replaced the represented message.".into();
                            store.set_notice(Some(next), cx);
                        }),
                        1 => root.update(cx, |root, cx| root.dispatch(Intent::OpenSettings(crate::navigation::SettingsPage::Appearance), cx)),
                        _ => {
                            root.update(cx, |root, cx| {
                                root.dispatch(Intent::Navigate(Route::World), cx);
                                root.dispatch(Intent::Navigate(original.route().clone()), cx);
                            });
                            store.update(cx, |store, cx| store.set_notice(Some(notice.clone()), cx));
                            assert_eq!(store.read(cx).snapshot().route(), original.route());
                            assert_eq!(store.read(cx).notice(), Some(&notice));
                            assert_ne!(store.read(cx).snapshot().session().reading.current.id, original.session().reading.current.id);
                        }
                    }
                    window.dispatch_event(gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                        position: bounds.center(), modifiers: gpui::Modifiers::none(), button: gpui::MouseButton::Left, click_count: 1,
                    }), cx);
                    for key in ["enter", "space"] {
                        let keystroke = gpui::Keystroke::parse(key).expect("disclosure key");
                        window.dispatch_event(gpui::PlatformInput::KeyDown(gpui::KeyDownEvent {
                            keystroke: keystroke.clone(), is_held: false, prefer_character_input: false,
                        }), cx);
                        window.dispatch_event(gpui::PlatformInput::KeyUp(gpui::KeyUpEvent { keystroke }), cx);
                    }
                    window.simulate_a11y_action(gpui::accesskit::ActionRequest {
                        action: gpui::AccessibleAction::Click, target_tree: gpui::accesskit::TreeId::ROOT, target_node: node, data: None,
                    }, cx);
                    assert_eq!(window.focused(cx), held, "old disclosure cannot return focus or open the later message");
                    previous.focus(window, cx);
                    window.dispatch_event(gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                        position: bounds.center(), modifiers: gpui::Modifiers::none(), button: gpui::MouseButton::Left,
                        click_count: 1, first_mouse: false,
                    }), cx);
                    window.dispatch_event(gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                        position: bounds.center(), modifiers: gpui::Modifiers::none(), button: gpui::MouseButton::Left, click_count: 1,
                    }), cx);
                    assert_eq!(window.focused(cx), Some(previous), "rejected native pointer input cannot take focus first");
                    assert_eq!(window.a11y_frame_number(), frame);
                    window.focused(cx)
                });
                assert_eq!(rig.cx.update(|window, cx| window.focused(cx)), focus);
                assert_eq!(rig.cx.update(|window, _| window.a11y_frame_number()), frame, "post-effect assertion still precedes the watcher redraw");
                rig.settle();
                if movement == 1 { rig.go(Intent::DismissOverlay); }
                long_notice(&mut rig);
                let fresh = crate::shell::tests::native_bounds_id(&mut rig, "status-details", "Button", "Show status details", true).expect("renewed disclosure");
                rig.cx.simulate_click(fresh.center(), gpui::Modifiers::none());
                rig.settle();
                assert!(crate::shell::tests::native_bounds(&mut rig, "Dialog", "Status details", false).is_some(), "fresh local disclosure remains usable");
                rig.keys("escape");
                assert!(crate::shell::tests::native_bounds(&mut rig, "Dialog", "Status details", false).is_none());
            }
        }
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
