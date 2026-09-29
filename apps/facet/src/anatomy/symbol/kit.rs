//! The page's small parts: its type roles, its measure and palette bundled
//! as [`Env`], text that publishes itself to the probe, the rail joint,
//! chips, section heads, and prose with code runs and links.

use super::host::{Act, Host};
use super::key::{Key, Part, Sec};
use super::layout::Layout;
use crate::measure::{Measure, Set};
use crate::probe::{self, TextOverflow};
use crate::tokens::{Face, Palette, TypeRole};
use gpui::{
    AnyElement, App, Div, FontStyle, FontWeight, HighlightStyle, Hsla, InteractiveElement, InteractiveText, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, StyledText, UnderlineStyle, Window, canvas, div, fill, px,
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
    pub const KIND: TypeRole = role(Face::Ui, 600.0, 11.0, 16.0, 0.07);
    /// A path.
    pub const PATH: TypeRole = role(Face::Mono, 400.0, 12.5, 16.0, 0.0);
    /// The language tag.
    pub const LANG: TypeRole = role(Face::Mono, 500.0, 10.5, 14.0, 0.04);
    /// The lede.
    pub const LEDE: TypeRole = role(Face::Serif, 400.0, 19.0, 27.0, 0.0);
    /// A section head.
    pub const HEAD: TypeRole = role(Face::Ui, 600.0, 11.0, 16.0, 0.08);
    /// A count beside a head.
    pub const COUNT: TypeRole = role(Face::Mono, 400.0, 11.5, 16.0, 0.0);
    /// A quiet aside.
    pub const ASIDE: TypeRole = role(Face::Ui, 400.0, 12.0, 16.0, 0.0);
    /// A quieter, smaller aside.
    pub const ASIDE_SMALL: TypeRole = role(Face::Ui, 400.0, 11.5, 16.0, 0.0);
    /// A port's or case's name.
    pub const NAME: TypeRole = role(Face::Mono, 500.0, 13.5, 20.0, 0.0);
    /// A sub-row's name.
    pub const SUB: TypeRole = role(Face::Mono, 500.0, 12.5, 18.0, 0.0);
    /// A rail label (`GIVES`, `OR FAILS`).
    pub const LABEL: TypeRole = role(Face::Ui, 600.0, 11.0, 16.0, 0.06);
    /// A type's plain word.
    pub const WORD: TypeRole = role(Face::Ui, 500.0, 13.5, 20.0, 0.0);
    /// What is written.
    pub const WRITTEN: TypeRole = role(Face::Mono, 400.0, 12.0, 16.0, 0.0);
    /// A note beside a port.
    pub const NOTE: TypeRole = role(Face::Ui, 400.0, 13.0, 18.0, 0.0);
    /// The "when" of an outcome.
    pub const WHEN: TypeRole = role(Face::Ui, 400.0, 13.0, 19.0, 0.0);
    /// Prose.
    pub const PROSE: TypeRole = role(Face::Ui, 400.0, 15.0, 24.0, 0.0);
    /// Prose in a card or a fail block.
    pub const BODY: TypeRole = role(Face::Ui, 400.0, 14.0, 22.0, 0.0);
    /// A generic pill.
    pub const PILL: TypeRole = role(Face::Mono, 600.0, 12.0, 16.0, 0.0);
    /// A case's or field's name.
    pub const CASE: TypeRole = role(Face::Mono, 600.0, 14.0, 20.0, 0.0);
    /// A doc line on a row.
    pub const DOC: TypeRole = role(Face::Ui, 400.0, 13.0, 18.0, 0.0);
    /// A method's name.
    pub const METHOD: TypeRole = role(Face::Mono, 500.0, 13.0, 20.0, 0.0);
    /// A method's signature.
    pub const SIG: TypeRole = role(Face::Ui, 400.0, 12.5, 18.0, 0.0);
    /// A chip.
    pub const CHIP: TypeRole = role(Face::Ui, 500.0, 12.0, 16.0, 0.0);
    /// A uses row's place.
    pub const PLACE: TypeRole = role(Face::Mono, 400.0, 11.5, 16.0, 0.0);
    /// A uses row's code.
    pub const CODE: TypeRole = role(Face::Mono, 400.0, 12.5, 18.0, 0.0);
    /// A group's heading.
    pub const PACKAGE_UI: TypeRole = role(Face::Ui, 600.0, 12.5, 18.0, 0.0);
    /// A package's name.
    pub const PACKAGE: TypeRole = role(Face::Mono, 500.0, 13.0, 18.0, 0.0);
    /// A rail block's head.
    pub const RAIL_HEAD: TypeRole = role(Face::Ui, 600.0, 11.0, 16.0, 0.08);
    /// A rail row.
    pub const RAIL: TypeRole = role(Face::Mono, 400.0, 12.5, 18.0, 0.0);
    /// A rail sentence.
    pub const RAIL_SAY: TypeRole = role(Face::Ui, 400.0, 12.0, 18.0, 0.0);
    /// A card's title.
    pub const CARD_TITLE: TypeRole = role(Face::Display, 600.0, 15.0, 20.0, 0.0);
    /// A card's small label.
    pub const CARD_LABEL: TypeRole = role(Face::Ui, 600.0, 10.5, 14.0, 0.08);
}

/// A part's shared surroundings.
#[derive(Clone, Copy)]
pub(super) struct Env<'a> {
    /// The measure of the room the part is in.
    pub m: Measure,
    /// The palette.
    pub p: &'static Palette,
    /// The shell.
    pub host: &'a dyn Host,
    /// What the room decided.
    pub lay: Layout,
}

impl Env<'_> {
    /// `value` px at 100 % text and comfortable density, following both.
    pub fn k(&self, value: f32) -> gpui::Pixels {
        crate::anatomy::k(&self.m, value)
    }

    /// `value` px at 100 % text, following the text scale only.
    pub fn s(&self, value: f32) -> gpui::Pixels {
        px(value * self.m.scale())
    }

    /// The same parts in a room of another width.
    pub fn within(&self, width: gpui::Pixels) -> Self {
        Self { m: self.m.within(width), ..*self }
    }
}

/// The palette's colours the page draws with.
#[derive(Clone, Copy)]
pub(super) struct Ink {
    pub ink0: Hsla,
    pub ink1: Hsla,
    pub ink2: Hsla,
    pub ink3: Hsla,
    pub ink4: Hsla,
    pub line1: Hsla,
    pub line2: Hsla,
    pub line3: Hsla,
    pub g0: Hsla,
    pub g1: Hsla,
    pub g2: Hsla,
    pub plate2: Hsla,
    pub plate3: Hsla,
    pub table: Hsla,
    pub mint: Hsla,
    pub peri: Hsla,
    pub peri_hi: Hsla,
    pub coral: Hsla,
    pub coral_soft: Hsla,
    pub amber: Hsla,
    pub teal: Hsla,
    pub violet: Hsla,
    pub violet_soft: Hsla,
    pub slate: Hsla,
}

pub(super) fn ink(p: &Palette) -> Ink {
    Ink {
        ink0: p.ink0.hsla(),
        ink1: p.ink1.hsla(),
        ink2: p.ink2.hsla(),
        ink3: p.ink3.hsla(),
        ink4: p.ink4.hsla(),
        line1: p.line1.hsla(),
        line2: p.line2.hsla(),
        line3: p.line3.hsla(),
        g0: p.g0.hsla(),
        g1: p.g1.hsla(),
        g2: p.g2.hsla(),
        plate2: p.plate2.hsla(),
        plate3: p.plate3.hsla(),
        table: p.table.hsla(),
        mint: p.mint.base.hsla(),
        peri: p.peri.base.hsla(),
        peri_hi: p.peri_hi.hsla(),
        coral: p.coral.base.hsla(),
        coral_soft: p.coral.soft.hsla(),
        amber: p.amber.base.hsla(),
        teal: p.f_type.hue.hsla(),
        violet: p.f_con.hue.hsla(),
        violet_soft: p.f_con.bg.hsla(),
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
            canvas(
                |_, _, _| {},
                move |bounds, (), window, _| {
                    let y = bounds.origin.y + bounds.size.height - px(1.5);
                    let mut x = bounds.origin.x;
                    while x < bounds.origin.x + bounds.size.width {
                        window.paint_quad(fill(gpui::Bounds::new(gpui::point(x, y), gpui::size(px(1.4), px(1.4))), color));
                        x += px(3.6);
                    }
                },
            )
            .absolute()
            .bottom_0()
            .left_0()
            .w_full()
            .h(px(3.0)),
        )
        .into_any_element()
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
    pub const fn of(n: usize, total: usize) -> Self {
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

/// A filter chip: a label, chosen or not, running `act` when clicked.
pub(super) fn chip(env: &Env<'_>, key: &Key, label: AnyElement, chosen: Chosen, voice: Voice, tone: Hsla, act: Act) -> AnyElement {
    let i = ink(env.p);
    let on = chosen == Chosen::On;
    let mut el = div()
        .id(key.id())
        .flex()
        .items_center()
        .gap(env.k(6.0))
        .h(env.s(26.0))
        .px(env.k(10.0))
        .cursor_pointer()
        .bg(if on { i.plate2 } else { i.g1 })
        .border_1()
        .border_color(if on { tone } else { i.line2 })
        .hover(|style| style.bg(i.g2))
        .child(label);
    if voice == Voice::Quiet && !on {
        el = el.opacity(0.85);
    }
    let label = key.text();
    env.host.target(key, label, act.clone(), el.on_click(move |_, window, cx| act(window, cx)).into_any_element())
}

/// A section's head: small caps, a count, an aside at the right.
pub(super) fn head(env: &Env<'_>, section: Sec, title: &str, count: Option<String>, aside: Option<AnyElement>) -> AnyElement {
    let i = ink(env.p);
    let key = Key::of(Part::Sec(section)).field("head");
    let mut row = div().flex().flex_wrap().items_baseline().gap_x(env.k(10.0)).mb(env.k(12.0)).child(said(env, &key.field("title"), caps(title), roles::HEAD, i.ink2));
    if let Some(count) = count {
        row = row.child(said(env, &key.field("count"), count, roles::COUNT, i.ink3));
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
        text.push_str(piece.text());
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
        InteractiveText::new(key.field("run").id(), styled)
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

/// A plate: the page's boxed surface (the call, a shape).
pub(super) fn plate(env: &Env<'_>) -> Div {
    let i = ink(env.p);
    div().flex().flex_col().bg(i.g1).border_1().border_color(i.line2)
}

/// An `Rc` action from a closure.
pub(super) fn act(f: impl Fn(&mut Window, &mut App) + 'static) -> Act {
    Rc::new(f)
}
