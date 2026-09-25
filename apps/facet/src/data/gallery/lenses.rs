//! The lens board (calm `v4/shots/Lenses.png`): a release comb whose tick 40
//! is rested by a scripted pointer, so the real float layer opens the real
//! release lens with its connector; the module and language lenses beside
//! it on still plates (only one card floats at a time).

use super::super::{Tick, TickTone, comb};
use crate::icons::{Kind, Lang};
use crate::overlay::lens::{self, Lens, LensItem, Sigil, lens_card};
use crate::overlay::float::{self, FloatKind};
use crate::theme::ActiveFacet;
use crate::tokens::{Face, TypeRole, ty};
use crate::{Set, Typeset};
use gpui::{AnyElement, App, IntoElement, ParentElement, Styled, Window, div, px};

/// Where the scripted pointer rests: tick 40 of 90 on a 640 px comb at
/// x = 56, on the comb's field.
pub(crate) const POKES: &[(u64, f32, f32)] = &[(0, 56.0 + 1.0 + 40.0 * (638.0 / 89.0), 389.0)];

const H1: TypeRole = TypeRole {
    face: Face::Display,
    weight: 720.0,
    size: 34.0,
    line: 34.0,
    tracking: -0.03,
    italic: false,
};
const LABEL: TypeRole = TypeRole {
    weight: 500.0,
    size: 11.0,
    line: 14.0,
    ..ty::SMALL
};

pub(crate) fn release_lens(i: usize) -> Lens {
    Lens::release(
        format!("1.0.{}", 160 + i),
        "3 weeks ago",
        vec![
            ("from_str".into(), true),
            (" changed — your code calls it in ".into(), false),
            ("2".into(), true),
            (" places".into(), false),
        ],
        vec![
            LensItem::new("SerializeMap::serialize_key").kind(Kind::Method).sigil(Sigil::Added),
            LensItem::new("IgnoredAny").kind(Kind::Struct).sigil(Sigil::Added),
            LensItem::new("de::from_str").kind(Kind::Function).sigil(Sigil::Changed),
            LensItem::new("1"),
            LensItem::new("2"),
            LensItem::new("3"),
            LensItem::new("4"),
            LensItem::new("5"),
        ],
    )
}

pub(crate) fn board(_width: f32, _window: &mut Window, cx: &mut App) -> AnyElement {
    let facet = cx.facet();
    let palette = facet.palette();
    let s = facet.text_scale;
    let label = |words: &'static str| {
        div()
            .typeset(LABEL, &facet)
            .text_color(palette.ink4.hsla())
            .mb(px(10.0 * s))
            .child(words)
    };
    let ticks: Vec<Tick> = (0..90usize)
        .map(|i| {
            #[allow(clippy::cast_precision_loss)]
            let t = Tick::new(8.0 + ((i * 7) % 9) as f32).split(1.0 + (i % 2) as f32, 2.0);
            if i == 30 { t.tone(TickTone::Pin) } else { t }
        })
        .collect();
    let m = facet.measure(px(640.0 * s));
    let strip = comb("lens-comb", ticks, &m)
        .thickness(26.0)
        .rung(crate::measure::Rung::Row)
        .door(lens::door(|i| Some(release_lens(i))));

    let plate = |content: AnyElement| float::plate(FloatKind::Lens, false, palette).child(content);
    let card_m = facet.measure(px(340.0 * s));
    let most = vec![
        LensItem::new("Deserialize").kind(Kind::Trait),
        LensItem::new("Visitor").kind(Kind::Trait),
        LensItem::new("from_str").kind(Kind::Function),
    ];
    let module = plate(lens_card(&Lens::module("de", 214, 17, most), &card_m, None, cx));
    let language = plate(lens_card(
        &Lens::language(
            Lang::Python,
            "python",
            7,
            &[("numpy", 22), ("requests", 12), ("pydantic", 9), ("rich", 7), ("httpx", 5), ("attrs", 3), ("bytes-py", 2)],
        ),
        &card_m,
        None,
        cx,
    ));

    div()
        .size_full()
        .flex()
        .flex_col()
        .px(px(56.0))
        .pt(px(44.0))
        .gap(px(34.0))
        .child(div().set(H1, &facet.measure(px(1440.0))).text_color(palette.ink0.hsla()).child("Lenses"))
        .child(
            div()
                .flex()
                .flex_col()
                .child(label("Resting on a release tick"))
                .child(div().w(px(640.0 * s)).h(px(262.0 * s)).flex().flex_col().justify_end().child(strip)),
        )
        .child(
            div()
                .flex()
                .gap(px(28.0))
                .child(div().flex().flex_col().child(label("Resting on a module region")).child(module))
                .child(div().flex().flex_col().child(label("Resting on a language")).child(language)),
        )
        .into_any_element()
}
