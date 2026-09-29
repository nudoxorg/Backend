//! The body of the page: the docs (examples and long prose fold), "If it
//! fails", "What it is" (an enum is a fork, a struct a bracket, a trait what
//! you write), and "What you can do with it" (methods by verb).

use super::call::{Tone, ty_el};
use super::card::Cards;
use super::host::Change;
use super::ink::{G, group_mark, mark};
use super::key::{FoldKey, Key, Part, Sec};
use super::kit::{Env, Ellipsis, Ink, Stop, caps, head, ink, joint, prose, roles, said, said_in, truncated, wrapped};
use super::view::{Block, Case, Docs, Fails, Field, Group, Outcomes, Owed, Row, Shape};
use crate::anatomy::page::named_as;
use crate::anatomy::unroll::unroll;
use gpui::{AnyElement, InteractiveElement, IntoElement, ParentElement, Pixels, StatefulInteractiveElement, Styled, div};

fn section(env: &Env<'_>, sec: Sec) -> gpui::Stateful<gpui::Div> {
    div().id(Key::of(Part::Sec(sec)).id()).mt(env.s(env.lay.section)).flex().flex_col()
}

/// A disclosure's line: its words and a caret, running `toggle`.
fn fold_line(env: &Env<'_>, key: &Key, words: &str, open: bool, toggle: super::host::Act) -> AnyElement {
    let i = ink(env.p);
    let caret = if open { "▾" } else { "▸" };
    div()
        .id(key.field("line").id())
        .flex()
        .items_center()
        .gap(env.k(8.0))
        .cursor_pointer()
        .child(said(env, &key.field("words"), words.to_owned(), roles::CHIP, i.peri))
        .child(said(env, &key.field("caret"), caret, roles::CHIP, i.peri))
        .on_click(move |_, window, cx| toggle(window, cx))
        .into_any_element()
}

/// A fold: its line, and its body unrolled in place beneath it.
fn folded(env: &Env<'_>, fold: &FoldKey, key: &Key, words: &str, body: AnyElement) -> AnyElement {
    let Some(state) = env.host.unfold(fold) else {
        return body;
    };
    div()
        .flex()
        .flex_col()
        .child(fold_line(env, key, words, state.open, state.toggle.clone()))
        .child(unroll(key.field("unroll").id(), state.open, state.presence, div().pt(env.k(8.0)).child(body)))
        .into_any_element()
}

// ------------------------------------------------------------------ docs

fn block(env: &Env<'_>, i: &Ink, key: &Key, block: &Block) -> AnyElement {
    match block {
        Block::Para(text) => div().mb(env.k(12.0)).max_w(env.s(720.0)).child(prose(env, key, text, roles::PROSE, i.ink1)).into_any_element(),
        Block::Head(text) => div().mt(env.k(18.0)).mb(env.k(8.0)).child(said(env, key, caps(text), roles::HEAD, i.ink3)).into_any_element(),
        Block::Item(text) => div()
            .flex()
            .items_start()
            .gap(env.k(8.0))
            .mb(env.k(6.0))
            .ml(env.k(6.0))
            .max_w(env.s(720.0))
            .child(div().flex_none().mt(env.s(9.0)).size(env.s(4.0)).bg(i.ink3))
            .child(prose(env, key, text, roles::PROSE, i.ink1))
            .into_any_element(),
        Block::Code(code) => div().mb(env.k(12.0)).p(env.k(12.0)).bg(i.table).border_1().border_color(i.line1).child(wrapped(env, key, code.clone(), roles::CODE, i.ink1)).into_any_element(),
    }
}

/// One doc block at its place: an example folds as "Example".
fn doc_block(env: &Env<'_>, i: &Ink, n: usize, b: &Block) -> AnyElement {
    let key = Key::of(Part::Doc).at(n);
    match b {
        Block::Code(_) => {
            let example = Key::of(Part::Example).at(n);
            div().mb(env.k(12.0)).child(folded(env, &FoldKey::Example(n), &example, "Example", block(env, i, &example.field("code"), b))).into_any_element()
        }
        other => block(env, i, &key, other),
    }
}

/// The docs after the lede: two blocks, then "N more" folded; each example
/// folds as "Example ▸".
pub(super) fn docs(env: &Env<'_>, docs: &Docs) -> Option<AnyElement> {
    if docs.blocks.is_empty() {
        return None;
    }
    let i = ink(env.p);
    let mut shown: Vec<AnyElement> = Vec::new();
    let mut hidden: Vec<AnyElement> = Vec::new();
    let mut prose_hidden = 0;
    let mut text_blocks = 0;
    for (n, b) in docs.blocks.iter().enumerate() {
        let is_text = !matches!(b, Block::Code(_));
        if is_text {
            text_blocks += 1;
        }
        if text_blocks > 2 {
            if is_text {
                prose_hidden += 1;
            }
            hidden.push(doc_block(env, &i, n, b));
        } else {
            shown.push(doc_block(env, &i, n, b));
        }
    }
    if !hidden.is_empty() {
        let count = prose_hidden.max(1);
        let words = format!("{count} more paragraph{}", if count == 1 { "" } else { "s" });
        shown.push(folded(env, &FoldKey::MoreDocs, &Key::of(Part::Doc).field("more"), &words, div().flex().flex_col().children(hidden).into_any_element()));
    }
    Some(div().id(Key::of(Part::Sec(Sec::Docs)).id()).mt(env.s(env.lay.section - 8.0)).flex().flex_col().children(shown).into_any_element())
}

// ------------------------------------------------------------------ if it fails

/// "If it fails": the error, when, and what kinds it tells you.
pub(super) fn fails(env: &Env<'_>, cards: &Cards, fails: &Fails) -> AnyElement {
    let i = ink(env.p);
    let key = Key::of(Part::Sec(Sec::Fails));
    let mut top = div().flex().flex_wrap().items_baseline().gap(env.k(10.0)).child(div().flex_none().child(ty_el(env, cards, &key.field("type"), &fails.ty, Tone::Error)));
    if !fails.when.trim().is_empty() {
        top = top.child(div().min_w_0().flex_1().child(prose(env, &key.field("when"), &fails.when, roles::BODY, i.ink1)));
    }
    let mut block = div().flex().flex_col().gap(env.k(12.0)).px(env.k(16.0)).py(env.k(14.0)).bg(i.coral_soft).border_l_2().border_color(i.coral).child(top);
    if !fails.kinds.is_empty() {
        let mut kinds = div().flex().flex_wrap().gap_x(env.k(22.0)).gap_y(env.k(8.0));
        for (n, kind) in fails.kinds.iter().enumerate() {
            let dim = kind.impossible.is_some();
            let kk = Key::of(Part::ErrKind).at(n);
            kinds = kinds.child(
                div()
                    .flex()
                    .items_center()
                    .gap(env.k(8.0))
                    .child(mark(if dim { G::Verb(super::view::Verb::Names) } else { G::Fail }, env.p, 11.0 * env.m.scale()))
                    .child(said(env, &kk.field("name"), kind.name.clone(), roles::PACKAGE, if dim { i.ink3 } else { i.ink0 }))
                    .child(said(env, &kk.field("doc"), kind.doc.clone(), roles::DOC, if dim { i.ink3 } else { i.ink2 })),
            );
        }
        block = block.child(kinds);
        let mut foot: Vec<String> = Vec::new();
        if let Some(tells) = &fails.tells {
            foot.push(format!("{} tells you which with {tells}.", fails.ty.word));
        }
        foot.extend(fails.kinds.iter().filter_map(|kind| kind.impossible.clone()));
        if !foot.is_empty() {
            block = block.child(wrapped(env, &key.field("foot"), foot.join(" "), roles::ASIDE_SMALL, i.ink3));
        }
    }
    section(env, Sec::Fails).child(head(env, Sec::Fails, "If it fails", None, None)).child(block).into_any_element()
}

// ------------------------------------------------------------------ what it is

fn name_width(env: &Env<'_>, names: impl Iterator<Item = usize>) -> Pixels {
    let longest = names.max().unwrap_or(4) as f32;
    env.s((longest * 8.6 + 14.0).clamp(96.0, 240.0))
}

/// Whether case and field rows stack their name, type and doc: a room too
/// narrow for three columns.
fn stacked(env: &Env<'_>) -> bool {
    env.m.effective() < 760.0
}

fn hue_alpha(color: gpui::Hsla, alpha: f32) -> gpui::Hsla {
    let mut color = color;
    color.alpha *= alpha;
    color
}

/// A row that is one door: its name opens its page.
fn door(env: &Env<'_>, i: &Ink, key: &Key, name: &str, link: Option<&str>, role: crate::tokens::TypeRole) -> AnyElement {
    let m = env.m;
    let colour = i.ink0;
    let (word, name_key) = (name.to_owned(), key.field("name"));
    named_as(key.field("door").text(), name, link, colour, env.host, false, move |_| said_in(&m, &name_key, word, role, colour))
}

fn row_line(env: &Env<'_>, key: &Key, mark_el: AnyElement, stop: Stop, rail: gpui::Hsla, cells: RowCells, yours: Option<String>) -> AnyElement {
    let i = ink(env.p);
    let mut row = div().flex().items_center().gap(env.k(16.0)).min_h(env.s(38.0)).child(joint(env, mark_el, stop, rail));
    if stacked(env) {
        row = row.child(div().flex().flex_col().min_w_0().flex_1().py(env.k(6.0)).child(div().flex().flex_wrap().items_center().gap(env.k(10.0)).child(cells.name).child(cells.middle)).child(cells.doc));
    } else {
        row = row.child(div().w(cells.name_width).flex_none().child(cells.name)).child(div().min_w(env.s(150.0)).flex_none().child(cells.middle)).child(div().min_w_0().flex_1().py(env.k(6.0)).child(cells.doc));
    }
    if let Some(tail) = cells.tail {
        row = row.child(tail);
    }
    if let Some(words) = yours {
        row = row.child(said(env, &key.field("yours"), words, roles::WRITTEN, i.mint));
    }
    row.into_any_element()
}

/// The cells of a row of a fork, a bracket or a socket.
struct RowCells {
    name: AnyElement,
    name_width: Pixels,
    middle: AnyElement,
    doc: AnyElement,
    tail: Option<AnyElement>,
}

fn case_row(env: &Env<'_>, cards: &Cards, case: &Case, n: usize, total: usize, width: Pixels) -> AnyElement {
    let i = ink(env.p);
    let key = Key::of(Part::Case).at(n);
    let mut holds = div().flex().flex_wrap().items_center().gap_x(env.k(14.0)).gap_y(env.k(4.0)).min_w_0();
    if case.holds.is_empty() {
        holds = holds.child(said(env, &key.field("nothing"), "nothing inside", roles::DOC, i.ink3));
    } else {
        for (k, ty) in case.holds.iter().enumerate() {
            holds = holds.child(div().flex_none().child(ty_el(env, cards, &key.at(k).field("holds"), ty, Tone::Plain)));
        }
    }
    let cells = RowCells {
        name: door(env, &i, &key, &case.name, case.link.as_deref(), roles::CASE),
        name_width: width,
        middle: holds.into_any_element(),
        doc: wrapped(env, &key.field("doc"), case.doc.clone(), roles::DOC, i.ink3),
        tail: None,
    };
    let line = row_line(env, &key, mark(G::Case, env.p, 12.0 * env.m.scale()), Stop::of(n, total), hue_alpha(i.teal, 0.45), cells, (case.yours > 0).then(|| case.yours.to_string()));
    let mut wrap = div().id(key.id()).flex().flex_col().hover(|style| style.bg(i.g2)).child(line);
    if let Some(more) = &case.more {
        let fold = FoldKey::More(case.name.clone());
        if let Some(state) = env.host.unfold(&fold) {
            let toggle = state.toggle.clone();
            let text = div().pl(env.s(56.0)).pb(env.k(8.0)).child(prose(env, &key.field("more"), more, roles::DOC, i.ink2));
            wrap = wrap.cursor_pointer().on_click(move |_, window, cx| toggle(window, cx)).child(unroll(key.field("unroll").id(), state.open, state.presence, text));
        }
    }
    wrap.into_any_element()
}

fn field_row(env: &Env<'_>, cards: &Cards, field: &Field, n: usize, total: usize, width: Pixels) -> AnyElement {
    let i = ink(env.p);
    let key = Key::of(Part::Field).at(n);
    let cells = RowCells {
        name: door(env, &i, &key, &field.name, field.link.as_deref(), roles::CASE),
        name_width: width,
        middle: ty_el(env, cards, &key.field("type"), &field.ty, Tone::Plain),
        doc: wrapped(env, &key.field("doc"), field.doc.clone(), roles::DOC, i.ink3),
        tail: None,
    };
    let line = row_line(env, &key, mark(G::Field, env.p, 10.0 * env.m.scale()), Stop::of(n, total), hue_alpha(i.peri, 0.28), cells, (field.yours > 0).then(|| format!("you read it · {}", field.yours)));
    div().id(key.id()).hover(|style| style.bg(i.g2)).child(line).into_any_element()
}

fn owed_row(env: &Env<'_>, cards: &Cards, row: &Owed, n: usize, total: usize, width: Pixels) -> AnyElement {
    let i = ink(env.p);
    let key = Key::of(Part::Owed).at(n);
    let cells = RowCells {
        name: door(env, &i, &key, &row.name, row.link.as_deref(), roles::CASE),
        name_width: width,
        middle: ty_el(env, cards, &key.field("takes"), &row.takes, Tone::Plain),
        doc: wrapped(env, &key.field("doc"), row.doc.clone(), roles::DOC, i.ink3),
        tail: Some(outcome_marks(env, row.outcomes)),
    };
    let line = row_line(env, &key, mark(G::Field, env.p, 10.0 * env.m.scale()), Stop::of(n, total), hue_alpha(i.peri, 0.28), cells, None);
    div().id(key.id()).hover(|style| style.bg(i.g2)).child(line).into_any_element()
}

/// The small outcome glyphs of a row.
pub(super) fn outcome_marks(env: &Env<'_>, o: Outcomes) -> AnyElement {
    let size = 11.0 * env.m.scale();
    let mut row = div().flex().items_center().gap(env.k(3.0)).flex_none();
    if o.fails {
        row = row.child(mark(G::Fail, env.p, size));
    }
    if o.none {
        row = row.child(mark(G::None, env.p, size));
    }
    if o.later {
        row = row.child(mark(G::Later, env.p, size));
    }
    if o.many {
        row = row.child(mark(G::Many, env.p, size));
    }
    row.into_any_element()
}

/// "What it is".
pub(super) fn shape(env: &Env<'_>, cards: &Cards, shape: &Shape) -> AnyElement {
    let i = ink(env.p);
    let key = Key::of(Part::Sec(Sec::Shape));
    let (title, count, aside, rows): (&str, String, Option<AnyElement>, Vec<AnyElement>) = match shape {
        Shape::OneOf(cases) => {
            let width = name_width(env, cases.iter().map(|c| c.name.chars().count()));
            let nests = cases.iter().any(|c| c.holds.iter().any(|t| t.loops));
            let mut aside = div().flex().items_center().gap(env.k(6.0)).child(said(env, &key.field("aside"), "Exactly one of these at a time", roles::ASIDE, i.ink3));
            if nests {
                aside = aside.child(mark(G::Loop, env.p, 14.0 * env.m.scale())).child(said(env, &key.field("aside-loop"), "holds more of itself", roles::ASIDE, i.ink3));
            }
            ("What it is", format!("one of {}", cases.len()), Some(aside.into_any_element()), cases.iter().enumerate().map(|(n, c)| case_row(env, cards, c, n, cases.len(), width)).collect())
        }
        Shape::Holds { fields, hidden } => {
            let width = name_width(env, fields.iter().map(|f| f.name.chars().count()));
            let mut aside = "All of these, together".to_owned();
            if *hidden > 0 {
                aside = format!("{aside} · {hidden} private");
            }
            ("What it is", format!("holds {}", fields.len()), Some(said(env, &key.field("aside"), aside, roles::ASIDE, i.ink3)), fields.iter().enumerate().map(|(n, f)| field_row(env, cards, f, n, fields.len(), width)).collect())
        }
        Shape::Write { rows, .. } => {
            let width = name_width(env, rows.iter().map(|r| r.name.chars().count()));
            let words = if rows.len() == 1 { "Implement this, or derive it" } else { "Implement these, or derive it" };
            ("What you write", rows.len().to_string(), Some(said(env, &key.field("aside"), words, roles::ASIDE, i.ink3)), rows.iter().enumerate().map(|(n, r)| owed_row(env, cards, r, n, rows.len(), width)).collect())
        }
    };
    let plate = div().flex().flex_col().bg(i.g1).border_1().border_color(i.line2).pl(env.k(10.0)).pr(env.k(16.0)).py(env.k(6.0)).children(rows);
    let mut out = section(env, Sec::Shape).child(head(env, Sec::Shape, title, Some(count), aside)).child(plate);
    if let Shape::Write { implementors: Some(n), .. } = shape {
        out = out.child(div().mt(env.k(12.0)).child(wrapped(env, &key.field("implementors"), format!("{n} types implement it on this machine."), roles::ASIDE_SMALL, i.ink3)));
    }
    out.into_any_element()
}

// ------------------------------------------------------------------ what you can do

fn method_row(env: &Env<'_>, row: &Row, key: &Key) -> AnyElement {
    let i = ink(env.p);
    let mut sig = format!("({})", row.takes.join(", "));
    if let Some(gives) = &row.gives {
        sig = format!("{sig} → {gives}");
    }
    let mut line = div()
        .flex()
        .items_center()
        .gap(env.k(10.0))
        .px(env.k(4.0))
        .py(env.k(5.0))
        .child(door(env, &i, key, &row.name, row.link.as_deref(), roles::METHOD))
        .child(div().min_w_0().flex_1().child(truncated(env, &key.field("sig"), sig, roles::SIG, i.ink3, Ellipsis::End)))
        .child(outcome_marks(env, row.outcomes));
    if row.yours > 0 {
        line = line.child(
            div()
                .flex()
                .items_center()
                .gap(env.k(4.0))
                .flex_none()
                .child(div().size(env.s(6.0)).rounded_full().bg(i.mint))
                .child(said(env, &key.field("yours"), row.yours.to_string(), roles::COUNT, i.mint)),
        );
    }
    div().id(key.id()).hover(|style| style.bg(i.g2)).child(line).into_any_element()
}

fn group(env: &Env<'_>, group: &Group, n: usize) -> AnyElement {
    let i = ink(env.p);
    let key = Key::of(Part::Group).at(n);
    let head = div()
        .flex()
        .items_center()
        .gap(env.k(8.0))
        .pb(env.k(6.0))
        .mb(env.k(2.0))
        .border_b_1()
        .border_color(i.line2)
        .child(mark(group_mark(group.verb), env.p, 14.0 * env.m.scale()))
        .child(said(env, &key.field("head"), group.verb.head(), roles::PACKAGE_UI, i.ink1))
        .child(said(env, &key.field("count"), group.rows.len().to_string(), roles::COUNT, i.ink3));
    let shown = group.rows.iter().take(6).enumerate().map(|(k, row)| method_row(env, row, &key.then(Part::Method, k)));
    let mut col = div().flex().flex_col().min_w_0().child(head).children(shown);
    if group.rows.len() > 6 {
        let rest: Vec<AnyElement> = group.rows.iter().enumerate().skip(6).map(|(k, row)| method_row(env, row, &key.then(Part::Method, k))).collect();
        let words = format!("{} more", rest.len());
        col = col.child(div().mt(env.k(2.0)).child(folded(env, &FoldKey::MoreMethods(group.verb), &key.field("more"), &words, div().flex().flex_col().children(rest).into_any_element())));
    }
    col.into_any_element()
}

/// "What you can do with it".
pub(super) fn verbs(env: &Env<'_>, groups: &[Group]) -> Option<AnyElement> {
    if groups.is_empty() {
        return None;
    }
    let i = ink(env.p);
    let key = Key::of(Part::Sec(Sec::Verbs));
    let aside = div()
        .flex()
        .items_center()
        .gap(env.k(6.0))
        .child(div().size(env.s(6.0)).rounded_full().bg(i.mint))
        .child(said(env, &key.field("aside"), "your workspace uses it", roles::ASIDE, i.ink3))
        .into_any_element();
    let two = env.m.effective() >= 900.0;
    let cells: Vec<AnyElement> = groups.iter().enumerate().map(|(n, g)| group(env, g, n)).collect();
    let body = if two {
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut cells = cells.into_iter();
        while let Some(left) = cells.next() {
            let right = cells.next();
            rows.push(div().flex().items_start().gap(env.k(28.0)).child(div().flex_1().min_w_0().child(left)).child(div().flex_1().min_w_0().children(right)).into_any_element());
        }
        div().flex().flex_col().gap(env.k(18.0)).children(rows)
    } else {
        div().flex().flex_col().gap(env.k(18.0)).children(cells)
    };
    Some(section(env, Sec::Verbs).child(head(env, Sec::Verbs, "What you can do with it", None, Some(aside))).child(body).into_any_element())
}
