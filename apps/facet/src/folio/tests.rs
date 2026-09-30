//! Folio components rendered in a test window: real pointer and key events
//! through the element tree on the virtual clock, and every assertion reads
//! what the frame *said* (the probe ledger's texts and boxes), never a count.

#![allow(clippy::expect_used, clippy::panic, clippy::too_many_lines)]

use super::berg::{Basis, BergBlock, BergFacts, berg};
use super::cards::{CardFacts, Change, symbol_card};
use super::crest::stamp;
use super::features::{FeatureFacts, FeatureNode, features};
use super::fixture::{self, MPSC, TOML};
use super::heads::{Place, Sighting, Signals, findings, heads};
use super::shingles::{ModuleFacts, ShingleFacts, shingles};
use super::state::{Extent, Fold, Names, Nominal, Pick, Standing, Time, Unsafe, Use};
use super::ticker::{Release, TickerFacts, ticker};
use crate::marks::badges::{Item, Lang};
use crate::marks::license::LicenseFacts;
use crate::overlay::{dialog, float};
use crate::probe::{self, Ledger};
use crate::theme::ActiveFacet;
use crate::tokens::Family;
use gpui::{
    AnyElement, Context, IntoElement, Modifiers, MouseButton, ParentElement, Render, Styled, TestAppContext, VisualTestContext, Window, div,
    point, px, size,
};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

type Log = Rc<RefCell<Vec<String>>>;

const WIDTH: f32 = 1100.0;

struct Page {
    build: Box<dyn Fn(&mut Window, &mut Context<Page>, &Log) -> AnyElement>,
    log: Log,
}

impl Render for Page {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        probe::draw_started(cx);
        let palette = cx.facet().palette();
        let content = (self.build)(window, cx, &self.log);
        div()
            .relative()
            .size_full()
            .bg(gpui::Hsla::from(palette.g1))
            .child(div().absolute().left(px(20.0)).top(px(20.0)).w(px(WIDTH - 40.0)).child(content))
            .child(float::layer(window, cx))
    }
}

fn frame(cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        window.simulate_next_frame(cx);
        window.refresh();
        window.draw(cx).clear(cx);
    });
}

fn advance(cx: &mut VisualTestContext, millis: u64) {
    cx.executor().advance_clock(Duration::from_millis(millis));
    cx.run_until_parked();
    frame(cx);
    frame(cx);
}

fn open(cx: &mut TestAppContext, build: impl Fn(&mut Window, &mut Context<Page>, &Log) -> AnyElement + 'static) -> (&mut VisualTestContext, Log) {
    let log: Log = Rc::default();
    let page_log = log.clone();
    let (_view, cx) = cx.add_window_view(move |_, _| Page { build: Box::new(build), log: page_log });
    cx.update(|_, cx| probe::enable(cx));
    cx.simulate_resize(size(px(WIDTH), px(700.0)));
    advance(cx, 16);
    (cx, log)
}

fn ledger(cx: &mut VisualTestContext) -> Ledger {
    frame(cx);
    cx.update(|_, cx| probe::take(cx))
}

fn said(cx: &mut VisualTestContext) -> Vec<String> {
    ledger(cx).texts.into_iter().map(|t| t.content).collect()
}

fn says(cx: &mut VisualTestContext, words: &str) -> bool {
    said(cx).iter().any(|t| t == words)
}

fn text_at(cx: &mut VisualTestContext, key_part: &str) -> Option<(String, f32, f32, f32, f32)> {
    ledger(cx).texts.into_iter().find(|t| t.key.contains(key_part)).map(|t| (t.content, t.bounds.x, t.bounds.y, t.bounds.width, t.bounds.height))
}

fn move_to(cx: &mut VisualTestContext, x: f32, y: f32) {
    cx.simulate_mouse_move(point(px(x), px(y)), None, Modifiers::none());
    frame(cx);
}

fn click(cx: &mut VisualTestContext, x: f32, y: f32) {
    move_to(cx, x, y);
    cx.simulate_mouse_down(point(px(x), px(y)), MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(point(px(x), px(y)), MouseButton::Left, Modifiers::none());
    frame(cx);
}

fn rust_card(name: &str, kind: crate::icons::Kind, signature: &str, doc: Option<&str>) -> Rc<CardFacts> {
    Rc::new(CardFacts::of(&Item::new(name, Lang::Rust).kind(Some(kind)).signature(Some(signature)), doc))
}

// ------------------------------------------------------------------ cards

#[gpui::test]
fn a_card_says_badge_words_and_never_the_code(cx: &mut TestAppContext) {
    let (cx, _) = open(cx, |_, cx, _| {
        let m = cx.facet().measure(px(400.0));
        let (name, kind, signature, doc) = TOML[0];
        symbol_card("c", rust_card(name, kind, signature, doc), &m).width(px(360.0)).into_any_element()
    });
    let said = said(cx);
    for word in ["from_str", "fn", "Deserializes a string into a type.", "takes 1", "T", "can fail"] {
        assert!(said.iter().any(|t| t == word), "the card never said `{word}`: {said:?}");
    }
    assert!(said.iter().all(|t| !t.contains("pub fn") && !t.contains("Result<") && !t.contains("where")), "code leaked onto the card: {said:?}");
}

#[gpui::test]
fn an_undocumented_card_says_so_and_a_card_in_the_past_names_what_happened(cx: &mut TestAppContext) {
    let (cx, _) = open(cx, |_, cx, _| {
        let m = cx.facet().measure(px(900.0));
        let (name, kind, signature, doc) = MPSC[9];
        let gone = Rc::new(CardFacts::of(&Item::new(name, Lang::Rust).kind(Some(kind)).signature(Some(signature)), doc).change(Some(Change::Gone)));
        div()
            .flex()
            .gap(px(12.0))
            .child(symbol_card("gone", gone, &m).width(px(300.0)).at("0.5.11"))
            .child(symbol_card("new", Rc::new((*rust_card("Fresh", crate::icons::Kind::Struct, "pub struct Fresh;", Some("Newly added."))).clone().change(Some(Change::New))), &m).width(px(300.0)).at("0.5.11"))
            .into_any_element()
    });
    let said = said(cx);
    assert!(said.iter().any(|t| t == "undocumented"), "{said:?}");
    assert!(said.iter().any(|t| t == "gone at 0.5.11"), "{said:?}");
    assert!(said.iter().any(|t| t == "new at 0.5.11"), "{said:?}");
}

#[gpui::test]
fn resting_on_a_badge_floats_its_meaning_and_moves_nothing(cx: &mut TestAppContext) {
    let (cx, _) = open(cx, |_, cx, _| {
        let m = cx.facet().measure(px(400.0));
        let (name, kind, signature, doc) = TOML[0];
        symbol_card("c", rust_card(name, kind, signature, doc), &m).width(px(360.0)).into_any_element()
    });
    // The tip of the third badge (`can fail`) lives on the float layer under this key.
    let tip = gpui::ElementId::NamedChild(std::sync::Arc::new(super::text::key(&gpui::ElementId::Name("c".into()), "badge-2")), "tip".into());
    let floating = |cx: &mut VisualTestContext| cx.update(|window, cx| float::is_open(&tip, window, cx));
    let card = |cx: &mut VisualTestContext| -> Vec<(String, [f32; 4])> {
        ledger(cx).texts.iter().filter(|t| t.key.starts_with("c-")).map(|t| (t.key.clone(), [t.bounds.x, t.bounds.y, t.bounds.width, t.bounds.height])).collect()
    };
    let before = card(cx);
    assert!(!floating(cx), "a meaning was open at rest");
    let (_, x, y, w, h) = text_at(cx, "badge-2-word").expect("the can-fail badge");
    move_to(cx, x + w * 0.5, y + h * 0.5);
    advance(cx, 60);
    assert!(!floating(cx), "the meaning opened with no rest: a pointer passing over a badge flashes it");
    advance(cx, 500);
    assert!(floating(cx), "the meaning never floated");
    // The card lifts a little under a pointer, as one piece: nothing on it moves against the rest.
    let after = card(cx);
    assert_eq!(after.iter().map(|(k, _)| k).collect::<Vec<_>>(), before.iter().map(|(k, _)| k).collect::<Vec<_>>());
    let lift = (after[0].1[0] - before[0].1[0], after[0].1[1] - before[0].1[1]);
    for ((key, was), (_, is)) in before.iter().zip(&after) {
        assert!((is[0] - was[0] - lift.0).abs() < 0.6 && (is[1] - was[1] - lift.1).abs() < 0.6 && (is[2] - was[2]).abs() < 0.6 && (is[3] - was[3]).abs() < 0.6, "`{key}` moved against the card ({was:?} to {is:?}, the card lifted {lift:?}): the tip must float, never reflow");
    }
    move_to(cx, 5.0, 690.0);
    advance(cx, 600);
    assert!(!floating(cx), "the meaning stayed open after the pointer left");
}

// ------------------------------------------------------------------ shingles

fn map() -> impl Fn(&mut Window, &mut Context<Page>, &Log) -> AnyElement {
    move |_, cx, log| {
        let m = cx.facet().measure(px(WIDTH - 40.0));
        let modules: Vec<ModuleFacts> = fixture::TOKIO_MODULES
            .iter()
            .enumerate()
            .map(|(i, (name, n))| {
                let mut module = ModuleFacts::new(*name, (0..*n).map(|j| ShingleFacts { name: format!("Item{i}_{j}").into(), family: Family::Type, yours: if j == 2 { Use::Yours } else { Use::Elsewhere }, state: None }).collect());
                if i == 0 {
                    module.doc = Some("A multi-producer queue.".into());
                }
                module.extent = if *n > 14 { Extent::Page } else { Extent::Inline };
                module
            })
            .collect();
        let log = log.clone();
        shingles("map", modules.into(), &m).on_open(move |module, item, _, _| log.borrow_mut().push(format!("open {module} {item:?}"))).into_any_element()
    }
}

#[gpui::test]
fn resting_on_a_region_reads_the_module_in_the_foot_and_a_click_opens_it(cx: &mut TestAppContext) {
    let (cx, log) = open(cx, map());
    let names = said(cx);
    assert!(names.iter().any(|t| t == "sync::mpsc") && names.iter().any(|t| t == "task::join_set"), "every module's label is drawn whole: {names:?}");
    assert!(!said(cx).iter().any(|t| t.contains("you use")), "the foot is quiet at rest");
    let (_, x, y, w, h) = text_at(cx, "region-sync::mpsc").expect("the first region's label");
    move_to(cx, x + w * 0.5, y + h * 0.5);
    advance(cx, 60);
    assert_eq!(text_at(cx, "foot-name").map(|t| t.0), Some("sync::mpsc".to_owned()));
    assert_eq!(text_at(cx, "foot-doc").map(|t| t.0), Some("A multi-producer queue.".to_owned()));
    assert_eq!(text_at(cx, "foot-more").map(|t| t.0), Some("15 types · you use 1".to_owned()));
    assert_eq!(text_at(cx, "foot-go").map(|t| t.0), Some("opens its own page".to_owned()), "15 names is over the dedicated-page line");
    click(cx, x + w * 0.5, y + h * 0.5);
    assert_eq!(log.borrow().as_slice(), ["open 0 None"]);
}

#[gpui::test]
fn resting_on_a_shingle_names_it_on_a_plate_and_a_click_opens_its_module_at_it(cx: &mut TestAppContext) {
    let (cx, log) = open(cx, map());
    let shingle = ledger(cx).bounds.iter().find(|b| b.key.ends_with("shingle-1-2")).expect("the third shingle of the second region").clone();
    let (x, y) = (shingle.x + shingle.width * 0.5, shingle.y + shingle.height * 0.5);
    move_to(cx, x, y);
    advance(cx, 60);
    assert_eq!(text_at(cx, "plate").map(|t| t.0), Some("Item1_2".to_owned()), "a plate names the shingle");
    click(cx, x, y);
    assert_eq!(log.borrow().as_slice(), ["open 1 Some(2)"]);
}

/// The keyboard is the host's (one system for the page): the region it stands
/// on is rested on, and the foot reads it as it reads a hovered one.
#[gpui::test]
fn a_region_the_host_rests_on_reads_itself_in_the_foot(cx: &mut TestAppContext) {
    let (cx, _) = open(cx, |_, cx, _| {
        let m = cx.facet().measure(px(WIDTH - 40.0));
        let modules: Vec<ModuleFacts> = fixture::TOKIO_MODULES.iter().enumerate().map(|(i, (name, n))| ModuleFacts::new(*name, fixture::shingles(i, *n))).collect();
        shingles("map", modules.into(), &m).rest(Some(crate::folio::shingles::Spot::Region(1))).into_any_element()
    });
    assert_eq!(text_at(cx, "foot-name").map(|t| t.0), Some(fixture::TOKIO_MODULES[1].0.to_owned()), "the foot names the region the host rests on");
}

// ------------------------------------------------------------------ ticker

fn releases(dated: bool) -> Rc<TickerFacts> {
    let rows: Vec<Release> = fixture::TOKIO_RELEASES
        .iter()
        .map(|(v, d)| Release {
            version: (*v).to_owned(),
            date: dated.then(|| (*d).to_owned()),
            standing: if *v == "0.1.3" { Standing::Yanked } else { Standing::Available },
            names: if *v == "0.1.7" { Names::Unread } else { Names::Read },
        })
        .collect();
    Rc::new(TickerFacts::new(&rows, Some("1.47.0"), "2026-09-28"))
}

fn ticker_page(facts: Rc<TickerFacts>) -> impl Fn(&mut Window, &mut Context<Page>, &Log) -> AnyElement {
    move |_, cx, log| {
        let m = cx.facet().measure(px(WIDTH - 40.0));
        let log = log.clone();
        let facts = facts.clone();
        let names: Vec<String> = facts.ticks.iter().map(|t| t.version.to_string()).collect();
        ticker("tick", facts, &m).on_travel(move |i, _, _| log.borrow_mut().push(format!("travel {}", names[i]))).into_any_element()
    }
}

fn bar(cx: &mut VisualTestContext, version: &str) -> (f32, f32) {
    let ledger = ledger(cx);
    let b = ledger.bounds.iter().find(|b| b.key.ends_with(&format!("bar-{version}"))).unwrap_or_else(|| panic!("no bar {version}")).clone();
    (b.x + b.width * 0.5, b.y + b.height * 0.7)
}

#[gpui::test]
fn the_ticker_label_rides_the_release_under_the_pointer_and_leaving_takes_it_away(cx: &mut TestAppContext) {
    let (cx, _) = open(cx, ticker_page(releases(true)));
    assert!(!said(cx).iter().any(|t| t.contains("2023-04-19")), "a label was up at rest");
    let (x, y) = bar(cx, "1.28.0");
    move_to(cx, x, y);
    advance(cx, 200);
    let label = text_at(cx, "label").map(|t| t.0).expect("a label rides the hot release");
    assert!(label.contains("1.28.0") && label.contains("2023-04-19") && label.contains("features") && label.contains("3.4 years ago") || label.contains("years ago"), "{label}");
    assert!(label.ends_with("read"), "{label}");
    move_to(cx, 5.0, 690.0);
    advance(cx, 300);
    assert!(text_at(cx, "label").is_none(), "the label outlived the pointer");
}

#[gpui::test]
fn pressing_and_dragging_travels_to_each_release_the_pointer_crosses(cx: &mut TestAppContext) {
    let (cx, log) = open(cx, ticker_page(releases(true)));
    let (x1, y1) = bar(cx, "1.28.0");
    cx.simulate_mouse_move(point(px(x1), px(y1)), None, Modifiers::none());
    advance(cx, 120);
    let (x1, y1) = bar(cx, "1.28.0");
    cx.simulate_mouse_down(point(px(x1), px(y1)), MouseButton::Left, Modifiers::none());
    frame(cx);
    assert_eq!(log.borrow().last().map(String::as_str), Some("travel 1.28.0"), "{:?}", log.borrow());
    // Dragging right, well past the next bars, travels on to a later release.
    let (mut x2, y2) = (x1, y1);
    for _ in 0..8 {
        x2 += 9.0;
        cx.simulate_mouse_move(point(px(x2), px(y2)), Some(MouseButton::Left), Modifiers::none());
        frame(cx);
    }
    let travelled: Vec<String> = log.borrow().clone();
    let last = travelled.last().and_then(|l| l.strip_prefix("travel ")).unwrap_or("").to_owned();
    assert!(travelled.len() >= 2 && crate::marks::semver::cmp(&last, "1.28.0").is_gt(), "the drag never reached a later release: {travelled:?}");
    cx.simulate_mouse_up(point(px(x2), px(y2)), MouseButton::Left, Modifiers::none());
    frame(cx);
}

#[gpui::test]
fn the_label_of_the_pin_and_of_the_newest_say_so(cx: &mut TestAppContext) {
    let (cx, _) = open(cx, ticker_page(releases(true)));
    let (x, y) = bar(cx, "1.47.0");
    move_to(cx, x, y);
    advance(cx, 200);
    let label = text_at(cx, "label").map(|t| t.0).expect("a label rides the pin");
    assert!(label.starts_with("1.47.0") && label.contains("your pin"), "the release you pin says so: {label}");
    // The bars are magnified under a pointer: find the newest where it rests.
    move_to(cx, 5.0, 690.0);
    advance(cx, 400);
    let (x, y) = bar(cx, "1.53.1");
    move_to(cx, x, y);
    advance(cx, 200);
    let label = text_at(cx, "label").map(|t| t.0).unwrap_or_else(|| panic!("a label rides the newest: {:?}", said(cx)));
    assert!(label.starts_with("1.53.1") && label.contains("newest") && !label.contains("your pin"), "the newest says so: {label}");
}

#[gpui::test]
fn the_pin_and_the_newest_wear_flags_and_the_one_being_read_wears_a_caret(cx: &mut TestAppContext) {
    let (cx, _) = open(cx, |_, cx, _| {
        let m = cx.facet().measure(px(WIDTH - 40.0));
        let facts = Rc::new((*releases(true)).clone().reading(Some("1.9.0")));
        ticker("tick", facts, &m).into_any_element()
    });
    let said = said(cx);
    assert!(said.iter().any(|t| t == "your pin 1.47.0"), "{said:?}");
    assert!(said.iter().any(|t| t == "newest"), "the newest is near the pin, so its flag keeps only its first word: {said:?}");
    assert!(said.iter().any(|t| t == "2021"), "the years mark the axis: {said:?}");
}

#[gpui::test]
fn undated_releases_show_their_ends_not_invented_years(cx: &mut TestAppContext) {
    let (cx, _) = open(cx, ticker_page(releases(false)));
    let said = said(cx);
    assert!(said.iter().any(|t| t == "0.1.0") && said.iter().any(|t| t == "1.53.1"), "{said:?}");
    assert!(!said.iter().any(|t| t.len() == 4 && t.starts_with("20")), "an undated ticker named a year: {said:?}");
}

// ------------------------------------------------------------------ crest

#[gpui::test]
fn the_licence_stamp_unfolds_in_place_and_folds_back(cx: &mut TestAppContext) {
    let (cx, _) = open(cx, |_, cx, _| {
        let m = cx.facet().measure(px(WIDTH - 40.0));
        stamp("lic", Rc::new(LicenseFacts::new(Some("MIT OR Apache-2.0"), Some("MIT OR Apache-2.0"), "backend")), px(330.0), &m).into_any_element()
    });
    let at_rest = said(cx);
    assert!(at_rest.iter().any(|t| t == "Permissive") && at_rest.iter().any(|t| t == "MIT"), "{at_rest:?}");
    assert!(!at_rest.iter().any(|t| t.contains("keep the notice")), "the stamp was unfolded at rest: {at_rest:?}");
    let (_, x, y, w, h) = text_at(cx, "verdict").expect("the verdict");
    move_to(cx, x + w * 0.5, y + h * 0.5);
    advance(cx, 400);
    let open = said(cx);
    assert!(open.iter().any(|t| t.contains("Either fits your MIT/Apache-2.0 project")), "the fit sentence never unfolded: {open:?}");
    assert!(open.iter().any(|t| t.contains("keep the notice")), "{open:?}");
    move_to(cx, 5.0, 690.0);
    advance(cx, 500);
    assert!(!said(cx).iter().any(|t| t.contains("keep the notice")), "the stamp stayed unfolded");
}

#[gpui::test]
fn a_pointer_only_passing_over_the_stamp_or_the_hand_opens_nothing_and_resting_opens_it(cx: &mut TestAppContext) {
    let (cx, _) = open(cx, |_, cx, _| {
        let m = cx.facet().measure(px(WIDTH - 40.0));
        div()
            .flex()
            .gap(px(20.0))
            .child(stamp("lic", Rc::new(LicenseFacts::new(Some("MIT OR Apache-2.0"), Some("MIT OR Apache-2.0"), "backend")), px(330.0), &m))
            .child(heads("heads", "tokio", Rc::new(findings(&signals())), px(230.0), px(500.0), Nominal::px(124.0), &m))
            .into_any_element()
    });
    let words = ["keep the notice", "Starts programs"];
    let open = |cx: &mut VisualTestContext| words.iter().any(|w| said(cx).iter().any(|t| t.contains(w)));
    // Across the stamp and out again in a few frames: nothing opens, at any moment.
    let (_, x, y, w, h) = text_at(cx, "verdict").expect("the verdict");
    for step in 0..8 {
        move_to(cx, x + w * 0.5 - 40.0 + step as f32 * 12.0, y + h * 0.5);
        advance(cx, 8);
        assert!(!open(cx), "a plate opened while the pointer was only passing over it (step {step})");
    }
    move_to(cx, 5.0, 690.0);
    advance(cx, 400);
    assert!(!open(cx), "a plate opened after the pointer had gone");
    // Resting opens it, after the house rest.
    move_to(cx, x + w * 0.5, y + h * 0.5);
    advance(cx, 40);
    assert!(!open(cx), "it opened before the house rest");
    advance(cx, 400);
    assert!(open(cx), "resting on it never opened it");
}

fn signals() -> Signals {
    let place = |file: &str, line: usize, text: &str| Place { file: file.to_owned().into(), line, text: text.to_owned().into() };
    Signals {
        unsafe_count: 1047,
        process: Sighting { count: 4, places: vec![place("src/process/unix/pidfd_reaper.rs", 234, "Command::new(\"uname\")")] },
        net: Sighting { count: 141, places: vec![place("src/net/addr.rs", 3, "use std::net::SocketAddr;")] },
        files: Sighting { count: 81, places: vec![] },
        ..Signals::default()
    }
}

#[gpui::test]
fn the_heads_up_hand_fans_out_with_words_and_a_click_opens_the_sheet_of_evidence(cx: &mut TestAppContext) {
    let (cx, _) = open(cx, |_, cx, _| {
        let m = cx.facet().measure(px(WIDTH - 40.0));
        heads("heads", "tokio", Rc::new(findings(&signals())), px(230.0), px(900.0), Nominal::px(124.0), &m).into_any_element()
    });
    let rest = said(cx);
    assert!(rest.iter().any(|t| t == "HEADS-UP") && rest.iter().any(|t| t == "2"), "the label carries the count of warnings (programs, and unsafe past fifty): {rest:?}");
    assert!(!rest.iter().any(|t| t == "Starts programs"), "words were out at rest: {rest:?}");
    move_to(cx, 60.0, 70.0);
    advance(cx, 400);
    let fanned = said(cx);
    for word in ["Starts programs", "Opens network connections", "Touches files", "Unsafe blocks"] {
        assert!(fanned.iter().any(|t| t == word), "`{word}` did not fan out: {fanned:?}");
    }
    click(cx, 60.0, 70.0);
    advance(cx, 500);
    assert!(cx.update(|window, cx| dialog::is_open(window, cx)), "a click opens the sheet");
    let sheet = said(cx);
    assert!(sheet.iter().any(|t| t == "4 places" || t == "141 places"), "each finding counts its places: {sheet:?}");
    assert!(sheet.iter().any(|t| t == "It spawns other programs."), "and says why it matters: {sheet:?}");
    assert!(sheet.iter().any(|t| t == "src/process/unix/pidfd_reaper.rs:234"), "the evidence names its file and line: {sheet:?}");
    assert!(sheet.iter().any(|t| t == "src/net/addr.rs:3"), "{sheet:?}");
    assert!(sheet.iter().any(|t| t == "Command::new(\"uname\")"), "and shows the line: {sheet:?}");
}

/// A hand of one finding says it at rest (a lone icon in an otherwise empty
/// tile says nothing); a hand of several keeps to icons until it is rested on
/// (`the_heads_up_hand_fans_out...`).
#[gpui::test]
fn a_lone_finding_says_its_words_at_rest(cx: &mut TestAppContext) {
    let (cx, _) = open(cx, |_, cx, _| {
        let m = cx.facet().measure(px(WIDTH - 40.0));
        let forbids = Signals { unsafe_code: Unsafe::Forbidden, ..Signals::default() };
        heads("heads", "present", Rc::new(findings(&forbids)), px(246.0), px(900.0), Nominal::px(124.0), &m).into_any_element()
    });
    let rest = said(cx);
    assert!(rest.iter().any(|t| t == "Forbids unsafe code"), "the only finding is said, not just drawn: {rest:?}");
}

// ------------------------------------------------------------------ features

fn tokio_features() -> Rc<FeatureFacts> {
    Rc::new(FeatureFacts {
        names: fixture::TOKIO_FEATURES.iter().map(|(n, _, _)| (*n).to_owned()).collect(),
        default: Vec::new(),
        graph: fixture::TOKIO_FEATURES.iter().map(|(n, e, d)| ((*n).to_owned(), FeatureNode { enables: e.iter().map(ToString::to_string).collect(), deps: d.iter().map(ToString::to_string).collect() })).collect(),
        sizes: fixture::TOKIO_DEP_LINES.iter().map(|(n, l)| ((*n).to_owned(), Some(*l))).collect(),
    })
}

fn chip(cx: &mut VisualTestContext, name: &str) -> (f32, f32) {
    let ledger = ledger(cx);
    let found = ledger.texts.iter().find(|t| t.content == name && t.key.contains("chip-") && t.key.ends_with("name")).unwrap_or_else(|| panic!("no chip {name}: {:?}", ledger.texts.iter().map(|t| t.content.clone()).collect::<Vec<_>>())).clone();
    (found.bounds.x + found.bounds.width * 0.5, found.bounds.y + found.bounds.height * 0.5)
}

#[gpui::test]
fn turning_full_on_locks_what_it_needs_counts_what_it_pulls_in_and_a_locked_chip_says_who_holds_it(cx: &mut TestAppContext) {
    let (cx, _) = open(cx, |_, cx, _| {
        let m = cx.facet().measure(px(WIDTH - 40.0));
        features("fb", tokio_features(), px(WIDTH - 40.0), &m).into_any_element()
    });
    assert!(says(cx, "of 25 on") && says(cx, "0"), "{:?}", said(cx));
    let (x, y) = chip(cx, "full");
    click(cx, x, y);
    advance(cx, 500);
    let after = said(cx);
    assert!(after.iter().any(|t| t == "12") && after.iter().any(|t| t == "of 25 on"), "twelve features on: {after:?}");
    assert!(after.iter().any(|t| t == "8") && after.iter().any(|t| t == "(471K lines)"), "it pulls in eight packages, 471K lines: {after:?}");
    // A held chip does not turn off: it shakes and names who holds it.
    let (nx, ny) = chip(cx, "net");
    click(cx, nx, ny);
    advance(cx, 400);
    assert!(says(cx, "net is held on by full"), "{:?}", said(cx));
    assert!(says(cx, "12"), "the count did not change: {:?}", said(cx));
    // Turning `full` back off releases everything.
    let (fx, fy) = chip(cx, "full");
    click(cx, fx, fy);
    advance(cx, 400);
    assert!(says(cx, "0") && !says(cx, "(471K lines)"), "{:?}", said(cx));
}

// ------------------------------------------------------------------ berg

fn tokio_berg() -> Rc<BergFacts> {
    Rc::new(BergFacts {
        name: "tokio".into(),
        own: 49_187,
        blocks: fixture::TOKIO_BERG
            .iter()
            .map(|(name, sloc, layer, parent, deps)| BergBlock { name: (*name).into(), version: "1.0.0".into(), sloc: *sloc, layer: *layer, parent: *parent, deps: deps.to_vec() })
            .collect(),
        missing: 0,
        basis: Some(Basis::Lock),
    })
}

#[gpui::test]
fn the_berg_names_a_block_on_a_plate_and_says_what_share_of_the_weight_it_carries(cx: &mut TestAppContext) {
    let (cx, log) = open(cx, |_, cx, log| {
        let m = cx.facet().measure(px(WIDTH - 40.0));
        let log = log.clone();
        berg("berg", tokio_berg(), &m).on_go(move |i, _, _| log.borrow_mut().push(format!("go {i}"))).into_any_element()
    });
    let calm = text_at(cx, "caption").map(|t| t.0).expect("a caption");
    assert!(calm.contains("above water") && calm.contains("lines beneath in 20 packages"), "{calm}");
    let windows = ledger(cx).bounds.iter().find(|b| b.key.ends_with("block-9")).expect("the windows-sys block").clone();
    move_to(cx, windows.x + windows.width * 0.5, windows.y + windows.height * 0.5);
    advance(cx, 300);
    assert_eq!(text_at(cx, "plate").map(|t| t.0), Some("windows-sys · 334K".to_owned()));
    let hot = text_at(cx, "caption").map(|t| t.0).expect("a caption");
    assert!(hot.starts_with("windows-sys carries 62% of the weight beneath (335K lines, 1 package under it)"), "the share it carries is a number that adds up: {hot}");
    assert!(hot.contains("reached via mio") && hot.contains("click to go there"), "{hot}");
    click(cx, windows.x + windows.width * 0.5, windows.y + windows.height * 0.5);
    assert_eq!(log.borrow().as_slice(), ["go 9"]);
}
