//! Rich text: prose and signatures as ONE text run each, with rest-able
//! word ranges.
//!
//! Anything with mixed styling or hoverable words is one `StyledText` with
//! runs, never a row of divs, so it wraps as text, shapes as text and spaces
//! as text. [`Words`] is the element: a styled text plus a set of linked
//! ranges; resting on a range reports to the float layer with that range's
//! exact rect (so a peek opens on the word, and inside a card it chains),
//! and every linked range re-anchors its open card each frame.
//!
//! - [`prose`]: a tiny markup — `` `code` `` (mono, upright), `*emphasis*`
//!   (upright, brighter) — on a serif (or any) base role. Code spans whose
//!   text a resolver knows become rest-able: this is how any page makes
//!   identifiers in its prose rest-able.
//! - [`Sig`]: a signature built span by span with explicit spacing, coloured
//!   from `palette.syntax`, with linked names and a `+N` overflow chip.

use super::float::{self, FloatRequest};
use crate::fonts;
use crate::measure::{Measure, Set};
use crate::theme::ActiveFacet;
use crate::tokens::{Face, Palette, TypeRole};
use gpui::{
    AnyElement, App, BorderStyle, Bounds, Element, ElementId, FontStyle, FontWeight, GlobalElementId,
    Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId, MouseMoveEvent,
    ParentElement, Pixels, SharedString, StyledText, TextLayout, TextRun, Window, div, fill,
    outline, point, px, size,
};
use std::ops::Range;
use std::rc::Rc;

/// What resting on a linked range opens.
#[derive(Clone)]
pub struct Link {
    /// The trigger key (unique per word occurrence).
    pub key: ElementId,
    /// Builds the request for the range's rect.
    pub request: Rc<dyn Fn(Bounds<Pixels>) -> FloatRequest>,
}

impl Link {
    /// A link that opens `request(rect)` when rested on.
    pub fn new(
        key: impl Into<ElementId>,
        request: impl Fn(Bounds<Pixels>) -> FloatRequest + 'static,
    ) -> Self {
        Self {
            key: key.into(),
            request: Rc::new(request),
        }
    }
}

/// A decoration painted under a range of the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decor {
    /// A hairline underline (a rest-able word at rest).
    Hairline,
    /// A dashed box (the `+N` overflow chip).
    Chip,
}

// ------------------------------------------------------------------ Words

/// A styled text with rest-able ranges. Build with [`words`].
pub struct Words {
    id: ElementId,
    text: StyledText,
    links: Vec<(Range<usize>, Link)>,
    decor: Vec<(Range<usize>, Decor)>,
    hairline: Hsla,
}

/// Wraps `text` so each `(range, link)` rests into the float layer.
#[must_use]
pub fn words(
    id: impl Into<ElementId>,
    text: StyledText,
    links: Vec<(Range<usize>, Link)>,
    palette: &Palette,
) -> Words {
    Words {
        id: id.into(),
        text,
        links,
        decor: Vec::new(),
        hairline: palette.line3.into(),
    }
}

impl Words {
    /// Adds a decoration under `range`.
    #[must_use]
    pub fn decor(mut self, range: Range<usize>, decor: Decor) -> Self {
        self.decor.push((range, decor));
        self
    }
}

/// The rect of `range` in a laid-out text: its first line's segment.
#[must_use]
pub fn range_rect(layout: &TextLayout, range: &Range<usize>) -> Option<Bounds<Pixels>> {
    let start = layout.position_for_index(range.start)?;
    let line = layout.line_height();
    let end = layout
        .position_for_index(range.end)
        .filter(|end| (end.y - start.y).abs() < px(0.5) && end.x > start.x)
        .unwrap_or_else(|| point(layout.bounds().right(), start.y));
    Some(Bounds::from_corners(start, point(end.x, start.y + line)))
}

impl IntoElement for Words {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// Prepaint state of [`Words`].
pub struct WordsPrepaint {
    hitbox: Hitbox,
    rects: Vec<Option<Bounds<Pixels>>>,
}

impl Element for Words {
    type RequestLayoutState = ();
    type PrepaintState = WordsPrepaint;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        self.text.request_layout(None, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> WordsPrepaint {
        self.text.prepaint(None, inspector_id, bounds, state, window, cx);
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let layout = self.text.layout().clone();
        let rects: Vec<Option<Bounds<Pixels>>> = self
            .links
            .iter()
            .map(|(range, link)| {
                let rect = range_rect(&layout, range);
                if let Some(rect) = rect {
                    float::anchor(&link.key, rect, window, cx);
                    let request = link.request.clone();
                    super::hint::target(rect, move |window, cx| float::open(request(rect), window, cx), window, cx);
                }
                rect
            })
            .collect();
        WordsPrepaint { hitbox, rects }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut (),
        prepaint: &mut WordsPrepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let layout = self.text.layout().clone();
        let line3 = self.hairline;
        for (range, decor) in &self.decor {
            let Some(rect) = range_rect(&layout, range) else {
                continue;
            };
            match decor {
                Decor::Hairline => {
                    let underline = Bounds::new(
                        point(rect.origin.x, rect.bottom() - px(1.0)),
                        size(rect.size.width, px(1.0)),
                    );
                    window.paint_quad(fill(underline, line3));
                }
                Decor::Chip => {
                    let inset = rect.size.height * 0.12;
                    let chip = Bounds::from_corners(
                        point(rect.origin.x, rect.origin.y + inset),
                        point(rect.right(), rect.bottom() - inset),
                    );
                    window.paint_quad(outline(chip, line3, BorderStyle::Dashed));
                }
            }
        }
        self.text
            .paint(None, inspector_id, bounds, state, &mut (), window, cx);
        let hitbox = prepaint.hitbox.clone();
        let links: Vec<(Range<usize>, Link, Option<Bounds<Pixels>>)> = self
            .links
            .iter()
            .zip(prepaint.rects.iter())
            .map(|((range, link), rect)| (range.clone(), link.clone(), *rect))
            .collect();
        if links.is_empty() {
            return;
        }
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase != gpui::DispatchPhase::Capture {
                return;
            }
            let index = if hitbox.is_hovered(window) {
                layout.index_for_position(event.position).ok()
            } else {
                None
            };
            for (range, link, rect) in &links {
                let Some(rect) = *rect else { continue };
                let hovered = index.is_some_and(|index| range.contains(&index));
                let request = link.request.clone();
                float::report(&link.key, rect, hovered, move || request(rect), window, cx);
            }
        });
    }
}

// ------------------------------------------------------------------ runs

/// A font for a role (family, weight, style and features exactly as
/// `Typeset` would set them).
#[must_use]
pub fn font(role: TypeRole) -> gpui::Font {
    gpui::Font {
        family: SharedString::new_static(fonts::family(role)),
        features: fonts::features(role),
        fallbacks: None,
        weight: FontWeight(role.weight),
        style: if role.italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        },
    }
}

fn run(len: usize, role: TypeRole, color: Hsla) -> TextRun {
    TextRun {
        len,
        font: font(role),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
        letter_spacing: None,
    }
}

/// Accumulates text and runs.
#[derive(Default)]
struct Runs {
    text: String,
    runs: Vec<TextRun>,
}

impl Runs {
    fn push(&mut self, text: &str, role: TypeRole, color: Hsla) -> Range<usize> {
        let start = self.text.len();
        if text.is_empty() {
            return start..start;
        }
        self.text.push_str(text);
        // Merge with the previous run when it looks identical.
        if let Some(last) = self.runs.last_mut()
            && last.font == font(role)
            && last.color == color
        {
            last.len += text.len();
        } else {
            self.runs.push(run(text.len(), role, color));
        }
        start..self.text.len()
    }

    fn finish(self) -> StyledText {
        StyledText::new(self.text).with_runs(self.runs)
    }
}

// ------------------------------------------------------------------ prose

/// One piece of parsed prose markup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Piece {
    /// Plain text in the base role.
    Plain(String),
    /// `` `code` ``: mono, upright.
    Code(String),
    /// `*emphasis*`: upright, brighter.
    Emphasis(String),
}

/// Parses the prose markup. Unclosed markers are literal text.
#[must_use]
pub fn parse(markup: &str) -> Vec<Piece> {
    let mut pieces = Vec::new();
    let mut plain = String::new();
    let mut rest = markup;
    while let Some(at) = rest.find(['`', '*']) {
        let marker = rest[at..].chars().next().unwrap_or('`');
        let after = &rest[at + 1..];
        match after.find(marker) {
            Some(end) if end > 0 => {
                plain.push_str(&rest[..at]);
                if !plain.is_empty() {
                    pieces.push(Piece::Plain(std::mem::take(&mut plain)));
                }
                let inner = after[..end].to_owned();
                pieces.push(if marker == '`' {
                    Piece::Code(inner)
                } else {
                    Piece::Emphasis(inner)
                });
                rest = &after[end + 1..];
            }
            _ => {
                plain.push_str(&rest[..=at]);
                rest = after;
            }
        }
    }
    plain.push_str(rest);
    if !plain.is_empty() {
        pieces.push(Piece::Plain(plain));
    }
    pieces
}

/// Resolves a code span to a link (or `None`: plain code).
pub type Resolver = Rc<dyn Fn(&str, usize) -> Option<Link>>;

/// A paragraph of prose: build with [`prose`].
#[derive(IntoElement)]
pub struct Prose {
    id: ElementId,
    markup: SharedString,
    role: TypeRole,
    color: Option<Hsla>,
    code_color: Option<Hsla>,
    resolver: Option<Resolver>,
    measure: Measure,
}

/// Prose in `role` (a serif lede or caption, usually) for `measure`.
#[must_use]
pub fn prose(
    id: impl Into<ElementId>,
    markup: impl Into<SharedString>,
    role: TypeRole,
    measure: &Measure,
) -> Prose {
    Prose {
        id: id.into(),
        markup: markup.into(),
        role,
        color: None,
        code_color: None,
        resolver: None,
        measure: *measure,
    }
}

impl Prose {
    /// The base ink (default `ink1`).
    #[must_use]
    pub fn color(mut self, color: impl Into<Hsla>) -> Self {
        self.color = Some(color.into());
        self
    }

    /// The ink of code spans that are not links (default `ink0`).
    #[must_use]
    pub fn code_color(mut self, color: impl Into<Hsla>) -> Self {
        self.code_color = Some(color.into());
        self
    }

    /// Makes code spans rest-able: `resolve(text, occurrence)` returns the
    /// link for an identifier it knows.
    #[must_use]
    pub fn links(mut self, resolve: impl Fn(&str, usize) -> Option<Link> + 'static) -> Self {
        self.resolver = Some(Rc::new(resolve));
        self
    }

    /// The styled text and its links, for embedding.
    #[must_use]
    pub fn build(&self, palette: &Palette) -> (StyledText, Vec<(Range<usize>, Link)>, Vec<Range<usize>>) {
        let base = self.role;
        let code = TypeRole {
            face: Face::Mono,
            weight: 400.0,
            italic: false,
            tracking: 0.0,
            ..base
        };
        let emphasis = TypeRole {
            italic: false,
            ..base
        };
        let ink = self.color.unwrap_or_else(|| palette.ink1.into());
        let bright: Hsla = palette.ink0.into();
        let quiet_code = self.code_color.unwrap_or(bright);
        let mut runs = Runs::default();
        let mut links = Vec::new();
        let mut linked = Vec::new();
        for (occurrence, piece) in parse(&self.markup).into_iter().enumerate() {
            match piece {
                Piece::Plain(text) => {
                    runs.push(&text, base, ink);
                }
                Piece::Emphasis(text) => {
                    runs.push(&text, emphasis, bright);
                }
                Piece::Code(text) => {
                    let link = self
                        .resolver
                        .as_ref()
                        .and_then(|resolve| resolve(&text, occurrence));
                    let color = if link.is_some() {
                        palette.syntax.type_name.into()
                    } else {
                        quiet_code
                    };
                    let range = runs.push(&text, code, color);
                    if let Some(link) = link {
                        linked.push(range.clone());
                        links.push((range, link));
                    }
                }
            }
        }
        (runs.finish(), links, linked)
    }
}

impl gpui::RenderOnce for Prose {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.facet().palette();
        let (text, links, linked) = self.build(palette);
        let mut element = words(self.id.clone(), text, links, palette);
        for range in linked {
            element = element.decor(range, Decor::Hairline);
        }
        div().set(self.role, &self.measure).child(element)
    }
}

// ------------------------------------------------------------------ signatures

/// What a signature span is, for colour.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Role {
    /// `pub`, `fn`, `enum`, `impl`.
    Keyword,
    /// Type names.
    Type,
    /// Traits and interfaces.
    Contract,
    /// Functions and methods.
    Function,
    /// Values, variants, constants.
    Value,
    /// Parameters.
    Param,
    /// Lifetimes and macros.
    Macro,
    /// Attributes.
    Attribute,
    /// Strings.
    Str,
    /// Numbers.
    Number,
    /// Punctuation.
    Punct,
    /// Anything else.
    Plain,
    /// A name at full strength (ink0).
    Strong,
    /// A quiet name (ink2).
    Quiet,
}

impl Role {
    /// The colour from the palette's syntax roles.
    #[must_use]
    pub fn color(self, palette: &Palette) -> Hsla {
        let syntax = &palette.syntax;
        match self {
            Self::Keyword => syntax.keyword,
            Self::Type => syntax.type_name,
            Self::Contract => syntax.contract,
            Self::Function => syntax.function,
            Self::Value => syntax.constant,
            Self::Param => syntax.parameter,
            Self::Macro => syntax.macro_name,
            Self::Attribute => syntax.attribute,
            Self::Str => syntax.string,
            Self::Number => syntax.number,
            Self::Punct => syntax.punctuation,
            Self::Plain => palette.ink1,
            Self::Strong => palette.ink0,
            Self::Quiet => palette.ink2,
        }
        .into()
    }
}

/// One span of a signature.
#[derive(Clone)]
pub struct Span {
    /// The exact text (spacing included; nothing is added).
    pub text: SharedString,
    /// Its colour role.
    pub role: Role,
    /// What resting on it opens.
    pub link: Option<Link>,
}

/// A signature, built span by span. Spacing is explicit: `Vec<T>` is
/// `ty("Vec").p("<").ty("T").p(">")`, never padded.
#[derive(Clone, Default)]
pub struct Sig {
    spans: Vec<Span>,
    more: Option<usize>,
    anchor: Option<usize>,
}

impl Sig {
    /// An empty signature.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Any span.
    #[must_use]
    pub fn span(mut self, text: impl Into<SharedString>, role: Role) -> Self {
        self.spans.push(Span {
            text: text.into(),
            role,
            link: None,
        });
        self
    }

    /// A span that rests into `link`.
    #[must_use]
    pub fn link(mut self, text: impl Into<SharedString>, role: Role, link: Link) -> Self {
        self.spans.push(Span {
            text: text.into(),
            role,
            link: Some(link),
        });
        self
    }

    /// A keyword.
    #[must_use]
    pub fn kw(self, text: impl Into<SharedString>) -> Self {
        self.span(text, Role::Keyword)
    }

    /// A type name.
    #[must_use]
    pub fn ty(self, text: impl Into<SharedString>) -> Self {
        self.span(text, Role::Type)
    }

    /// A value (variant, constant).
    #[must_use]
    pub fn val(self, text: impl Into<SharedString>) -> Self {
        self.span(text, Role::Value)
    }

    /// A function name.
    #[must_use]
    pub fn call(self, text: impl Into<SharedString>) -> Self {
        self.span(text, Role::Function)
    }

    /// Punctuation.
    #[must_use]
    pub fn p(self, text: impl Into<SharedString>) -> Self {
        self.span(text, Role::Punct)
    }

    /// One space.
    #[must_use]
    pub fn sp(self) -> Self {
        self.span(" ", Role::Plain)
    }

    /// Marks the most recent span as the anchor word (the one whose card is
    /// open, drawn lit by the layer; here it reads brighter).
    #[must_use]
    pub fn anchored(mut self) -> Self {
        self.anchor = self.spans.len().checked_sub(1);
        self
    }

    /// Appends the `+N` overflow chip before whatever follows.
    #[must_use]
    pub fn more(mut self, hidden: usize) -> Self {
        self.more = Some(self.spans.len());
        self.spans.push(Span {
            text: format!("\u{2009}+{hidden}\u{2009}").into(),
            role: Role::Plain,
            link: None,
        });
        self
    }

    /// The plain text (for tests and accessibility).
    #[must_use]
    pub fn text(&self) -> String {
        self.spans.iter().map(|span| span.text.as_ref()).collect()
    }

    /// The styled text in `role` with its links and decorations.
    #[must_use]
    pub fn build(
        &self,
        role: TypeRole,
        palette: &Palette,
    ) -> (StyledText, Vec<(Range<usize>, Link)>, Vec<(Range<usize>, Decor)>) {
        let mut runs = Runs::default();
        let mut links = Vec::new();
        let mut decor = Vec::new();
        for (index, span) in self.spans.iter().enumerate() {
            let color = if Some(index) == self.more {
                palette.ink2.into()
            } else if Some(index) == self.anchor {
                palette.ink0.into()
            } else {
                span.role.color(palette)
            };
            let range = runs.push(&span.text, role, color);
            if Some(index) == self.more {
                decor.push((range.clone(), Decor::Chip));
            }
            if let Some(link) = &span.link {
                links.push((range, link.clone()));
            }
        }
        (runs.finish(), links, decor)
    }

    /// The signature as one text for `measure`, in `role` (mono).
    #[must_use]
    pub fn render(
        &self,
        id: impl Into<ElementId>,
        role: TypeRole,
        measure: &Measure,
        palette: &Palette,
    ) -> AnyElement {
        let (text, links, decor) = self.build(role, palette);
        let mut element = words(id, text, links, palette);
        for (range, kind) in decor {
            element = element.decor(range, kind);
        }
        div().set(role, measure).child(element).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{Piece, Sig, parse};
    use crate::tokens::{ABYSS, GLACIER, ty};

    #[test]
    fn markup_parses_code_and_emphasis_and_keeps_unclosed_markers() {
        assert_eq!(
            parse("A call through `value.method()`, *resolved* by the compiler."),
            [
                Piece::Plain("A call through ".into()),
                Piece::Code("value.method()".into()),
                Piece::Plain(", ".into()),
                Piece::Emphasis("resolved".into()),
                Piece::Plain(" by the compiler.".into()),
            ]
        );
        assert_eq!(parse("2 * 3 and a `lone tick"), [Piece::Plain("2 * 3 and a `lone tick".into())]);
    }

    #[test]
    fn a_signature_is_one_text_spaced_exactly_as_built() {
        let sig = Sig::new()
            .kw("pub fn")
            .sp()
            .call("get")
            .p("(&")
            .kw("self")
            .p(") -> ")
            .ty("Vec")
            .p("<")
            .ty("T")
            .p(">");
        assert_eq!(sig.text(), "pub fn get(&self) -> Vec<T>");
        let (_, links, decor) = sig.build(ty::CODE, &ABYSS);
        assert!(links.is_empty() && decor.is_empty());
    }

    #[test]
    fn signature_colours_come_from_the_palette_syntax_roles() {
        let sig = Sig::new().kw("enum").sp().ty("Kind");
        for palette in [&ABYSS, &GLACIER] {
            let keyword: gpui::Hsla = palette.syntax.keyword.into();
            let type_name: gpui::Hsla = palette.syntax.type_name.into();
            assert_eq!(super::Role::Keyword.color(palette), keyword);
            assert_eq!(super::Role::Type.color(palette), type_name);
        }
        assert_eq!(sig.text(), "enum Kind");
    }

    #[test]
    fn the_more_chip_is_decorated_and_counted_in_the_text() {
        let sig = Sig::new().val("Calls").p(", ").more(8).p(" }");
        let (_, _, decor) = sig.build(ty::CODE, &ABYSS);
        assert_eq!(decor.len(), 1);
        let text = sig.text();
        assert_eq!(&text[decor[0].0.clone()], "\u{2009}+8\u{2009}");
    }
}
