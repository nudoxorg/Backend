//! The socket: a contract drawn as a chamfered outline whose left edge is
//! the spine. What an implementor must write is a notch cut into that edge
//! (dotted when it may be left out); what it gets for free is a tab on the
//! right edge; members the producer said neither of are plain rows; fields
//! it holds sit inside below. Who does it plugs in from the left margin,
//! its count on the plug. One row per member, 24 px, its doc in the peek.
//! The strokes are [`super::ink`]'s.

use super::{
    Doors, FOLD_AT, Geometry, anchor, at, folds, hue, mono_w, named, said, ty_lit, words_w,
};
use crate::anatomy::page::Anchors;
use crate::anatomy::plan::{Contract, Fam, Part, SectionId, Slot};
use crate::hover;
use crate::hover::Lit;
use crate::measure::Measure;
use crate::tokens::{Palette, rhythm, scale};
use gpui::{AnyElement, IntoElement, ParentElement, Pixels, Styled, div, px};
use std::rc::Rc;

/// The words on the plug: "486 types do it · 17 yours", or for Go
/// "41 types satisfy it · computed".
fn plug_words(contract: &Contract) -> Option<(String, Option<String>)> {
    let doers = &contract.doers;
    if doers.count == 0 {
        return None;
    }
    let what = if doers.count == 1 { "type" } else { "types" };
    let verb = if doers.computed {
        if doers.count == 1 {
            "satisfies it"
        } else {
            "satisfy it"
        }
    } else if doers.count == 1 {
        "does it"
    } else {
        "do it"
    };
    let mut words = format!("{} {what} {verb}", doers.count);
    if doers.computed {
        words.push_str(" · computed");
    }
    Some((
        words,
        (doers.yours > 0).then(|| format!("{} yours", doers.yours)),
    ))
}

/// The socket for `contract`.
pub(super) fn socket(
    contract: &Contract,
    fam: Fam,
    geo: &Geometry,
    anchors: &Rc<Anchors>,
    m: &Measure,
    palette: &Palette,
    doors: &dyn Doors,
) -> AnyElement {
    let s = geo.scale;
    let pitch = px(rhythm::ROW_PITCH * s);
    let kind = hue(fam, palette).hsla();
    let xray = m.reveal().xray;

    // The count line: what an implementor writes, what it gets.
    let mut count = div()
        .flex()
        .items_baseline()
        .gap(px(5.0 * s))
        .h(px(16.0 * s))
        .mb(px(8.0 * s));
    let mut parts = Vec::new();
    if !contract.write.is_empty() {
        parts.push(("you write", contract.write.len()));
    }
    if !contract.get.is_empty() {
        parts.push(("you get", contract.get.len()));
    }
    if !contract.held.is_empty() {
        parts.push(("it holds", contract.held.len()));
    }
    for (n, (words, k)) in parts.iter().enumerate() {
        if n > 0 {
            count = count.child(said(
                format!("page-count-dot-{n}"),
                "·",
                scale::LABEL,
                palette.ink3,
                m,
            ));
        }
        doors.say(&format!("{words} {k}"));
        count = count
            .child(said(
                format!("page-count-{n}-words"),
                *words,
                scale::LABEL,
                palette.ink3,
                m,
            ))
            .child(said(
                format!("page-count-{n}"),
                k.to_string(),
                scale::LABEL,
                palette.ink2,
                m,
            ));
    }
    let mut body = div().flex().flex_col().relative().child(anchor(
        at(SectionId::Spec, Part::Count),
        anchors,
        count,
    ));

    // The rows: notches, plain, tabs, then what it holds.
    let all = contract
        .write
        .iter()
        .map(|slot| (slot, 0_u8))
        .chain(contract.other.iter().map(|slot| (slot, 1)))
        .chain(contract.get.iter().map(|slot| (slot, 2)))
        .collect::<Vec<_>>();
    let name_w = all
        .iter()
        .map(|(slot, _)| {
            mono_w(
                slot.name.chars().count() + usize::from(slot.optional),
                scale::MONO_NAME,
                s,
            )
        })
        .fold(px(0.0), Pixels::max)
        + px(24.0 * s);
    let gives_w = all
        .iter()
        .map(|(slot, _)| {
            slot.gives.as_ref().map_or(px(0.0), |ty| {
                words_w(ty.plain().chars().count(), scale::BODY, s) + px(15.0 * s)
            })
        })
        .fold(px(0.0), Pixels::max);
    let inner = (name_w + gives_w + px(28.0 * s))
        .max(px(220.0 * s))
        .min(geo.col_w - px(24.0 * s));
    let shown = if folds(all.len()) { FOLD_AT } else { all.len() };
    let mut rows = div().flex().flex_col().w(inner);
    for (n, (slot, group)) in all.iter().take(shown).enumerate() {
        rows = rows.child(anchor(
            at(SectionId::Spec, Part::Row(n as u16)),
            anchors,
            slot_row(n, slot, *group, name_w, kind, m, palette, doors, xray),
        ));
    }
    if all.len() > shown {
        let words = format!("and {} more", all.len() - shown);
        doors.say(&words);
        rows = rows.child(div().h(pitch).flex().items_center().child(said(
            "page-slots-more",
            words,
            scale::LABEL,
            palette.ink3,
            m,
        )));
    }
    for (n, rung) in contract.held.iter().enumerate() {
        doors.say(&rung.name);
        let row = div()
            .h(pitch)
            .flex()
            .items_center()
            .child(div().w(name_w).flex_none().child(said(
                format!("page-held-{n}-name"),
                format!("{}{}", rung.name, if rung.optional { "?" } else { "" }),
                scale::MONO,
                palette.ink1,
                m,
            )))
            .child(ty_lit(
                &format!("page-held-{n}-type"),
                &rung.ty,
                m,
                palette,
                xray,
                Lit::Rest,
            ));
        rows = rows.child(anchor(
            at(SectionId::Spec, Part::Shared(n as u16)),
            anchors,
            row,
        ));
    }
    body = body.child(div().pl(px(14.0 * s)).child(anchor(
        at(SectionId::Spec, Part::Plate),
        anchors,
        rows,
    )));

    // Who does it: in the left margin, plugged into the socket, when the
    // margins carry the edges; a row under it otherwise.
    if let Some((words, yours)) = plug_words(contract) {
        doors.say(&words);
        // The plug hangs in the margin only when its words and names fit
        // left of the spine; otherwise it is a row under the socket.
        let room = geo.reach - (geo.col - geo.spine) - px(34.0 * s) - px(8.0 * s);
        let head_w = mono_w(
            words.chars().count() + yours.as_ref().map_or(0, |y| y.chars().count() + 1),
            scale::LABEL_MONO,
            s,
        );
        let names_w = contract
            .doers
            .names
            .iter()
            .take(5)
            .map(|(name, ..)| mono_w(name.chars().count(), scale::MONO, s))
            .fold(px(0.0), Pixels::max);
        let in_margin = geo.margins() && head_w <= room && names_w <= room;
        let shown = contract
            .doers
            .names
            .len()
            .min(if in_margin { 5 } else { 3 });
        if in_margin {
            let mut plug = div()
                .absolute()
                .right(geo.col_w + (geo.col - geo.spine) + px(34.0 * s))
                .top(px(24.0 * s))
                .flex()
                .flex_col()
                .items_end();
            let mut head = div()
                .h(px(18.0 * s))
                .flex()
                .items_center()
                .gap(px(6.0 * s))
                .child(said(
                    "page-plug-count",
                    words,
                    scale::LABEL_MONO,
                    palette.ink3,
                    m,
                ));
            if let Some(yours) = &yours {
                doors.say(yours);
                head = head.child(said(
                    "page-plug-yours",
                    yours.clone(),
                    scale::LABEL_MONO,
                    palette.mint.base,
                    m,
                ));
            }
            plug = plug.child(head);
            for (n, (name, link, mine)) in contract.doers.names.iter().take(shown).enumerate() {
                doors.say(name);
                let (mm, pal, label, key) = (*m, *palette, name.clone(), format!("page-doer-{n}"));
                let rest = if *mine {
                    palette.mint.base
                } else {
                    palette.ink1
                };
                let el = named(
                    format!("{key}-door"),
                    name,
                    link.as_deref(),
                    kind,
                    doors,
                    move |lit| said(key, label, scale::MONO, hover::ink(rest, lit, &pal), &mm),
                );
                plug = plug.child(anchor(
                    at(SectionId::Spec, Part::Doer(n as u16)),
                    anchors,
                    div().h(px(20.0 * s)).flex().items_center().child(el),
                ));
            }
            // The plug hangs in the margin: the socket keeps room for it, so the
            // next section's stub never lands on a name.
            body = body
                .min_h(px((24.0 + 18.0 + shown as f32 * 20.0 + 10.0) * s))
                .child(plug);
        } else {
            let mut row = div()
                .mt(px(8.0 * s))
                .min_h(pitch)
                .flex()
                .flex_wrap()
                .items_center()
                .gap_x(px(10.0 * s))
                .child(said(
                    "page-plug-count",
                    words,
                    scale::LABEL_MONO,
                    palette.ink3,
                    m,
                ));
            if let Some(yours) = &yours {
                doors.say(yours);
                row = row.child(said(
                    "page-plug-yours",
                    yours.clone(),
                    scale::LABEL_MONO,
                    palette.mint.base,
                    m,
                ));
            }
            for (n, (name, link, mine)) in contract.doers.names.iter().take(shown).enumerate() {
                doors.say(name);
                let (mm, pal, label, key) = (*m, *palette, name.clone(), format!("page-doer-{n}"));
                let rest = if *mine {
                    palette.mint.base
                } else {
                    palette.ink1
                };
                row = row.child(named(
                    format!("{key}-door"),
                    name,
                    link.as_deref(),
                    kind,
                    doors,
                    move |lit| said(key, label, scale::MONO, hover::ink(rest, lit, &pal), &mm),
                ));
            }
            let more = (contract.doers.count as usize).saturating_sub(shown);
            if more > 0 {
                let words = format!("and {more} more");
                doors.say(&words);
                row = row.child(said("page-plug-more", words, scale::LABEL, palette.ink3, m));
            }
            body = body.child(row);
        }
    }
    body.into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn slot_row(
    n: usize,
    slot: &Slot,
    group: u8,
    name_w: Pixels,
    kind: gpui::Hsla,
    m: &Measure,
    palette: &Palette,
    doors: &dyn Doors,
    xray: bool,
) -> AnyElement {
    let s = m.scale();
    let key = format!("page-slot-{n}");
    doors.say(&slot.name);
    let (mm, pal, label, name_key, optional) = (
        *m,
        *palette,
        slot.name.clone(),
        format!("{key}-name"),
        slot.optional,
    );
    // What you write reads strongest; what you get, one step quieter.
    let rest = if group == 0 {
        palette.ink0
    } else {
        palette.ink1
    };
    let name = named(
        format!("{key}-door"),
        &slot.name,
        slot.link.as_deref(),
        kind,
        doors,
        move |lit| {
            said(
                name_key,
                format!("{label}{}", if optional { "?" } else { "" }),
                scale::MONO_NAME,
                hover::ink(rest, lit, &pal),
                &mm,
            )
        },
    );
    let mut row = div()
        .h(px(rhythm::ROW_PITCH * s))
        .flex()
        .items_center()
        .child(div().w(name_w).flex_none().child(name));
    if let Some(ty) = &slot.gives {
        doors.say(&ty.plain());
        row = row.child(ty_lit(
            &format!("{key}-gives"),
            ty,
            m,
            palette,
            xray,
            Lit::Rest,
        ));
    }
    if slot.fails {
        row = row.child(div().pl(px(6.0 * s)).child(said(
            format!("{key}-fails"),
            "?",
            scale::MONO_NAME,
            palette.coral.base,
            m,
        )));
    }
    row.into_any_element()
}
