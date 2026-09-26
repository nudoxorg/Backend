//! The upgrade lens's two elements.
//!
//! - [`shelf_line`]: the crate-wide line W-Controls places under its version
//!   comb — "71 breaking · 77 added · 7 respelled · none of your 80 uses
//!   change". Each count is a door (a tip saying what it counts).
//! - [`section`]: the page section above the anatomy — "Upgrading to 1.1.6",
//!   the your-code line, up to four affected statements (the used name
//!   underlined in periwinkle, as In use marks a hit), the respelled uses
//!   folded into one quiet line of places (each a door to its statement),
//!   then this symbol's own changes as rows: the old parts of a changed
//!   signature struck, the new parts underlined in mint, what both share
//!   plain.
//!
//! Calm (§6.2): monochrome words; mint only under what is new, periwinkle
//! only under the hit; ⌥ spells each signature's Rust source.

use super::{Impacted, Lens, Mark, Marked, ROWS, Row, SITES, Summary, What};
use crate::anatomy::{Deco, Line, Links, TypeInk, k, roles, stacked};
use crate::data::door::Door;
use crate::data::spell::spell;
use crate::measure::{Measure, Set, Space};
use crate::overlay::tooltip::{self, TipText};
use crate::probe;
use crate::semantics::types::Piece;
use crate::theme::ActiveFacet;
use crate::tokens::{Face, Palette, TypeRole};
use gpui::{
    AnyElement, App, ElementId, Hsla, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, div,
};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

/// The shelf line's words.
const SHELF: TypeRole = TypeRole { face: Face::Ui, weight: 400.0, size: 11.5, line: 16.0, tracking: 0.0, italic: false };
/// The shelf line's counts.
const COUNT: TypeRole = TypeRole { weight: 600.0, ..SHELF };
/// The your-code line.
const YOURS: TypeRole = TypeRole { face: Face::Ui, weight: 400.0, size: 13.0, line: 20.0, tracking: 0.0, italic: false };
/// The quiet respelled line.
const QUIET: TypeRole = TypeRole { face: Face::Ui, weight: 400.0, size: 12.0, line: 16.0, tracking: 0.0, italic: false };
/// A place (`forge/manifest.rs:91`).
const PLACE: TypeRole = TypeRole { face: Face::Mono, weight: 400.0, size: 11.5, line: 16.0, tracking: 0.0, italic: false };
/// A row's word (`added`, `changed`).
const WORD: TypeRole = TypeRole { face: Face::Ui, weight: 400.0, size: 12.0, line: 20.0, tracking: 0.0, italic: false };

fn child(id: &ElementId, part: impl Into<SharedString>) -> ElementId {
    ElementId::NamedChild(Arc::new(id.clone()), part.into())
}

fn tip(body: String) -> AnyElementBuilder {
    Rc::new(move |measure: &Measure, window: &mut Window, cx: &mut App| {
        tooltip::content(TipText { title: None, body: SharedString::from(body.clone()), chord: Vec::new() })(measure, window, cx)
    })
}

type AnyElementBuilder = Rc<dyn Fn(&Measure, &mut Window, &mut App) -> AnyElement>;

/// Words in one face, published for the layout and contrast lints (they
/// wrap; only a word wider than its box would be a clip).
fn said(key: ElementId, text: impl Into<SharedString>, role: TypeRole, color: Hsla, m: &Measure) -> AnyElement {
    let text: SharedString = text.into();
    let words = div().set(role, m).text_color(color).child(text.clone());
    probe::text(key, text, m.role(role), 1.0, probe::TextOverflow::Wrap, words).into_any_element()
}

// ------------------------------------------------------------------ the shelf line

/// The crate-wide line for the shelf (W-Controls places it under the comb).
#[must_use]
pub fn shelf_line(id: impl Into<ElementId>, summary: &Summary, from: &str, to: &str, measure: &Measure, palette: &Palette) -> AnyElement {
    let id: ElementId = id.into();
    let mut line = spell(measure).id(id).row_gap(1.0);
    let mut tips: Vec<AnyElementBuilder> = Vec::new();
    let (quiet, count) = (palette.ink3.hsla(), palette.ink1.hsla());
    let from = super::short(from).to_owned();
    if !summary.local {
        return line.text(summary.words(to), SHELF, quiet).into_any_element();
    }
    let mut part = |line: crate::data::Spell, n: Option<usize>, words: String, says: String| {
        let k = tips.len();
        tips.push(tip(says));
        let line = match n {
            Some(n) => line.part_text(k, n.to_string(), COUNT, count),
            None => line,
        };
        line.part_text(k, words, SHELF, quiet)
    };
    line = if summary.breaking > 0 {
        part(line, Some(summary.breaking), " breaking".into(), format!("changes that can stop code written for {from} compiling"))
    } else {
        part(line, None, "nothing breaking".into(), format!("nothing written for {from} stops compiling"))
    };
    line = line.text(" · ", SHELF, quiet).seg(crate::data::Seg::Break);
    line = part(line, None, format!("{} added", summary.added), "new items; nothing you wrote changes".into());
    if summary.respelled > 0 {
        line = line.text(" · ", SHELF, quiet).seg(crate::data::Seg::Break);
        line = part(
            line,
            None,
            format!("{} respelled", summary.respelled),
            "changed only in their Rust spelling (a lifetime, Self, a path); they read the same in plain words".into(),
        );
    }
    if summary.uses > 0 {
        line = line.text(" · ", SHELF, quiet).seg(crate::data::Seg::Break);
        let s = if summary.uses == 1 { "" } else { "s" };
        line = if summary.changing > 0 {
            part(line, Some(summary.changing), format!(" of your {} use{s} change", summary.uses), "places in your code whose item really changes".into())
        } else {
            part(line, None, format!("none of your {} use{s} change", summary.uses), "every place your code uses the crate reads the same after the move".into())
        };
    }
    if let Some(slip) = summary.slip_words() {
        line = line.seg(crate::data::Seg::Break).text(format!(" · {slip}"), SHELF, palette.ink2.hsla());
    }
    let tips = Rc::new(tips);
    line.door(Door::tip(move |k, measure, window, cx| match tips.get(k) {
        Some(build) => build(measure, window, cx),
        None => div().into_any_element(),
    }))
    .into_any_element()
}

// ------------------------------------------------------------------ the page section

/// The upgrade section of a symbol's page. Build with [`section`].
#[derive(IntoElement)]
pub struct Section {
    id: ElementId,
    lens: Lens,
    symbol: SharedString,
    pinned: SharedString,
    measure: Measure,
    links: Links,
    xray: bool,
}

/// The section for `lens` on `symbol`'s page (pinned at `pinned`), at
/// `measure`; ⌥ (`xray`) spells each signature's source.
#[must_use]
pub fn section(
    id: impl Into<ElementId>,
    lens: Lens,
    symbol: impl Into<SharedString>,
    pinned: impl Into<SharedString>,
    measure: &Measure,
    links: &Links,
    xray: bool,
) -> Section {
    Section { id: id.into(), lens, symbol: symbol.into(), pinned: pinned.into(), measure: *measure, links: links.clone(), xray }
}

/// Pushes one side of a change, piece by piece, and marks what differs:
/// struck (old) or underlined in mint (new).
fn push_marked(line: &mut Line, marked: &Marked, ink: &TypeInk, links: &Links, palette: &Palette, xray: bool, quiet: bool) {
    let words_role = roles::words(ink.base);
    let italic = roles::italic(ink.base);
    let dim = |c: Hsla| if quiet { palette.ink3.hsla() } else { c };
    let mut runs: Vec<(Range<usize>, Mark)> = Vec::new();
    for (piece, mark) in &marked.pieces {
        let range = match piece {
            Piece::Word(w) => line.push(w, words_role, dim(ink.words)),
            Piece::Punct(p) => line.push(p, ink.base, ink.punct),
            Piece::Var(v) | Piece::Assoc(v) => line.push(v, italic, dim(ink.var)),
            Piece::Prim(p) => line.push(p, ink.base, dim(ink.prim)),
            // A space in the words face: a mono space would double every gap.
            Piece::Space => line.push(" ", words_role, ink.words),
            Piece::Name { text, target } => {
                let color = if (links.yours)(target) { ink.yours } else { dim(ink.link) };
                line.link(text, ink.base, color, target.clone())
            }
        };
        match runs.last_mut() {
            Some((last, m)) if *m == *mark && last.end == range.start => last.end = range.end,
            _ => runs.push((range, *mark)),
        }
    }
    for (range, mark) in runs {
        // A run's outer spaces are not part of what changed.
        let text = &line.text()[range.clone()];
        let range = (range.start + (text.len() - text.trim_start().len()))..(range.end - (text.len() - text.trim_end().len()));
        match mark {
            Mark::Same => {}
            Mark::Old => line.decorate(range, Deco::Strike(palette.ink3.hsla())),
            Mark::New => line.decorate(range, Deco::Underline(palette.mint.base.hsla())),
        }
    }
    if xray && !marked.source.is_empty() {
        line.push("  ", ink.base, ink.source);
        line.push(&marked.source, ink.base, ink.source);
    }
}

impl Section {
    fn heading(&self, palette: &Palette) -> AnyElement {
        said(child(&self.id, "heading"), self.lens.heading(), roles::SECTION, palette.ink0.hsla(), &self.measure)
    }

    /// "4 touch toml::from_str, respelled but the same in plain words:
    /// forge/manifest.rs:91, …": each place a door to its statement.
    fn respelled(&self, palette: &Palette) -> Option<AnyElement> {
        let (lead, places, more) = self.lens.respelled_line()?;
        let m = &self.measure;
        let mut line = spell(m).id(child(&self.id, "respelled")).row_gap(1.0).text(lead, QUIET, palette.ink3.hsla());
        let mut statements = Vec::new();
        for (n, place) in places.into_iter().enumerate() {
            if n > 0 {
                line = line.text(", ", QUIET, palette.ink4.hsla());
            }
            line = line.seg(crate::data::Seg::Break).part_text(n, place, PLACE, palette.ink3.hsla());
            statements.push(self.lens.respelled[n].site.text.to_string());
        }
        if let Some(more) = more {
            line = line.text(more, QUIET, palette.ink3.hsla());
        }
        let statements = Rc::new(statements);
        Some(
            line.door(Door::tip(move |k, measure, window, cx| {
                let body = statements.get(k).cloned().unwrap_or_default();
                tip(body)(measure, window, cx)
            }))
            .into_any_element(),
        )
    }

    /// One affected statement: its place and what changed, then the
    /// statement with the used name underlined in periwinkle.
    fn affected(&self, n: usize, u: &Impacted, palette: &Palette) -> AnyElement {
        let m = &self.measure;
        let mut caption = Line::new();
        caption.push(&u.site.place(), roles::CAPTION, palette.ink2.hsla());
        caption.push("  ", roles::CAPTION, palette.ink4.hsla());
        let tail: Vec<&str> = u.change.path.rsplit("::").take(2).collect();
        let tail = tail.into_iter().rev().collect::<Vec<_>>().join("::");
        caption.push(&format!("{} · {tail}", u.change.what.word()), roles::words(roles::CAPTION), palette.ink3.hsla());
        let pad_x = k(m, 14.0);
        let code_measure = m.within(m.width() - pad_x * 2.0);
        let mut code = Line::new();
        let text = u.site.text.as_ref();
        let name = u.site.path.rsplit("::").next().unwrap_or("");
        match (!name.is_empty()).then(|| text.find(name)).flatten() {
            Some(at) => {
                code.push(&text[..at], roles::CODE, palette.ink1.hsla());
                let hit = code.push(name, TypeRole { weight: 600.0, ..roles::CODE }, palette.ink0.hsla());
                code.mark(hit);
                code.push(&text[at + name.len()..], roles::CODE, palette.ink1.hsla());
            }
            None => {
                code.push(text, roles::CODE, palette.ink1.hsla());
            }
        }
        div()
            .flex()
            .flex_col()
            .gap(k(m, 6.0))
            .child(caption.element(child(&self.id, format!("site-{n}")), roles::CAPTION, m, &self.links, palette))
            .child(
                div()
                    .px(pad_x)
                    .py(k(m, 10.0))
                    .bg(palette.well.hsla())
                    .child(code.element(child(&self.id, format!("code-{n}")), roles::CODE, &code_measure, &self.links, palette)),
            )
            .into_any_element()
    }

    /// One change to the symbol: its word, then its name and signature as
    /// one line of text that wraps like prose (a changed item's new
    /// signature on a second line).
    fn row(&self, n: usize, row: &Row, palette: &Palette) -> AnyElement {
        let m = &self.measure;
        let ink = TypeInk::new(roles::TYPE, palette);
        let word = div().flex_none().w(k(m, 98.0)).child(said(
            child(&self.id, format!("row-{n}-word")),
            row.what.word(),
            WORD,
            palette.ink3.hsla(),
            m,
        ));
        let name_ink = if row.respelled { palette.ink2.hsla() } else { palette.ink0.hsla() };
        let element = |k: usize, line: Line| line.element(child(&self.id, format!("row-{n}-{k}")), roles::TYPE, m, &self.links, palette);
        let named = || {
            let mut line = Line::new();
            line.push(&row.name, roles::ROW, name_ink);
            line.push(" ", roles::ROW, ink.punct);
            line
        };
        let mut lines = div().flex().flex_col().min_w_0().flex_1().gap(k(m, 2.0));
        if row.respelled {
            let mut first = named();
            first.push("reads the same in plain words; only its Rust spelling moved", QUIET, palette.ink3.hsla());
            lines = lines.child(element(0, first));
            if let Some(after) = &row.after {
                let mut second = Line::new();
                push_marked(&mut second, after, &ink, &self.links, palette, self.xray, true);
                lines = lines.child(element(1, second));
            }
        } else if row.what == What::Changed {
            let mut first = named();
            if let Some(before) = &row.before {
                push_marked(&mut first, before, &ink, &self.links, palette, self.xray, true);
            }
            lines = lines.child(element(0, first));
            if let Some(after) = &row.after {
                let mut second = Line::new();
                push_marked(&mut second, after, &ink, &self.links, palette, self.xray, false);
                lines = lines.child(element(1, second));
            }
        } else {
            let mut first = named();
            if let Some(side) = row.after.as_ref().or(row.before.as_ref()) {
                push_marked(&mut first, side, &ink, &self.links, palette, self.xray, row.what == What::Removed);
            }
            lines = lines.child(element(0, first));
        }
        if stacked(m) {
            div().flex().flex_col().gap(k(m, 2.0)).child(word).child(lines).into_any_element()
        } else {
            div().flex().items_start().child(word).child(lines).into_any_element()
        }
    }
}

impl RenderOnce for Section {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.facet().palette();
        let m = self.measure;
        let mut root = div().flex().flex_col().gap(k(&m, 8.0)).child(div().mb(m.space(Space::Tight)).child(self.heading(palette)));
        if !self.lens.compared() {
            return root.child(said(
                child(&self.id, "absent"),
                format!("{} is not on this machine, so its API cannot be compared; only its date is known.", self.lens.to),
                QUIET,
                palette.ink3.hsla(),
                &m,
            ));
        }
        let mut you = div().flex().flex_col().gap(k(&m, 4.0));
        if let Some(words) = self.lens.your_code() {
            you = you.child(said(child(&self.id, "yours"), words, YOURS, palette.ink1.hsla(), &m));
        }
        if let Some(line) = self.respelled(palette) {
            you = you.child(line);
        }
        root = root.child(you);
        if !self.lens.affected.is_empty() {
            let mut sites = div().flex().flex_col().gap(k(&m, 14.0)).py(k(&m, 6.0));
            for (n, u) in self.lens.affected.iter().take(SITES).enumerate() {
                sites = sites.child(self.affected(n, u, palette));
            }
            root = root.child(sites);
        }
        let mut rows = div().flex().flex_col().gap(k(&m, 8.0));
        if self.lens.rows.is_empty() {
            rows = rows.child(said(
                child(&self.id, "unchanged"),
                format!("{} itself is unchanged between {} and {}.", self.symbol, super::short(&self.pinned), self.lens.to),
                QUIET,
                palette.ink3.hsla(),
                &m,
            ));
        }
        for (n, row) in self.lens.rows.iter().take(ROWS).enumerate() {
            rows = rows.child(self.row(n, row, palette));
        }
        root = root.child(rows);
        if let Some(more) = self.lens.more() {
            root = root.child(said(child(&self.id, "more"), more, QUIET, palette.ink3.hsla(), &m));
        }
        root
    }
}
