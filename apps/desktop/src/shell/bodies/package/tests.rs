//! The package page through the real shell: what it paints (the probe
//! ledger, never the model it was built from), what a module opens into,
//! what a click or a key does, and what it says when its source is missing
//! or read. Page data is the shell test fixture's dossier; the source on disk
//! is a small crate written to a temp directory and installed the way a
//! read would land.

#![allow(clippy::expect_used, clippy::panic)]

use crate::model::pages::PackageRef;
use crate::model::source_facts::{self, Reading};
use crate::navigation::Intent;
use crate::shell::anatomy_tests::{install, package_route, painted};
use crate::shell::tests::{PACKAGE, Rig, dossier, page_route, rig};
use facet::probe::Ledger;
use gpui::{Modifiers, TestAppContext, point, px};
use std::path::PathBuf;
use std::sync::Arc;

/// The words painted under a key containing `part`, in paint order.
fn said(ledger: &Ledger, part: &str) -> Vec<String> {
    ledger.texts.iter().filter(|t| t.key.contains(part)).map(|t| t.content.clone()).collect()
}

fn has(ledger: &Ledger, part: &str) -> bool {
    !said(ledger, part).is_empty()
}

/// The centre of the first text painted under a key containing `part`.
fn centre(ledger: &Ledger, part: &str) -> (f32, f32) {
    let text = ledger.texts.iter().find(|t| t.key.contains(part)).unwrap_or_else(|| panic!("nothing painted under `{part}`: {:?}", ledger.texts.iter().map(|t| &t.key).collect::<Vec<_>>()));
    (text.bounds.x + text.bounds.width / 2.0, text.bounds.y + text.bounds.height / 2.0)
}

fn click(rig: &mut Rig, at: (f32, f32)) {
    rig.cx.simulate_click(point(px(at.0), px(at.1)), Modifiers::default());
    rig.settle();
}

fn move_to(rig: &mut Rig, at: (f32, f32)) {
    rig.cx.simulate_mouse_move(point(px(at.0), px(at.1)), None, Modifiers::default());
    rig.settle();
}

/// A crate on disk that says a few things about itself.
fn krate() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("nudox-folio-{}-present", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let write = |path: &str, body: &str| {
        let at = dir.join(path);
        std::fs::create_dir_all(at.parent().expect("parent")).expect("dir");
        std::fs::write(at, body).expect("file");
    };
    write(
        "Cargo.toml",
        "[package]\nname = \"present\"\nversion = \"0.4.2\"\nedition = \"2021\"\nlicense = \"MIT\"\nauthors = [\"Ada Lovelace <ada@example.org>\"]\n[features]\ndefault = []\nfast = []\n",
    );
    write("src/lib.rs", "//! How one symbol page reads.\npub mod glyph;\n");
    write(
        "src/glyph.rs",
        "//! Glyphs for relations.\nuse std::process::Command;\n\n/// Names a relation.\npub enum RelationLabel { Typed, Related }\n\n/// Which way it points.\npub enum RelationDirection { In, Out }\n\n/// A glyph for one kind.\npub struct KindGlyph;\n\n/// Who is named.\npub struct Identity;\n\n/// The outline.\npub struct Outline;\n\n/// The label of a relation.\npub fn relation_label(link: &str) -> Option<String> {\n    let _ = Command::new(\"true\");\n    unsafe { std::hint::unreachable_unchecked() }\n}\n",
    );
    dir
}

fn package() -> PackageRef {
    dossier().package
}

/// Installs what reading `krate()` said, and goes to the package page.
fn read_and_open(rig: &mut Rig) {
    let facts = source_facts::read(&krate(), &std::collections::HashMap::new(), None).expect("the crate reads");
    let package = package();
    rig.cx.update(|_, cx| {
        facet::probe::enable(cx);
        source_facts::install(&package, Reading::Ready(Arc::new(facts)), cx);
    });
    rig.go(Intent::Navigate(package_route()));
}

#[gpui::test]
fn a_page_whose_source_is_missing_says_so_instead_of_inventing_facts(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(package_route()), 1440.0, 900.0);
    install(&mut rig);
    let ledger = painted(&mut rig);
    let heads = said(&ledger, "heads-words");
    assert_eq!(heads, ["Its source is not on this machine. Nothing is read for build scripts, network, files and unsafe."], "{heads:?}");
    let weight = said(&ledger, "weight-words");
    assert_eq!(weight, ["Its source is not on this machine. Nothing is read for lines of code."], "{weight:?}");
    assert_eq!(said(&ledger, "advisories-word"), ["yours"], "a project of yours is not checked against feeds of published releases");
    assert_eq!(said(&ledger, "licence-verdict"), ["Permissive"]);
    assert!(!has(&ledger, "weight-note") && !has(&ledger, "heads-note"), "nothing is labelled `from its source` when nothing was read from it");
    assert!(!has(&ledger, "berg") && !has(&ledger, "features"), "no berg and no features bar without a manifest");
}

#[gpui::test]
fn the_outline_is_the_territory_its_modules_are_regions_and_a_module_opens_into_cards_that_say_badges_never_code(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(package_route()), 1440.0, 900.0);
    install(&mut rig);
    let ledger = painted(&mut rig);
    // The fixture puts every name in `glyph.rs`, so the outline is one module of six names.
    assert_eq!(said(&ledger, "present-names").first().map(String::as_str), Some("6"), "six public names");
    assert_eq!(said(&ledger, "shingles-region-"), ["glyph"]);
    let said_lines = rig.said();
    assert!(!said_lines.iter().any(|line| line == "Start here" || line == "what you hold"), "no fixture-ranked tour: {said_lines:#?}");
    click(&mut rig, centre(&ledger, "shingles-region-glyph"));
    let ledger = painted(&mut rig);
    let names: Vec<String> = ledger.texts.iter().filter(|t| t.key.contains("module-card-") && t.key.ends_with("-name")).map(|t| t.content.clone()).collect();
    assert_eq!(names, ["Identity", "RelationLabel", "RelationDirection", "KindGlyph", "relation_label", "Outline"], "the recorded order, one card each");
    let kinds: Vec<String> = ledger.texts.iter().filter(|t| t.key.contains("module-card-") && t.key.ends_with("-kind")).map(|t| t.content.clone()).collect();
    assert_eq!(kinds, ["struct", "enum", "enum", "struct", "fn", "struct"]);
    assert!(!ledger.texts.iter().any(|t| t.content.contains("pub struct") || t.content.contains("pub enum") || t.content.contains('{')), "a card says what a name is, not its code");
    assert!(has(&ledger, "rail-close-name"), "the rail offers a way back");
}

#[gpui::test]
fn a_card_is_a_door_to_its_exact_indexed_page(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(package_route()), 1440.0, 900.0);
    install(&mut rig);
    let ledger = painted(&mut rig);
    click(&mut rig, centre(&ledger, "shingles-region-glyph"));
    let ledger = painted(&mut rig);
    let key = format!("pkg-card-{}", crate::shell::tests::coordinate("RelationLabel"));
    let card = ledger.targets.iter().find(|target| target.key == key).unwrap_or_else(|| panic!("no card door `{key}`")).bounds.clone();
    click(&mut rig, (card.x + card.width / 2.0, card.y + card.height / 2.0));
    assert_eq!(rig.route(), page_route("RelationLabel"));
}

#[gpui::test]
fn j_walks_the_modules_enter_opens_one_and_j_then_enter_opens_a_name(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(package_route()), 1440.0, 900.0);
    install(&mut rig);
    let _ = painted(&mut rig);
    rig.keys("j");
    let (_, focused) = rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx));
    assert_eq!(focused.as_deref(), Some("pkg-module-glyph"), "j lands on the first module");
    rig.keys("enter");
    let ledger = painted(&mut rig);
    assert_eq!(rig.route(), package_route(), "opening a module stays on the page");
    assert!(has(&ledger, "module-card-0-name"), "the module's cards are out");
    rig.keys("j");
    let (_, focused) = rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx));
    assert_eq!(focused.as_deref().map(|id| id.starts_with("pkg-card-")), Some(true), "j now walks the cards: {focused:?}");
    rig.keys("enter");
    assert!(matches!(rig.route(), crate::navigation::Route::Symbol(_)), "enter on a card opens its page: {:?}", rig.route());
}

#[gpui::test]
fn what_the_source_says_is_read_from_it_and_labelled_so(cx: &mut TestAppContext) {
    let mut rig = rig(cx, None, 1440.0, 900.0);
    read_and_open(&mut rig);
    let ledger = painted(&mut rig);
    // Heads-up: it starts a program (a warning) and has one unsafe block.
    assert_eq!(said(&ledger, "heads-label"), ["HEADS-UP"]);
    assert_eq!(said(&ledger, "heads-accent"), ["1"], "one warning: the program it starts");
    assert_eq!(said(&ledger, "heads-note"), ["from its source"]);
    // Weight: a crate with no dependencies is all above the water.
    assert_eq!(said(&ledger, "weight-note"), ["from its source"]);
    assert!(said(&ledger, "weight-caption").iter().any(|c| c.starts_with("0 packages beneath") && c.contains("default features")), "{:?}", said(&ledger, "weight-caption"));
    // The features bar reads its manifest: nothing on by default.
    assert_eq!(said(&ledger, "features-label"), ["Features"]);
    assert_eq!(said(&ledger, "features-of"), ["of 1 on"]);
    // Names take their declarations and first sentences from the source.
    assert_eq!(said(&ledger, "shingles-region-"), ["glyph"]);
    click(&mut rig, centre(&ledger, "shingles-region-glyph"));
    let ledger = painted(&mut rig);
    let docs: Vec<String> = ledger.texts.iter().filter(|t| t.key.contains("module-card-") && t.key.ends_with("-doc")).map(|t| t.content.clone()).collect();
    assert!(docs.iter().any(|d| d == "Names a relation"), "the first sentence of `RelationLabel`'s doc: {docs:?}");
    assert!(!docs.iter().any(|d| d == "undocumented") || docs.len() > 1, "{docs:?}");
}

#[gpui::test]
fn the_heads_up_hand_fans_out_and_a_click_opens_the_evidence_read_from_source(cx: &mut TestAppContext) {
    let mut rig = rig(cx, None, 1440.0, 900.0);
    read_and_open(&mut rig);
    let ledger = painted(&mut rig);
    let (x, y) = centre(&ledger, "heads-label");
    move_to(&mut rig, (x, y + 27.0));
    rig.frame(400);
    let ledger = painted(&mut rig);
    let words = said(&ledger, "heads-word-");
    assert!(words.iter().any(|w| w == "Starts programs") && words.iter().any(|w| w == "Unsafe blocks"), "hovering spreads the hand into words: {words:?}");
    click(&mut rig, (x, y + 27.0));
    rig.frame(400);
    let ledger = painted(&mut rig);
    let evidence: Vec<&str> = ledger.texts.iter().map(|t| t.content.as_str()).filter(|c| c.starts_with("src/glyph.rs:")).collect();
    assert!(!evidence.is_empty(), "the sheet names the file and line of what it found: {:?}", ledger.texts.iter().map(|t| &t.content).collect::<Vec<_>>());
    assert!(ledger.texts.iter().any(|t| t.content.contains("Command::new(\"true\")")), "and shows the line");
}
