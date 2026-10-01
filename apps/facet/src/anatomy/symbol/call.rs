//! The call: one rail down the left of a plate, one row per port, and at the
//! rail's end how the call can end. Rows wrap freely; each draws its own
//! rail segment, so nothing is laid out absolutely.
//!
//! Fallibility lives at the end of the rail, and it is colour + shape + word
//! everywhere: an arrow (gives), a dashed ring (or nothing), a coral block
//! (or fails), a clock (later), a double arrow (gives each).

use super::card::{Cards, Request};
use super::host::Act;
use super::ink::{G, change_marks, mark};
use super::key::{FoldKey, Key, Part, PortName, Sec, Slot};
use super::kit::{
    Ellipsis, Env, Ink, Stop, action, caps, dotted, frame, ink, joint, pill_frame, roles, said,
    said_in, truncated, wrapped,
};
use super::view::{Call, Joint, OptionRow, Port, Ty, View};
use crate::anatomy::page::named_as;
use crate::anatomy::unroll::unroll;
use crate::overlay::float;
use crate::tokens::TypeRole;
use crate::tokens::fluid::Screen;
use gpui::{AnyElement, Hsla, InteractiveElement, IntoElement, ParentElement, Styled, div};
use std::rc::Rc;

/// How a type is toned.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum Tone {
    /// An ordinary type.
    Plain,
    /// An error type.
    Error,
}

/// The violet pill of a generic parameter.
fn pill(env: &Env<'_>, i: &Ink, key: &Key, name: &str) -> AnyElement {
    pill_frame(&env.m, env.p)
        .child(said(
            env,
            &key.field(Slot::Name),
            name.to_owned(),
            roles::PILL,
            i.g0,
        ))
        .into_any_element()
}

/// `child` as the trigger of a hover card, which the keyboard opens too: it is
/// a stop in the walk, and Enter on it opens the card where it is.
fn card_trigger(
    env: &Env<'_>,
    trigger: Key,
    label: String,
    request: Request,
    child: AnyElement,
) -> AnyElement {
    let spots = env.host.spots();
    let open: Act = {
        let (request, trigger, spots) = (request.clone(), trigger.clone(), Rc::clone(&spots));
        Rc::new(move |window, cx| {
            if let Some(bounds) = spots.frame(&trigger) {
                float::open(request(&trigger, bounds), window, cx);
            }
        })
    };
    let shown = {
        let trigger = trigger.clone();
        float::trigger(trigger.id(), move |bounds| request(&trigger, bounds), child)
            .into_any_element()
    };
    // The card's trigger is a stop in the walk, at least 24 px tall.
    env.host.target(
        &trigger,
        label.into(),
        open,
        frame(
            &trigger,
            &spots,
            div()
                .min_h(env.s(24.0))
                .min_w(env.s(24.0))
                .flex()
                .items_center()
                .justify_center()
                .child(shown)
                .into_any_element(),
        ),
    )
}

/// A type: its generic pill (a hover card), its plain word (a door when it
/// names a declaration, dotted when it is read rather than declared), a mark
/// when it nests, then what is written, quieter.
pub(super) fn ty_el(env: &Env<'_>, cards: &Cards, key: &Key, ty: &Ty, tone: Tone) -> AnyElement {
    let i = ink(env.p);
    let mut row = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x(env.k(7.0))
        .gap_y(env.k(4.0))
        .min_w_0();
    if let Some(generic) = &ty.generic {
        let pill = pill(env, &i, &key.field(Slot::Pill), generic);
        let pill = match cards.generic(generic) {
            Some(request) => card_trigger(
                env,
                key.field(Slot::Pill),
                format!("{generic}: what you choose"),
                request,
                pill,
            ),
            None => pill,
        };
        row = row.child(pill);
    }
    if ty.generic.as_deref() != Some(ty.word.as_str()) && !ty.word.is_empty() {
        let colour = if tone == Tone::Error { i.coral } else { i.ink0 };
        let m = env.m;
        let word_key = key.field(Slot::Word);
        let word = ty.word.clone();
        // A door is a stop in the walk: at least 24 px tall (a pointer's
        // target), whatever its type's size.
        let height = env.s(24.0);
        let build = {
            let (word, word_key) = (word.clone(), word_key.clone());
            move |_lit| {
                div()
                    .min_h(height)
                    .flex()
                    .items_center()
                    .child(said_in(&m, &word_key, word, roles::WORD, colour))
                    .into_any_element()
            }
        };
        let mut el = named_as(
            key.field(Slot::Door).text(),
            &word,
            ty.link.as_deref(),
            colour,
            env.host,
            false,
            build,
        );
        if ty.origin.dotted() {
            el = dotted(el, i.ink3);
        }
        if tone == Tone::Error && ty.link.is_none() {
            if let Some(request) = cards.error() {
                el = card_trigger(
                    env,
                    key.field(Slot::Card),
                    format!("{word}: what it is"),
                    request,
                    el,
                );
            }
        }
        row = row.child(el);
    }
    if ty.loops {
        row = row.child(
            div()
                .flex_none()
                .child(mark(G::Loop, env.p, 14.0 * env.m.scale())),
        );
    }
    if let Some(written) = &ty.written {
        row = row.child(div().min_w_0().max_w(env.s(340.0)).child(truncated(
            env,
            &key.field(Slot::Written),
            written.clone(),
            roles::WRITTEN,
            i.ink3,
            Ellipsis::End,
        )));
    }
    row.into_any_element()
}

/// One row of the rail.
struct Row {
    mark: AnyElement,
    name: AnyElement,
    value: AnyElement,
    rail: Hsla,
    indent: bool,
}

/// A row's name cell: a column beside the value, or the line above it on a phone.
fn name_cell(env: &Env<'_>) -> gpui::Div {
    if env.lay.screen == Screen::Phone {
        div()
    } else {
        div().min_w(env.s(env.lay.label)).flex_none()
    }
}

fn label(env: &Env<'_>, key: &Key, words: &str, color: Hsla) -> AnyElement {
    name_cell(env)
        .child(said(env, key, caps(words), roles::LABEL, color))
        .into_any_element()
}

fn name(env: &Env<'_>, key: &Key, words: &str, role: TypeRole, color: Hsla) -> AnyElement {
    name_cell(env)
        .child(said(env, key, words.to_owned(), role, color))
        .into_any_element()
}

fn value(env: &Env<'_>, children: Vec<AnyElement>) -> AnyElement {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x(env.k(10.0))
        .gap_y(env.k(6.0))
        .min_w_0()
        .flex_1()
        .py(env.k(4.0))
        .children(children)
        .into_any_element()
}

/// One row of the rail: the joint, the name, the value.
fn one(env: &Env<'_>, row: Row, stop: Stop) -> AnyElement {
    let mut el = div()
        .flex()
        .items_center()
        .gap(env.k(12.0))
        .min_h(env.s(34.0))
        .child(joint(env, row.mark, stop, row.rail));
    if row.indent {
        el = el.child(
            div()
                .w(env.s(if env.lay.screen == Screen::Phone {
                    12.0
                } else {
                    28.0
                }))
                .flex_none(),
        );
    }
    if env.lay.screen == Screen::Phone {
        // A phone has no room for a label column: the name is the line above its value.
        return el
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .flex_1()
                    .py(env.k(4.0))
                    .child(row.name)
                    .child(row.value),
            )
            .into_any_element();
    }
    el.child(row.name).child(row.value).into_any_element()
}

/// A port's option rows, folded under the row at `after`.
struct Suboptions {
    after: usize,
    port: PortName,
    rows: Vec<Row>,
}

/// The rows in order, each port's option rows folded inside a keyed unroll
/// right after it.
fn build_rows(env: &Env<'_>, rows: Vec<Row>, mut subs: Vec<Suboptions>) -> Vec<AnyElement> {
    let total = rows.len();
    let mut out = Vec::new();
    for (n, row) in rows.into_iter().enumerate() {
        out.push(one(env, row, Stop::of(n, total)));
        while let Some(at) = subs.iter().position(|sub| sub.after == n) {
            let Suboptions {
                after,
                port,
                rows: options,
            } = subs.remove(at);
            let items: Vec<AnyElement> = options
                .into_iter()
                .map(|option| one(env, option, Stop::Between))
                .collect();
            let body = div().flex().flex_col().children(items);
            if let Some(fold) = env.host.unfold(&FoldKey::Options(port)) {
                out.push(
                    unroll(
                        Key::of(Part::Fold).field(Slot::Options).at(after).id(),
                        fold.open,
                        fold.presence,
                        body,
                    )
                    .into_any_element(),
                );
            }
        }
    }
    out
}

fn options_row(env: &Env<'_>, cards: &Cards, key: &Key, option: &OptionRow) -> Row {
    let i = ink(env.p);
    let mut children = vec![
        div()
            .flex_none()
            .child(ty_el(
                env,
                cards,
                &key.field(Slot::Ty),
                &option.ty,
                Tone::Plain,
            ))
            .into_any_element(),
    ];
    if !option.note.is_empty() {
        children.push(wrapped(
            env,
            &key.field(Slot::Note),
            option.note.clone(),
            roles::NOTE,
            i.ink3,
        ));
    }
    if let Some(change) = option.change {
        let (from, to) = change_marks(change);
        children.push(
            div()
                .flex()
                .items_center()
                .gap(env.k(4.0))
                .flex_none()
                .child(mark(from, env.p, 11.0 * env.m.scale()))
                .child(said(env, &key.field(Slot::Arrow), "→", roles::NOTE, i.ink3))
                .child(mark(to, env.p, 11.0 * env.m.scale()))
                .into_any_element(),
        );
    }
    Row {
        mark: div().into_any_element(),
        name: name(
            env,
            &key.field(Slot::Name),
            &option.name,
            roles::SUB,
            i.ink1,
        ),
        value: value(env, children),
        rail: i.line3,
        indent: true,
    }
}

fn port_rows(
    env: &Env<'_>,
    cards: &Cards,
    port: &Port,
    n: usize,
    out: &mut Vec<Row>,
    subs: &mut Vec<Suboptions>,
) {
    let i = ink(env.p);
    let key = Key::of(Part::Port).at(n);
    let glyph = match port.joint {
        Joint::Required => G::In,
        Joint::Optional => G::Opt,
        Joint::Rest => G::Rest,
    };
    let mut children = vec![
        div()
            .flex_none()
            .child(ty_el(
                env,
                cards,
                &key.field(Slot::Type),
                &port.ty,
                Tone::Plain,
            ))
            .into_any_element(),
    ];
    if let Some(default) = &port.default {
        children.push(said(
            env,
            &key.field(Slot::Default),
            format!("= {default}"),
            roles::WRITTEN,
            i.ink3,
        ));
    } else if port.joint == Joint::Optional {
        children.push(said(
            env,
            &key.field(Slot::Optional),
            "optional",
            roles::WRITTEN,
            i.ink3,
        ));
    }
    if let Some(note) = &port.note {
        children.push(wrapped(
            env,
            &key.field(Slot::Note),
            note.clone(),
            roles::NOTE,
            i.ink3,
        ));
    }
    if !port.options.is_empty() {
        let changing = port.options.iter().filter(|o| o.change.is_some()).count();
        let words = if changing > 0 {
            format!(
                "{} options, {changing} change what it gives",
                port.options.len()
            )
        } else {
            format!("{} options", port.options.len())
        };
        if let Some(fold) = env
            .host
            .unfold(&FoldKey::Options(PortName::new(&port.name)))
        {
            let caret = if fold.open { "▾" } else { "▸" };
            let text = format!("{words} {caret}");
            children.push(
                div()
                    .min_w_0()
                    .child(action(
                        env,
                        &key.field(Slot::Options),
                        text.clone(),
                        fold.toggle.clone(),
                        wrapped(
                            env,
                            &key.field(Slot::OptionsWords),
                            text,
                            roles::WRITTEN,
                            i.peri,
                        ),
                    ))
                    .into_any_element(),
            );
        }
        let rows: Vec<Row> = port
            .options
            .iter()
            .enumerate()
            .map(|(k, option)| options_row(env, cards, &Key::of(Part::Opt).at(n).at(k), option))
            .collect();
        subs.push(Suboptions {
            after: out.len(),
            port: PortName::new(&port.name),
            rows,
        });
    }
    out.push(Row {
        mark: mark(glyph, env.p, 12.0 * env.m.scale()),
        name: name(env, &key.field(Slot::Name), &port.name, roles::NAME, i.ink0),
        value: value(env, children),
        rail: i.line3,
        indent: false,
    });
}

/// The call, when the page has one.
pub(super) fn call(env: &Env<'_>, cards: &Cards, view: &View) -> Option<AnyElement> {
    let call: &Call = view.call.as_ref()?;
    let i = ink(env.p);
    let mut rows: Vec<Row> = Vec::new();
    let mut subs: Vec<Suboptions> = Vec::new();
    if let Some(receiver) = &call.receiver {
        let key = Key::of(Part::Recv);
        let mut children = vec![
            div()
                .flex_none()
                .child(ty_el(
                    env,
                    cards,
                    &key.field(Slot::Type),
                    &receiver.ty,
                    Tone::Plain,
                ))
                .into_any_element(),
        ];
        if let Some(effect) = receiver.effect {
            children.push(said(
                env,
                &key.field(Slot::Says),
                effect.says(),
                roles::NOTE,
                i.ink3,
            ));
        }
        rows.push(Row {
            mark: mark(G::Recv(receiver.effect), env.p, 12.0 * env.m.scale()),
            name: name(env, &key.field(Slot::Name), "self", roles::NAME, i.ink0),
            value: value(env, children),
            rail: i.line3,
            indent: false,
        });
    }
    for (n, port) in call.ports.iter().enumerate() {
        port_rows(env, cards, port, n, &mut rows, &mut subs);
    }
    // The rail's end.
    let end = env.p.peri.base.alpha(0.5).hsla();
    if let Some(later) = &call.later {
        let key = Key::of(Part::Later);
        rows.push(Row {
            mark: mark(G::Later, env.p, 16.0 * env.m.scale()),
            name: label(env, &key.field(Slot::Label), "later", i.peri),
            value: value(
                env,
                vec![wrapped(
                    env,
                    &key.field(Slot::When),
                    later.clone(),
                    roles::WHEN,
                    i.ink2,
                )],
            ),
            rail: end,
            indent: false,
        });
    }
    let key = Key::of(Part::Gives);
    let (gives_label, gives_glyph) = if call.gives.many {
        ("gives each", G::Many)
    } else {
        ("gives", G::Out)
    };
    let mut gives: Vec<AnyElement> = Vec::new();
    match &call.gives.ty {
        Some(ty) => gives.push(
            div()
                .flex_none()
                .child(ty_el(env, cards, &key.field(Slot::Type), ty, Tone::Plain))
                .into_any_element(),
        ),
        None => gives.push(said(
            env,
            &key.field(Slot::Nothing),
            "nothing",
            roles::WHEN,
            i.ink2,
        )),
    }
    if let Some(role) = &call.gives.role {
        gives.push(said(
            env,
            &key.field(Slot::Role),
            role.clone(),
            roles::NOTE,
            i.violet,
        ));
    }
    rows.push(Row {
        mark: mark(gives_glyph, env.p, 14.0 * env.m.scale()),
        name: label(env, &key.field(Slot::Label), gives_label, i.peri_hi),
        value: value(env, gives),
        rail: end,
        indent: false,
    });
    if let Some(when) = &call.none {
        let key = Key::of(Part::Nothing);
        rows.push(Row {
            mark: mark(G::None, env.p, 14.0 * env.m.scale()),
            name: label(env, &key.field(Slot::Label), "or nothing", i.slate),
            value: value(
                env,
                vec![wrapped(
                    env,
                    &key.field(Slot::When),
                    when.clone(),
                    roles::WHEN,
                    i.ink2,
                )],
            ),
            rail: end,
            indent: false,
        });
    }
    if let Some(fail) = &call.fails {
        let key = Key::of(Part::Fail);
        let mut children = vec![
            div()
                .flex_none()
                .child(ty_el(
                    env,
                    cards,
                    &key.field(Slot::Type),
                    &fail.ty,
                    Tone::Error,
                ))
                .into_any_element(),
        ];
        if !fail.when.is_empty() {
            children.push(wrapped(
                env,
                &key.field(Slot::When),
                fail.when.clone(),
                roles::WHEN,
                i.ink2,
            ));
        }
        rows.push(Row {
            mark: mark(G::Fail, env.p, 14.0 * env.m.scale()),
            name: label(env, &key.field(Slot::Label), fail.word.label(), i.coral),
            value: value(env, children),
            rail: end,
            indent: false,
        });
    }
    let built = build_rows(env, rows, subs);
    let plate = div()
        .flex()
        .flex_col()
        .bg(i.g1)
        .border_1()
        .border_color(i.line2)
        .py(env.k(12.0))
        .pr(env.k(18.0))
        .pl(env.k(12.0))
        .children(built);
    Some(
        div()
            .id(Key::of(Part::Sec(Sec::Call)).id())
            .mt(env.k(22.0))
            .child(plate)
            .into_any_element(),
    )
}
