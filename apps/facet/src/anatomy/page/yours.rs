//! Your code and it: the centre of the symbol page (the v6 board's
//! `yoursHtml`, ported).
//!
//! - **The reach bar** is a relation you scrub. For a type its segments are
//!   the members your code reaches through it (then the type itself);
//!   otherwise the crates that name it. Pointing at a segment gives it a
//!   fisheye share of the bar, names the member in the caption beneath, and
//!   answers everywhere else on the page through the one hover grammar: the
//!   segment's subject is `reach:<id>`, so a crate row's chip for the same
//!   member underlines, every crate that never reaches it dims, and every
//!   deck brings that member's lines to its front.
//! - **A crate row** is the crate (mint), a bar of its share, its count, the
//!   members it reaches, and its **deck**: its real lines, stacked as
//!   chamfered cards. Resting on the deck fans it in place (five lines);
//!   a click opens every line it kept.
//! - **A crate that does not name it** says what it names of its siblings.
//! - Where nothing was read the section says so: it never claims that
//!   nothing uses the symbol.
//!
//! Everything is laid out from this frame's hover target: the body is built
//! lazily, inside layout, where the window is at hand.

use super::lazy::lazy;
use super::{Doors, Geometry, said};
use crate::anatomy::reach::{CrateUse, Instead, Line, Reach, Scope, Segment, SegmentKind};
use crate::hover::{self, Lit, Subject};
use crate::measure::{Measure, Set};
use crate::paint::geom::{Poly, fill_poly};
use crate::probe::{self, TextOverflow};
use crate::tokens::{Palette, rhythm, scale};
use gpui::{
    AnyElement, App, Bounds, ColorExt, Element, ElementId, Global, GlobalElementId, HighlightStyle,
    Hsla, InspectorElementId, InteractiveElement, IntoElement, LayoutId, ParentElement, Pixels,
    SharedString, StatefulInteractiveElement, Styled, StyledText, UnderlineStyle, Window, canvas,
    div, px,
};
use std::collections::HashSet;
use std::rc::Rc;

/// Lines a fanned deck shows.
const FAN: usize = 5;
/// Layers a stacked deck shows behind its top card.
const LAYERS: usize = 4;
/// A card's height at 100 % text.
const CARD: f32 = 26.0;

/// The decks that a click opened, by key (kept for the process: a deck
/// stays open across repaints and across the pages that come back to it).
#[derive(Default)]
struct Opened(HashSet<SharedString>);

impl Global for Opened {}

fn is_open(key: &str, cx: &App) -> bool {
    cx.try_global::<Opened>()
        .is_some_and(|opened| opened.0.contains(key))
}

fn toggle(key: &SharedString, window: &mut Window, cx: &mut App) {
    let opened = cx.default_global::<Opened>();
    if !opened.0.remove(key) {
        if opened.0.len() >= 256 {
            opened.0.clear();
        }
        opened.0.insert(key.clone());
    }
    window.refresh();
}

/// The subject a reach segment (and everything that shares its member)
/// lights under.
#[must_use]
pub fn scrub_subject(id: &str) -> Subject {
    Subject::new(format!("reach:{id}"))
}

/// The id a scrubbed subject names, when it is a reach subject.
fn scrubbed(subject: &Subject) -> Option<String> {
    subject.0.strip_prefix("reach:").map(ToOwned::to_owned)
}

// ------------------------------------------------------------------ the section

/// The body of Your code and it, when there is anything to say (`itself` is
/// the symbol's own name, for the bar's last segment).
#[must_use]
pub fn yours(
    reach: &Reach,
    itself: &str,
    geo: &Geometry,
    m: &Measure,
    palette: &Palette,
    doors: &dyn Doors,
) -> Option<AnyElement> {
    // The section is only asked for when the plan has it (a usable kind):
    // a reach nobody read says `Unknown`, it is never silent.
    // What the page says, recorded now: the body itself is built in layout.
    for used in reach.yours.iter().chain(&reach.others) {
        doors.say(&used.name);
        doors.say(&used.count.to_string());
        for (member, n) in &used.members {
            doors.say(member);
            doors.say(&n.to_string());
        }
    }
    let fold = doors.fold("page-yours-crates");
    let (reach, itself, geo, m, palette) = (
        Rc::new(reach.clone()),
        itself.to_owned(),
        *geo,
        *m,
        *palette,
    );
    Some(
        lazy(move |window, cx| {
            let active = hover::hovered(window, cx).as_ref().and_then(scrubbed);
            body(
                &reach,
                &itself,
                active.as_deref(),
                fold,
                &geo,
                &m,
                &palette,
                window,
                cx,
            )
        })
        .into_any_element(),
    )
}

#[allow(clippy::too_many_arguments)]
fn body(
    reach: &Reach,
    itself: &str,
    active: Option<&str>,
    fold: Option<super::Fold>,
    geo: &Geometry,
    m: &Measure,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let s = geo.scale;
    let mint = palette.mint.base.hsla();
    let mut body = div()
        .flex()
        .flex_col()
        .gap(px(rhythm::GROUP * s))
        .w(geo.col_w);
    if !reach.read() {
        return body.child(said(
            "page-yours-unknown",
            "Unknown: no use of it by your code has been read yet. The page never says that nothing uses it.",
            scale::BODY,
            palette.ink3,
            m,
        )).into_any_element();
    }
    let segments = reach.segments(itself);
    if reach.yours.is_empty() {
        body = body.child(said(
            "page-yours-none",
            none_words(reach, itself),
            scale::BODY,
            palette.ink2,
            m,
        ));
    } else {
        body = body.child(caption(reach, itself, &segments, active, geo, m, palette));
        body = body.child(bar(&segments, active, geo, m, palette, mint));
        let name_w = name_column(&reach.yours, s);
        let most = reach.yours.iter().map(|used| used.count).max().unwrap_or(1);
        let mut rows = div().flex().flex_col();
        // A group of more than eight folds to seven (the page's one rule);
        // a page that cannot unroll it shows every crate.
        let folded =
            fold.as_ref().is_some_and(|fold| !fold.open) && super::folds(reach.yours.len());
        let shown = if folded {
            super::FOLD_AT
        } else {
            reach.yours.len()
        };
        for (n, used) in reach.yours.iter().take(shown).enumerate() {
            rows = rows.child(crate_row(
                &format!("page-yours-{n}"),
                used,
                most,
                segments
                    .first()
                    .is_some_and(|segment| segment.kind != SegmentKind::Crate),
                itself,
                active,
                true,
                name_w,
                geo,
                m,
                palette,
                window,
                cx,
            ));
        }
        if let (true, Some(fold)) = (folded, &fold) {
            let toggle = Rc::clone(&fold.toggle);
            let words = format!("and {} more crates", reach.yours.len() - super::FOLD_AT);
            rows = rows.child(
                div()
                    .id("page-yours-crates-more")
                    .cursor_pointer()
                    .h(px(rhythm::ROW_PITCH * s))
                    .flex()
                    .items_center()
                    .border_t_1()
                    .border_color(palette.line1.hsla())
                    .on_click(move |_, window, cx| toggle(window, cx))
                    .child(said(
                        "page-yours-crates-more-words",
                        words,
                        scale::LABEL,
                        palette.ink2,
                        m,
                    )),
            );
        }
        body = body.child(rows);
    }
    for (n, instead) in reach.instead.iter().take(3).enumerate() {
        body = body.child(instead_row(n, instead, m, palette));
    }
    let with_lines: Vec<&CrateUse> = reach
        .others
        .iter()
        .filter(|used| !used.lines.is_empty())
        .take(3)
        .collect();
    let bare: Vec<&CrateUse> = reach
        .others
        .iter()
        .filter(|used| used.lines.is_empty())
        .collect();
    if !with_lines.is_empty() || !bare.is_empty() {
        let name_w = name_column(
            &with_lines
                .iter()
                .map(|used| (*used).clone())
                .collect::<Vec<_>>(),
            s,
        );
        let label = if reach.yours.is_empty() {
            "Who uses it instead"
        } else {
            "Other packages that use it"
        };
        let most_other = with_lines.iter().map(|used| used.count).max().unwrap_or(1);
        let mut others = div().flex().flex_col().gap(px(4.0 * s)).child(said(
            "page-yours-others",
            label,
            scale::LABEL,
            palette.ink3,
            m,
        ));
        for (n, used) in with_lines.iter().enumerate() {
            others = others.child(crate_row(
                &format!("page-yours-other-{n}"),
                used,
                most_other,
                true,
                itself,
                None,
                false,
                name_w,
                geo,
                m,
                palette,
                window,
                cx,
            ));
        }
        if !bare.is_empty() {
            let names = bare
                .iter()
                .take(6)
                .map(|used| used.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let more = if bare.len() > 6 {
                format!(" and {} more", bare.len() - 6)
            } else {
                String::new()
            };
            others = others.child(said(
                "page-yours-others-bare",
                format!("{names}{more} name it too"),
                scale::LABEL,
                palette.ink3,
                m,
            ));
        }
        body = body.child(others);
    }
    if let Some(note) = &reach.note {
        body = body.child(said(
            "page-yours-note",
            note.clone(),
            scale::LABEL,
            palette.ink3,
            m,
        ));
    }
    let _ = mint;
    body.into_any_element()
}

/// The crate-name column: as wide as the longest name, between 132 and 200 px.
fn name_column(crates: &[CrateUse], s: f32) -> Pixels {
    let longest = crates
        .iter()
        .map(|used| used.name.chars().count())
        .max()
        .unwrap_or(0) as f32;
    px((longest * scale::MONO_NAME.size * 0.6 * s + 32.0 * s).clamp(132.0 * s, 200.0 * s))
}

fn none_words(reach: &Reach, itself: &str) -> String {
    if reach.instead.is_empty() {
        format!(
            "None of your crates name {itself}, and none name anything of its package directly."
        )
    } else {
        format!("None of your crates name {itself}.")
    }
}

// ------------------------------------------------------------------ the reach bar

/// The caption under the bar: what the scrubbed segment is, and which of
/// your crates reach it; at rest, what the bar is.
fn caption(
    reach: &Reach,
    itself: &str,
    segments: &[Segment],
    active: Option<&str>,
    geo: &Geometry,
    m: &Measure,
    palette: &Palette,
) -> AnyElement {
    let s = geo.scale;
    let words = match active.and_then(|id| segments.iter().find(|segment| segment.id == id)) {
        Some(segment) => {
            let mut parts = vec![segment.label.clone()];
            if segment.kind == SegmentKind::Member {
                if let Some(reached) = reach.reached_member(&segment.id) {
                    parts.push(effect_words(reached.effect).to_owned());
                    if let Some(gives) = &reached.gives {
                        parts.push(format!("gives {gives}"));
                    }
                }
                let crates = reach
                    .yours
                    .iter()
                    .filter_map(|used| {
                        used.reaches(&segment.id)
                            .map(|n| format!("{} {n}", used.name))
                    })
                    .collect::<Vec<_>>();
                if !crates.is_empty() {
                    parts.push(crates.join("  "));
                }
            } else {
                parts.push(format!(
                    "{} {}",
                    segment.count,
                    if segment.count == 1 {
                        "place"
                    } else {
                        "places"
                    }
                ));
            }
            parts.join(" · ")
        }
        None => match segments.first().map(|segment| segment.kind) {
            Some(SegmentKind::Crate) => format!("Which of your crates name {itself}"),
            _ => format!("What your code reaches through {itself}"),
        },
    };
    let lit = active.is_some();
    div()
        .h(px(18.0 * s))
        .flex()
        .items_center()
        .gap(px(10.0 * s))
        .child(said(
            "page-yours-caption",
            words,
            scale::LABEL,
            if lit { palette.ink1 } else { palette.ink3 },
            m,
        ))
        .child(said(
            "page-yours-scrub",
            "scrub",
            scale::LABEL_MONO,
            palette.ink3,
            m,
        ))
        .into_any_element()
}

fn effect_words(effect: crate::anatomy::plan::Effect) -> &'static str {
    use crate::anatomy::plan::Effect;
    match effect {
        Effect::Reads => "reads it",
        Effect::Changes => "changes it",
        Effect::UsesUp => "uses it up",
        Effect::Makes => "makes one",
        Effect::None => "a member",
    }
}

/// Segment widths: proportional to counts with a floor, the pointed-at
/// segment given a fisheye share and the others giving way.
fn widths(counts: &[u32], room: f32, gap: f32, floor: f32, focus: Option<usize>) -> Vec<f32> {
    let n = counts.len();
    if n == 0 {
        return Vec::new();
    }
    let room = (room - gap * (n as f32 - 1.0)).max(floor * n as f32);
    let total = counts.iter().map(|c| f64::from(*c)).sum::<f64>().max(1.0);
    let mut out: Vec<f32> = counts
        .iter()
        .map(|c| (f64::from(*c) / total) as f32 * room)
        .collect();
    if let Some(at) = focus.filter(|at| *at < n) {
        // The focused segment takes at least a third of the bar.
        let want = out[at].max(room * 0.34);
        let rest: f32 = out
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != at)
            .map(|(_, w)| *w)
            .sum();
        let left = (room - want).max(0.0);
        let k = if rest > 0.0 { left / rest } else { 0.0 };
        for (i, w) in out.iter_mut().enumerate() {
            *w = if i == at { want } else { *w * k };
        }
    }
    // The floor: what is below it is raised, and the rest give the room.
    let mut fixed = 0.0;
    let mut free = 0.0;
    for w in &out {
        if *w < floor {
            fixed += floor;
        } else {
            free += *w;
        }
    }
    let k = if free > 0.0 {
        ((room - fixed) / free).min(1.0)
    } else {
        1.0
    };
    out.iter()
        .map(|w| if *w < floor { floor } else { *w * k })
        .collect()
}

fn bar(
    segments: &[Segment],
    active: Option<&str>,
    geo: &Geometry,
    m: &Measure,
    palette: &Palette,
    mint: Hsla,
) -> AnyElement {
    let s = geo.scale;
    let counts: Vec<u32> = segments.iter().map(|segment| segment.count).collect();
    let focus = active.and_then(|id| segments.iter().position(|segment| segment.id == id));
    let sizes = widths(&counts, f32::from(geo.col_w), 2.0 * s, 24.0 * s, focus);
    let mut row = div().flex().gap(px(2.0 * s)).h(px(30.0 * s)).w(geo.col_w);
    for (n, (segment, w)) in segments.iter().zip(sizes).enumerate() {
        let lit_now = active == Some(segment.id.as_str());
        let (fill, ring) = match segment.kind {
            SegmentKind::Itself => (palette.plate.hsla(), palette.line2.hsla()),
            _ => (
                mint.opacity(if lit_now { 0.30 } else { 0.13 }),
                mint.opacity(if lit_now { 0.9 } else { 0.4 }),
            ),
        };
        let (label, count, ink, muted) = (
            segment.label.clone(),
            segment.count.to_string(),
            palette.ink1,
            palette.ink3,
        );
        let (mm, id) = (*m, segment.id.clone());
        let key = format!("page-yours-seg-{n}");
        // What a segment can say: its member and count, its member, or its count.
        let glyphs = |chars: usize| chars as f32 * scale::LABEL_MONO.size * 0.6 * s;
        let both = w
            >= glyphs(segment.label.chars().count() + 1 + segment.count.to_string().len())
                + 24.0 * s;
        let count_only = w >= glyphs(segment.count.to_string().len()) + 16.0 * s;
        let wide = both;
        let cell = hover::hoverable(
            ElementId::Name(SharedString::from(key.clone())),
            scrub_subject(&segment.id),
            mint,
            move |_| {
                let mut cell = div()
                    .w(px(w))
                    .h(px(30.0 * s))
                    .flex()
                    .items_center()
                    .gap(px(6.0 * s))
                    .px(px(8.0 * s))
                    .overflow_hidden();
                if wide {
                    cell = cell.child(said(
                        format!("{key}-label"),
                        label,
                        scale::LABEL_MONO,
                        ink,
                        &mm,
                    ));
                }
                if wide || count_only {
                    cell = cell.child(said(
                        format!("{key}-count"),
                        count,
                        scale::LABEL_MONO,
                        muted,
                        &mm,
                    ));
                }
                cell.into_any_element()
            },
        )
        .shape(hover::Shape::Chamfer(5.0 * s));
        let _ = id;
        row = row.child(
            div()
                .w(px(w))
                .h(px(30.0 * s))
                .flex_none()
                .relative()
                .child(plate(fill, ring, 5.0 * s))
                .child(cell),
        );
    }
    row.into_any_element()
}

/// A chamfered plate behind its parent's content: fill and a 1 px ring.
fn plate(fill: Hsla, ring: Hsla, chamfer: f32) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            let (x, y) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
            let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
            let poly = Poly::chamfer(x + 0.5, y + 0.5, w - 1.0, h - 1.0, chamfer);
            fill_poly(window, &poly, fill);
            for edge in poly.stroke_ring(1.0) {
                fill_poly(window, &edge, ring);
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
    .into_any_element()
}

// ------------------------------------------------------------------ a crate

#[allow(clippy::too_many_arguments)]
fn crate_row(
    key: &str,
    used: &CrateUse,
    most: u32,
    by_members: bool,
    itself: &str,
    active: Option<&str>,
    yours: bool,
    name_w: Pixels,
    geo: &Geometry,
    m: &Measure,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let s = geo.scale;
    let mint = palette.mint.base.hsla();
    let tone = if yours {
        palette.mint.base
    } else {
        palette.ink1
    };
    // A scrubbed member the crate never reaches dims it.
    // (On a bar of crates the scrubbed id is a crate's name.)
    let dim = active.is_some_and(|id| {
        let by_crate = !by_members;
        yours
            && if by_crate {
                used.name != id
            } else {
                used.reaches(id).is_none()
                    && !(id.is_empty()
                        && used.members.iter().map(|(_, n)| n).sum::<u32>() < used.count)
            }
    });
    let mut head = div()
        .flex()
        .items_center()
        .gap(px(12.0 * s))
        .h(px(rhythm::ROW_PITCH * s));
    head = head.child(
        div()
            .w(name_w)
            .flex_none()
            .overflow_hidden()
            .flex()
            .items_center()
            .gap(px(7.0 * s))
            .child(
                crate::icons::ui(
                    crate::icons::Icon::Diamond,
                    crate::icons::IconSize::S12,
                    tone,
                )
                .size(m.icon(11.0)),
            )
            .child(said(
                format!("{key}-name"),
                used.name.clone(),
                scale::MONO_NAME,
                tone,
                m,
            )),
    );
    // Its share, against the crate that names it most.
    let share = used.count as f32 / most.max(1) as f32;
    let bar_w = 88.0 * s;
    head = head.child(
        div()
            .w(px(bar_w))
            .h(px(4.0 * s))
            .flex_none()
            .bg(palette.line1.hsla())
            .child(
                div()
                    .w(px((bar_w * share).max(2.0)))
                    .h(px(4.0 * s))
                    .bg(if yours { mint } else { palette.ink3.hsla() }),
            ),
    );
    head = head.child(div().w(px(28.0 * s)).flex_none().child(said(
        format!("{key}-count"),
        used.count.to_string(),
        scale::MONO_NAME,
        palette.ink0,
        m,
    )));
    let mut chips = div()
        .flex()
        .items_center()
        .gap(px(12.0 * s))
        .overflow_hidden();
    // As many as fit beside the count, each its name and count.
    let room = f32::from(geo.col_w - name_w) - 88.0 * s - 28.0 * s - 40.0 * s;
    let mut used_w = 0.0;
    let fitting: Vec<&(String, u32)> = used
        .members
        .iter()
        .take(4)
        .take_while(|(member, n)| {
            let need = (member.chars().count() + n.to_string().len() + 1) as f32
                * scale::LABEL_MONO.size
                * 0.6
                * s
                + 12.0 * s;
            used_w += need;
            used_w <= room
        })
        .collect();
    for (q, (member, n)) in fitting.into_iter().enumerate() {
        let (mm, pal, name, count, id) =
            (*m, *palette, member.clone(), n.to_string(), member.clone());
        let chip_key = format!("{key}-member-{q}");
        chips = chips.child(hover::hoverable(
            ElementId::Name(SharedString::from(chip_key.clone())),
            scrub_subject(&id),
            mint,
            move |lit| {
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(4.0 * s))
                    .child(said(
                        format!("{chip_key}-name"),
                        name,
                        scale::LABEL_MONO,
                        hover::ink(pal.ink2, lit, &pal),
                        &mm,
                    ))
                    .child(said(
                        format!("{chip_key}-count"),
                        count,
                        scale::LABEL_MONO,
                        pal.ink3,
                        &mm,
                    ))
                    .into_any_element()
            },
        ));
    }
    head = head.child(chips);
    let lines_w = geo.col_w - name_w - px(12.0 * s);
    let deck = deck(key, used, itself, active, lines_w, m, palette, window, cx);
    let mut row = div()
        .flex()
        .flex_col()
        .gap(px(4.0 * s))
        .py(px(8.0 * s))
        .border_t_1()
        .border_color(palette.line1.hsla())
        .child(head)
        .child(
            div()
                .flex()
                .child(div().w(name_w + px(12.0 * s)).flex_none())
                .child(deck),
        );
    if dim {
        row = row.opacity(0.4);
    }
    row.into_any_element()
}

fn instead_row(n: usize, instead: &Instead, m: &Measure, palette: &Palette) -> AnyElement {
    let key = format!("page-yours-instead-{n}");
    let from = match instead.scope {
        Scope::Module => "from this module",
        Scope::Package => "from this package",
    };
    div()
        .flex()
        .items_baseline()
        .gap(px(6.0 * m.scale()))
        .flex_wrap()
        .child(said(
            format!("{key}-name"),
            instead.name.clone(),
            scale::MONO_NAME,
            palette.mint.base,
            m,
        ))
        .child(said(
            format!("{key}-uses"),
            format!("uses {}", instead.uses.join(", ")),
            scale::LABEL,
            palette.ink2,
            m,
        ))
        .child(said(
            format!("{key}-from"),
            format!("{from}, not this"),
            scale::LABEL,
            palette.ink3,
            m,
        ))
        .into_any_element()
}

// ------------------------------------------------------------------ a deck

/// Where a line's place reads: `manifest.rs:33`, with its folder when that
/// keeps it under 24 characters (`local_package/manifest.rs:33`); a plain
/// `src/` is never worth saying.
fn place(line: &Line) -> String {
    place_fit(line, usize::MAX)
}

/// [`place`], its file name cut from the front (`…source_journey.rs:174`)
/// when it is longer than `max` characters.
fn place_fit(line: &Line, max: usize) -> String {
    let parts: Vec<&str> = line.file.split('/').collect();
    let Some(file) = parts.last() else {
        return format!(":{}", line.line);
    };
    let tail = format!("{file}:{}", line.line);
    let full = match parts.len().checked_sub(2).map(|at| parts[at]) {
        Some(folder) if folder != "src" && folder.len() + 1 + tail.len() <= 24 => {
            format!("{folder}/{tail}")
        }
        _ => tail,
    };
    let chars: Vec<char> = full.chars().collect();
    if chars.len() <= max {
        return full;
    }
    let keep = max.saturating_sub(1).max(4);
    format!(
        "…{}",
        chars[chars.len() - keep..].iter().collect::<String>()
    )
}

/// The line cropped to `fits` characters around the name, with the name's
/// range in the cropped text (bytes).
fn crop(line: &Line, fits: usize) -> (String, Option<(usize, usize)>) {
    let text = line.text.trim();
    let lead = line.text.len() - line.text.trim_start().len();
    let mark = line
        .mark
        .map(|(a, b)| (a as usize, b as usize))
        .and_then(|(a, b)| a.checked_sub(lead).zip(b.checked_sub(lead)));
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    if chars.len() <= fits {
        return (text.to_owned(), mark);
    }
    let centre = mark.map_or(0, |(a, b)| {
        chars
            .iter()
            .position(|(at, _)| *at >= (a + b) / 2)
            .unwrap_or(0)
    });
    let keep = fits.saturating_sub(2).max(8);
    let start = centre
        .saturating_sub(keep / 2)
        .min(chars.len() - keep.min(chars.len()));
    let end = (start + keep).min(chars.len());
    let from = chars[start].0;
    let to = chars.get(end).map_or(text.len(), |(at, _)| *at);
    let prefix = if start > 0 { "…" } else { "" };
    let suffix = if end < chars.len() { "…" } else { "" };
    let body = &text[from..to];
    let shifted = mark.and_then(|(a, b)| {
        (a >= from && b <= to).then(|| (a - from + prefix.len(), b - from + prefix.len()))
    });
    (format!("{prefix}{body}{suffix}"), shifted)
}

/// A crate's deck of lines: stacked at rest, fanned while pointed at, open
/// after a click.
#[allow(clippy::too_many_arguments)]
fn deck(
    key: &str,
    used: &CrateUse,
    itself: &str,
    active: Option<&str>,
    width: Pixels,
    m: &Measure,
    palette: &Palette,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let s = m.scale();
    if used.lines.is_empty() {
        return said(
            format!("{key}-nolines"),
            format!(
                "named {} {}; no line kept",
                used.count,
                if used.count == 1 { "time" } else { "times" }
            ),
            scale::LABEL,
            palette.ink3,
            m,
        );
    }
    let deck_key = SharedString::from(format!("{key}-deck"));
    // Whether it is open is the symbol's and the crate's, not the row's position.
    let open_key = SharedString::from(format!("{itself}:{}:{}", used.name, key));
    let open = is_open(&open_key, cx);
    // The scrubbed member's lines come to the front, in their own order.
    let mut lines: Vec<&Line> = used.lines.iter().collect();
    if let Some(id) = active {
        let reaches = |line: &&Line| line.member.as_deref().unwrap_or("") == id;
        if used.reaches(id).is_some() || id.is_empty() {
            lines.sort_by_key(|line| !reaches(line));
        }
    }
    let matches = |line: &Line| active.is_none_or(|id| line.member.as_deref().unwrap_or("") == id);
    let (mm, pal, itself_owned, lines_owned, unkept, key_owned) = (
        *m,
        *palette,
        itself.to_owned(),
        lines.iter().map(|line| (*line).clone()).collect::<Vec<_>>(),
        used.unkept(),
        key.to_owned(),
    );
    let matching: Vec<bool> = lines.iter().map(|line| matches(line)).collect();
    // The places column is as wide as the longest place, up to a third.
    let longest = used
        .lines
        .iter()
        .map(|line| place(line).chars().count())
        .max()
        .unwrap_or(0)
        .min(26);
    let place_w = px(
        (longest as f32 * scale::LABEL_MONO.size * 0.6 * s + 4.0 * s).clamp(96.0 * s, 200.0 * s),
    );
    let place_chars = longest.max(12);
    let toggle_key = open_key;
    let hoverable = hover::hoverable(
        ElementId::Name(deck_key.clone()),
        Subject::new(format!("deck:{deck_key}")),
        palette.mint.base.hsla(),
        move |lit| {
            let fanned = lit == Lit::Target;
            let shown = if open {
                lines_owned.len()
            } else if fanned {
                lines_owned.len().min(FAN)
            } else {
                1
            };
            let n_layers = if open || fanned {
                0
            } else {
                (lines_owned.len() - 1 + usize::from(unkept > 0)).min(LAYERS)
            };
            let peek = px(4.0 * s) * n_layers as f32;
            let card_w = width - peek;
            let mut column = div()
                .relative()
                .w(width)
                .flex()
                .flex_col()
                .gap(px(4.0 * s))
                .pb(peek);
            if n_layers > 0 {
                // Behind: the layers peek out below and to the right.
                for layer in (1..=n_layers).rev() {
                    let off = px(4.0 * s) * layer as f32;
                    column = column.child(
                        div()
                            .absolute()
                            .top(off)
                            .left(off)
                            .w(card_w)
                            .h(px(CARD * s))
                            .child(plate(
                                pal.plate.hsla(),
                                pal.line2.hsla().opacity(0.8 - 0.12 * layer as f32),
                                5.0 * s,
                            )),
                    );
                }
            }
            for (i, line) in lines_owned.iter().take(shown).enumerate() {
                column = column.child(card(
                    &format!("{key_owned}-line-{i}"),
                    line,
                    &itself_owned,
                    if n_layers > 0 { card_w } else { width },
                    place_w,
                    place_chars,
                    matching[i],
                    &mm,
                    &pal,
                ));
            }
            if open || fanned {
                let more = if open {
                    unkept
                } else {
                    u32::try_from(lines_owned.len().saturating_sub(FAN)).unwrap_or(0) + unkept
                };
                if more > 0 {
                    let words = if open {
                        format!("and {more} more counted, lines not kept")
                    } else {
                        format!("and {more} more: click to open")
                    };
                    column = column.child(said(
                        format!("{key_owned}-more"),
                        words,
                        scale::LABEL,
                        pal.ink3,
                        &mm,
                    ));
                }
            } else if unkept > 0 && lines_owned.len() == 1 {
                // A single kept line with more counted: the stack says so.
            }
            column.into_any_element()
        },
    );
    div()
        .id(ElementId::Name(SharedString::from(format!("{key}-hit"))))
        .cursor_pointer()
        .on_click(move |_, window, cx| toggle(&toggle_key, window, cx))
        .child(hoverable)
        .into_any_element()
}

/// One line card: a chamfered plate holding where it is and the line itself,
/// cropped around the name, the name underlined in mint.
fn card(
    key: &str,
    line: &Line,
    itself: &str,
    width: Pixels,
    place_w: Pixels,
    place_chars: usize,
    lit: bool,
    m: &Measure,
    palette: &Palette,
) -> AnyElement {
    let s = m.scale();
    let _ = itself;
    let fits = (((f32::from(width - place_w) - 38.0 * s) / (scale::LABEL_MONO.size * 0.6 * s))
        .floor() as usize)
        .max(10);
    let (text, mark) = crop(line, fits);
    let mut highlights = Vec::new();
    if let Some((a, b)) = mark.filter(|(a, b)| a < b && *b <= text.len()) {
        highlights.push((
            a..b,
            HighlightStyle {
                color: Some(palette.ink0.hsla()),
                underline: Some(UnderlineStyle {
                    thickness: px(1.5),
                    color: Some(palette.mint.base.hsla()),
                    wavy: false,
                }),
                ..HighlightStyle::default()
            },
        ));
    }
    let shared = SharedString::from(text.clone());
    let ink = if lit { palette.ink1 } else { palette.ink3 };
    let code = probe::text(
        ElementId::Name(SharedString::from(format!("{key}-code"))),
        shared.clone(),
        m.role(scale::LABEL_MONO),
        1.0,
        TextOverflow::Clip,
        div()
            .set(scale::LABEL_MONO, m)
            .text_color(ink.hsla())
            .whitespace_nowrap()
            .child(StyledText::new(shared).with_highlights(highlights)),
    );
    let ring = if lit {
        palette.line3.hsla()
    } else {
        palette.line1.hsla()
    };
    div()
        .relative()
        .w(width)
        .h(px(CARD * s))
        .flex_none()
        .child(plate(palette.plate.hsla(), ring, 5.0 * s))
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .gap(px(10.0 * s))
                .px(px(9.0 * s))
                .overflow_hidden()
                .child(div().w(place_w).flex_none().overflow_hidden().child(said(
                    format!("{key}-place"),
                    place_fit(line, place_chars),
                    scale::LABEL_MONO,
                    palette.ink3,
                    m,
                )))
                .child(div().flex_1().min_w(px(0.0)).overflow_hidden().child(code)),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scrubbed_segment_takes_a_third_of_the_bar_and_the_rest_give_way() {
        let counts = [26, 17, 6, 6, 6, 3, 38];
        let rest = widths(&counts, 640.0, 2.0, 24.0, None);
        let focus = widths(&counts, 640.0, 2.0, 24.0, Some(5));
        assert!(
            focus[5] > rest[5] * 3.0,
            "the pointed-at segment grows: {} vs {}",
            focus[5],
            rest[5]
        );
        assert!(focus[6] < rest[6], "its neighbours give way");
        assert!(
            focus.iter().all(|w| *w >= 24.0 - 0.01),
            "no segment falls under the floor"
        );
        let used: f32 = focus.iter().sum::<f32>() + 2.0 * 6.0;
        assert!(
            (used - 640.0).abs() < 8.0,
            "the bar keeps its length: {used}"
        );
    }

    #[test]
    fn a_long_line_is_cropped_around_its_name() {
        let line = Line {
            file: "src/harness.rs".to_owned(),
            line: 607,
            text: "let parsed: toml::Value = toml::from_str(&bytes).map_err(|error| format!(\"read {error}\"))?;".to_owned(),
            mark: Some((18, 23)),
            ..Line::default()
        };
        let (text, mark) = crop(&line, 32);
        let (a, b) = mark.expect("the name stays in view");
        assert_eq!(&text[a..b], "Value");
        assert!(text.chars().count() <= 34);
    }

    #[test]
    fn a_place_keeps_the_last_two_parts_of_the_path() {
        let long = Line {
            file: "src/model/local_package/manifest.rs".to_owned(),
            line: 33,
            ..Line::default()
        };
        assert_eq!(
            place(&long),
            "manifest.rs:33",
            "a folder that would not fit is dropped"
        );
        let short = Line {
            file: "src/harness.rs".to_owned(),
            line: 607,
            ..Line::default()
        };
        assert_eq!(place(&short), "harness.rs:607", "src is never said");
        let near = Line {
            file: "src/builtin/browse.rs".to_owned(),
            line: 183,
            ..Line::default()
        };
        assert_eq!(place(&near), "builtin/browse.rs:183");
    }
}
