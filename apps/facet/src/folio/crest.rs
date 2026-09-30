//! The crest: the hero's own instruments, each a small cut plate that opens
//! inside itself. This module holds the frame every cell shares, the seal,
//! the licence stamp (a stamp that unfolds in place, judged against your
//! own project's licence) and the advisories cell (what the advisory feeds
//! say about this release, honest when no feed is configured).
//!
//! The row is a fixed height: the licence stamp unfolds into the band
//! reserved under the row, so opening it moves nothing else on the page.

use super::state::{Fit, Nominal, Pose};
use super::text::{ellipsis, key, one, wrap};
use crate::controls::state::{Touch, hover_zone, track};
use crate::marks::badges::{Glyph, glyph};
use crate::marks::license::LicenseFacts;
use crate::marks::spdx::{self, Expr, Family};
use crate::measure::{Measure, Space};
use crate::motion::spec;
use crate::paint::geom::{Fill, Poly, pt};
use crate::paint::{Bevel, Chamfer, Edge, Plate, cut, mix};
use crate::theme::ActiveFacet;
use crate::tokens::{Face, Palette, TypeRole, Voice, ty};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Bounds, Element, ElementId, GlobalElementId, Hsla, InspectorElementId, InteractiveElement, IntoElement, LayoutId,
    ParentElement, Pixels, Refineable, RenderOnce, SharedString, Style, StyleRefinement, Styled, Window, deferred, div, px,
};
use std::rc::Rc;

/// A cell at rest, px at 100 %.
pub const REST: Nominal = Nominal::px(138.0);
/// The band the licence stamp may unfold into, px at 100 %.
pub const FULL: Nominal = Nominal::px(188.0);

const LABEL: TypeRole = TypeRole { weight: 620.0, size: 10.5, line: 14.0, tracking: 0.08, ..ty::LABEL };
const VERDICT: TypeRole = TypeRole { face: Face::Display, weight: 700.0, size: 16.0, line: 20.0, tracking: -0.01, italic: false };
const EXPR: TypeRole = TypeRole { size: 12.0, line: 16.0, ..ty::MONO_SMALL };
const LINE: TypeRole = TypeRole { size: 12.5, line: 17.0, ..ty::SMALL };
const TERMS: TypeRole = TypeRole { size: 12.0, line: 16.0, ..ty::SMALL };
const NOTE: TypeRole = TypeRole { weight: 400.0, size: 10.5, line: 14.0, ..ty::SMALL };
const AND: TypeRole = TypeRole { size: 11.5, line: 16.0, ..ty::CAPTION };

/// The voice a tone speaks in.
fn ink_of(tone: Voice, palette: &Palette) -> Hsla {
    palette.voice(tone).base.into()
}

/// The frame every crest cell shares: a cut plate with its small-caps
/// label at the head (`accent` is a count in amber beside it).
#[must_use]
pub fn cell(id: &ElementId, label: &str, accent: Option<String>, note: Option<&str>, measure: &Measure, palette: &'static Palette) -> crate::paint::Cut {
    let scale = measure.scale();
    let mut head = div().flex().items_center().gap(measure.space(Space::Snug)).child(one(
        key(id, "label"),
        label.to_uppercase(),
        LABEL,
        palette.ink3,
        measure,
    ));
    if let Some(accent) = accent {
        head = head.child(one(key(id, "accent"), accent, LABEL, palette.amber.base, measure));
    }
    if let Some(note) = note {
        // Where the fact was read from: never presented as an index fact.
        head = head.child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .justify_end()
                .child(wrap(key(id, "note"), note.to_owned(), NOTE, palette.ink3, measure, None)),
        );
    }
    let mut edge = Edge::of(Bevel::Rest, palette);
    edge.hi = palette.line3.into();
    edge.lo = palette.line2.into();
    cut()
        .chamfer(Chamfer::Px(9.0 * scale))
        .edge(edge)
        .plate(Plate::Flat)
        .fill(palette.plate)
        .flex()
        .flex_col()
        .gap(measure.space(Space::Snug))
        .px(measure.space(Space::Roomy))
        .pt(measure.space(Space::Base))
        .pb(measure.space(Space::Roomy))
        .child(head)
}

/// The seal: a ring in `ink` with `glyph` inside, `size` px.
pub fn seal(g: Glyph, size: f32, ink: Hsla) -> impl IntoElement {
    SealElement { glyph: g, size, ink, style: StyleRefinement::default() }.flex_none().size(px(size))
}

struct SealElement { glyph: Glyph, size: f32, ink: Hsla, style: StyleRefinement }

impl Styled for SealElement {
    fn style(&mut self) -> &mut StyleRefinement { &mut self.style }
}

impl IntoElement for SealElement {
    type Element = Self;
    fn into_element(self) -> Self { self }
}

impl Element for SealElement {
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
        let (glyph, ink) = (self.glyph, self.ink);
        style.paint(bounds, window, cx, |window, _| {
            let (x, y) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
            let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
            let (cx, cy, r) = (x + w * 0.5, y + h * 0.5, w.min(h) * 0.5 - 0.8);
            let ring = Poly::new((0..16).map(|i| {
                let t = (i as f32 * 22.5).to_radians();
                pt(cx + r * t.cos(), cy + r * t.sin())
            }));
            let mut fill = Fill::new();
            for quad in ring.offset(-0.8).stroke_ring(1.6) { fill.poly(&quad); }
            fill.paint(window, ink);
            let inner = w.min(h) * 0.5;
            let at = Bounds::new(gpui::point(px(cx - inner * 0.5), px(cy - inner * 0.5)), gpui::size(px(inner), px(inner)));
            glyph::paint(window, at, glyph, ink);
        });
    }
}

// ------------------------------------------------------------ the licence

/// One piece of a licence expression, as the stamp writes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Part {
    /// A licence; `on` when it is the one the stamp is judging.
    Id {
        /// The SPDX id.
        id: String,
        /// Whether it is the option judged.
        fit: Fit,
    },
    /// `or`: your choice.
    Or,
    /// `and`: all apply.
    And,
}

/// What the stamp says about a licence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    /// The one word (`Permissive`, `Copyleft`).
    pub word: &'static str,
    /// The voice it speaks in.
    pub tone: Voice,
    /// The sentence that unfolds.
    pub line: String,
    /// The expression, `MIT or Apache-2.0`.
    pub expression: Vec<Part>,
    /// What it lets you do.
    pub permits: Vec<&'static str>,
    /// What it asks of you.
    pub asks: Vec<&'static str>,
    /// What it will not promise.
    pub limits: Vec<&'static str>,
}

fn rank(family: Family) -> u8 {
    match family {
        Family::Public => 0,
        Family::Permissive => 1,
        Family::Weak => 2,
        Family::Strong => 3,
        Family::Unknown => 4,
    }
}

/// Reads a licence into what the stamp shows: the option that asks least of
/// you is the one judged, and the fit against your own project's licence
/// (when it is known) picks the colour.
#[must_use]
pub fn verdict(facts: &LicenseFacts) -> Verdict {
    let reading = facts.reading();
    let Some(expr) = facts.expr() else {
        return Verdict {
            word: "No licence",
            tone: Voice::Coral,
            line: "It declares none. By default that reserves every right: you have no permission to copy or ship it.".to_owned(),
            expression: Vec::new(),
            permits: Vec::new(),
            asks: Vec::new(),
            limits: Vec::new(),
        };
    };
    let options = spdx::options(&expr);
    let chosen: Vec<String> = options
        .iter()
        .min_by_key(|option| option.iter().map(|id| rank(spdx::family(id))).max().unwrap_or(4))
        .cloned()
        .unwrap_or_default();
    let family = chosen.iter().map(|id| spdx::family(id)).max_by_key(|f| rank(*f)).unwrap_or(Family::Unknown);
    let (word, family_tone) = match family {
        Family::Public => ("Public domain", Voice::Mint),
        Family::Permissive => ("Permissive", Voice::Mint),
        Family::Weak => ("Weak copyleft", Voice::Amber),
        Family::Strong => ("Copyleft", Voice::Coral),
        Family::Unknown => ("Unrecognised", Voice::Amber),
    };
    let tone = match reading.fit.as_ref().map(|fit| fit.level) {
        Some(0) => Voice::Mint,
        Some(1 | 3) => Voice::Amber,
        Some(_) => Voice::Coral,
        None => family_tone,
    };
    let line = reading.fit.as_ref().map_or_else(
        || match family {
            Family::Public => "Nothing is asked of you.".to_owned(),
            Family::Permissive => "Keep its notice when you ship.".to_owned(),
            Family::Weak => "Fine to use as is. If you change its files, share those changes.".to_owned(),
            Family::Strong => "Shipping your program with it puts your whole program under its terms.".to_owned(),
            Family::Unknown => "Its terms are not ones this reads: read them before you ship.".to_owned(),
        },
        |fit| fit.line.clone(),
    );
    let mut expression = Vec::new();
    fn walk(e: &Expr, chosen: &[String], out: &mut Vec<Part>) {
        match e {
            Expr::Id(id) => out.push(Part::Id { id: spdx::short_id(id), fit: if chosen.iter().any(|c| c == id) { Fit::Judged } else { Fit::Aside } }),
            Expr::Or(any) => {
                for (i, e) in any.iter().enumerate() {
                    if i > 0 {
                        out.push(Part::Or);
                    }
                    walk(e, chosen, out);
                }
            }
            Expr::And(all) => {
                for (i, e) in all.iter().enumerate() {
                    if i > 0 {
                        out.push(Part::And);
                    }
                    walk(e, chosen, out);
                }
            }
        }
    }
    walk(&expr, &chosen, &mut expression);
    let mut permits: Vec<&'static str> = Vec::new();
    let mut asks: Vec<&'static str> = Vec::new();
    let mut limits: Vec<&'static str> = Vec::new();
    for terms in chosen.iter().filter_map(|id| spdx::terms(id)) {
        for (list, from) in [(&mut permits, terms.permissions), (&mut asks, terms.conditions), (&mut limits, terms.limitations)] {
            for word in from {
                if !list.contains(word) {
                    list.push(word);
                }
            }
        }
    }
    Verdict { word, tone, line, expression, permits, asks, limits }
}

/// The licence stamp (see [`stamp`]).
#[derive(IntoElement)]
pub struct Stamp {
    id: ElementId,
    facts: Rc<LicenseFacts>,
    measure: Measure,
    width: Pixels,
    held: Pose,
}

/// A stamp for `facts`, `width` px wide.
#[must_use]
pub fn stamp(id: impl Into<ElementId>, facts: Rc<LicenseFacts>, width: Pixels, measure: &Measure) -> Stamp {
    Stamp { id: id.into(), facts, measure: *measure, width, held: Pose::Live }
}

impl Stamp {
    /// The pose the stamp is held in (`Held`: unfolded whatever the pointer does).
    #[must_use]
    pub const fn pose(mut self, pose: Pose) -> Self {
        self.held = pose;
        self
    }

    /// Shows the stamp unfolded whatever the pointer does (scenes, tests).
    #[must_use]
    pub const fn open(mut self) -> Self {
        self.held = Pose::Held;
        self
    }
}

impl RenderOnce for Stamp {
    #[allow(clippy::too_many_lines)]
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.palette();
        let measure = self.measure;
        let scale = measure.scale();
        let verdict = verdict(&self.facts);
        let touch = Touch::read(&self.id, crate::controls::Look::LIVE, true, window, cx);
        let motion = touch.motion.clone();
        let wanted = touch.hovered || touch.focused || self.held == Pose::Held;
        let open = motion.animate(track(&self.id, "open"), if wanted { 1.0 } else { 0.0 }, super::state::plate(wanted), window, cx);
        let open = open.clamp(0.0, 1.05);
        let hover = motion.animate(track(&self.id, "hover"), if touch.hovered { 1.0 } else { 0.0 }, spec::HOVER, window, cx);
        let tone = ink_of(verdict.tone, palette);
        let glyph_of = if verdict.tone == Voice::Mint { Glyph::Shield } else { Glyph::Unsafe };

        // The expression: the judged option in full ink, the rest quiet.
        let mut expression = div().min_w_0().flex().flex_wrap().items_center().gap_x(measure.space(Space::Snug));
        if verdict.expression.is_empty() {
            expression = expression.child(one(key(&self.id, "expr"), "no licence declared", EXPR, palette.ink3, &measure));
        }
        for (i, part) in verdict.expression.iter().enumerate() {
            expression = expression.child(match part {
                Part::Id { id, fit } => one(key(&self.id, format!("id-{i}")), id.clone(), EXPR, if *fit == Fit::Judged { palette.ink0 } else { palette.ink3 }, &measure).into_any_element(),
                Part::Or => one(key(&self.id, format!("op-{i}")), "or", AND, palette.ink3, &measure).into_any_element(),
                Part::And => one(key(&self.id, format!("op-{i}")), "and", AND, palette.amber.base, &measure).into_any_element(),
            });
        }

        let face = div()
            .flex()
            .items_center()
            .gap(measure.space(Space::Roomy))
            .child(seal(glyph_of, 30.0 * scale, tone))
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .flex_col()
                    .child(one(key(&self.id, "verdict"), verdict.word, VERDICT, tone, &measure))
                    .child(expression),
            );

        // What unfolds: the sentence, then what it permits, asks and won't promise.
        let term = |g: Glyph, ink: Hsla, words: &[&str], name: &'static str| {
            (!words.is_empty()).then(|| {
                div()
                    .flex()
                    .items_center()
                    .gap(measure.space(Space::Snug))
                    .child(glyph(g, 12.0 * scale, ink))
                    .child(div().flex_1().min_w_0().flex().child(ellipsis(key(&self.id, name), words.join(" · "), TERMS, palette.ink1, &measure)))
            })
        };
        let more = div()
            .flex()
            .flex_col()
            .gap(measure.space(Space::Tight))
            .pt(measure.space(Space::Snug))
            .child(wrap(key(&self.id, "line"), verdict.line.clone(), LINE, palette.ink1, &measure, Some(3)))
            .children(term(Glyph::Makes, palette.mint.base.into(), &verdict.permits, "permits"))
            .children(term(Glyph::Takes, palette.peri_hi.into(), &verdict.asks, "asks"))
            .children(term(Glyph::Error, palette.coral.base.into(), &verdict.limits, "limits"));

        let height = (REST.value() + (FULL.value() - REST.value()) * open) * scale;
        let mut edge = Edge::of(Bevel::Rest, palette);
        edge.hi = palette.line3.into();
        edge.lo = palette.line2.into();
        let edge = edge.mix(Edge::of(Bevel::Peri, palette), hover);
        let plate = cell(&self.id, "Licence", None, None, &measure, palette)
            .edge(edge)
            .fill(mix(palette.plate.into(), palette.plate2.into(), hover))
            .w(self.width)
            .h(px(height))
            .overflow_hidden()
            .child(face)
            .when(open > 0.02, |plate| plate.child(more))
            .id(self.id.clone());
        let plate = crate::controls::button::wire(plate, &touch, None);
        if open > 0.001 || touch.hovered {
            // The plate unfolds over what lies below it (drawn late, hit
            // first), so opening it moves nothing on the page.
            let plate = plate.absolute().top_0().left_0();
            div().relative().flex_none().w(self.width).h(REST.at(scale)).child(deferred(hover_zone(plate, &touch, 9.0 * scale, true)).with_priority(1)).into_any_element()
        } else {
            // At rest it is part of the page's own flow, so a page change
            // that cuts the page cuts it too (a deferred draw would not be).
            div().flex_none().w(self.width).h(REST.at(scale)).child(hover_zone(plate, &touch, 9.0 * scale, true)).into_any_element()
        }
    }
}

// ------------------------------------------------------------ advisories

/// Why nothing is known about a release's advisories.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Silence {
    /// It is a project of yours: feeds check published releases.
    Yours,
    /// No feed is configured.
    NoFeed,
    /// The record this page is about has not been read.
    NotRead,
    /// The index says nothing else.
    Unknown,
}

impl Silence {
    /// The one word the cell says.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Yours => "yours",
            Self::NoFeed => "no feed",
            Self::NotRead => "not read",
            Self::Unknown => "unknown",
        }
    }
}

/// What the advisory feeds say about one release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Advisories {
    /// No feed answered; `why` says so in words.
    Unknown {
        /// Why nothing is known.
        why: Silence,
        /// What could be read, when something could.
        note: SharedString,
    },
    /// The feeds were read and nothing matches.
    Clear {
        /// What was checked.
        note: SharedString,
    },
    /// Advisories match this release.
    Found {
        /// How many.
        count: usize,
        /// The worst severity, in a word.
        worst: Option<SharedString>,
        /// What the acquisition decision was.
        decision: SharedString,
    },
}

/// The advisories cell (see [`advisories`]).
#[derive(IntoElement)]
pub struct AdvisoriesCell {
    id: ElementId,
    facts: Advisories,
    measure: Measure,
    width: Pixels,
}

/// The advisories cell, `width` px wide.
#[must_use]
pub fn advisories(id: impl Into<ElementId>, facts: Advisories, width: Pixels, measure: &Measure) -> AdvisoriesCell {
    AdvisoriesCell { id: id.into(), facts, measure: *measure, width }
}

impl RenderOnce for AdvisoriesCell {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.palette();
        let measure = self.measure;
        let scale = measure.scale();
        let (word, tone, quiet, note): (SharedString, Hsla, bool, SharedString) = match &self.facts {
            Advisories::Unknown { why, note } => (SharedString::from(why.word()), palette.ink3.into(), true, note.clone()),
            Advisories::Clear { note } => ("clear".into(), palette.mint.base.into(), false, note.clone()),
            Advisories::Found { count, worst, decision } => {
                let word = format!("{count} {}", if *count == 1 { "advisory" } else { "advisories" });
                let tone: Hsla = match decision.as_ref() {
                    "deny" => palette.coral.base.into(),
                    _ => palette.amber.base.into(),
                };
                (
                    word.into(),
                    tone,
                    false,
                    match worst {
                        Some(worst) => format!("worst {worst} · {decision}").into(),
                        None => format!("decision {decision}").into(),
                    },
                )
            }
        };
        let face_word = TypeRole { weight: if quiet { 560.0 } else { 700.0 }, size: if quiet { 14.0 } else { 16.0 }, ..VERDICT };
        cell(&self.id, "Advisories", None, None, &measure, palette)
            .w(self.width)
            .h(REST.at(scale))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(measure.space(Space::Roomy))
                    .child(seal(Glyph::Shield, 30.0 * scale, if quiet { palette.ink3.into() } else { tone }))
                    .child(one(key(&self.id, "word"), word, face_word, tone, &measure)),
            )
            .child(wrap(key(&self.id, "note"), note, TERMS, palette.ink2, &measure, Some(3)))
            .into_any_element()
    }
}

// ------------------------------------------------------------ not read yet

/// A cell whose facts are read from the package's source on disk, before
/// they have been (or when there is no source to read).
#[derive(IntoElement)]
pub struct Unread {
    id: ElementId,
    label: SharedString,
    words: SharedString,
    note: Option<SharedString>,
    measure: Measure,
    width: Pixels,
}

/// An unread cell: `label` at the head, `words` (`reading its source…`)
/// beneath, `width` px wide.
#[must_use]
pub fn unread(id: impl Into<ElementId>, label: impl Into<SharedString>, words: impl Into<SharedString>, width: Pixels, measure: &Measure) -> Unread {
    Unread { id: id.into(), label: label.into(), words: words.into(), note: None, measure: *measure, width }
}

impl Unread {
    /// Where the fact would be read from, in the cell's head.
    #[must_use]
    pub fn note(mut self, note: impl Into<SharedString>) -> Self {
        self.note = Some(note.into());
        self
    }
}

impl RenderOnce for Unread {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.palette();
        let measure = self.measure;
        cell(&self.id, &self.label, None, self.note.as_deref(), &measure, palette)
            .w(self.width)
            .h(REST.at(measure.scale()))
            .child(wrap(key(&self.id, "words"), self.words.clone(), TERMS, palette.ink3, &measure, Some(4)))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{Part, verdict};
    use crate::folio::state::Fit;
    use crate::marks::license::LicenseFacts;
    use crate::tokens::Voice;

    fn parts(v: &super::Verdict) -> Vec<String> {
        v.expression
            .iter()
            .map(|p| match p {
                Part::Id { id, fit } => format!("{id}{}", if *fit == Fit::Judged { "*" } else { "" }),
                Part::Or => "or".into(),
                Part::And => "and".into(),
            })
            .collect()
    }

    #[test]
    fn a_dual_permissive_licence_reads_permissive_and_judges_the_lighter_option() {
        let v = verdict(&LicenseFacts::new(Some("MIT OR Apache-2.0"), Some("MIT OR Apache-2.0"), "backend"));
        assert_eq!(v.word, "Permissive");
        assert_eq!(v.tone, Voice::Mint);
        assert_eq!(parts(&v), ["MIT*", "or", "Apache-2.0"]);
        assert!(v.permits.contains(&"commercial use") && v.asks.contains(&"keep the notice") && v.limits.contains(&"no warranty"), "{v:?}");
    }

    #[test]
    fn copyleft_is_coral_and_says_what_it_does_to_your_project() {
        let v = verdict(&LicenseFacts::new(Some("GPL-3.0-only"), Some("MIT OR Apache-2.0"), "backend"));
        assert_eq!(v.word, "Copyleft");
        assert_eq!(v.tone, Voice::Coral);
        assert!(v.line.contains("backend"), "the fit sentence names your project: {}", v.line);
    }

    #[test]
    fn a_choice_with_a_copyleft_option_judges_the_permissive_one() {
        let v = verdict(&LicenseFacts::new(Some("GPL-2.0-only OR MIT"), None, "backend"));
        assert_eq!(v.word, "Permissive");
        assert_eq!(parts(&v), ["GPL-2.0", "or", "MIT*"]);
    }

    #[test]
    fn no_licence_is_coral_and_no_fit_means_no_fit_sentence_about_you() {
        let v = verdict(&LicenseFacts::new(None, Some("MIT"), "backend"));
        assert_eq!(v.word, "No licence");
        assert_eq!(v.tone, Voice::Coral);
        let unknown_yours = verdict(&LicenseFacts::new(Some("MIT"), None, "backend"));
        assert!(!unknown_yours.line.contains("backend"), "without your licence, the stamp does not compare: {}", unknown_yours.line);
        assert_eq!(unknown_yours.tone, Voice::Mint);
    }

    #[test]
    fn a_licence_the_table_does_not_know_is_unrecognised_not_permissive() {
        let v = verdict(&LicenseFacts::new(Some("LicenseRef-Custom"), None, "backend"));
        assert_eq!(v.word, "Unrecognised");
        assert_eq!(v.tone, Voice::Amber);
    }
}
