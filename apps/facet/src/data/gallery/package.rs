//! The package page's folio (calm `v4/shots/PackagePage*.png`): the hero,
//! one facts line, the release comb with its one caption line, the lens
//! bar, "start with", and the territory map. The reader column only (the
//! titlebar and shelf belong to the shell).

use super::super::{
    Door, Region, Run, Spot, Tab, TickTone, comb, facts, lens_bar, spell, territory,
};
use crate::icons::{Kind, Lang};
use crate::overlay::lens::{self, Lens, LensItem};
use crate::paint::gem;
use crate::theme::ActiveFacet;
use crate::tokens::{Face, TypeRole, ty};
use crate::{Set, Typeset};
use gpui::{AnyElement, App, IntoElement, ParentElement, Styled, div, px};

/// serde's modules: `(name, public items, items your code reaches)`.
pub(crate) const MODULES: [(&str, usize, usize); 10] = [
    ("de", 96, 17),
    ("ser", 72, 9),
    ("de::value", 58, 3),
    ("de::impls", 44, 0),
    ("ser::impls", 38, 0),
    ("de::size_hint", 9, 0),
    ("ser::fmt", 12, 1),
    ("__private", 41, 0),
    ("macros", 6, 2),
    ("de::ignored_any", 7, 1),
];

/// A deterministic spread of `k` reached items among `n` for `name`.
fn reached(name: &str, n: usize, k: usize) -> Vec<usize> {
    let mut seed = name.bytes().fold(0x9e37_79b9_u64, |h, b| h.wrapping_mul(31).wrapping_add(u64::from(b)));
    let mut out: Vec<usize> = Vec::with_capacity(k);
    while out.len() < k.min(n) {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        #[allow(clippy::cast_possible_truncation)]
        let i = ((seed >> 33) as usize) % n;
        if !out.contains(&i) {
            out.push(i);
        }
    }
    out
}

pub(crate) fn regions() -> Vec<Region> {
    MODULES
        .iter()
        .map(|&(name, n, k)| {
            let region = Region::new(name, n).reached(reached(name, n, k));
            if name.starts_with("__") { region.private() } else { region }
        })
        .collect()
}

/// The module lens for region `i`.
pub(crate) fn module_lens(i: usize) -> Option<Lens> {
    let &(name, n, k) = MODULES.get(i)?;
    let most = match name {
        "de" => vec![
            LensItem::new("Deserialize").kind(Kind::Trait),
            LensItem::new("Visitor").kind(Kind::Trait),
            LensItem::new("from_str").kind(Kind::Function),
        ],
        "ser" => vec![
            LensItem::new("Serialize").kind(Kind::Trait),
            LensItem::new("Serializer").kind(Kind::Trait),
            LensItem::new("to_string").kind(Kind::Function),
        ],
        _ => Vec::new(),
    };
    Some(Lens::module(name, n, k, most))
}

const HERO_NAME: TypeRole = TypeRole {
    face: Face::Display,
    weight: 640.0,
    size: 44.0,
    line: 46.0,
    tracking: -0.035,
    italic: false,
};

/// The folio for a reader `width` px wide; `rested` shows a spot as rested.
pub(crate) fn folio(width: f32, rested: Option<Spot>, cx: &mut App) -> AnyElement {
    let facet = cx.facet();
    let palette = facet.palette();
    let scale = facet.text_scale;
    // .cfol: max-width 896, padding clamp(22, 5cqi, 64) clamp(16, 4cqi, 48).
    let cqi = width / 100.0;
    let pad_x = (4.0 * cqi).clamp(16.0, 48.0);
    let pad_y = (5.0 * cqi).clamp(22.0, 64.0);
    let column = width.min(896.0) - pad_x * 2.0;
    let m = facet.measure(px(column));

    let hero = div()
        .flex()
        .items_center()
        .gap(px((2.0 * cqi).clamp(14.0, 22.0)))
        .child(gem(Kind::Package).size(64.0 * scale))
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .child(
                    div()
                        .set(HERO_NAME, &m)
                        .text_color(palette.ink0.hsla())
                        .child("serde"),
                )
                .child(
                    div()
                        .mt(px(6.0 * scale))
                        .set(ty::LEDE, &m)
                        .text_color(palette.ink2.hsla())
                        .child("A generic serialization and deserialization framework."),
                ),
        );

    let facts_line = facts(&m, palette)
        .fact([
            Run::Mark {
                path: Lang::Rust.path().into(),
                color: Lang::Rust.color(),
            },
            Run::Words("crates.io".into()),
        ])
        .fact([Run::Mono("1.0.193".into())])
        .fact([
            Run::Words("used by ".into()),
            Run::Yours("3".into()),
            Run::Words(" of your projects".into()),
        ])
        .door("facts", Door::tip(|_, _, _, _| div().into_any_element()));

    let mut ticks = super::releases();
    for (i, tick) in ticks.iter_mut().enumerate() {
        tick.height = match tick.tone {
            Some(TickTone::Pin | TickTone::Current) => 18.0,
            Some(TickTone::Touches) => 14.0,
            _ => 6.0 + ((i * 7 + 3) % 10) as f32,
        };
    }
    let releases = comb("releases", ticks, &m)
        .caption([("19 releases since your pin, ", false), ("2", true), (" touch your code", false)])
        .door(lens::door(|i| {
            Some(Lens::release(
                format!("1.0.{}", 147 + i),
                "3 weeks ago",
                vec![
                    ("from_str".into(), true),
                    (" changed — your code calls it in ".into(), false),
                    ("2".into(), true),
                    (" places".into(), false),
                ],
                vec![
                    LensItem::new("SerializeMap::serialize_key").kind(Kind::Method).sigil(lens::Sigil::Added),
                    LensItem::new("IgnoredAny").kind(Kind::Struct).sigil(lens::Sigil::Added),
                    LensItem::new("de::from_str").kind(Kind::Function).sigil(lens::Sigil::Changed),
                    LensItem::new("a"),
                    LensItem::new("b"),
                    LensItem::new("c"),
                    LensItem::new("d"),
                    LensItem::new("e"),
                ],
            ))
        }));

    let tabs = lens_bar(
        "tabs",
        vec![
            Tab::new("Map"),
            Tab::new("Readme"),
            Tab::new("Depends").count("3"),
            Tab::new("Used by").count("41k"),
            Tab::new("Changes").count("64"),
        ],
        0,
        &m,
    );

    const START: TypeRole = TypeRole {
        size: 13.0,
        line: 18.0,
        ..ty::SMALL
    };
    const START_MONO: TypeRole = TypeRole {
        weight: 500.0,
        size: 12.5,
        line: 18.0,
        ..ty::MONO_ROW
    };
    let start = spell(&m)
        .text("Start with ", START, palette.ink3)
        .text("Deserialize", START_MONO, palette.ink1)
        .text(", then ", START, palette.ink3)
        .text("derive", START_MONO, palette.ink1)
        .text(".", START, palette.ink3);

    let map = territory("territory", regions(), &m)
        .rest(rested)
        .region_door(lens::door(module_lens))
        .stone_door(Door::tip(|_, _, _, _| div().into_any_element()));

    let gap = 30.0 * facet.density.space() * scale;
    div()
        .size_full()
        .flex()
        .justify_center()
        .child(
            div()
                .w(px(column + pad_x * 2.0))
                .px(px(pad_x))
                .pt(px(pad_y))
                .flex()
                .flex_col()
                .child(hero)
                .child(div().mt(px(gap - 12.0 * scale)).child(facts_line))
                .child(div().mt(px(gap - 6.0 * scale)).child(releases))
                .child(div().mt(px(gap)).child(tabs))
                .child(div().mt(px(gap)).child(start))
                .child(div().mt(px(gap - 12.0 * scale)).child(map)),
        )
        .typeset(ty::BODY, &facet)
        .into_any_element()
}

