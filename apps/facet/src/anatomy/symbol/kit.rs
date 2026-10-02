//! The page's small parts: its type roles, its measure and palette bundled
//! as [`Env`], text that publishes itself to the probe, the rail joint,
//! chips, section heads, and prose with code runs and links.

use super::host::{Act, Host, Spots};
use super::key::{Key, Part, Sec, Slot};
use super::layout::Layout;
use crate::measure::{Measure, Set};
use crate::probe::{self, TextOverflow};
use crate::tokens::{Face, Palette, TypeRole};
use gpui::{
    AnyElement, App, Bounds, Div, Element, ElementId, FontStyle, FontWeight, GlobalElementId, HighlightStyle, Hsla,
    InspectorElementId, InteractiveElement, InteractiveText, IntoElement, LayoutId, ParentElement, Pixels,
    Refineable, SharedString, StatefulInteractiveElement, Style, StyleRefinement, Styled, StyledText,
    UnderlineStyle, Window, div, fill, px,
};
use std::ops::Range;
use std::rc::Rc;

const fn role(face: Face, weight: f32, size: f32, line: f32, tracking: f32) -> TypeRole {
    TypeRole { face, weight, size, line, tracking, italic: matches!(face, Face::Serif) }
}

/// The page's type roles, at 100 % text.
pub(super) mod roles {
    use super::{Face, TypeRole, role};

    /// The kind word above the name.
    pub(in crate::anatomy::symbol) const KIND: TypeRole = role(Face::Ui, 600.0, 11.0, 16.0, 0.07);
    /// A path.
    pub(in crate::anatomy::symbol) const PATH: TypeRole = role(Face::Mono, 400.0, 12.5, 16.0, 0.0);
    /// The language tag.
    pub(in crate::anatomy::symbol) const LANG: TypeRole = role(Face::Mono, 500.0, 10.5, 14.0, 0.04);
    /// The lede.
    pub(in crate::anatomy::symbol) const LEDE: TypeRole = role(Face::Serif, 400.0, 19.0, 27.0, 0.0);
    /// A section head.
    pub(in crate::anatomy::symbol) const HEAD: TypeRole = role(Face::Ui, 600.0, 11.0, 16.0, 0.08);
    /// A count beside a head.
    pub(in crate::anatomy::symbol) const COUNT: TypeRole = role(Face::Mono, 400.0, 11.5, 16.0, 0.0);
    /// A quiet aside.
    pub(in crate::anatomy::symbol) const ASIDE: TypeRole = role(Face::Ui, 400.0, 12.0, 16.0, 0.0);
    /// A quieter, smaller aside.
    pub(in crate::anatomy::symbol) const ASIDE_SMALL: TypeRole = role(Face::Ui, 400.0, 11.5, 16.0, 0.0);
    /// A port's or case's name.
    pub(in crate::anatomy::symbol) const NAME: TypeRole = role(Face::Mono, 500.0, 13.5, 20.0, 0.0);
    /// A sub-row's name.
    pub(in crate::anatomy::symbol) const SUB: TypeRole = role(Face::Mono, 500.0, 12.5, 18.0, 0.0);
    /// A rail label (`GIVES`, `OR FAILS`).
    pub(in crate::anatomy::symbol) const LABEL: TypeRole = role(Face::Ui, 600.0, 11.0, 16.0, 0.06);
    /// A type's plain word.
    pub(in crate::anatomy::symbol) const WORD: TypeRole = role(Face::Ui, 500.0, 13.5, 20.0, 0.0);
    /// What is written.
    pub(in crate::anatomy::symbol) const WRITTEN: TypeRole = role(Face::Mono, 400.0, 12.0, 16.0, 0.0);
    /// A note beside a port.
    pub(in crate::anatomy::symbol) const NOTE: TypeRole = role(Face::Ui, 400.0, 13.0, 18.0, 0.0);
    /// The "when" of an outcome.
    pub(in crate::anatomy::symbol) const WHEN: TypeRole = role(Face::Ui, 400.0, 13.0, 19.0, 0.0);
    /// Prose.
    pub(in crate::anatomy::symbol) const PROSE: TypeRole = role(Face::Ui, 400.0, 15.0, 24.0, 0.0);
    /// Prose in a card or a fail block.
    pub(in crate::anatomy::symbol) const BODY: TypeRole = role(Face::Ui, 400.0, 14.0, 22.0, 0.0);
    /// A generic pill.
    pub(in crate::anatomy::symbol) const PILL: TypeRole = role(Face::Mono, 600.0, 12.0, 16.0, 0.0);
    /// A case's or field's name.
    pub(in crate::anatomy::symbol) const CASE: TypeRole = role(Face::Mono, 600.0, 14.0, 20.0, 0.0);
    /// A doc line on a row.
    pub(in crate::anatomy::symbol) const DOC: TypeRole = role(Face::Ui, 400.0, 13.0, 18.0, 0.0);
    /// A method's name.
    pub(in crate::anatomy::symbol) const METHOD: TypeRole = role(Face::Mono, 500.0, 13.0, 20.0, 0.0);
    /// A method's signature.
    pub(in crate::anatomy::symbol) const SIG: TypeRole = role(Face::Ui, 400.0, 12.5, 18.0, 0.0);
    /// A chip.
    pub(in crate::anatomy::symbol) const CHIP: TypeRole = role(Face::Ui, 500.0, 12.0, 16.0, 0.0);
    /// A uses row's place.
    pub(in crate::anatomy::symbol) const PLACE: TypeRole = role(Face::Mono, 400.0, 11.5, 16.0, 0.0);
    /// A uses row's code.
    pub(in crate::anatomy::symbol) const CODE: TypeRole = role(Face::Mono, 400.0, 12.5, 18.0, 0.0);
    /// A group's heading.
    pub(in crate::anatomy::symbol) const PACKAGE_UI: TypeRole = role(Face::Ui, 600.0, 12.5, 18.0, 0.0);
    /// A package's name.
    pub(in crate::anatomy::symbol) const PACKAGE: TypeRole = role(Face::Mono, 500.0, 13.0, 18.0, 0.0);
    /// A rail block's head.
    pub(in crate::anatomy::symbol) const RAIL_HEAD: TypeRole = role(Face::Ui, 600.0, 11.0, 16.0, 0.08);
    /// A rail row.
    pub(in crate::anatomy::symbol) const RAIL: TypeRole = role(Face::Mono, 400.0, 12.5, 18.0, 0.0);
    /// A rail sentence.
    pub(in crate::anatomy::symbol) const RAIL_SAY: TypeRole = role(Face::Ui, 400.0, 12.0, 18.0, 0.0);
    /// A card's title.
    pub(in crate::anatomy::symbol) const CARD_TITLE: TypeRole = role(Face::Display, 600.0, 15.0, 20.0, 0.0);
    /// A card's small label.
    pub(in crate::anatomy::symbol) const CARD_LABEL: TypeRole = role(Face::Ui, 600.0, 10.5, 14.0, 0.08);
}

/// A part's shared surroundings.
#[derive(Clone, Copy)]
pub(super) struct Env<'a> {
    /// The measure of the room the part is in.
    pub(super) m: Measure,
    /// The palette.
    pub(super) p: &'static Palette,
    /// The shell.
    pub(super) host: &'a dyn Host,
    /// What the room decided.
    pub(super) lay: Layout,
}

impl Env<'_> {
    /// `value` px at 100 % text and comfortable density, following both.
    pub(super) fn k(&self, value: f32) -> Pixels {
        crate::anatomy::k(&self.m, value)
    }

    /// `value` px at 100 % text, following the text scale only.
    pub(super) fn s(&self, value: f32) -> Pixels {
        px(value * self.m.scale())
    }
}

/// The palette's colours the page draws with.
#[derive(Clone, Copy)]
pub(super) struct Ink {
    pub(super) ink0: Hsla,
    pub(super) ink1: Hsla,
    pub(super) ink2: Hsla,
    pub(super) ink3: Hsla,
    pub(super) line1: Hsla,
    pub(super) line2: Hsla,
    pub(super) line3: Hsla,
    pub(super) g0: Hsla,
    pub(super) g1: Hsla,
    pub(super) g2: Hsla,
    pub(super) g3: Hsla,
    pub(super) plate2: Hsla,
    pub(super) table: Hsla,
    pub(super) mint: Hsla,
    pub(super) peri: Hsla,
    pub(super) peri_hi: Hsla,
    pub(super) coral: Hsla,
    pub(super) coral_soft: Hsla,
    pub(super) amber: Hsla,
    pub(super) teal: Hsla,
    pub(super) violet: Hsla,
    pub(super) slate: Hsla,
}

pub(super) fn ink(p: &Palette) -> Ink {
    Ink {
        ink0: p.ink0.hsla(),
        ink1: p.ink1.hsla(),
        ink2: p.ink2.hsla(),
        ink3: p.ink3.hsla(),
        line1: p.line1.hsla(),
        line2: p.line2.hsla(),
        line3: p.line3.hsla(),
        g0: p.g0.hsla(),
        g1: p.g1.hsla(),
        g2: p.g2.hsla(),
        g3: p.g3.hsla(),
        plate2: p.plate2.hsla(),
        table: p.table.hsla(),
        mint: p.mint.base.hsla(),
        peri: p.peri.base.hsla(),
        peri_hi: p.peri_hi.hsla(),
        coral: p.coral.base.hsla(),
        coral_soft: p.coral.soft.hsla(),
        amber: p.amber.base.hsla(),
        teal: p.f_type.hue.hsla(),
        violet: p.f_con.hue.hsla(),
        slate: p.f_ns.hue.hsla(),
    }
}

/// Text on one line, published to the probe under `key`.
pub(super) fn said(env: &Env<'_>, key: &Key, content: impl Into<SharedString>, role: TypeRole, color: Hsla) -> AnyElement {
    said_in(&env.m, key, content, role, color)
}

/// [`said`] for a closure that holds only the measure.
pub(super) fn said_in(m: &Measure, key: &Key, content: impl Into<SharedString>, role: TypeRole, color: Hsla) -> AnyElement {
    let content = content.into();
    probe::text(
        key.id(),
        content.clone(),
        m.role(role),
        1.0,
        TextOverflow::Clip,
        div().set(role, m).text_color(color).whitespace_nowrap().flex_none().child(content),
    )
    .into_any_element()
}

/// Text that may wrap, published to the probe under `key`.
pub(super) fn wrapped(env: &Env<'_>, key: &Key, content: impl Into<SharedString>, role: TypeRole, color: Hsla) -> AnyElement {
    let content = content.into();
    probe::text(
        key.id(),
        content.clone(),
        env.m.role(role),
        1.0,
        TextOverflow::Wrap,
        div().set(role, &env.m).text_color(color).min_w_0().child(content),
    )
    .into_any_element()
}

/// Which end of a line gives way to an ellipsis.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Ellipsis {
    /// The end (words, code).
    End,
    /// The start (a path keeps its file name).
    Start,
}

/// [`wrapped`] for code that holds only the measure (a float card).
pub(super) fn wrapped_in(m: &Measure, key: &Key, content: impl Into<SharedString>, role: TypeRole, color: Hsla) -> AnyElement {
    let content = content.into();
    probe::text(key.id(), content.clone(), m.role(role), 1.0, TextOverflow::Wrap, div().set(role, m).text_color(color).min_w_0().child(content)).into_any_element()
}

/// Text that gives way with an ellipsis.
pub(super) fn truncated(env: &Env<'_>, key: &Key, content: impl Into<SharedString>, role: TypeRole, color: Hsla, gives: Ellipsis) -> AnyElement {
    let content = content.into();
    let mut inner = div().set(role, &env.m).text_color(color).whitespace_nowrap().overflow_hidden().min_w_0().w_full();
    inner = match gives {
        Ellipsis::Start => inner.text_ellipsis_start(),
        Ellipsis::End => inner.text_ellipsis(),
    };
    probe::text(key.id(), content.clone(), env.m.role(role), 1.0, TextOverflow::Ellipsis, inner.child(content)).into_any_element()
}

/// Uppercase for small-caps heads (the face has no small caps).
pub(super) fn caps(text: &str) -> String {
    text.to_uppercase()
}

/// A dotted underline painted under `child`: the mark of a type that is read
/// from docs or code rather than declared.
pub(super) fn dotted(child: AnyElement, color: Hsla) -> AnyElement {
    div()
        .relative()
        .flex_none()
        .child(child)
        .child(
            DottedUnderline { color, style: StyleRefinement::default() }
                .absolute().bottom_0().left_0().w_full().h(px(3.0)),
        )
        .into_any_element()
}

/// The dotted rule under a declaration's doc-sourced type.
struct DottedUnderline {
    color: Hsla,
    style: StyleRefinement,
}

impl Styled for DottedUnderline {
    fn style(&mut self) -> &mut StyleRefinement { &mut self.style }
}

impl IntoElement for DottedUnderline {
    type Element = Self;
    fn into_element(self) -> Self { self }
}

impl Element for DottedUnderline {
    type RequestLayoutState = Style;
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> { None }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> { None }
    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, Style) {
        let mut style = Style::default();
        style.refine(&self.style);
        let layout = window.request_layout(style.clone(), [], cx);
        (layout, style)
    }
    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut Style, _: &mut Window, _: &mut App) {}
    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, style: &mut Style, _: &mut (), window: &mut Window, cx: &mut App) {
        let color = self.color;
        style.paint(bounds, window, cx, |window, _| {
            let y = bounds.origin.y + bounds.size.height - px(1.5);
            let mut x = bounds.origin.x;
            while x < bounds.origin.x + bounds.size.width {
                window.paint_quad(fill(Bounds::new(gpui::point(x, y), gpui::size(px(1.4), px(1.4))), color));
                x += px(3.6);
            }
        });
    }
}

/// Where a row sits on its rail.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Stop {
    /// The first row: no rail above it.
    First,
    /// A row with rail on both sides.
    Between,
    /// The last row: no rail below it.
    Last,
    /// The only row.
    Only,
}

impl Stop {
    /// The stop of row `n` of `total`.
    pub(super) const fn of(n: usize, total: usize) -> Self {
        match (n == 0, n + 1 >= total) {
            (true, true) => Self::Only,
            (true, false) => Self::First,
            (false, true) => Self::Last,
            (false, false) => Self::Between,
        }
    }

    const fn above(self) -> bool {
        matches!(self, Self::Between | Self::Last)
    }

    const fn below(self) -> bool {
        matches!(self, Self::First | Self::Between)
    }
}

/// The rail's joint cell: a segment above and below a mark, so a row can
/// wrap and grow and the rail still runs through it.
pub(super) fn joint(env: &Env<'_>, mark: AnyElement, stop: Stop, rail: Hsla) -> AnyElement {
    let line = |shown: bool| {
        let line = div().w(env.s(1.5)).flex_1().min_h(px(2.0));
        if shown { line.bg(rail) } else { line }
    };
    div()
        .flex()
        .flex_col()
        .items_center()
        .flex_none()
        .self_stretch()
        .w(env.s(28.0))
        .child(line(stop.above()))
        .child(div().flex_none().py(px(1.0)).child(mark))
        .child(line(stop.below()))
        .into_any_element()
}

/// Whether a chip is the one chosen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Chosen {
    /// Not chosen.
    Off,
    /// Chosen.
    On,
}

/// How loud a chip is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Voice {
    /// As usual.
    Plain,
    /// Quiet: dimmed until chosen.
    Quiet,
}

/// When a chip runs its action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Fires {
    /// When it is clicked (a filter).
    Click,
    /// When it is pressed, as a menu opens: the press outside the open menu
    /// closes it and this press does not reopen it, and the menu takes focus
    /// after the press has focused what it will.
    Press,
}

/// A filter chip: what it says, whether it is the one chosen, how loud it is,
/// and when it fires.
pub(super) struct Chip<'a> {
    pub key: &'a Key,
    pub label: AnyElement,
    pub chosen: Chosen,
    pub voice: Voice,
    pub tone: Hsla,
    pub fires: Fires,
}

/// A filter chip running `act`.
pub(super) fn chip(env: &Env<'_>, chip: Chip<'_>, act: Act) -> AnyElement {
    let i = ink(env.p);
    let Chip { key, label, chosen, voice, tone, fires } = chip;
    let on = chosen == Chosen::On;
    let el = div()
        .id(key.id())
        .flex()
        .items_center()
        .gap(env.k(6.0))
        .h(env.s(26.0))
        .px(env.k(10.0))
        .cursor_pointer()
        .bg(if on { i.plate2 } else { i.g1 })
        .border_1()
        // Quiet is a quieter edge: the words keep the ink they were given.
        .border_color(if on { tone } else if voice == Voice::Quiet { i.line1 } else { i.line2 })
        .hover(|style| style.bg(i.g3))
        .child(label);
    let words = key.text();
    let run = act.clone();
    let el = match fires {
        Fires::Click => el.on_click(move |_, window, cx| run(window, cx)).into_any_element(),
        Fires::Press => el
            .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                run(window, cx);
                // What the press opened has the focus: nothing under it takes it back.
                cx.stop_propagation();
            })
            .into_any_element(),
    };
    env.host.target(key, words, act, el)
}

/// A section's head: small caps, a count, an aside at the right.
pub(super) fn head(env: &Env<'_>, section: Sec, title: &str, count: Option<String>, aside: Option<AnyElement>) -> AnyElement {
    let i = ink(env.p);
    let key = Key::of(Part::Sec(section)).field(Slot::Head);
    let mut row = div().flex().flex_wrap().items_baseline().gap_x(env.k(10.0)).mb(env.k(12.0)).child(said(env, &key.field(Slot::Title), caps(title), roles::HEAD, i.ink2));
    if let Some(count) = count {
        row = row.child(said(env, &key.field(Slot::Count), count, roles::COUNT, i.ink3));
    }
    if let Some(aside) = aside {
        row = row.child(div().ml_auto().child(aside));
    }
    row.into_any_element()
}

/// Markup as runs: the text, the highlights, and the reference spans.
pub(super) fn runs(markup: &str, i: &Ink, code_bg: Hsla) -> (String, Vec<(Range<usize>, HighlightStyle)>, Vec<(Range<usize>, String)>) {
    use crate::overlay::text::Piece;
    let mut text = String::new();
    let mut highlights = Vec::new();
    let mut links = Vec::new();
    for piece in crate::overlay::text::parse(markup) {
        let start = text.len();
        // A link's label reads without the code ticks its source wrapped it in.
        match &piece {
            Piece::Reference { label, .. } | Piece::Shortcut { label, .. } => text.push_str(&label.replace('`', "")),
            other => text.push_str(other.text()),
        }
        let range = start..text.len();
        match piece {
            Piece::Plain(_) => {}
            Piece::Code(_) => highlights.push((range, HighlightStyle { color: Some(i.ink0), font_weight: Some(FontWeight(560.0)), background_color: Some(code_bg), ..HighlightStyle::default() })),
            Piece::Emphasis(_) => highlights.push((range, HighlightStyle { font_style: Some(FontStyle::Italic), ..HighlightStyle::default() })),
            Piece::Strong(_) => highlights.push((range, HighlightStyle { color: Some(i.ink0), font_weight: Some(FontWeight(700.0)), ..HighlightStyle::default() })),
            Piece::Reference { target, .. } => {
                if !["http:", "https:", "mailto:", "#"].iter().any(|prefix| target.starts_with(prefix)) {
                    highlights.push((
                        range.clone(),
                        HighlightStyle { color: Some(i.ink0), underline: Some(UnderlineStyle { thickness: px(1.0), color: Some(i.line3), wavy: false }), ..HighlightStyle::default() },
                    ));
                    links.push((range, target));
                }
            }
            Piece::Shortcut { code, .. } => {
                if code {
                    highlights.push((range, HighlightStyle { color: Some(i.ink0), font_weight: Some(FontWeight(560.0)), background_color: Some(code_bg), ..HighlightStyle::default() }));
                }
            }
        }
    }
    (text, highlights, links)
}

/// Prose in `role`: code as tinted runs, references as links, published to
/// the probe as its plain words.
pub(super) fn prose(env: &Env<'_>, key: &Key, markup: &str, role: TypeRole, color: Hsla) -> AnyElement {
    let i = ink(env.p);
    let (text, highlights, links) = runs(markup, &i, i.plate2);
    let shared = SharedString::from(text.clone());
    let styled = StyledText::new(shared.clone()).with_highlights(highlights);
    let host_links: Vec<(Range<usize>, Option<Act>)> = links.into_iter().map(|(range, target)| (range, env.host.lookup(&target))).collect();
    let body: AnyElement = if host_links.iter().any(|(_, act)| act.is_some()) {
        let (ranges, acts): (Vec<_>, Vec<_>) = host_links.into_iter().filter_map(|(r, a)| a.map(|a| (r, a))).unzip();
        InteractiveText::new(key.field(Slot::Run).id(), styled)
            .on_click(ranges, move |which, window: &mut Window, cx: &mut App| {
                if let Some(act) = acts.get(which) {
                    act(window, cx);
                }
            })
            .into_any_element()
    } else {
        styled.into_any_element()
    };
    probe::text(key.id(), shared, env.m.role(role), 1.0, TextOverflow::Wrap, div().set(role, &env.m).text_color(color).min_w_0().child(body)).into_any_element()
}

/// `color` at `alpha` of the alpha it has.
pub(super) fn faded(mut color: Hsla, alpha: f32) -> Hsla {
    color.alpha *= alpha;
    color
}

/// The frame of a generic's violet pill: the page's and the card's are one shape.
pub(super) fn pill_frame(m: &Measure, p: &crate::tokens::Palette) -> Div {
    div().flex().items_center().justify_center().flex_none().min_w(px(20.0 * m.scale())).h(px(20.0 * m.scale())).px(crate::anatomy::k(m, 6.0)).bg(p.f_con.hue.hsla())
}

// ------------------------------------------------------------------ spots

/// Records its bounds in prepaint, wherever `record` puts them.
struct Spot {
    record: Box<dyn Fn(Bounds<Pixels>, &mut App)>,
    /// Whether it records before its child is laid out or after it is painted.
    when: Recorded,
    child: AnyElement,
}

/// When a [`Spot`] records its bounds.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Recorded {
    /// As it is laid out.
    Laid,
    /// After its child painted (what a child recorded in laying out is known).
    Painted,
}

impl IntoElement for Spot {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl gpui::Element for Spot {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, (): &mut (), window: &mut Window, cx: &mut App) {
        if self.when == Recorded::Laid {
            (self.record)(bounds, cx);
        }
        self.child.prepaint(window, cx);
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, (): &mut (), (): &mut (), window: &mut Window, cx: &mut App) {
        self.child.paint(window, cx);
        if self.when == Recorded::Painted {
            (self.record)(bounds, cx);
        }
    }
}

/// `child`, its place remembered under `section` (scroll to it, anchor to it).
pub(super) fn spot(section: Sec, spots: &Rc<Spots>, child: AnyElement) -> AnyElement {
    let spots = Rc::clone(spots);
    Spot { record: Box::new(move |bounds, _| spots.record(section, bounds)), when: Recorded::Laid, child }.into_any_element()
}

/// `child`, its place remembered under `key`: a card the keyboard opens anchors to it.
pub(super) fn frame(key: &Key, spots: &Rc<Spots>, child: AnyElement) -> AnyElement {
    let (spots, key) = (Rc::clone(spots), key.clone());
    Spot { record: Box::new(move |bounds, _| spots.record_frame(&key, bounds)), when: Recorded::Laid, child }.into_any_element()
}

/// `child`, its bounds handed to `record` as it is laid out (a scene's scroll
/// container publishes its content extent to the probe this way).
pub(super) fn recorded(record: impl Fn(Bounds<Pixels>, &mut App) + 'static, child: AnyElement) -> AnyElement {
    Spot { record: Box::new(record), when: Recorded::Laid, child }.into_any_element()
}

/// [`recorded`], after `child` has painted: what it recorded while laying out is there to read.
pub(super) fn recorded_after(record: impl Fn(Bounds<Pixels>, &mut App) + 'static, child: AnyElement) -> AnyElement {
    Spot { record: Box::new(record), when: Recorded::Painted, child }.into_any_element()
}

/// A line of text that runs an action: the one hover treatment every action
/// on the page has (a plate under it, a pointer), and a stop in the keyboard
/// walk labelled `label`.
pub(super) fn action(env: &Env<'_>, key: &Key, label: impl Into<SharedString>, act: Act, child: AnyElement) -> AnyElement {
    let i = ink(env.p);
    let run = act.clone();
    let el = div()
        .id(key.id())
        .flex()
        .items_center()
        .min_h(env.s(24.0))
        .px(env.k(6.0))
        .mx(-env.k(6.0))
        .cursor_pointer()
        .hover(|style| style.bg(i.g3))
        .on_click(move |_, window, cx| run(window, cx))
        .child(child)
        .into_any_element();
    env.host.target(key, label.into(), act, el)
}
