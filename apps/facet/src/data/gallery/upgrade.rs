//! The upgrade lens against `v4/shots/graph/p-upgrade-toml.png`: toml's
//! page while the shelf's comb views 1.1.6 against the pinned 0.8.23. The
//! shelf carries W-Controls' comb (touches from `data::release`) and this
//! lane's crate-wide line; the page carries the section above the anatomy.
//! Data: the release fixture (a slice of the real toml releases and this
//! workspace's uses of them).

use super::super::release::{self, fixture, view};
use super::super::{Run, facts};
use crate::anatomy::Links;
use crate::controls::{Release, ReleaseId, Step, version_comb};
use crate::icons::Kind;
use crate::paint::gem;
use crate::semantics::types::Nowhere;
use crate::theme::ActiveFacet;
use crate::tokens::{Face, TypeRole, ty};
use crate::Set;
use gpui::{AnyElement, App, IntoElement, ParentElement, SharedString, Styled, Window, div, px};

const HERO_NAME: TypeRole = TypeRole { face: Face::Display, weight: 640.0, size: 40.0, line: 44.0, tracking: -0.035, italic: false };
const CRATE: TypeRole = TypeRole { face: Face::Mono, weight: 600.0, size: 13.0, line: 18.0, tracking: 0.0, italic: false };
const NOTE: TypeRole = TypeRole { face: Face::Mono, weight: 400.0, size: 11.5, line: 16.0, tracking: 0.0, italic: false };
const BACK: TypeRole = TypeRole { face: Face::Ui, weight: 400.0, size: 12.0, line: 16.0, tracking: 0.0, italic: false };

/// The version viewed in these scenes.
pub(crate) const VIEWED: &str = "1.1.6+spec-1.1.0";

/// `2014-11-11T05:37:45Z` read as an age against the scenes' today
/// (2026-09).
fn age(at: &str) -> SharedString {
    let year: i32 = at.get(0..4).and_then(|y| y.parse().ok()).unwrap_or(2026);
    let month: i32 = at.get(5..7).and_then(|m| m.parse().ok()).unwrap_or(9);
    let months = (2026 * 12 + 9) - (year * 12 + month);
    SharedString::from(match months {
        ..=0 => "this month".to_owned(),
        1 => "a month ago".to_owned(),
        2..=23 => format!("{months} months ago"),
        _ => format!("{} years ago", months / 12),
    })
}

/// The shelf: the crate, W-Controls' comb viewing `to`, and the line.
fn shelf(krate: &release::Crate, to: &str, cx: &mut App) -> AnyElement {
    let facet = cx.facet();
    let palette = facet.palette();
    let s = facet.text_scale;
    let m = facet.measure(px(230.0 * s));
    let versions: Vec<&release::Version> =
        krate.versions.iter().filter(|v| !v.yanked || v.v == krate.pinned).collect();
    let releases: Vec<Release> = versions
        .iter()
        .map(|v| Release {
            id: ReleaseId(v.v.clone()),
            version: SharedString::from(release::short(&v.v).to_owned()),
            step: Step::of(&v.v),
            age: age(&v.at),
        })
        .collect();
    let index = |v: &str| versions.iter().position(|x| x.v == v);
    let touches = krate.touches();
    let mut comb = version_comb("upgrade-comb", releases, &m)
        .touches(versions.iter().enumerate().filter(|(_, v)| touches.contains(&v.v)).map(|(k, _)| k));
    if let Some(pin) = index(&krate.pinned) {
        comb = comb.pinned(pin);
    }
    if let Some(viewed) = index(to) {
        comb = comb.selected(viewed);
    }
    let summary = release::summary(krate, &krate.pinned, to);
    div()
        .w(px(258.0 * s))
        .h_full()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(12.0 * s))
        .px(px(14.0 * s))
        .py(px(18.0 * s))
        .border_r_1()
        .border_color(palette.line1.hsla())
        .child(div().set(BACK, &m).text_color(palette.ink3.hsla()).child("‹ dependencies"))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(12.0 * s))
                .child(gem(Kind::Package).size(28.0 * s).state(crate::paint::GemState::Normal))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(div().set(CRATE, &m).text_color(palette.ink0.hsla()).child(krate.name.clone()))
                        .child(div().set(NOTE, &m).text_color(if krate.pinned == to { palette.ink3.hsla() } else { palette.peri.base.hsla() }).child(release::short(to).to_owned())),
                ),
        )
        .child(comb)
        .child(view::shelf_line("upgrade-shelf", &summary, &krate.pinned, to, &m, palette))
        .into_any_element()
}

/// The page column: the hero, one facts line, and the upgrade section.
#[allow(clippy::too_many_arguments)]
fn page(krate: &release::Crate, symbol: &str, (kind, name): (Kind, &str), lede: &str, to: &str, width: f32, xray: bool, cx: &mut App) -> AnyElement {
    let facet = cx.facet();
    let palette = facet.palette();
    let s = facet.text_scale;
    let m = facet.measure(px(width));
    let lens = release::lens(krate, symbol, to, &Nowhere);
    let module = symbol.rsplit_once("::").map_or(symbol, |(owner, _)| owner).to_owned();
    div()
        .w(px(width))
        .flex()
        .flex_col()
        .gap(px(18.0 * s))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(18.0 * s))
                .child(gem(kind).size(60.0 * s).state(crate::paint::GemState::Normal))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .child(div().set(HERO_NAME, &m).text_color(palette.ink0.hsla()).child(name.to_owned()))
                        .child(div().mt(px(4.0 * s)).set(ty::LEDE, &m).text_color(palette.ink2.hsla()).child(lede.to_owned())),
                ),
        )
        .child(
            facts(&m, palette)
                .fact([Run::Words(format!("{} in ", kind.name()).into()), Run::Mono(module.into())])
                .fact([Run::Words("used in 59 places".into())])
                .fact([Run::Yours("7".into()), Run::Words(" in your code".into())]),
        )
        .child(
            div()
                .mt(px(10.0 * s))
                .child(view::section("upgrade", lens, name.to_owned(), krate.pinned.clone(), &m, &Links::plain(), xray)),
        )
        .into_any_element()
}

fn toml() -> &'static release::Crate {
    fixture::get("toml").expect("toml is in the release fixture")
}

/// The target: toml::value::Value at 1440 × 900.
pub(crate) fn value(width: f32, _window: &mut Window, cx: &mut App) -> AnyElement {
    let s = cx.facet().text_scale;
    let column = (width - 258.0 * s - 199.0 * s - 64.0 * s).clamp(280.0, 720.0 * s);
    div()
        .size_full()
        .flex()
        .child(shelf(toml(), VIEWED, cx))
        .child(
            div()
                .flex_1()
                .pl(px(199.0 * s))
                .pt(px(62.0 * s))
                .child(page(toml(), "toml::value::Value", (Kind::Enum, "Value"), "Representation of a TOML value.", VIEWED, column, false, cx)),
        )
        .into_any_element()
}

/// `Map`: a real change row (`insert`), old struck and new underlined; ⌥
/// held spells each signature's Rust source.
pub(crate) fn map(width: f32, _window: &mut Window, cx: &mut App) -> AnyElement {
    let s = cx.facet().text_scale;
    let column = (width - 96.0 * s).clamp(280.0, 880.0 * s);
    let krate = toml();
    div()
        .size_full()
        .flex()
        .flex_col()
        .gap(px(48.0 * s))
        .px(px(48.0 * s))
        .py(px(40.0 * s))
        .child(page(krate, "toml::map::Map", (Kind::Struct, "Map"), "Type representing a TOML table.", VIEWED, column, false, cx))
        .child(page(krate, "toml::Deserializer", (Kind::Struct, "Deserializer"), "Deserialization for TOML documents.", VIEWED, column, true, cx))
        .into_any_element()
}

/// The page at a narrow reader (rows stack), and going back to 0.5.11.
pub(crate) fn narrow(width: f32, _window: &mut Window, cx: &mut App) -> AnyElement {
    let s = cx.facet().text_scale;
    let column = (width - 32.0 * s).max(200.0);
    let krate = toml();
    div()
        .size_full()
        .flex()
        .flex_col()
        .gap(px(40.0 * s))
        .px(px(16.0 * s))
        .py(px(24.0 * s))
        .child(page(krate, "toml::map::Map", (Kind::Struct, "Map"), "Type representing a TOML table.", VIEWED, column, false, cx))
        .child(page(krate, "toml::value::Value", (Kind::Enum, "Value"), "Representation of a TOML value.", "0.5.11", column, false, cx))
        .into_any_element()
}
