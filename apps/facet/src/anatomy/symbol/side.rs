//! The context rail: where it is defined, which of your packages use it,
//! what is next to it, what it can do, its releases, and how we know its
//! types.

use super::body::outcome_marks;
use super::host::{Change, Ui};
use super::ink::{G, mark};
use super::key::{Key, Part, Slot};
use super::kit::{Ellipsis, Env, caps, ink, roles, said, said_in, truncated, wrapped};
use super::view::{Cap, Kind, Listed, Rail, Uses, Verb};
use crate::anatomy::page::named_as;
use crate::icons::{self, KindSize};
use gpui::{
    AnyElement, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div,
};

const fn icon_kind(kind: Kind) -> icons::Kind {
    match kind {
        Kind::Function => icons::Kind::Function,
        Kind::Method => icons::Kind::Method,
        Kind::Enum => icons::Kind::Enum,
        Kind::Struct => icons::Kind::Struct,
        Kind::Trait => icons::Kind::Trait,
        Kind::Alias => icons::Kind::Type,
        Kind::Constant => icons::Kind::Constant,
        Kind::Module => icons::Kind::Module,
        Kind::Other => icons::Kind::Unknown,
    }
}

/// The kind mark, at a small size.
pub(super) fn kind_mark(env: &Env<'_>, kind: Kind, size: KindSize) -> AnyElement {
    icons::kind_mark(icon_kind(kind), size, env.p)
}

/// A rail block: its small-caps head, then its body.
fn block(env: &Env<'_>, n: usize, title: &str, body: Vec<AnyElement>) -> AnyElement {
    let i = ink(env.p);
    let key = Key::of(Part::Block).at(n);
    div()
        .id(key.id())
        .flex()
        .flex_col()
        .gap(env.k(8.0))
        .child(said(
            env,
            &key.field(Slot::Title),
            caps(title),
            roles::RAIL_HEAD,
            i.ink3,
        ))
        .children(body)
        .into_any_element()
}

fn cap_chip(env: &Env<'_>, cap: &Cap, n: usize) -> AnyElement {
    let i = ink(env.p);
    div()
        .flex()
        .items_center()
        .gap(env.k(6.0))
        .h(env.s(24.0))
        .px(env.k(8.0))
        .bg(i.g1)
        .border_1()
        .border_color(i.line2)
        .child(mark(G::Cap(cap.mark), env.p, 14.0 * env.m.scale()))
        .child(said(
            env,
            &Key::of(Part::Cap).at(n),
            cap.word.clone(),
            roles::RAIL_SAY,
            i.ink2,
        ))
        .into_any_element()
}

fn source_block(env: &Env<'_>, rail: &Rail, n: usize) -> Option<AnyElement> {
    let source = rail.source.as_ref()?;
    let i = ink(env.p);
    let key = Key::of(Part::Block).field(Slot::Source);
    let place = format!("{}:{}", source.file, source.line);
    let mut link = div()
        .id(key.id())
        .flex()
        .items_center()
        .min_h(env.s(24.0))
        .gap(env.k(8.0))
        .child(mark(G::Open, env.p, 11.0 * env.m.scale()))
        .child(said(
            env,
            &key.field(Slot::Place),
            place.clone(),
            roles::RAIL,
            i.ink1,
        ));
    let link = match &source.open {
        Some(path) => {
            let open = env.host.open_source(path, source.line);
            let again = open.clone();
            link = link
                .cursor_pointer()
                .on_click(move |_, window, cx| open(window, cx));
            env.host.target(
                &key,
                SharedString::from(place),
                again,
                link.into_any_element(),
            )
        }
        None => link.into_any_element(),
    };
    let sub = if source.pinned {
        format!("{} · your pin", source.package)
    } else {
        source.package.clone()
    };
    Some(block(
        env,
        n,
        "Source",
        vec![
            link,
            said(env, &key.field(Slot::Sub), sub, roles::RAIL_SAY, i.ink3),
        ],
    ))
}

fn packages_block(env: &Env<'_>, uses: &Uses, ui: &Ui, n: usize) -> AnyElement {
    let i = ink(env.p);
    let packages = uses.packages(Listed::USES);
    let mut body: Vec<AnyElement> = Vec::new();
    if let Some(note) = &uses.elsewhere {
        body.push(wrapped(
            env,
            &Key::of(Part::RailPackage).field(Slot::Elsewhere),
            note.clone(),
            roles::RAIL_SAY,
            i.ink2,
        ));
    } else if packages.is_empty() {
        body.push(said(
            env,
            &Key::of(Part::RailPackage).field(Slot::None),
            "None of them use it.",
            roles::RAIL_SAY,
            i.ink3,
        ));
    } else {
        for (k, entry) in packages.iter().take(9).enumerate() {
            let key = Key::of(Part::RailPackage).at(k);
            let verbs: Vec<Verb> = entry
                .verbs
                .iter()
                .copied()
                .filter(|v| *v != Verb::Imports)
                .collect();
            let on = ui.package.as_deref() == Some(entry.package.as_str());
            let pick = env
                .host
                .change(Change::Package(Some(entry.package.clone())));
            let picked = pick.clone();
            let mut row = div()
                .id(key.id())
                .flex()
                .items_center()
                .gap(env.k(8.0))
                .h(env.s(26.0))
                .px(env.k(4.0))
                .cursor_pointer()
                .hover(|style| style.bg(i.g3))
                .on_click(move |_, window, cx| pick(window, cx))
                .child(div().min_w_0().flex_1().child(truncated(
                    env,
                    &key.field(Slot::Name),
                    entry.package.clone(),
                    roles::RAIL,
                    i.mint,
                    Ellipsis::End,
                )))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(env.k(5.0))
                        .flex_none()
                        .children(
                            verbs
                                .iter()
                                .map(|v| mark(G::Verb(*v), env.p, 14.0 * env.m.scale())),
                        ),
                );
            if on {
                row = row.bg(i.g2);
            }
            body.push(env.host.target(
                &key,
                SharedString::from(entry.package.clone()),
                picked,
                row.into_any_element(),
            ));
        }
        if packages.len() > 9 {
            body.push(said(
                env,
                &Key::of(Part::RailPackage).field(Slot::More),
                format!("and {} more", packages.len() - 9),
                roles::RAIL_SAY,
                i.ink3,
            ));
        }
    }
    block(
        env,
        n,
        if uses.elsewhere.is_some() {
            "Where it's used"
        } else {
            "Your packages that use it"
        },
        body,
    )
}

/// The rail's blocks, in order.
pub(super) fn rail(env: &Env<'_>, rail: &Rail, uses: &Uses, ui: &Ui) -> Vec<AnyElement> {
    let i = ink(env.p);
    let mut out: Vec<AnyElement> = Vec::new();
    if let Some(el) = source_block(env, rail, out.len()) {
        out.push(el);
    }
    out.push(packages_block(env, uses, ui, out.len()));
    if !rail.siblings.is_empty() {
        let rows: Vec<AnyElement> = rail
            .siblings
            .iter()
            .enumerate()
            .map(|(k, sibling)| {
                let key = Key::of(Part::Sibling).at(k);
                let m = env.m;
                let colour = i.ink1;
                let (name, name_key, height) =
                    (sibling.name.clone(), key.field(Slot::Name), env.s(24.0));
                let door = named_as(
                    key.field(Slot::Door).text(),
                    &sibling.name,
                    sibling.link.as_deref(),
                    colour,
                    env.host,
                    false,
                    move |_| {
                        div()
                            .min_h(height)
                            .flex()
                            .items_center()
                            .child(said_in(&m, &name_key, name, roles::PACKAGE, colour))
                            .into_any_element()
                    },
                );
                div()
                    .flex()
                    .items_center()
                    .gap(env.k(8.0))
                    .h(env.s(26.0))
                    .px(env.k(4.0))
                    .child(kind_mark(env, sibling.kind, KindSize::Sm))
                    .child(door)
                    .child(div().min_w_0().flex_1().child(truncated(
                        env,
                        &key.field(Slot::Differs),
                        sibling.differs.clone(),
                        roles::RAIL_SAY,
                        i.ink3,
                        Ellipsis::End,
                    )))
                    .child(outcome_marks(env, sibling.outcomes))
                    .into_any_element()
            })
            .collect();
        out.push(block(env, out.len(), "Next to it", rows));
    }
    if !rail.can.is_empty() {
        let chips = rail
            .can
            .iter()
            .enumerate()
            .map(|(k, cap)| cap_chip(env, cap, k))
            .collect::<Vec<_>>();
        out.push(block(
            env,
            out.len(),
            "It can",
            vec![
                div()
                    .flex()
                    .flex_wrap()
                    .gap(env.k(6.0))
                    .children(chips)
                    .into_any_element(),
            ],
        ));
    }
    if rail.releases.len() > 1 {
        let differs = rail.releases.iter().any(|r| r.differs);
        let dots = div()
            .flex()
            .items_center()
            .gap(env.k(4.0))
            .flex_none()
            .children(rail.releases.iter().map(|release| {
                let colour = if release.pinned {
                    i.mint
                } else if release.differs {
                    i.amber
                } else {
                    i.ink3
                };
                div().size(env.s(8.0)).bg(colour)
            }));
        let words = match (&rail.across, differs) {
            (Some(across), _) => across.clone(),
            (None, false) => format!(
                "the same in {}",
                rail.releases
                    .iter()
                    .map(|r| r.version.clone())
                    .collect::<Vec<_>>()
                    .join(" and ")
            ),
            (None, true) => "it changed between releases".to_owned(),
        };
        let key = Key::of(Part::Block).field(Slot::Across);
        out.push(block(
            env,
            out.len(),
            "Across releases",
            vec![
                div()
                    .flex()
                    .items_center()
                    .gap(env.k(8.0))
                    .child(dots)
                    .child(div().min_w_0().flex_1().child(wrapped(
                        env,
                        &key,
                        words,
                        roles::RAIL_SAY,
                        i.ink2,
                    )))
                    .into_any_element(),
            ],
        ));
    }
    if let Some(how) = &rail.how {
        out.push(block(
            env,
            out.len(),
            "How we know",
            vec![wrapped(
                env,
                &Key::of(Part::Block).field(Slot::How),
                how.clone(),
                roles::RAIL_SAY,
                i.amber,
            )],
        ));
    }
    out
}
