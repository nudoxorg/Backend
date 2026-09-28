//! The marks, rendered: every assertion reads what was painted (the probe's
//! text ledger), not the data it was built from.

use super::deps::{DepFacts, DepKind, dep_line};
use super::eco::{Eco, EcoFacts, ecosystem_mark};
use super::gallery::stage;
use super::license::{LicenseFacts, license_mark};
use super::version::{VersionFacts, version_mark};
use super::{card, fixture, spdx};
use crate::gallery::{Frame, Scene, Shot, capture};
use crate::probe::{BoundsSample, TextSample};
use backend_gui_harness::Script;
use gpui::{AnyView, App, IntoElement, ParentElement, SharedString, Styled, Window, div, px};
use std::sync::Mutex;

fn run(build: fn(&mut Window, &mut App) -> AnyView, size: (u32, u32), times: &[u64], script: &str) -> Vec<Frame> {
    let scene = Scene { id: "marks-test", title: "marks test", size, build };
    run_scene(&scene, times, script)
}

// The headless platform owns process-global native state: this module's
// captures run one at a time.
static PLATFORM: Mutex<()> = Mutex::new(());

fn run_scene(scene: &Scene, times: &[u64], script: &str) -> Vec<Frame> {
    let _platform = PLATFORM.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut shot = Shot::new(scene);
    shot.scale = 1;
    shot.probe = true;
    shot.times = times.to_vec();
    shot.script = Some(Script::parse(script).unwrap_or_else(|error| panic!("script {script:?}: {error}")));
    capture(scene, &shot).unwrap_or_else(|error| panic!("{} captures: {error}", scene.id))
}

fn board(id: &str) -> Scene {
    crate::gallery::find(id).unwrap_or_else(|| panic!("scene {id} is registered"))
}

/// Every painted text whose key ends with `key`.
fn painted<'a>(frame: &'a Frame, key: &str) -> Vec<&'a TextSample> {
    frame.ledger.texts.iter().filter(|t| t.key == key || t.key.ends_with(&format!("-{key}")) || t.key.ends_with(key)).collect()
}

fn contents(frame: &Frame, key: &str) -> Vec<String> {
    painted(frame, key).iter().map(|t| t.content.clone()).collect()
}

fn one(frame: &Frame, key: &str) -> String {
    let all = contents(frame, key);
    assert_eq!(all.len(), 1, "exactly one {key} painted, got {all:?}");
    all[0].clone()
}

/// The part of a text that can be seen: its box cut by the mask it was
/// painted under.
fn visible(t: &TextSample) -> Option<BoundsSample> {
    let b = &t.bounds;
    let Some(c) = &t.paint_clip else { return Some(b.clone()) };
    let x0 = b.x.max(c.x);
    let y0 = b.y.max(c.y);
    let x1 = (b.x + b.width).min(c.x + c.width);
    let y1 = (b.y + b.height).min(c.y + c.height);
    (x1 - x0 > 0.5 && y1 - y0 > 0.5).then(|| BoundsSample { key: t.key.clone(), x: x0, y: y0, width: x1 - x0, height: y1 - y0 })
}

// ------------------------------------------------------------------ ecosystem

fn eco_open(_: &mut Window, cx: &mut App) -> AnyView {
    stage(
        |measure, _, _| {
            div()
                .absolute()
                .left(px(40.0))
                .top(px(40.0))
                .child(ecosystem_mark("eco", fixture::eco("toml"), measure).open_look())
                .into_any_element()
        },
        cx,
    )
}

#[test]
fn the_ecosystem_card_says_what_it_is_where_it_lives_and_how_to_install_it() {
    let frames = run(eco_open, (700, 400), &[900], "");
    let f = &frames[0];
    assert_eq!(one(f, "mk-eco-kind"), "Rust crate");
    assert_eq!(one(f, "mk-eco-where"), "crates.io");
    assert_eq!(one(f, "mk-eco-install"), "cargo add toml");
    assert_eq!(one(f, "eco-word"), "crates.io", "the hero mark carries its word");
}

fn quiet_rows(_: &mut Window, cx: &mut App) -> AnyView {
    stage(
        |measure, _, _| {
            div()
                .absolute()
                .left(px(40.0))
                .top(px(40.0))
                .flex()
                .flex_col()
                .gap(px(44.0))
                .child(ecosystem_mark("quiet", fixture::eco("toml"), measure).quiet())
                .child(ecosystem_mark("loud", fixture::eco("toml"), measure).glyph_only())
                .into_any_element()
        },
        cx,
    )
}

#[test]
fn a_quiet_mark_never_opens_a_card_where_its_loud_twin_does() {
    // Resting on the quiet stone, long past the hover delay and the unfurl.
    let quiet = run(quiet_rows, (600, 400), &[1000], "move 48,48 @10");
    assert!(
        contents(&quiet[0], "mk-eco-kind").is_empty(),
        "a quiet mark opened a card: {:?}",
        contents(&quiet[0], "mk-eco-kind")
    );
    // The same rest on the loud one (60 px lower) opens its card: the pointer
    // path works, so the silence above is the quiet option's.
    let loud = run(quiet_rows, (600, 400), &[1000], "move 48,108 @10");
    assert_eq!(one(&loud[0], "mk-eco-kind"), "Rust crate");
}

// ------------------------------------------------------------------ license

fn license_open(_: &mut Window, cx: &mut App) -> AnyView {
    stage(
        |measure, _, _| {
            let facts = fixture::license(fixture::package("toml").license.as_deref(), Some("toml"));
            div().absolute().left(px(40.0)).top(px(40.0)).child(license_mark("lic", facts, measure).open_look()).into_any_element()
        },
        cx,
    )
}

#[test]
fn a_license_choice_names_the_option_it_assumed_and_the_card_ends_with_the_hedge() {
    let frames = run(license_open, (700, 600), &[900], "");
    let f = &frames[0];
    assert_eq!(one(f, "mk-lic-title"), "MIT OR Apache-2.0");
    assert_eq!(one(f, "mk-lic-place"), "your choice of two");
    assert_eq!(one(f, "mk-lic-fit"), "Either fits your MIT/Apache-2.0 project; Apache-2.0 adds a patent grant.");
    assert_eq!(one(f, "mk-lic-assumed"), "assuming you take it under MIT");
    assert_eq!(one(f, "mk-lic-hedge"), spdx::HEDGE);
    assert_eq!(spdx::HEDGE, "a plain-words summary, not legal advice");
    // The hedge is the card's foot: nothing it says sits below it.
    let hedge = painted(f, "mk-lic-hedge")[0].bounds.y;
    let lowest = f.ledger.texts.iter().filter(|t| t.key.starts_with("mk-lic")).map(|t| t.bounds.y).fold(0.0_f32, f32::max);
    assert!((hedge - lowest).abs() < 0.5, "the hedge ({hedge}) is not the lowest line ({lowest})");
    assert_eq!(one(f, "lic-word"), "MIT/Apache-2.0");
}

fn copyleft_known_yours(_: &mut Window, cx: &mut App) -> AnyView {
    stage(
        |measure, _, _| {
            let facts = LicenseFacts::new(Some("GPL-2.0"), Some("MIT"), "present");
            div().absolute().left(px(40.0)).top(px(40.0)).child(license_mark("gpl-known", facts, measure).open_look()).into_any_element()
        },
        cx,
    )
}

fn copyleft_unknown_yours(_: &mut Window, cx: &mut App) -> AnyView {
    stage(
        |measure, _, _| {
            let facts = LicenseFacts::new(Some("GPL-2.0"), None, "present");
            div().absolute().left(px(40.0)).top(px(40.0)).child(license_mark("gpl-unknown", facts, measure).open_look()).into_any_element()
        },
        cx,
    )
}

#[test]
fn a_known_project_license_reads_the_real_copyleft_consequence() {
    let frames = run(copyleft_known_yours, (700, 600), &[900], "");
    assert_eq!(
        one(&frames[0], "mk-lic-fit"),
        "Shipping it would make present GPL-2.0: anyone you ship to could ask for all of its source."
    );
}

#[test]
fn an_unknown_project_license_omits_the_fit_line_rather_than_guess() {
    let frames = run(copyleft_unknown_yours, (700, 600), &[900], "");
    assert!(
        contents(&frames[0], "mk-lic-fit").is_empty(),
        "the fit line must not be drawn when the reader's own license is not known: {:?}",
        contents(&frames[0], "mk-lic-fit")
    );
    // Everything else about the dependency's own license still reads.
    assert_eq!(one(&frames[0], "mk-lic-title"), "GNU GPL v2.0");
}

#[test]
fn every_license_card_on_the_board_reads_the_tree_and_hedges() {
    let frames = run_scene(&board("marks-license"), &[20], "");
    let f = &frames[0];
    let mut assumed = contents(f, "mk-lic-assumed");
    assumed.sort();
    assert_eq!(
        assumed,
        vec![
            "assuming you take it under Apache-2.0".to_owned(),
            "assuming you take it under MIT".to_owned(),
            "assuming you take it under MIT".to_owned()
        ],
        "toml and unicode-ident assume MIT, self_cell Apache-2.0; MPL, a license file and your own project assume nothing"
    );
    assert_eq!(contents(f, "mk-lic-hedge").len(), 6, "all six cards carry the hedge");
    let tree = contents(f, "mk-lic-tree");
    assert!(
        tree.contains(&"One of three packages in your tree with weak copyleft terms; the others are cbindgen (build-time only) and dwrote.".to_owned()),
        "{tree:?}"
    );
    assert!(
        tree.contains(&"1,189 packages: permissive throughout, plus MPL-2.0 in dwrote and option-ext (cbindgen too, at build time only) and custom terms in cfg_block.".to_owned()),
        "{tree:?}"
    );
    assert!(tree.contains(&"The only package in your tree with custom terms.".to_owned()), "{tree:?}");
    assert_eq!(one(f, "mk-lic-unread"), "32 are not unpacked here, so unread.");
    let titles = contents(f, "mk-lic-title");
    assert!(titles.contains(&"Custom terms".to_owned()) && titles.contains(&"Mozilla Public License 2.0".to_owned()), "{titles:?}");
    assert!(contents(f, "mk-lic-place").contains(&"license-file = \"LICENSE\"".to_owned()));
    assert!(contents(f, "mk-lic-fit").contains(
        &"Choose Apache-2.0: it fits your MIT/Apache-2.0 project. GPL-2.0 would make present GPL-2.0.".to_owned()
    ));
}

// ------------------------------------------------------------------ version

#[test]
fn the_version_cards_read_the_history_honestly() {
    let frames = run_scene(&board("marks-version"), &[20], "");
    let f = &frames[0];
    let read = contents(f, "mk-ver-read");
    for line in [
        "28 releases behind · two of them breaking · 15 months",
        "Up to date · the newest release",
        "Not in your tree · 707 releases · the newest 17 days ago",
    ] {
        assert!(read.contains(&line.to_owned()), "missing {line:?} in {read:?}");
    }
    let yours = contents(f, "mk-ver-yours");
    assert!(yours.contains(&"None of your 80 uses change; 4 calls to from_str are respelled, not changed.".to_owned()), "{yours:?}");
    assert!(
        yours.contains(&"No API changes through 1.16.1; 1.16.2 is not measured yet.".to_owned()),
        "smallvec: no API changes, said only as far as measured: {yours:?}"
    );
    assert!(contents(f, "mk-also-fact").contains(&"Neither is yours to move: 60 crates still ask for 2.x.".to_owned()));
    assert!(contents(f, "mk-also-fact").contains(&"Moving yours to 1.1.5 drops a copy; none of your 80 uses change.".to_owned()));
    assert!(contents(f, "mk-also-say").contains(
        &"Two copies of toml compile. 1.1.5 comes through embed-resource, ra_ap_project_model and trybuild; your 4 packages pin 0.8.23.".to_owned()
    ));
    // The rider's number is the pin's own version.
    assert!(contents(f, "v-rider-comb-number").contains(&"0.8.23".to_owned()));
}

// ------------------------------------------------------------------ unknowns

fn unknowns(_: &mut Window, cx: &mut App) -> AnyView {
    stage(
        |measure, window, cx| {
            let mystery = VersionFacts {
                name: "mystery".into(),
                releases: Vec::new(),
                pin: Some("0.3.0".into()),
                also: Vec::new(),
                yours: Vec::new(),
                pin_via: Vec::new(),
                measured: None,
                local: None,
                now: fixture::FIXTURE.now.clone().into(),
            };
            let dep = DepFacts {
                name: "unread".into(),
                kind: DepKind::Normal,
                req: None,
                resolved: None,
                newest: None,
                optional: false,
                features: Vec::new(),
                on_by_default: None,
                on_in_tree: None,
                uses: None,
                items: Vec::new(),
                purpose: None,
                local: false,
                in_tree: None,
                tree_note: None,
                target: Some("crates.io/unread".into()),
            };
            let bare = EcoFacts::new(Eco::Npm, None);
            let unlicensed = LicenseFacts::new(None, Some("MIT OR Apache-2.0"), "present");
            div()
                .absolute()
                .left(px(40.0))
                .top(px(40.0))
                .flex()
                .gap(px(30.0))
                .child(version_mark("mystery", mystery, &measure.within(px(300.0))).number_look())
                .child(card::in_place(&super::deps::board_card(&dep, "present"), window, cx))
                .child(card::in_place(&super::eco::board_card(&bare), window, cx))
                .child(card::in_place(&super::license::board_card(&unlicensed), window, cx))
                .into_any_element()
        },
        cx,
    )
}

#[test]
fn what_is_not_known_is_said_as_unknown_never_invented() {
    let frames = run(unknowns, (1800, 700), &[900], "");
    let f = &frames[0];
    assert_eq!(one(f, "mk-ver-say"), "No release history is known for mystery.");
    assert_eq!(one(f, "mk-dep-say"), "What it is used for is not known yet.");
    assert_eq!(one(f, "mk-dep-uses"), "uses unknown: not read yet");
    let rows: Vec<&str> = f.ledger.texts.iter().filter(|t| t.key.starts_with("mk-dep-kv-")).map(|t| t.content.as_str()).collect();
    assert_eq!(rows, vec!["always", "not in your tree"], "no version row: nothing about it is known");
    assert!(contents(f, "mk-eco-install").is_empty(), "no install line was known, so none is drawn");
    assert_eq!(one(f, "mk-eco-where"), "npmjs.com");
    assert_eq!(one(f, "mk-lic-title"), "No license");
    assert_eq!(one(f, "mk-lic-fit"), "No license at all: by default nobody may copy it. Ask the author before you ship it.");
}

// ------------------------------------------------------------------ depends on

#[test]
fn the_fold_shows_what_fits_and_counts_the_rest() {
    let frames = run_scene(&board("marks-deps"), &[20], "");
    let f = &frames[0];
    assert_eq!(one(f, "deps-tight-more-words"), "and 3 more", "380 px holds serde and serde_spanned, then the fold");
    let tight: Vec<String> = f
        .ledger
        .texts
        .iter()
        .filter(|t| t.key.starts_with("deps-tight-dep-") && t.key.ends_with("-name"))
        .map(|t| t.content.clone())
        .collect();
    assert_eq!(tight, vec!["serde".to_owned(), "serde_spanned".to_owned()], "yours and required first, alphabetical");
    assert!(contents(f, "deps-rest-more-words").is_empty(), "640 px holds all five: no fold");
    // The narrow hero folds instead of leaving one name alone on a line.
    let hero = board("marks-hero-toml");
    let mut shot = Shot::new(&hero);
    shot.size = (480, 900);
    shot.scale = 1;
    shot.probe = true;
    shot.times = vec![20];
    shot.script = Some(Script::new());
    let narrow = {
        let _platform = PLATFORM.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        capture(&hero, &shot).expect("hero at 480 captures")
    };
    assert_eq!(one(&narrow[0], "hero-deps-more-words"), "and 2 more");
    let names: Vec<&TextSample> =
        narrow[0].ledger.texts.iter().filter(|t| t.key.starts_with("hero-deps-dep-") && t.key.ends_with("-name")).collect();
    let rows: std::collections::BTreeSet<i32> = names.iter().map(|t| t.bounds.y.round() as i32).collect();
    assert_eq!(rows.len(), 1, "the line never wraps at rest: {names:?}");
}

static OPENED: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn links(_: &mut Window, cx: &mut App) -> AnyView {
    stage(
        |measure, _, _| {
            div()
                .absolute()
                .left(px(40.0))
                .top(px(40.0))
                .child(
                    dep_line("links", fixture::deps("present"), "present", &measure.within(px(900.0))).on_open(
                        |target: &SharedString, _, _| {
                            OPENED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(target.to_string());
                        },
                    ),
                )
                .into_any_element()
        },
        cx,
    )
}

#[test]
fn a_dependency_link_goes_to_the_package_it_names() {
    // Where each name was painted…
    let first = run(links, (1000, 300), &[20], "");
    let spot = |name: &str| {
        let t = first[0]
            .ledger
            .texts
            .iter()
            .find(|t| t.key.starts_with("links-dep-") && t.key.ends_with("-name") && t.content == name)
            .unwrap_or_else(|| panic!("{name} is painted"));
        (t.bounds.x + t.bounds.width / 2.0, t.bounds.y + t.bounds.height / 2.0)
    };
    let (sx, sy) = spot("serde");
    let (lx, ly) = spot("backend-library");
    OPENED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
    // …then click them.
    let _ = run(links, (1000, 300), &[400], &format!("click {sx:.0},{sy:.0} @50; click {lx:.0},{ly:.0} @200"));
    let opened = OPENED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    assert_eq!(opened, vec!["crates.io/serde".to_owned(), "workspace/backend-library".to_owned()]);
}

static UNRESOLVED_OPENED: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn unresolved_dep_line(_: &mut Window, cx: &mut App) -> AnyView {
    stage(
        |measure, _, _| {
            let resolved = fixture::dep("present", "serde", DepKind::Normal);
            let unresolved = DepFacts { name: "unpublished-thing".into(), target: None, ..resolved.clone() };
            div()
                .absolute()
                .left(px(40.0))
                .top(px(40.0))
                .child(
                    dep_line("mix", vec![resolved, unresolved], "present", &measure.within(px(900.0))).on_open(
                        |target: &SharedString, _, _| {
                            UNRESOLVED_OPENED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(target.to_string());
                        },
                    ),
                )
                .into_any_element()
        },
        cx,
    )
}

#[test]
fn a_dependency_without_a_resolved_target_is_drawn_as_text_not_a_link() {
    // Dead end #15 (`shell/kit.rs`): a link without a place is drawn as
    // text, never as a control that looks live but goes nowhere.
    let frames = run(unresolved_dep_line, (1000, 300), &[20], "");
    let spot = |name: &str| {
        let t = frames[0]
            .ledger
            .texts
            .iter()
            .find(|t| t.key.starts_with("mix-dep-") && t.key.ends_with("-name") && t.content == name)
            .unwrap_or_else(|| panic!("{name} is painted"));
        (t.bounds.x + t.bounds.width / 2.0, t.bounds.y + t.bounds.height / 2.0)
    };
    // Both names still paint (never hidden)…
    let (ux, uy) = spot("unpublished-thing");
    let (rx, ry) = spot("serde");
    UNRESOLVED_OPENED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
    // …but clicking the unresolved one goes nowhere, while its resolved
    // neighbour on the same line still opens.
    let _ = run(unresolved_dep_line, (1000, 300), &[300], &format!("click {ux:.0},{uy:.0} @50; click {rx:.0},{ry:.0} @150"));
    let opened = UNRESOLVED_OPENED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    assert_eq!(opened, vec!["crates.io/serde".to_owned()], "only the resolved dependency's click opened anything");
}

// ------------------------------------------------------------------ motion

#[test]
fn the_copy_never_lies_over_the_line_it_came_from() {
    let times: Vec<u64> = (0..=30).map(|i| 880 + i * 20).collect();
    let frames = run_scene(&board("marks-copy-film"), &times, "");
    let mut saw_ghost = false;
    let mut saw_both_words = false;
    for f in &frames {
        let line = painted(f, "mk-eco-install");
        let ghost = painted(f, "mk-eco-ghost");
        for g in ghost.iter().filter_map(|g| visible(g)) {
            saw_ghost = true;
            for l in line.iter().filter_map(|l| visible(l)) {
                assert!(!g.overlaps(&l), "t={} the dropping copy {g:?} lies over the line {l:?}", f.time_ms);
            }
        }
        let copy: Vec<BoundsSample> = painted(f, "mk-eco-copy").iter().filter_map(|t| visible(t)).collect();
        let copied: Vec<BoundsSample> = painted(f, "mk-eco-copied").iter().filter_map(|t| visible(t)).collect();
        saw_both_words |= !copy.is_empty() && !copied.is_empty();
        for a in &copy {
            for b in &copied {
                assert!(!a.overlaps(b), "t={} the reel's words overlap: {a:?} {b:?}", f.time_ms);
            }
        }
    }
    assert!(saw_ghost, "the copy was never seen falling (the film proves nothing)");
    assert!(saw_both_words, "the reel never rolled (the film proves nothing)");
}

#[test]
fn mono_runs_render_exactly_as_written() {
    use image::GenericImageView;
    let scene = board("marks-calt");
    let mut shot = Shot::new(&scene);
    shot.scale = 2;
    shot.times = vec![0];
    shot.script = Some(Script::new());
    let frames = {
        let _platform = PLATFORM.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        capture(&scene, &shot).expect("the ligature scene captures")
    };
    let im = &frames[0].image;
    // Row k's text strip, 2x (see `gallery::calt`: rows are 80 px apart).
    let row = |k: u32| im.view(80, (30 + 80 * k + 20) * 2, 1360, 80).to_image();
    let (mono, calt_on, all_off) = (row(0), row(1), row(2));
    let (ctl_on, ctl_off, ctl_mono) = (row(3), row(4), row(5));
    assert!(ctl_on != ctl_off, "with ligatures on, `->` and `!=` join: the check can see a join");
    assert!(ctl_mono == ctl_off, "the mono role joins `->`, `=>` or `!=`: code must read as written");
    assert!(mono == all_off, "the mono role changes `Polyfill --version`");
    assert!(calt_on == all_off, "contextual alternates alone do not join `--` in GPUI");
}

// ------------------------------------------------------------------ placement and keys

fn toml_license() -> LicenseFacts {
    fixture::license(fixture::package("toml").license.as_deref(), Some("toml"))
}

fn hung_left(_: &mut Window, cx: &mut App) -> AnyView {
    stage(
        |measure, _, _| {
            div().absolute().left(px(40.0)).top(px(40.0)).child(license_mark("hung", toml_license(), measure).open_look()).into_any_element()
        },
        cx,
    )
}

fn hung_right(_: &mut Window, cx: &mut App) -> AnyView {
    stage(
        |measure, _, _| {
            div().absolute().right(px(40.0)).top(px(40.0)).child(license_mark("flip", toml_license(), measure).open_look()).into_any_element()
        },
        cx,
    )
}

/// The open card's painted plate, from the float layer's stack.
fn plate(frame: &Frame, key: &str) -> BoundsSample {
    frame
        .ledger
        .stacks
        .iter()
        .flat_map(|stack| stack.entries.iter())
        .find(|entry| entry.key == key)
        .and_then(|entry| entry.bounds.clone())
        .unwrap_or_else(|| panic!("no painted card for {key}: {:?}", frame.ledger.stacks))
}

#[test]
fn a_marks_card_hangs_from_the_mark_and_flips_to_its_end_at_the_window_edge() {
    // Room on the right: the plate starts 12 px left of the mark, so the
    // card's text lines up with the mark's (the plate pads 14 px).
    let left = run(hung_left, (900, 600), &[900], "");
    let card = plate(&left[0], "hung-card");
    assert!((card.x - 28.0).abs() < 0.6, "plate at {card:?}, mark at x = 40");
    let hedge = &painted(&left[0], "mk-lic-hedge")[0].bounds;
    assert!((hedge.x - 42.0).abs() < 0.6, "the card's text starts at {}, the mark's at 40 (+2)", hedge.x);
    // Against the right edge: the plate ends 12 px past the mark's end.
    let right = run(hung_right, (900, 600), &[900], "");
    let card = plate(&right[0], "flip-card");
    let word = &painted(&right[0], "flip-word")[0].bounds;
    let mark_end = word.x + word.width;
    assert!((mark_end - 860.0).abs() < 1.0, "the mark ends at {mark_end}, placed 40 px from the right");
    assert!((card.x + card.width - (mark_end + 12.0)).abs() < 1.0, "plate {card:?} does not end 12 px past the mark ({mark_end})");
    assert!(card.x + card.width <= 892.0 + 0.01, "the plate stays inside the window's margin");
}

fn keyed(_: &mut Window, cx: &mut App) -> AnyView {
    stage(
        |measure, _, _| div().absolute().left(px(40.0)).top(px(40.0)).child(license_mark("keyed", toml_license(), measure)).into_any_element(),
        cx,
    )
}

#[test]
fn enter_or_space_on_a_focused_mark_opens_its_card() {
    // Tab alone focuses the mark and opens nothing.
    let tab = run(keyed, (700, 600), &[700], "key tab @10");
    assert!(contents(&tab[0], "mk-lic-title").is_empty(), "focus alone opened the card");
    for key in ["enter", "space"] {
        let frames = run(keyed, (700, 600), &[40, 700], &format!("key tab @10; key {key} @60"));
        assert!(contents(&frames[0], "mk-lic-title").is_empty(), "{key}: a card before the key");
        assert_eq!(one(&frames[1], "mk-lic-title"), "MIT OR Apache-2.0", "{key} opens the card");
        assert_eq!(one(&frames[1], "mk-lic-hedge"), spdx::HEDGE);
    }
}

fn rider(_: &mut Window, cx: &mut App) -> AnyView {
    stage(
        |measure, _, _| {
            div()
                .absolute()
                .left(px(40.0))
                .top(px(60.0))
                .child(version_mark("rider", fixture::version("toml"), &measure.within(px(900.0))))
                .into_any_element()
        },
        cx,
    )
}

#[test]
fn dragging_the_rider_scrubs_and_escape_brings_it_home() {
    let first = run(rider, (1000, 300), &[20], "");
    let number = painted(&first[0], "rider-comb-number")[0].bounds.clone();
    assert_eq!(one(&first[0], "rider-comb-number"), "0.8.23", "the rider rests on the pin");
    let (x0, y) = (number.x + 6.0, number.y + number.height * 0.5);
    let x1 = 40.0 + 900.0 - 120.0;
    let script = format!("drag left {x0:.0},{y:.0} -> {x1:.0},{y:.0} over 300 @100; key escape @1400");
    let frames = run(rider, (1000, 300), &[1200, 2600], &script);
    let away = one(&frames[0], "rider-comb-number");
    assert!(
        super::semver::cmp(&away, "1.0.0").is_gt(),
        "a drag to the newer end rode the number to {away}, not past 1.0.0"
    );
    assert_eq!(one(&frames[1], "rider-comb-number"), "0.8.23", "Esc walked the rider home");
}
