//! Folio scenes: the symbol cards (tokio's mpsc and toml's `de` from real
//! signatures, every change state, badges open), the glyph sheet. Each new
//! folio component adds its scene here.

#![allow(clippy::too_many_lines)]

use super::cards::{CardFacts, Change, columns, symbol_card};
use super::berg::{Basis, BergBlock, BergFacts, berg, weight};
use super::crest::{Advisories, Silence, advisories, stamp, unread};
use super::features::{FeatureFacts, FeatureNode, features};
use super::heads::{Place, Sighting, Signals, findings, heads};
use super::shingles::{ModuleFacts, Spot, shingles};
use super::state::{Build as Scripts, Extent, Fold, Library, Names, Nominal, Pose, Standing, Time, Unsafe, Use};
use super::ticker::{Release, TickerFacts, ticker};
use super::fixture::{self, Decl};
use crate::Set;
use crate::gallery::Scene;
use crate::icons::Lang;
use crate::marks::badges::{Glyph, Item, badge, glyph};
use crate::marks::license::LicenseFacts;
use crate::marks::gallery::{Build, stage};
use crate::measure::Measure;
use crate::theme::ActiveFacet;
use crate::tokens::{Face, TypeRole, ty};
use gpui::{AnyElement, AnyView, App, IntoElement, ParentElement, SharedString, Styled, Window, div, px};
use std::rc::Rc;

pub(crate) const SCENES: &[Scene] = &[
    Scene { id: "folio-cards", title: "Symbol cards: tokio::sync::mpsc from real signatures; badges, never code", size: (1440, 1180), build: cards },
    Scene { id: "folio-cards-past", title: "Symbol cards in the past: new, changed, gone, not yet", size: (1440, 560), build: past },
    Scene { id: "folio-shingles", title: "The shingles: tokio's 16 modules; a region lit, its foot reading it", size: (1440, 520), build: shingles_scene },
    Scene { id: "folio-shingles-800", title: "The shingles at 800 px", size: (800, 700), build: shingles_800 },
    Scene { id: "folio-shingles-2560", title: "The shingles at 2560 px", size: (2560, 700), build: shingles_2560 },
    Scene { id: "folio-ticker", title: "The release ticker: tokio's history on a time axis, the pointer resting on 1.28.0", size: (1440, 260), build: ticker_scene },
    Scene { id: "folio-ticker-past", title: "The ticker while reading 1.9.0 (the caret), undated releases evenly spaced", size: (1440, 260), build: ticker_past },
    Scene { id: "folio-crest", title: "The crest row: licence (unfolded), heads-up and weight (not read), advisories (no feed)", size: (1440, 420), build: crest_scene },
    Scene { id: "folio-crest-warm", title: "The crest with a copyleft licence judged against your MIT OR Apache-2.0, at rest", size: (1440, 320), build: crest_warm },
    Scene { id: "folio-heads", title: "Heads-up: tokio's hand at rest, then fanned out (deferred plate over its neighbours)", size: (1440, 520), build: heads_scene },
    Scene { id: "folio-berg", title: "Weight: the glyph and tokio's berg with windows-sys surfaced", size: (1440, 520), build: berg_scene },
    Scene { id: "folio-features", title: "Features: tokio at rest, then with `full` on (its closure locks)", size: (1440, 420), build: features_scene },
    Scene { id: "folio-glyphs", title: "The badge glyphs at 12 and 24 px, and open badges", size: (1000, 620), build: glyphs },
];

const TITLE: TypeRole = TypeRole { face: Face::Display, weight: 640.0, size: 30.0, line: 36.0, tracking: -0.02, italic: false };
const SECTION: TypeRole = TypeRole { face: Face::Ui, weight: 600.0, size: 11.0, line: 14.0, tracking: 0.06, italic: false };

fn board(title: &'static str, sections: Vec<(&'static str, AnyElement)>, measure: &Measure, cx: &App) -> AnyElement {
    let palette = cx.facet().palette();
    let mut column = div()
        .absolute()
        .left(px(52.0))
        .top(px(36.0))
        .w(px(1336.0))
        .flex()
        .flex_col()
        .gap(px(22.0))
        .child(div().set(TITLE, measure).text_color(palette.ink0.hsla()).child(title));
    for (label, body) in sections {
        column = column.child(
            div()
                .flex()
                .flex_col()
                .gap(px(12.0))
                .child(div().set(SECTION, measure).text_color(palette.ink3.hsla()).child(label.to_uppercase()))
                .child(body),
        );
    }
    column.into_any_element()
}

fn grid(decls: &[Decl], measure: &Measure, prefix: &'static str, change: impl Fn(usize) -> Option<Change>, at: &str) -> AnyElement {
    let (count, width) = columns(measure, 300.0);
    let _ = count;
    let mut cards = div().flex().flex_wrap().gap(px(10.0));
    for (i, (name, kind, signature, doc)) in decls.iter().enumerate() {
        let item = Item::new(name, Lang::Rust).kind(Some(*kind)).signature(Some(signature));
        let facts = CardFacts::of(&item, *doc).yours(if i % 5 == 1 { Use::Yours } else { Use::Elsewhere }).change(change(i));
        cards = cards.child(symbol_card((prefix, i), Rc::new(facts), measure).width(width).at(at.to_owned()));
    }
    cards.into_any_element()
}

fn cards(_: &mut Window, cx: &mut App) -> AnyView {
    stage(
        |measure, _, cx| {
            let m = measure.within(px(1336.0));
            board(
                "Package folio",
                vec![
                    ("tokio::sync::mpsc, 16 declarations", grid(fixture::MPSC, &m, "mpsc", |_| None, "")),
                ],
                measure,
                cx,
            )
        },
        cx,
    )
}

fn past(_: &mut Window, cx: &mut App) -> AnyView {
    stage(
        |measure, _, cx| {
            let m = measure.within(px(1336.0));
            let change = |i: usize| match i {
                0 => Some(Change::New),
                1 => Some(Change::Changed),
                2 => Some(Change::Gone),
                3 => Some(Change::Absent),
                _ => None,
            };
            board(
                "Package folio, in the past",
                vec![("toml at 0.5.11: new, changed, gone, not yet", grid(&fixture::TOML[..8], &m, "past", change, "0.5.11"))],
                measure,
                cx,
            )
        },
        cx,
    )
}

fn glyphs(_: &mut Window, cx: &mut App) -> AnyView {
    let build: Build = |measure, _, cx| {
        let palette = cx.facet().palette();
        let mut sheet = div().flex().flex_wrap().gap(px(18.0));
        for (i, g) in Glyph::ALL.iter().enumerate() {
            sheet = sheet.child(
                div()
                    .w(px(96.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(8.0))
                    .child(div().flex().items_end().gap(px(10.0)).child(glyph(*g, 12.0, palette.ink1)).child(glyph(*g, 24.0, palette.ink0)))
                    .child(div().set(ty::MONO_SMALL, measure).text_color(palette.ink3.hsla()).child(SharedString::from(g.name()))),
            );
            let _ = i;
        }
        let mut open = div().flex().flex_wrap().gap(px(10.0));
        for (i, decl) in [fixture::TOML[0], fixture::MPSC[13], fixture::TOML[10]].iter().enumerate() {
            let reading = crate::marks::badges::read(&Item::new(decl.0, Lang::Rust).kind(Some(decl.1)).signature(Some(decl.2)));
            for (j, b) in reading.badges.iter().enumerate() {
                let view = badge(("open", i * 10 + j), b, measure);
                open = open.child(if j == 1 { view.open() } else { view });
            }
        }
        board("Badge glyphs", vec![("Every glyph", sheet.into_any_element()), ("Badges, the second of each open", open.into_any_element())], measure, cx)
    };
    stage(build, cx)
}

fn tokio_map(measure: &Measure, prefix: &'static str, time: Time, rest: Option<Spot>) -> AnyElement {
    let modules: Vec<ModuleFacts> = fixture::TOKIO_MODULES
        .iter()
        .enumerate()
        .map(|(i, (name, n))| {
            let mut m = ModuleFacts::new(*name, fixture::shingles(i, *n));
            if i == 0 {
                m.doc = Some("A multi-producer, single-consumer queue for sending values between asynchronous tasks.".into());
                for (j, s) in m.shingles.iter_mut().enumerate() {
                    s.yours = if j % 4 == 0 { Use::Yours } else { Use::Elsewhere };
                }
            }
            m.extent = if *n > 14 { Extent::Page } else { Extent::Inline };
            m
        })
        .collect();
    shingles(prefix, modules.into(), measure).time(time).rest(rest).open(Some(5)).into_any_element()
}

fn shingles_scene(_: &mut Window, cx: &mut App) -> AnyView {
    stage(
        |measure, _, cx| {
            let m = measure.within(px(1336.0));
            board("Package folio", vec![("103 public names in 16 modules", tokio_map(&m, "tokio", Time::Now, Some(Spot::Shingle(0, 6))))], measure, cx)
        },
        cx,
    )
}

fn shingles_800(_: &mut Window, cx: &mut App) -> AnyView {
    stage(
        |measure, _, cx| {
            let m = measure.within(px(720.0));
            board("Package folio", vec![("103 public names in 16 modules", tokio_map(&m, "tokio800", Time::Now, Some(Spot::Region(3))))], measure, cx)
        },
        cx,
    )
}

fn shingles_2560(_: &mut Window, cx: &mut App) -> AnyView {
    stage(
        |measure, _, cx| {
            let m = measure.within(px(1336.0));
            board("Package folio", vec![("103 public names in 16 modules", tokio_map(&m, "tokio2560", Time::Past, Some(Spot::Region(1))))], measure, cx)
        },
        cx,
    )
}

fn tokio_ticker(reading: Option<&str>, dated: bool) -> TickerFacts {
    let releases: Vec<Release> = fixture::TOKIO_RELEASES
        .iter()
        .map(|(v, d)| Release {
            version: (*v).to_owned(),
            date: dated.then(|| (*d).to_owned()),
            standing: if *v == "0.1.3" { Standing::Yanked } else { Standing::Available },
            names: if *v == "0.1.7" { Names::Unread } else { Names::Read },
        })
        .collect();
    TickerFacts::new(&releases, Some("1.47.0"), "2026-09-28").reading(reading)
}

fn ticker_scene(_: &mut Window, cx: &mut App) -> AnyView {
    stage(
        |measure, _, cx| {
            let m = measure.within(px(1336.0));
            let facts = Rc::new(tokio_ticker(None, true));
            let rest = facts.positions(1336.0, 8.0).get(38).copied();
            board("Package folio", vec![("Releases", ticker("tick", facts, &m).rest(rest).into_any_element())], measure, cx)
        },
        cx,
    )
}

fn ticker_past(_: &mut Window, cx: &mut App) -> AnyView {
    stage(
        |measure, _, cx| {
            let m = measure.within(px(1336.0));
            let facts = Rc::new(tokio_ticker(Some("1.9.0"), false));
            let rest = facts.positions(1336.0, 8.0).get(30).copied();
            board("Package folio", vec![("Releases, undated", ticker("tickp", facts, &m).rest(rest).into_any_element())], measure, cx)
        },
        cx,
    )
}

fn crest_row(m: &Measure, licence: &str, pose: Pose, adv: Advisories) -> AnyElement {
    let facts = Rc::new(LicenseFacts::new(Some(licence), Some("MIT OR Apache-2.0"), "backend"));
    let mut stamp = stamp("licence", facts, px(330.0), m);
    if pose == Pose::Held {
        stamp = stamp.open();
    }
    div()
        .flex()
        .items_start()
        .gap(px(10.0))
        .child(stamp)
        .child(unread("heads", "Heads-up", "Reading its source on disk for build scripts, network, files and unsafe.", px(230.0), m))
        .child(unread("weight", "Weight", "Reading its source on disk for lines of code.", px(230.0), m))
        .child(advisories("adv", adv, px(250.0), m))
        .into_any_element()
}

fn crest_scene(_: &mut Window, cx: &mut App) -> AnyView {
    stage(
        |measure, _, cx| {
            let m = measure.within(px(1336.0));
            let adv = Advisories::Unknown { why: Silence::NoFeed, note: "RustSec, OSV and GHSA can be read; none configured.".into() };
            board("Package folio", vec![("Crest", crest_row(&m, "MIT OR Apache-2.0", Pose::Held, adv))], measure, cx)
        },
        cx,
    )
}

fn crest_warm(_: &mut Window, cx: &mut App) -> AnyView {
    stage(
        |measure, _, cx| {
            let m = measure.within(px(1336.0));
            let adv = Advisories::Found { count: 2, worst: Some("high".into()), decision: "warn".into() };
            board("Package folio", vec![("Crest", crest_row(&m, "GPL-3.0-only", Pose::Live, adv))], measure, cx)
        },
        cx,
    )
}

fn place(file: &str, line: usize, text: &str) -> Place {
    Place { file: file.to_owned().into(), line, text: text.to_owned().into() }
}

fn tokio_signals() -> Signals {
    Signals {
        build: Scripts::Plain,
        library: Library::Plain,
        unsafe_code: Unsafe::Allowed,
        unsafe_count: 1047,
        process: Sighting { count: 4, places: vec![place("src/process/unix/pidfd_reaper.rs", 234, "let Output { stdout, status, .. } = Command::new(\"uname\").arg(\"-r\").output().unwrap();"), place("src/process/unix/pidfd_reaper.rs", 260, "let child = Command::new(\"true\").spawn().unwrap();")] },
        ffi: Sighting::default(),
        net: Sighting { count: 141, places: vec![place("src/net/addr.rs", 3, "use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};"), place("src/net/lookup_host.rs", 5, "use std::net::SocketAddr;")] },
        files: Sighting { count: 81, places: vec![place("src/fs/canonicalize.rs", 48, "asyncify(move || std::fs::canonicalize(path)).await")] },
        env: Sighting { count: 1, places: vec![place("src/loom/std/mod.rs", 90, "match std::env::var(ENV_WORKER_THREADS) {")] },
    }
}

fn heads_row(m: &Measure, prefix: &'static str, pose: Pose, signals: &Signals) -> AnyElement {
    let list = Rc::new(findings(signals));
    let mut cell = heads(prefix, "tokio", list, px(230.0), px(1000.0), Nominal::px(124.0), m);
    if pose == Pose::Held {
        cell = cell.open();
    }
    div()
        .flex()
        .items_start()
        .gap(px(10.0))
        .child(unread("licence", "Licence", "MIT", px(330.0), m))
        .child(cell)
        .child(unread("weight", "Weight", "648K lines beneath", px(230.0), m))
        .child(unread("adv", "Advisories", "no feed", px(250.0), m))
        .into_any_element()
}

fn heads_scene(_: &mut Window, cx: &mut App) -> AnyView {
    stage(
        |measure, _, cx| {
            let m = measure.within(px(1336.0));
            board(
                "Package folio",
                vec![
                    ("At rest: a hand of chips, one over the next", heads_row(&m, "hr", Pose::Live, &tokio_signals())),
                    ("Fanned out", heads_row(&m, "ho", Pose::Held, &tokio_signals())),
                    ("Nothing to flag", heads_row(&m, "hn", Pose::Live, &Signals { unsafe_code: Unsafe::Forbidden, ..Signals::default() })),
                ],
                measure,
                cx,
            )
        },
        cx,
    )
}

fn tokio_berg() -> BergFacts {
    BergFacts {
        name: "tokio".into(),
        own: 49_187,
        blocks: fixture::TOKIO_BERG
            .iter()
            .map(|(name, sloc, layer, parent, deps)| BergBlock { name: (*name).into(), version: "1.0.0".into(), sloc: *sloc, layer: *layer, parent: *parent, deps: deps.to_vec() })
            .collect(),
        missing: 0,
        basis: Some(Basis::Lock),
    }
}

fn berg_scene(_: &mut Window, cx: &mut App) -> AnyView {
    stage(
        |measure, _, cx| {
            let m = measure.within(px(1336.0));
            let facts = Rc::new(tokio_berg());
            let glyph_cell = weight("wcell", facts.clone(), px(230.0), Nominal::px(124.0), Fold::Folded, &m).into_any_element();
            let surfaced = berg("bergx", facts.clone(), &m).rest(Some(9)).on_go(|_, _, _| {}).into_any_element();
            let calm = berg("bergc", facts, &m).into_any_element();
            board("Package folio", vec![("The crest's glyph", glyph_cell), ("The berg, windows-sys surfaced", surfaced), ("At rest", calm)], measure, cx)
        },
        cx,
    )
}

fn tokio_features() -> Rc<FeatureFacts> {
    Rc::new(FeatureFacts {
        names: fixture::TOKIO_FEATURES.iter().map(|(n, _, _)| (*n).to_owned()).collect(),
        default: Vec::new(),
        graph: fixture::TOKIO_FEATURES
            .iter()
            .map(|(n, e, d)| ((*n).to_owned(), FeatureNode { enables: e.iter().map(ToString::to_string).collect(), deps: d.iter().map(ToString::to_string).collect() }))
            .collect(),
        sizes: fixture::TOKIO_DEP_LINES.iter().map(|(n, l)| ((*n).to_owned(), Some(*l))).collect(),
    })
}

fn features_scene(_: &mut Window, cx: &mut App) -> AnyView {
    stage(
        |measure, _, cx| {
            let m = measure.within(px(1336.0));
            let facts = tokio_features();
            board(
                "Package folio",
                vec![
                    ("At rest: nothing on by default", features("fbar0", facts.clone(), px(1336.0), &m).into_any_element()),
                    ("`full` on: its closure locks, what it pulls in is counted", features("fbar1", facts, px(1336.0), &m).chosen(&["full"]).into_any_element()),
                ],
                measure,
                cx,
            )
        },
        cx,
    )
}
