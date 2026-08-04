//! A Wadler–Lindig document algebra.
//!
//! This is the layout foundation for every language backend. It separates
//! *what* to print (the document's structure) from *where* to break it (a
//! decision the printer makes lazily, once it knows the available width).
//! Backends build a [`Doc`] out of a handful of composable constructors and
//! never concatenate strings with baked-in newlines or indentation.
//!
//! The vocabulary (Wadler 1998, "A prettier printer"):
//!
//! | Constructor        | Meaning                                              |
//! |--------------------|------------------------------------------------------|
//! | [`nil`]            | the empty document (identity for concatenation)      |
//! | [`text`]           | a literal string, never broken                       |
//! | [`line`]           | a space when flat, a newline+indent when broken      |
//! | [`softline`]       | nothing when flat, a newline+indent when broken      |
//! | [`hardline`]       | always a newline; forces every enclosing group broken |
//! | [`Doc::append`]    | associative concatenation (also the `+` operator)    |
//! | [`Doc::nest`]      | add `n` columns of indent to line breaks inside       |
//! | [`Doc::group`]     | render flat if it fits the width, otherwise broken   |
//! | [`Doc::annotate`]  | tag a sub-document (doc comments, keywords, …)        |
//!
//! The printer is Lindig's strict reformulation (2000): the document is walked
//! as an explicit stack of `(indent, mode, doc)` triples, and the `fits` check
//! is bounded to the remaining columns, so nested groups cost O(width) rather
//! than the exponential blow-up a naive strict `group` would incur.
//!
//! [`nil`]: Doc::nil
//! [`text`]: Doc::text
//! [`line`]: Doc::line
//! [`softline`]: Doc::softline
//! [`hardline`]: Doc::hardline

use std::rc::Rc;

/// A document tree, generic over an annotation type `A`.
///
/// Cheap to clone: every recursive position is an [`Rc`], so sharing a
/// sub-document (a rendered type used in several places, say) never copies it.
#[derive(Clone)]
pub enum Doc<A> {
    /// The empty document.
    Nil,
    /// Concatenation of two documents.
    Cat(Rc<Doc<A>>, Rc<Doc<A>>),
    /// A literal string. Must not contain `'\n'` — use [`Doc::hardline`].
    Text(Rc<str>),
    /// A space when flat, a newline+indent when broken.
    Line,
    /// Nothing when flat, a newline+indent when broken.
    SoftLine,
    /// Always a newline+indent; forces every enclosing group to break.
    HardLine,
    /// Indent the line breaks inside the child by `n` columns.
    Nest(isize, Rc<Doc<A>>),
    /// Render the child flat if it fits; otherwise render it broken.
    Group(Rc<Doc<A>>),
    /// Attach an annotation to a sub-document (surfaced by the printer).
    Annot(A, Rc<Doc<A>>),
    /// Render `flat` when the enclosing group is flat, `broken` when broken.
    FlatAlt(Rc<Doc<A>>, Rc<Doc<A>>),
}

/// Rendering mode chosen per group by the printer.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Flat,
    Break,
}

impl<A: Clone> Doc<A> {
    /// The empty document.
    pub fn nil() -> Self {
        Doc::Nil
    }

    /// A literal string, rendered verbatim and never broken.
    pub fn text(s: impl Into<Rc<str>>) -> Self {
        Doc::Text(s.into())
    }

    /// A break that is a single space when the enclosing group is flat.
    pub fn line() -> Self {
        Doc::Line
    }

    /// A break that disappears entirely when the enclosing group is flat.
    pub fn softline() -> Self {
        Doc::SoftLine
    }

    /// A break that is always taken, forcing every enclosing group to break.
    pub fn hardline() -> Self {
        Doc::HardLine
    }

    /// Concatenate `self` with `other`. See also the `+` operator.
    pub fn append(self, other: Doc<A>) -> Self {
        match (&self, &other) {
            (Doc::Nil, _) => other,
            (_, Doc::Nil) => self,
            _ => Doc::Cat(Rc::new(self), Rc::new(other)),
        }
    }

    /// Indent line breaks inside `self` by `n` additional columns.
    pub fn nest(self, n: isize) -> Self {
        Doc::Nest(n, Rc::new(self))
    }

    /// Try to render `self` flat; fall back to broken if it does not fit.
    pub fn group(self) -> Self {
        Doc::Group(Rc::new(self))
    }

    /// Tag `self` with an annotation the printer can act on.
    pub fn annotate(self, a: A) -> Self {
        Doc::Annot(a, Rc::new(self))
    }

    /// Concatenate a sequence of documents with no separator.
    pub fn concat(docs: impl IntoIterator<Item = Doc<A>>) -> Self {
        docs.into_iter().fold(Doc::nil(), Doc::append)
    }

    /// Render `flat` in flat mode, `broken` in broken mode.
    pub fn flat_alt(flat: Doc<A>, broken: Doc<A>) -> Self {
        Doc::FlatAlt(Rc::new(flat), Rc::new(broken))
    }

    /// Concatenate a sequence of documents, placing `sep` between each pair.
    pub fn join(sep: Doc<A>, docs: impl IntoIterator<Item = Doc<A>>) -> Self {
        let mut out = Doc::nil();
        for (i, d) in docs.into_iter().enumerate() {
            if i > 0 {
                out = out.append(sep.clone());
            }
            out = out.append(d);
        }
        out
    }

    /// Greedy fill combinator: pack items separated by `sep` as many per line as
    /// fit; when a break is needed, use `hardline` instead of `sep`.
    ///
    /// `fill(sep, [x]) = x`
    /// `fill(sep, x:xs) = x <> flat_alt(sep <> fill(sep, xs), hardline() <> fill(sep, xs))`
    pub fn fill(sep: Doc<A>, items: impl IntoIterator<Item = Doc<A>>) -> Self {
        fill_impl(sep, items.into_iter().collect())
    }

    /// Render the document to a string, breaking to keep lines within `width`.
    pub fn render(&self, width: usize) -> String {
        let mut sink = StringSink { out: String::new() };
        self.render_to(width, &mut sink);
        sink.out
    }

    /// Render, delivering annotation spans to `sink` alongside the text.
    pub fn render_to(&self, width: usize, sink: &mut impl Sink<A>) {
        let mut col = 0usize;
        // Work stack, processed top (last) first. `None` in the annotation slot
        // means "on pop, close the most recent annotation".
        let mut stack: Vec<Item<'_, A>> = vec![Item::Doc(0, Mode::Break, self)];

        while let Some(item) = stack.pop() {
            let (indent, mode, doc) = match item {
                Item::Doc(i, m, d) => (i, m, d),
                Item::PopAnnot => {
                    sink.end_annotation();
                    continue;
                }
            };
            match doc {
                Doc::Nil => {}
                Doc::Cat(a, b) => {
                    stack.push(Item::Doc(indent, mode, b));
                    stack.push(Item::Doc(indent, mode, a));
                }
                Doc::Nest(n, d) => stack.push(Item::Doc(add(indent, *n), mode, d)),
                Doc::Text(s) => {
                    sink.text(s);
                    col += width_of(s);
                }
                Doc::Line => match mode {
                    Mode::Flat => {
                        sink.text(" ");
                        col += 1;
                    }
                    Mode::Break => {
                        newline(sink, indent);
                        col = indent;
                    }
                },
                Doc::SoftLine => {
                    if mode == Mode::Break {
                        newline(sink, indent);
                        col = indent;
                    }
                }
                Doc::HardLine => {
                    newline(sink, indent);
                    col = indent;
                }
                Doc::Group(d) => {
                    let fit = fits(width as isize - col as isize, indent, d, &stack);
                    let m = if fit { Mode::Flat } else { Mode::Break };
                    stack.push(Item::Doc(indent, m, d));
                }
                Doc::Annot(a, d) => {
                    sink.begin_annotation(a);
                    stack.push(Item::PopAnnot);
                    stack.push(Item::Doc(indent, mode, d));
                }
                Doc::FlatAlt(flat, broken) => {
                    let d = if mode == Mode::Flat { flat } else { broken };
                    stack.push(Item::Doc(indent, mode, d));
                }
            }
        }
    }
}

/// A work item on the printer's stack.
enum Item<'a, A> {
    Doc(usize, Mode, &'a Doc<A>),
    PopAnnot,
}

/// Would `doc`, rendered flat, followed by whatever comes after it, fit within
/// `remaining` columns before the next line break? Bounded to O(remaining):
/// stops the moment the budget goes negative or a newline is reached.
fn fits<A>(mut remaining: isize, indent: usize, doc: &Doc<A>, rest: &[Item<'_, A>]) -> bool {
    let mut local: Vec<(usize, Mode, &Doc<A>)> = vec![(indent, Mode::Flat, doc)];
    let mut rest_idx = rest.len();

    loop {
        if remaining < 0 {
            return false;
        }
        let (ind, mode, d) = match local.pop() {
            Some(it) => it,
            None => {
                // Fall through to the continuation, preserving its own modes.
                loop {
                    if rest_idx == 0 {
                        return true;
                    }
                    rest_idx -= 1;
                    match &rest[rest_idx] {
                        Item::Doc(i, m, d) => break (*i, *m, *d),
                        Item::PopAnnot => continue,
                    }
                }
            }
        };
        match d {
            Doc::Nil => {}
            Doc::Cat(a, b) => {
                local.push((ind, mode, b));
                local.push((ind, mode, a));
            }
            Doc::Nest(n, x) => local.push((add(ind, *n), mode, x)),
            Doc::Text(s) => remaining -= width_of(s) as isize,
            Doc::Line => match mode {
                Mode::Flat => remaining -= 1,
                Mode::Break => return true,
            },
            Doc::SoftLine => {
                if mode == Mode::Break {
                    return true;
                }
            }
            // A hard line while we are speculating flat means this group cannot
            // be flat; encountered in the (broken) continuation it ends the line.
            Doc::HardLine => match mode {
                Mode::Flat => return false,
                Mode::Break => return true,
            },
            Doc::Group(x) => local.push((ind, Mode::Flat, x)),
            Doc::Annot(_, x) => local.push((ind, mode, x)),
            Doc::FlatAlt(flat, broken) => {
                let d = if mode == Mode::Flat { flat } else { broken };
                local.push((ind, mode, d));
            }
        }
    }
}

fn fill_impl<A: Clone>(sep: Doc<A>, items: Vec<Doc<A>>) -> Doc<A> {
    if items.is_empty() {
        return Doc::nil();
    }
    let mut iter = items.into_iter();
    let x = iter.next().unwrap();
    let xs: Vec<Doc<A>> = iter.collect();
    if xs.is_empty() {
        return x;
    }
    let rest = fill_impl(sep.clone(), xs);
    x + Doc::flat_alt(sep + rest.clone(), Doc::hardline() + rest)
}

/// Add a signed nesting delta to an indent, clamping at zero.
fn add(indent: usize, delta: isize) -> usize {
    (indent as isize + delta).max(0) as usize
}

/// Display width of a string: ASCII chars are 1 column, wide CJK/emoji are 2.
fn width_of(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

fn char_width(c: char) -> usize {
    if (c as u32) < 128 {
        return 1;
    }
    match c {
        '\u{1100}'..='\u{115F}'
        | '\u{2E80}'..='\u{303E}'
        | '\u{3040}'..='\u{33FF}'
        | '\u{3400}'..='\u{4DBF}'
        | '\u{4E00}'..='\u{9FFF}'
        | '\u{A000}'..='\u{A48F}'
        | '\u{F900}'..='\u{FAFF}'
        | '\u{FE30}'..='\u{FE6F}'
        | '\u{FF01}'..='\u{FF60}'
        | '\u{FFE0}'..='\u{FFE6}'
        | '\u{1F300}'..='\u{1F9FF}' => 2,
        _ => 1,
    }
}

fn newline<A>(sink: &mut impl Sink<A>, indent: usize) {
    sink.newline(indent);
}

/// A destination for rendered output. The default [`StringSink`] ignores
/// annotations; richer sinks (syntax highlighting, HTML) can act on them.
pub trait Sink<A> {
    fn text(&mut self, s: &str);
    fn newline(&mut self, indent: usize);
    fn begin_annotation(&mut self, _a: &A) {}
    fn end_annotation(&mut self) {}
}

struct StringSink {
    out: String,
}

impl<A> Sink<A> for StringSink {
    fn text(&mut self, s: &str) {
        self.out.push_str(s);
    }
    fn newline(&mut self, indent: usize) {
        self.out.push('\n');
        for _ in 0..indent {
            self.out.push(' ');
        }
    }
}

impl<A: Clone> std::ops::Add for Doc<A> {
    type Output = Doc<A>;
    fn add(self, rhs: Doc<A>) -> Doc<A> {
        self.append(rhs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type D = Doc<()>;

    fn t(s: &str) -> D {
        Doc::text(s)
    }

    /// A braced, comma-separated group that inlines when it fits and explodes
    /// one-item-per-line when it does not — the canonical prettifier shape.
    fn braces(items: Vec<D>) -> D {
        let inner = Doc::join(Doc::text(",") + Doc::line(), items);
        (t("{") + (Doc::line() + inner).nest(4) + Doc::line() + t("}")).group()
    }

    #[test]
    fn inlines_when_it_fits() {
        let doc = braces(vec![t("a: i32"), t("b: i32")]);
        assert_eq!(doc.render(80), "{ a: i32, b: i32 }");
    }

    #[test]
    fn breaks_when_too_wide() {
        let doc = braces(vec![t("a: i32"), t("b: i32")]);
        assert_eq!(doc.render(12), "{\n    a: i32,\n    b: i32\n}");
    }

    #[test]
    fn hardline_forces_the_group_broken() {
        let doc = (t("a") + Doc::hardline() + t("b")).group();
        assert_eq!(doc.render(80), "a\nb");
    }

    #[test]
    fn nesting_is_relative_to_the_break_column() {
        let inner = (t("x") + Doc::line() + t("y")).nest(2);
        let doc = (t("(") + inner + t(")")).group();
        assert_eq!(doc.render(3), "(x\n  y)");
    }

    #[test]
    fn softline_vanishes_flat_but_breaks_wide() {
        let doc = (t("a") + Doc::softline() + t("b")).group();
        assert_eq!(doc.render(80), "ab");
        assert_eq!(doc.render(1), "a\nb");
    }

    #[test]
    fn append_absorbs_nil() {
        let doc = Doc::nil() + t("x") + Doc::nil();
        assert_eq!(doc.render(80), "x");
    }
}
