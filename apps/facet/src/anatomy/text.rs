//! One line of mixed text with links: names, types in plain words, quiet
//! punctuation and the ⌥ source, as ONE `StyledText` with runs.
//!
//! [`Line`] accumulates runs and remembers which byte ranges are links and
//! where they lead. [`Line::element`] hands the text to the float layer's
//! [`words`](crate::overlay::text::words) element (resting on a link raises
//! its peek, with the hairline under every link) and adds the click: a
//! click on a link dispatches [`Open`] for the shell to route.

use crate::measure::{Measure, Set};
use crate::overlay::peek::{self, Peek};
use crate::overlay::text::{Decor, Link, font, words};
use crate::semantics::types::{Piece, Spelled, Target};
use crate::tokens::{Palette, TypeRole};
use gpui::{
    AnyElement, ElementId, Hsla, InteractiveElement, IntoElement, MouseButton, MouseUpEvent, ParentElement,
    SharedString, StyledText, TextRun,
};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use super::roles;

/// Go to a symbol: dispatched when a link in the anatomy is clicked. The
/// shell routes it (a [`Target::Node`] is a page; a [`Target::Path`] asks
/// the index first).
#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(namespace = facet, no_json)]
pub struct Open {
    /// Where the link leads.
    pub target: Target,
}

/// What the page knows about the things its links name.
#[derive(Clone)]
pub struct Links {
    /// Whether a target is in your code (its link reads mint).
    pub yours: Rc<dyn Fn(&Target) -> bool>,
    /// The peek a target raises on rest (the same card the graph raises).
    pub peek: Rc<dyn Fn(&Target) -> Option<Peek>>,
}

impl Links {
    /// Links that are never yours and raise no peek (they still open).
    #[must_use]
    pub fn plain() -> Self {
        Self { yours: Rc::new(|_| false), peek: Rc::new(|_| None) }
    }
}

impl Default for Links {
    fn default() -> Self {
        Self::plain()
    }
}

/// How a spelled type's pieces are coloured and faced.
#[derive(Clone, Copy, Debug)]
pub struct TypeInk {
    /// Names and primitives.
    pub base: TypeRole,
    /// Links.
    pub link: Hsla,
    /// Links into your code.
    pub yours: Hsla,
    /// Plain words (`maybe`, `list of`).
    pub words: Hsla,
    /// Punctuation.
    pub punct: Hsla,
    /// Primitives.
    pub prim: Hsla,
    /// Generic variables and associated names.
    pub var: Hsla,
    /// The ⌥ source.
    pub source: Hsla,
}

impl TypeInk {
    /// The anatomy's type ink in `role`.
    #[must_use]
    pub fn new(role: TypeRole, palette: &Palette) -> Self {
        Self {
            base: role,
            link: palette.ink1.hsla(),
            yours: palette.mint.base.hsla(),
            words: palette.ink3.hsla(),
            punct: palette.ink4.hsla(),
            prim: palette.ink2.hsla(),
            var: palette.ink1.hsla(),
            source: palette.ink4.hsla(),
        }
    }
}

/// One line of runs and links.
#[derive(Default)]
pub struct Line {
    text: String,
    runs: Vec<TextRun>,
    links: Vec<(Range<usize>, Target)>,
    marks: Vec<Range<usize>>,
}

impl Line {
    /// An empty line.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether nothing was written.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The plain text so far.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The links so far: `(range, target)`.
    #[must_use]
    pub fn links(&self) -> &[(Range<usize>, Target)] {
        &self.links
    }

    /// Appends `text` in `role` and `color`; returns its byte range.
    pub fn push(&mut self, text: &str, role: TypeRole, color: Hsla) -> Range<usize> {
        let start = self.text.len();
        if text.is_empty() {
            return start..start;
        }
        self.text.push_str(text);
        let f = font(role);
        match self.runs.last_mut() {
            Some(last) if last.font == f && last.color == color => last.len += text.len(),
            _ => self.runs.push(TextRun {
                len: text.len(),
                font: f,
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
                letter_spacing: None,
            }),
        }
        start..self.text.len()
    }

    /// Appends a link to `target`.
    pub fn link(&mut self, text: &str, role: TypeRole, color: Hsla, target: Target) -> Range<usize> {
        let range = self.push(text, role, color);
        self.links.push((range.clone(), target));
        range
    }

    /// Underlines `range` in periwinkle (the In use hit).
    pub fn mark(&mut self, range: Range<usize>) {
        self.marks.push(range);
    }

    /// Appends a type in plain words; with `xray`, its exact source follows
    /// in the quietest ink.
    pub fn spelled(&mut self, spelled: &Spelled, ink: &TypeInk, links: &Links, xray: bool) {
        let words_role = roles::words(ink.base);
        let italic = roles::italic(ink.base);
        for piece in &spelled.pieces {
            match piece {
                Piece::Word(w) => {
                    self.push(w, words_role, ink.words);
                }
                Piece::Punct(p) => {
                    self.push(p, ink.base, ink.punct);
                }
                Piece::Var(v) | Piece::Assoc(v) => {
                    self.push(v, italic, ink.var);
                }
                Piece::Prim(p) => {
                    self.push(p, ink.base, ink.prim);
                }
                Piece::Space => {
                    self.push(" ", ink.base, ink.words);
                }
                Piece::Name { text, target } => {
                    let color = if (links.yours)(target) { ink.yours } else { ink.link };
                    self.link(text, ink.base, color, target.clone());
                }
            }
        }
        if xray && !spelled.source.is_empty() && spelled.plain() != spelled.source.as_ref() {
            self.push("  ", ink.base, ink.source);
            self.push(&spelled.source, ink.base, ink.source);
        }
    }

    /// The line as an element in `role` for `measure`: resting on a link
    /// raises its peek, clicking one dispatches [`Open`].
    #[must_use]
    pub fn element(self, id: impl Into<ElementId>, role: TypeRole, measure: &Measure, links: &Links, palette: &Palette) -> AnyElement {
        let id: ElementId = id.into();
        let text = StyledText::new(self.text).with_runs(self.runs);
        let layout = text.layout().clone();
        let mut rest = Vec::new();
        for (k, (range, target)) in self.links.iter().enumerate() {
            if let Some(card) = (links.peek)(target) {
                let key = ElementId::NamedChild(Arc::new(id.clone()), SharedString::from(format!("link-{k}")));
                let request_key = key.clone();
                rest.push((range.clone(), Link::new(key, move |rect| peek::request(request_key.clone(), rect, card.clone()))));
            }
        }
        let mut element = words(id.clone(), text, rest, palette);
        for (range, _) in &self.links {
            element = element.decor(range.clone(), Decor::Hairline);
        }
        let marks = self.marks;
        let targets = self.links;
        let mut root = gpui::div().set(role, measure).child(element);
        if !marks.is_empty() {
            root = root.child(Marks { layout: layout.clone(), ranges: marks, color: palette.peri.base.hsla() });
        }
        if targets.is_empty() {
            return root.into_any_element();
        }
        root.on_mouse_up(MouseButton::Left, move |event: &MouseUpEvent, window, cx| {
            let Ok(index) = layout.index_for_position(event.position) else { return };
            if let Some((_, target)) = targets.iter().find(|(range, _)| range.contains(&index)) {
                window.dispatch_action(Box::new(Open { target: target.clone() }), cx);
                cx.stop_propagation();
            }
        })
        .into_any_element()
    }
}

/// Periwinkle underlines under marked ranges of a laid-out text (painted
/// after the text, in its bounds).
struct Marks {
    layout: gpui::TextLayout,
    ranges: Vec<Range<usize>>,
    color: Hsla,
}

impl IntoElement for Marks {
    type Element = gpui::Canvas<()>;

    fn into_element(self) -> Self::Element {
        let Self { layout, ranges, color } = self;
        gpui::canvas(
            |_, _, _| {},
            move |_, (), window, _| {
                for range in &ranges {
                    if let Some(rect) = crate::overlay::text::range_rect(&layout, range) {
                        let bar = gpui::Bounds::new(
                            gpui::point(rect.origin.x, rect.bottom() - gpui::px(1.5)),
                            gpui::size(rect.size.width, gpui::px(1.5)),
                        );
                        window.paint_quad(gpui::fill(bar, color));
                    }
                }
            },
        )
        .absolute()
        .size_0()
    }
}

use gpui::Styled as _;
