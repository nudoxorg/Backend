//! The shelf's lenses through the real shell: Contents (at a release) · Rests
//! on · Used by list what the page already read, change the list and never
//! the page, and follow their rows to the right place.

use super::tests::{Fixture, Rig, dossier, registry_dossier, rig_with_reads};
use crate::core::PackageId;
use crate::model::pages::{
    Dependency, DependencyScope, Known, PackageRecord, PackageRef, PageValue, ReadFailure, RecordSource, Standing,
    VersionEntry,
};
use crate::navigation::{Intent, PackageLane, PackageRoute, Route};
use crate::runtime::reads::{PageReader, ReadContext, ReadPool, ReadRequest};
use gpui::{Modifiers, TestAppContext, point, px};
use std::sync::Arc;

const PINNED: &str = "pkg:cargo/toml@0.8.23";
// This is the fixture's exact comparison target, including build metadata.
const TARGET: &str = "1.1.6+spec-1.1.0";

/// toml at its pin, with three releases, two dependencies (one resolved to an
/// indexed release, one not) and one dependent, as the index answers.
struct Registry;

impl PageReader for Registry {
    fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
        let ReadRequest::Package(package) = request else {
            return Fixture.read(request, context);
        };
        let mut about = registry_dossier(package);
        let release = |version: &str, standing: Standing| VersionEntry {
            package: package.at(version).expect("exact sibling release"),
            version: Arc::from(version),
            standing,
            current: package.version() == Some(version),
        };
        about.versions = Known::Known(Arc::from([
            release(TARGET, Standing::Available),
            release("0.9.0", Standing::Yanked),
            release("0.8.23", Standing::Available),
        ]));
        about.dependencies = Known::Known(Arc::from([
            Dependency {
                name: Arc::from("toml_edit"),
                requirement: Arc::from("0.22"),
                scope: DependencyScope::Runtime,
                optional: false,
                resolved: Some(PackageRef::parse("pkg:cargo/toml_edit@0.22.27").expect("resolved")),
            },
            Dependency {
                name: Arc::from("indexmap"),
                requirement: Arc::from("2.0"),
                scope: DependencyScope::Optional,
                optional: true,
                resolved: None,
            },
        ]));
        let pin = PackageRef::parse("pkg:cargo/toml-pin-fixture@0.1.0").expect("dependent");
        about.dependents = Known::Known(Arc::from([PackageRecord {
            package: pin.clone(),
            source: RecordSource::LocalManifest,
            name: Arc::from("toml_pin"),
            version: Known::Known(Arc::from("0.1.0")),
            ..record_like(&about)
        }]));
        Ok(PageValue::Package(about))
    }
}

/// The fixture record's unknowns, for a record the test only names.
fn record_like(about: &crate::model::pages::PackageDossier) -> PackageRecord {
    about.record.known().cloned().expect("the fixture dossier carries a record")
}

fn toml() -> Route {
    Route::Package(PackageRoute {
        cargo: None,
        project: None,
        package: PackageId::new(PINNED).expect("package"),
        lane: PackageLane::Overview,
        selected: None,
        at: None,
    })
}

fn open(cx: &mut TestAppContext) -> Rig {
    let pool = ReadPool::start(1, |_| Registry).expect("pool");
    let mut rig = rig_with_reads(cx, Some(toml()), 1440.0, 900.0, pool);
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    rig.repaint();
    rig
}

/// Every text painted inside the shelf's column, top to bottom.
fn shelf_texts(rig: &mut Rig) -> Vec<(String, f32, f32, f32, f32)> {
    rig.repaint();
    let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
    let mut texts: Vec<_> = ledger
        .texts
        .iter()
        .filter(|text| text.bounds.x < 264.0 && text.bounds.y > 50.0)
        .map(|text| (text.content.to_string(), text.bounds.x, text.bounds.y, text.bounds.width, text.bounds.height))
        .collect();
    texts.sort_by(|a, b| a.2.total_cmp(&b.2).then(a.1.total_cmp(&b.1)));
    texts
}

fn click_text(rig: &mut Rig, words: &str) {
    let texts = shelf_texts(rig);
    let (_, x, y, w, h) = texts.iter().find(|text| text.0 == words).unwrap_or_else(|| panic!("{words:?} is not in the shelf: {texts:#?}")).clone();
    rig.cx.simulate_click(point(px(x + w / 2.0), px(y + h / 2.0)), Modifiers::default());
    rig.settle();
}

/// Click the release list's actual native row, never the identical pin or
/// viewed-release heading above it. This still exercises pointer activation.
fn click_release(rig: &mut Rig, version: &str) {
    rig.repaint();
    let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
    let row = ledger.texts.iter()
        .find(|text| text.key.starts_with("shelf-row:") && text.content == version)
        .unwrap_or_else(|| panic!("exact release row {version:?} is not drawn"));
    rig.cx.simulate_click(point(px(row.bounds.x + row.bounds.width / 2.0), px(row.bounds.y + row.bounds.height / 2.0)), Modifiers::default());
    rig.settle();
}

fn said(rig: &mut Rig) -> Vec<String> {
    shelf_texts(rig).into_iter().map(|text| text.0).collect()
}

/// The number a heading or a lens carries: the text right beside `words`
/// that is a count (the two are set on one line, and which of them the
/// probe lists first depends on their baselines).
fn count_beside(said: &[String], words: &str) -> Option<String> {
    let at = said.iter().position(|text| text == words)?;
    [at.checked_sub(1), Some(at + 1)]
        .into_iter()
        .flatten()
        .filter_map(|index| said.get(index))
        .find(|text| !text.is_empty() && text.chars().all(|c| c.is_ascii_digit()))
        .cloned()
}

/// The index of `words` in `said`, or a panic that prints everything said.
fn at(said: &[String], words: &str) -> usize {
    said.iter().position(|text| text == words).unwrap_or_else(|| panic!("{words:?} missing: {said:#?}"))
}

/// The words on the shelf's row keys (`shelf-row:`), top to bottom.
fn row_names(rig: &mut Rig) -> Vec<String> {
    rig.repaint();
    let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
    let mut rows: Vec<_> = ledger.texts.iter().filter(|text| text.key.starts_with("shelf-row:")).collect();
    rows.sort_by(|a, b| a.bounds.y.total_cmp(&b.bounds.y));
    rows.iter().map(|text| text.content.to_string()).collect()
}

/// The sidebar takes the keyboard (the reader had it).
fn into_the_sidebar(rig: &mut Rig) {
    let zone = |rig: &mut Rig| rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx)).0;
    let reader_stops = rig.shell.read_with(rig.cx, |shell, cx| {
        shell.reader_targets(cx).list_probe().upgrade().map_or(0, |list| list.borrow().len())
    });
    // Shift-Tab first walks the Reader's mounted native stops, then crosses
    // zones. Bound the walk by that exact list and the Shell's zone count.
    for _ in 0..reader_stops + super::focus::Zone::ALL.len() {
        if zone(rig) == super::focus::Zone::Shelf { break; }
        rig.keys("shift-tab");
    }
    assert_eq!(zone(rig), super::focus::Zone::Shelf, "native Shift-Tab walk reaches the shelf after the Reader's mounted stops");
}

/// RIG evidence (fixture reads): the sidebar's four lenses, the package intro
/// NOT repeating the page's modules, and what it lists instead.
#[gpui::test]
fn the_package_intro_lists_the_api_by_kind_and_not_the_modules_the_page_already_draws(cx: &mut TestAppContext) {
    let mut rig = open(cx);
    let said = said(&mut rig);
    for lens in ["Contents", "Versions", "Rests on", "Used by"] {
        assert!(said.iter().any(|text| text == lens), "the {lens:?} lens is not drawn: {said:#?}");
    }
    for module in ["identity", "glyph", "outline", "tests"] {
        assert!(!said.iter().any(|text| text == module), "the page draws the module {module:?}; the sidebar must not list it too: {said:#?}");
    }
    // What it lists instead: every kind family, with how many names.
    assert_eq!(said.get(at(&said, "Types") + 1).map(String::as_str), Some("5"), "{said:#?}");
    assert_eq!(said.get(at(&said, "Functions") + 1).map(String::as_str), Some("1"), "{said:#?}");
    assert!(at(&said, "BY KIND") < at(&said, "Types"), "{said:#?}");
    assert!(!said.iter().any(|text| text == "YOURS" || text == "CHANGES"), "nothing is known about your uses here, so nothing says so: {said:#?}");
    // The way out is the Library, and the counts follow the lens you are on.
    assert!(said.iter().any(|text| text == "Library"), "the way out is drawn: {said:#?}");
    assert_eq!(count_beside(&said, "Contents").as_deref(), Some("6"), "the intro's Contents counts what its families hold (the page's own \"public names\"): {said:#?}");
}

/// A kind family opens in place to a flat list: each name, and the module it
/// is in as a quiet word after it.
#[gpui::test]
fn a_kind_family_opens_in_place_to_its_names_each_with_its_module(cx: &mut TestAppContext) {
    let mut rig = open(cx);
    click_text(&mut rig, "Types");
    let said = said(&mut rig);
    let types = at(&said, "Types");
    let listed: Vec<&str> = said[types + 2..types + 12].iter().map(String::as_str).collect();
    assert_eq!(
        listed,
        ["Identity", "identity", "RelationLabel", "glyph", "RelationDirection", "glyph", "KindGlyph", "glyph", "Outline", "outline"],
        "the five types in outline order, each with its module: {said:#?}"
    );
    assert!(at(&said, "Types") < at(&said, "Functions"), "the other family is still folded below: {said:#?}");
    click_text(&mut rig, "Types");
    let said = self::said(&mut rig);
    assert!(!said.iter().any(|text| text == "RelationLabel"), "closed again: {said:#?}");
}

/// On a declaration's page the reader draws one declaration, not the
/// package, so the sidebar follows it: the outline, the page's module open.
#[gpui::test]
fn a_declarations_page_follows_in_the_outline_with_its_module_open(cx: &mut TestAppContext) {
    let mut rig = super::tests::rig(cx, Some(super::tests::page_route("RelationLabel")), 1440.0, 900.0);
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    assert_eq!(
        row_names(&mut rig),
        ["identity", "glyph", "RelationLabel", "RelationDirection", "KindGlyph", "relation_label", "outline", "tests"],
        "modules, the current one open, one fold for the test modules"
    );
    assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.shelf_current_symbols(cx)), [super::tests::symbol("RelationLabel")]);
    let said = said(&mut rig);
    assert!(!said.iter().any(|text| text == "BY KIND"), "the intro's cut is only for the page that draws the modules: {said:#?}");
    assert_eq!(count_beside(&said, "Contents").as_deref(), Some("11"), "the outline's Contents counts every name it can show, groups closed or open: {said:#?}");
}

#[gpui::test]
fn the_versions_lens_lists_releases_newest_first_and_choosing_one_reads_the_book_there(cx: &mut TestAppContext) {
    let mut rig = open(cx);
    click_text(&mut rig, "Versions");
    let said = said(&mut rig);
    let releases = &said[at(&said, "RELEASES") + 1..at(&said, "TRAIL")];
    // Newest first; the pin and a yanked release say so; the reader has not moved.
    assert_eq!(said.get(at(&said, "Versions") + 1).map(String::as_str), Some("3"), "the lens on show holds 3 releases: {said:#?}");
    assert!(at(&said, "RELEASES") < at(&said, TARGET), "{said:#?}");
    assert!(at(releases, TARGET) < at(releases, "0.9.0") && at(releases, "0.9.0") < at(releases, "0.8.23"), "newest first: {said:#?}");
    assert_eq!(releases.get(at(releases, "0.9.0") + 1).map(String::as_str), Some("yanked"), "{said:#?}");
    assert_eq!(releases.get(at(releases, "0.8.23") + 1).map(String::as_str), Some("your pin"), "{said:#?}");
    assert!(!said.iter().any(|text| text == "BY KIND"), "a lens changes the list, not the page: {said:#?}");
    assert_eq!(rig.route(), toml());
    // Choosing a release reads the book at it; the lens stays, and the row says so.
    click_release(&mut rig, TARGET);
    let Route::Package(route) = rig.route() else { panic!("still the package") };
    assert_eq!(route.at.as_ref().map(|at| at.as_str().to_owned()), Some(TARGET.to_owned()));
    let said = self::said(&mut rig);
    let releases = &said[at(&said, "RELEASES") + 1..at(&said, "TRAIL")];
    assert_eq!(releases.get(at(releases, TARGET) + 1).map(String::as_str), Some("reading"), "the row names the release being read: {said:#?}");
    assert!(said.iter().any(|text| text == "RELEASES"), "the lens is still Versions: {said:#?}");
    // Choosing the pin comes back.
    click_release(&mut rig, "0.8.23");
    assert_eq!(rig.route(), toml(), "the pin is the route without `at`");
}

#[gpui::test]
fn the_rests_on_lens_opens_what_it_resolved_and_says_what_it_could_not(cx: &mut TestAppContext) {
    let mut rig = open(cx);
    click_text(&mut rig, "Rests on");
    let said = said(&mut rig);
    assert_eq!(said.get(at(&said, "toml_edit") + 1).map(String::as_str), Some("0.22"), "{said:#?}");
    assert_eq!(said.get(at(&said, "indexmap") + 1).map(String::as_str), Some("2.0 · optional"), "{said:#?}");
    click_text(&mut rig, "toml_edit");
    let Route::Package(route) = rig.route() else { panic!("a package page") };
    assert_eq!(route.package.as_str(), "pkg:cargo/toml_edit@0.22.27");
    // A new book opens on its contents.
    let said = self::said(&mut rig);
    assert!(!said.iter().any(|text| text == "indexmap"), "the new book is not on toml's Rests on lens: {said:#?}");
    assert!(said.iter().any(|text| text == "BY KIND"), "it opens on its contents: {said:#?}");
}

#[gpui::test]
fn the_used_by_lens_lists_dependents_and_an_unknown_list_says_why(cx: &mut TestAppContext) {
    let mut rig = open(cx);
    click_text(&mut rig, "Used by");
    let said = said(&mut rig);
    assert!(said.iter().any(|text| text == "toml_pin"), "{said:#?}");
    assert!(at(&said, "YOURS") < at(&said, "toml_pin"), "a dependent that is a manifest of yours is yours: {said:#?}");
    // The plain fixture's own package is a local project: no dependents are
    // recorded, and the lens says so instead of drawing an empty list.
    let mut rig2 = super::tests::rig(cx, Some(super::tests::page_route("RelationLabel")), 1440.0, 900.0);
    rig2.cx.update(|_, cx| facet::probe::enable(cx));
    click_text(&mut rig2, "Used by");
    let said = self::said(&mut rig2);
    assert!(said.iter().any(|text| text.contains("local project")), "an honest reason, not an empty list: {said:#?}");
}

/// The chords: with the sidebar holding the keyboard, `G` then `C`, `V`,
/// `R` or `U` picks the lens (Linear's chords), and the reader does not move.
#[gpui::test]
fn g_then_a_letter_picks_the_lens_and_the_reader_stays_where_it_is(cx: &mut TestAppContext) {
    let mut rig = open(cx);
    into_the_sidebar(&mut rig);
    rig.keys("g v");
    let said = said(&mut rig);
    assert!(said.iter().any(|text| text == "RELEASES"), "G V is Versions: {said:#?}");
    rig.keys("g r");
    let said = self::said(&mut rig);
    assert!(said.iter().any(|text| text == "DIRECTLY"), "G R is Rests on: {said:#?}");
    rig.keys("g u");
    let said = self::said(&mut rig);
    assert!(said.iter().any(|text| text == "YOURS"), "G U is Used by: {said:#?}");
    rig.keys("g c");
    let said = self::said(&mut rig);
    assert!(said.iter().any(|text| text == "BY KIND"), "G C is Contents: {said:#?}");
    assert_eq!(rig.route(), toml(), "picking a lens is browsing");
    // Outside the sidebar the letters are the shell's again: G is the graph.
    rig.keys("tab");
    let (zone, _) = rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx));
    assert_ne!(zone, super::focus::Zone::Shelf);
}

/// The way out shows the Library without moving the reader; `→` on a module
/// scopes into it and `←` comes back.
#[gpui::test]
fn stepping_out_and_hoisting_browse_without_moving_the_reader(cx: &mut TestAppContext) {
    let mut rig = super::tests::rig(cx, Some(super::tests::page_route("RelationLabel")), 1440.0, 900.0);
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    let route = rig.route();
    click_text(&mut rig, "Library");
    let said = said(&mut rig);
    assert!(said.iter().any(|text| text == "0 projects · 1 package"), "the Library's title: {said:#?}");
    assert!(said.iter().any(|text| text == "present"), "its packages: {said:#?}");
    assert_eq!(rig.route(), route, "stepping out is browsing");
    // Reading another page follows the reader again.
    rig.go(Intent::Navigate(super::tests::page_route("Identity")));
    assert_eq!(row_names(&mut rig), ["identity", "Identity", "glyph", "outline", "tests"]);
    // → on a module scopes into it: its children only, the package as the way out.
    into_the_sidebar(&mut rig);
    rig.keys("down");
    rig.keys("right");
    let said = self::said(&mut rig);
    assert!(said.iter().any(|text| text == "present"), "the way out names the package: {said:#?}");
    assert!(said.iter().any(|text| text.starts_with("module") && text.contains("1 item")), "the scope's title says what it is: {said:#?}");
    assert_eq!(row_names(&mut rig), ["Identity"], "only what the module holds");
    assert_eq!(rig.route(), page_identity(), "hoisting is browsing");
    rig.keys("left");
    assert_eq!(row_names(&mut rig), ["identity", "Identity", "glyph", "outline", "tests"], "← steps out");
}

fn page_identity() -> Route {
    super::tests::page_route("Identity")
}

/// RIG evidence: typing while the sidebar has the keyboard narrows the
/// scope in place. It searches collapsed rows too, keeps each match's parents
/// as context, and counts what matched.
#[gpui::test]
fn typing_narrows_the_scope_in_place_through_collapsed_rows_keeping_parents_and_esc_restores(cx: &mut TestAppContext) {
    let mut rig = super::tests::rig(cx, Some(super::tests::page_route("RelationLabel")), 1440.0, 900.0);
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    into_the_sidebar(&mut rig);
    // `KindGlyph` is in the open module; its module matches nothing itself and stays as context.
    rig.keys("k i n d");
    assert_eq!(row_names(&mut rig), ["glyph", "KindGlyph", "kind in the whole library"], "the match and its parent, then the way to widen it");
    let said = said(&mut rig);
    assert!(said.iter().any(|text| text == "kind"), "the typed words are on the narrowing line: {said:#?}");
    assert!(said.iter().any(|text| text == "1 of 11"), "how much of the scope matched, collapsed rows counted: {said:#?}");
    // A match inside a folded-away module (the test modules' fold) is found too.
    rig.keys("escape");
    assert_eq!(row_names(&mut rig).len(), 8, "Esc restores the whole outline");
    rig.keys("f o l d s");
    assert_eq!(row_names(&mut rig), ["glyph_tests", "folds_rows", "folds in the whole library"], "a collapsed match shows, with its module");
    assert!(self::said(&mut rig).iter().any(|text| text == "1 of 11"));
    // The last row widens the same words to Find, and ↵ goes there.
    assert!(self::said(&mut rig).iter().any(|text| text == "folds in the whole library"), "{:#?}", self::said(&mut rig));
    rig.keys("down down enter");
    let Route::Orbit(crate::navigation::OrbitRoute::Browse(crate::navigation::BrowseRoute::Find(query))) = rig.route() else { panic!("Find: {:?}", rig.route()) };
    assert_eq!(query.text.as_ref(), "folds");
}

/// Narrowing works on every lens: releases here.
#[gpui::test]
fn typing_narrows_the_versions_lens_by_name_and_says_when_nothing_matches(cx: &mut TestAppContext) {
    let mut rig = open(cx);
    into_the_sidebar(&mut rig);
    rig.keys("g v");
    rig.keys("0 . 9");
    let said = said(&mut rig);
    assert!(said.iter().any(|text| text == "0.9.0") && !said.iter().any(|text| text == TARGET), "{said:#?}");
    assert!(said.iter().any(|text| text == "1 of 3"), "{said:#?}");
    assert!(said.iter().any(|text| text == "RELEASES"), "the heading of what stayed: {said:#?}");
    rig.keys("z");
    let said = self::said(&mut rig);
    assert!(said.iter().any(|text| text == "Nothing here matches."), "{said:#?}");
    assert!(said.iter().any(|text| text == "0.9z in the whole library"), "and widening is still offered: {said:#?}");
    rig.keys("backspace");
    assert!(self::said(&mut rig).iter().any(|text| text == "0.9.0"), "⌫ takes a character back");
}

/// A `G` left waiting is a letter: typing a word that starts with g narrows.
#[gpui::test]
fn a_g_that_waits_is_a_letter_and_one_followed_by_a_word_is_part_of_it(cx: &mut TestAppContext) {
    let mut rig = super::tests::rig(cx, Some(super::tests::page_route("RelationLabel")), 1440.0, 900.0);
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    into_the_sidebar(&mut rig);
    rig.keys("g");
    rig.frame(100);
    // The chord's window passed: the G is the query.
    let said = said(&mut rig);
    assert!(said.iter().any(|text| text == "3 of 11"), "glyph, KindGlyph and glyph_tests have a g: {said:#?}");
    rig.keys("escape");
    // A word that starts with g is not a chord.
    rig.keys("g l");
    assert_eq!(row_names(&mut rig), ["glyph", "KindGlyph", "glyph_tests", "gl in the whole library"], "gl is a word: glyph and its neighbours");
}

/// The peek follows the selection: Space shows it beside the sidebar, arrows
/// move it, H holds the row, and Esc puts it away before anything else.
#[gpui::test]
fn space_peeks_the_selection_beside_the_sidebar_and_the_peek_follows_the_arrows(cx: &mut TestAppContext) {
    let mut rig = super::tests::rig(cx, Some(super::tests::page_route("RelationLabel")), 1440.0, 900.0);
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    let foot = |rig: &mut Rig| -> Option<f32> {
        rig.repaint();
        let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
        ledger.texts.iter().find(|text| text.content == "hold").map(|text| text.bounds.x)
    };
    assert_eq!(foot(&mut rig), None, "no peek before Space");
    into_the_sidebar(&mut rig);
    rig.keys("down down");
    rig.keys("space");
    let x = foot(&mut rig).expect("the peek's foot names its keys");
    assert!(x >= 264.0, "the peek is beside the sidebar, over the reader: {x}");
    rig.keys("down");
    rig.frame(50);
    rig.settle();
    assert!(foot(&mut rig).is_some(), "the peek followed the selection to the next row");
    // H holds the row the peek is on.
    rig.keys("h");
    let held = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().hand.held().len());
    assert_eq!(held, 1, "H holds the selection");
    rig.keys("space");
    rig.keys("escape");
    assert_eq!(foot(&mut rig), None, "Esc puts the peek away");
}

/// toml's API as the index would answer it for the paths the release data
/// knows: `value::Value` (with `as_str`), `de::from_str`, `de::Deserializer`
/// (with `new`) and `map::Map` (with `serialize`).
struct TomlApi;

impl PageReader for TomlApi {
    fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
        let PageValue::Package(mut about) = Registry.read(request, context)? else { return Registry.read(request, context) };
        // The index names a declaration under the release it was read at.
        let release = about.package.as_str().to_owned();
        let node = |name: &str, kind: backend_library::DeclarationKind, children: Vec<crate::model::pages::OutlineNode>| crate::model::pages::OutlineNode {
            decl: crate::model::pages::DeclRef::from_label(&format!("{release}::glyph.rs:138::{name}"), None, Some(kind), None).expect("decl"),
            children: Arc::from(children),
        };
        use backend_library::DeclarationKind as K;
        about.outline = Known::Known(crate::model::pages::OutlineTree {
            roots: Arc::from([
                node("value", K::Module, vec![node("Value", K::Enum, vec![node("as_str", K::Method, vec![])])]),
                node("de", K::Module, vec![node("from_str", K::Function, vec![]), node("Deserializer", K::Struct, vec![node("new", K::Method, vec![])])]),
                node("map", K::Module, vec![node("Map", K::Struct, vec![node("serialize", K::Method, vec![])])]),
            ]),
            complete: true,
        });
        Ok(PageValue::Package(about))
    }
}

fn open_toml_api(cx: &mut TestAppContext, route: Route) -> Rig {
    let pool = ReadPool::start(1, |_| TomlApi).expect("pool");
    let mut rig = rig_with_reads(cx, None, 1440.0, 900.0, pool);
    rig.cx.update(|_, cx| {
        crate::runtime::fixture_releases::install(cx);
        facet::probe::enable(cx);
    });
    rig.go(Intent::Navigate(route));
    rig
}

/// RIG evidence: state on rows. What your code uses shows in mint (a module
/// rolls it up), what the release being read changes in amber, what is gone
/// in coral words. The package intro leads with what you use.
#[gpui::test]
fn the_intro_leads_with_what_your_code_uses_and_a_target_release_adds_what_changes(cx: &mut TestAppContext) {
    let mut rig = open_toml_api(cx, toml());
    let said = said(&mut rig);
    assert_eq!(count_beside(&said, "YOURS").as_deref(), Some("2"), "{said:#?}");
    // `Value` (29 uses) with its `as_str` (20) rolled in, then `from_str` (4): the busiest first.
    let value = at(&said, "Value");
    assert_eq!(&said[value..value + 3], ["Value", "value", "49"], "the item, its module, its uses: {said:#?}");
    let from_str = at(&said, "from_str");
    assert!(value < from_str, "{said:#?}");
    assert_eq!(&said[from_str..from_str + 3], ["from_str", "de", "4"], "{said:#?}");
    assert!(!said.iter().any(|text| text == "CHANGES"), "nothing is being compared at the pin: {said:#?}");
    // Reading 1.1.6: what changes for you and what is gone lead the list too.
    rig.go(Intent::SetRelease(Some(crate::navigation::ReleaseId::new(TARGET).expect("release"))));
    let said = self::said(&mut rig);
    let changes = at(&said, "CHANGES");
    assert!(at(&said, "YOURS") < changes && changes < at(&said, "BY KIND"), "{said:#?}");
    let listed: Vec<&str> = said[changes..at(&said, "BY KIND")].iter().map(String::as_str).collect();
    assert!(listed.contains(&"Map") && listed.contains(&"Deserializer"), "the items whose members go or change: {listed:?}");
}

/// RIG evidence: "Used by" answers "how much of this do we use" with a look.
/// Choosing one of your crates narrows Contents to what it uses, with the
/// parents that hold it, and Esc lets it go.
#[gpui::test]
fn choosing_one_of_your_crates_narrows_contents_to_what_it_uses_and_esc_clears_it(cx: &mut TestAppContext) {
    let mut rig = open_toml_api(cx, toml());
    click_text(&mut rig, "Used by");
    let said = said(&mut rig);
    assert!(at(&said, "YOURS") < at(&said, "desktop"), "your crates lead the lens: {said:#?}");
    click_text(&mut rig, "desktop");
    assert_eq!(
        row_names(&mut rig),
        ["value", "Value", "as_str", "de", "from_str"],
        "only what desktop uses, each with the modules that hold it (Deserializer and Map are never touched)"
    );
    let said = self::said(&mut rig);
    assert!(said.iter().any(|text| text == "only what") && said.iter().any(|text| text == "uses"), "the narrowing says so: {said:#?}");
    assert_eq!(rig.route(), toml(), "narrowing is browsing");
    rig.cx.update(|window, _| {
        assert!(
            window.context_stack().iter().any(|context| context.contains(super::keys::CONTEXT)),
            "replacing a clicked row preserves the shell keyboard owner"
        );
    });
    rig.keys("escape");
    let said = self::said(&mut rig);
    assert!(!said.iter().any(|text| text == "only what"), "Esc lets it go: {said:#?}");
    assert!(said.iter().any(|text| text == "YOURS"), "and the intro is back: {said:#?}");
}

/// On a declaration's page the outline carries the same state: a removed
/// member says `gone`.
#[gpui::test]
fn a_removed_member_says_gone_in_the_outline_of_the_release_being_read(cx: &mut TestAppContext) {
    let map = Route::Symbol(crate::navigation::SymbolRoute {
        project: None,
        package: PackageId::new(PINNED).expect("package"),
        id: crate::navigation::Coordinate::new(&format!("{PINNED}::glyph.rs:138::Map")).expect("coordinate"),
        at: Some(crate::navigation::ReleaseId::new(TARGET).expect("release")),
        view: crate::navigation::View::Page,
        line: None,
        selected: None,
    });
    let mut rig = open_toml_api(cx, map);
    let said = said(&mut rig);
    let serialize = at(&said, "serialize");
    assert_eq!(said.get(serialize + 1).map(String::as_str), Some("gone"), "coral words at the row's edge: {said:#?}");
}

/// RIG evidence: twins. The pointer on a declaration's row lights the
/// declaration (its other places draw a ring, above the regions), and
/// leaving puts it out.
#[gpui::test]
fn hovering_a_row_lights_its_declaration_everywhere_and_leaving_puts_it_out(cx: &mut TestAppContext) {
    let mut rig = super::tests::rig(cx, Some(super::tests::page_route("RelationLabel")), 1440.0, 900.0);
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    let lit = |rig: &mut Rig| rig.cx.update(|_, cx| super::side::twin::lit(cx));
    rig.repaint();
    let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
    let row = ledger
        .texts
        .iter()
        .find(|text| text.key.starts_with("shelf-row:") && text.content == "RelationDirection")
        .cloned()
        .expect("a declaration row in the shelf");
    assert_eq!(lit(&mut rig), None);
    rig.cx.simulate_mouse_move(point(px(row.bounds.x + 4.0), px(row.bounds.y + row.bounds.height / 2.0)), None, Modifiers::default());
    rig.settle();
    assert_eq!(lit(&mut rig), Some(super::tests::symbol("RelationDirection")), "the row's declaration is lit");
    rig.cx.simulate_mouse_move(point(px(900.0), px(700.0)), None, Modifiers::default());
    rig.settle();
    assert_eq!(lit(&mut rig), None, "the pointer left: nothing is lit");
}

/// RIG evidence: hold and trail. What the hand holds sits above the lenses
/// as chips with the key that goes to each; where you have been sits at the foot.
#[gpui::test]
fn what_you_hold_sits_above_the_lenses_and_where_you_have_been_at_the_foot(cx: &mut TestAppContext) {
    let mut rig = super::tests::rig(cx, Some(super::tests::page_route("RelationLabel")), 1440.0, 900.0);
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    let said = said(&mut rig);
    assert!(said.iter().any(|text| text == "TRAIL"), "the trail is at the foot: {said:#?}");
    assert!(!said.iter().any(|text| text == "⌘1"), "nothing is held yet: {said:#?}");
    rig.keys("secondary-d");
    let said = self::said(&mut rig);
    assert!(said.iter().any(|text| text == "⌘1"), "the held card is a chip with its key: {said:#?}");
    assert!(at(&said, "⌘1") < at(&said, "Contents"), "chips are above the lenses: {said:#?}");
    // Going elsewhere puts the place you left on the trail.
    rig.go(Intent::Navigate(super::tests::page_route("Identity")));
    let said = self::said(&mut rig);
    let trail = at(&said, "TRAIL");
    assert!(said[trail..].iter().any(|text| text == "RelationLabel"), "the page you came from is on the trail: {said:#?}");
}

/// The sidebar's one width ladder: below 232 design px a lens you are not on
/// says its chord letter and a row drops its quiet words.
#[test]
fn the_sidebar_is_tight_below_232_design_px_and_full_above_it() {
    use facet::fluid::Room;
    use facet::tokens::fluid::{SIDE, SideForm};
    let at = |width: f32, scale: f32| SIDE.at(Room::new(px(width), scale));
    assert_eq!(at(200.0, 1.0), SideForm::Tight, "the floor of the shelf");
    assert_eq!(at(264.0, 1.0), SideForm::Full, "its default width");
    assert_eq!(at(396.0, 1.5), SideForm::Full, "150 % text at the default width is still 264 design px");
    assert_eq!(at(300.0, 1.5), SideForm::Tight, "and a narrower shelf at 150 % text is tight");
}

/// W-Page2's R1 from the shelf's side: clicking a row that opens a
/// declaration hands that row's name box to the declaration's title key, so
/// the title grows out of the row; the page's title then owns the key.
#[gpui::test]
fn a_shelf_row_hands_its_name_to_the_title_it_opens(cx: &mut TestAppContext) {
    let mut rig = super::tests::rig(cx, Some(super::tests::page_route("RelationLabel")), 1440.0, 900.0);
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    rig.repaint();
    let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
    let row = ledger
        .texts
        .iter()
        // RelationDirection: a declaration beside the page's own (modules are
        // groups, which fold instead of opening a page).
        .find(|text| text.key.starts_with("shelf-row:") && text.content == "RelationDirection" && text.bounds.x < 264.0)
        .cloned()
        .unwrap_or_else(|| panic!("a declaration row in the shelf: {:#?}", ledger.texts.iter().filter(|t| t.key.starts_with("shelf-row:")).map(|t| (&t.key, &t.content)).collect::<Vec<_>>()));
    let address = row.key.trim_start_matches("shelf-row:").to_owned();
    let key = facet::anatomy::page::title_key(&address);
    rig.cx.simulate_click(point(px(row.bounds.x + 4.0), px(row.bounds.y + row.bounds.height / 2.0)), Modifiers::default());
    let rows: Vec<_> = ledger.texts.iter().filter(|t| t.key.starts_with("shelf-row:")).map(|t| (t.key.clone(), t.content.clone())).collect();
    let from = rig.cx.update(|window, cx| facet::motion::shared::last_bounds(key.clone(), window, cx)).unwrap_or_else(|| panic!("the click hands the row's name to the title key; clicked {:?}; rows {rows:#?}", row.key));
    // The handed box is the name's own element: it holds the painted name, on its line.
    let (x, y, w) = (f32::from(from.origin.x), f32::from(from.origin.y), f32::from(from.size.width));
    assert!(x <= row.bounds.x + 0.5 && x + w >= row.bounds.x + row.bounds.width - 0.5 && (y - row.bounds.y).abs() < 2.0 && x < 264.0,
        "the handed box is the row's name: {from:?} vs {:?}", row.bounds);
    rig.settle();
    rig.repaint();
    let on_title = rig.cx.update(|window, cx| facet::motion::shared::last_bounds(key.clone(), window, cx)).expect("the title owns the key");
    assert!(f32::from(on_title.origin.x) > 264.0, "after arriving, the key is the title's, in the reader: {on_title:?}");
}

/// The lens tabs are controls a person points at: each is a published
/// target (J9 could not click "Contents": its words were not a link), and a
/// click on one gives the sidebar the keyboard, so what is typed next
/// narrows it.
#[gpui::test]
fn a_lens_tab_is_a_target_and_a_click_on_it_gives_the_sidebar_the_keyboard(cx: &mut TestAppContext) {
    let mut rig = open(cx);
    rig.repaint();
    let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
    let tabs: Vec<&str> = ledger.targets.iter().map(|target| target.key.as_str()).filter(|key| key.starts_with("shelf-lens-")).collect();
    assert_eq!(tabs, ["shelf-lens-contents", "shelf-lens-versions", "shelf-lens-rests-on", "shelf-lens-used-by"]);
    let (zone, _) = rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx));
    assert_ne!(zone, super::focus::Zone::Shelf, "the sidebar does not have the keyboard yet");
    click_text(&mut rig, "Contents");
    let (zone, _) = rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx));
    assert_eq!(zone, super::focus::Zone::Shelf, "a click on a lens gives the sidebar the keyboard");
}


/// Native accessibility must never claim a node as its own descendant.
/// The right click changes GPUI native focus independently of Shelf Recall.
#[gpui::test]
fn forced_accessibility_survives_native_pointer_then_right_click_on_local_and_registry_rows(cx: &mut TestAppContext) {
    for registry in [false, true] {
        let mut rig = if registry { open(cx) } else {
            let mut local = super::tests::rig(cx, Some(super::tests::page_route("RelationLabel")), 1440.0, 900.0);
            local.cx.update(|_, cx| facet::probe::enable(cx));
            local
        };
        rig.cx.update(|window, _| window.set_a11y_forced(true));
        let route = rig.route();
        let words = if registry { "Types" } else { "glyph" };
        click_text(&mut rig, words);
        // Re-read the actual painted row after the left gesture; its bounds
        // may move as the fold opens. Deliver native right mouse events.
        let texts = shelf_texts(&mut rig);
        let (_, x, y, w, h) = texts.iter().find(|text| text.0 == words)
            .unwrap_or_else(|| panic!("the clicked row {words:?} disappeared: {texts:#?}")).clone();
        let at = point(px(x + w / 2.0), px(y + h / 2.0));
        rig.cx.simulate_mouse_down(at, gpui::MouseButton::Right, Modifiers::none());
        rig.cx.simulate_mouse_up(at, gpui::MouseButton::Right, Modifiers::none());
        rig.settle();
        rig.repaint();
        assert!(rig.cx.update(|window, _| window.is_a11y_active()), "the real tree was built");
        assert_eq!(rig.route(), route, "a row fold/context gesture does not navigate");
        assert!(!shelf_texts(&mut rig).is_empty(), "the native shelf remains rendered");
    }
}

/// Exact observed empty relationships, supplied by the same native read path
/// as positive fixtures; absence is never inferred from missing rows.
struct EmptyRelations;

impl PageReader for EmptyRelations {
    fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
        if let ReadRequest::Package(package) = request {
            let mut about = registry_dossier(package);
            about.dependencies = Known::Known(Arc::from([]));
            about.dependents = Known::Known(Arc::from([]));
            return Ok(PageValue::Package(about));
        }
        Fixture.read(request, context)
    }
}

fn fact_rig(cx: &mut TestAppContext, route: Route) -> (Rig, crate::runtime::owner::OwnerGate) {
    let root = crate::core::VersionedRoot::synthetic(backend_library::view_state_root(&[("shell".to_owned(), "tests".to_owned())]), 4);
    let gate = crate::runtime::owner::OwnerGate::ready(root, crate::model::ServiceMode::Attached);
    let mut rig = super::tests::rig_with_engine_gate(cx, Some(route), 1440.0, 900.0,
        ReadPool::start(2, |_| EmptyRelations).expect("pool"), super::tests::RootOnly, Some(gate.clone()));
    rig.cx.update(|window, cx| { facet::probe::enable(cx); window.set_a11y_forced(true); });
    rig.repaint();
    (rig, gate)
}

fn shelf_native_labels(rig: &mut Rig) -> Vec<String> {
    // Paint immediately, before servicing the gate watcher: a live owner fence
    // must revoke stale facts even while the Store still retains its old bytes.
    rig.repaint();
    let json = rig.cx.update(|window, _| window.debug_a11y_tree_json()).expect("native Shelf tree");
    let tree: serde_json::Value = serde_json::from_str(&json).expect("native tree JSON");
    tree["nodes"].as_object().expect("nodes").values()
        .filter_map(|node| node["aria"]["label"].as_str().map(str::to_owned)).collect()
}

#[gpui::test]
fn library_relationship_uncertainty_survives_native_paint_at_all_sizes(cx: &mut TestAppContext) {
    let (mut rig, _gate) = fact_rig(cx, Route::Orbit(crate::navigation::OrbitRoute::Home));
    let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
    for appearance in [crate::model::AppearancePreference::Abyss, crate::model::AppearancePreference::Glacier] {
        rig.go(Intent::SetAppearance(appearance));
        for percent in [100_u16, 150, 200] {
            rig.go(Intent::ZoomTo { display: display.clone(), percent });
            for width in [360.0, 480.0, 663.0, 1440.0] {
                rig.cx.simulate_resize(gpui::size(px(width), px(900.0)));
                rig.settle();
                let (overlays, drawer_open) = rig.shell.read_with(rig.cx, |shell, cx| (
                    shell.frame().expect("responsive frame").shelf_overlays,
                    shell.chrome_words(cx).iter().any(|(key, value)| *key == "drawer" && value == "open"),
                ));
                if overlays && !drawer_open { rig.keys("secondary-\\"); }
                for (lens, expected) in [
                    ("Rests on", "Library dependency relationships are not indexed. Choose a package to read its dependencies."),
                    ("Used by", "Library usage relationships are not indexed. Open a saved project's dependency tree to inspect its packages."),
                ] {
                    let lens = super::tests::native_bounds(&mut rig, "Tab", lens, true).expect("visible native lens control");
                    rig.cx.simulate_click(lens.center(), Modifiers::default());
                    rig.settle();
                    let labels = shelf_native_labels(&mut rig);
                    assert!(labels.iter().any(|label| label == expected), "{width}px/{percent}%: {labels:?}");
                    assert!(!labels.iter().any(|label| label == "The library rests on nothing." || label == "No project of yours uses the library yet."));
                    let bounds = super::tests::native_bounds(&mut rig, "Label", expected, false).expect("painted relation note");
                    assert!(bounds.left() >= px(0.0) && bounds.right() <= px(width) && bounds.bottom() <= px(900.0), "note outside native window at {width}px/{percent}%: {bounds:?}");
                    let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
                    assert!(ledger.texts.iter().any(|text| text.content == expected), "a native note must actually paint");
                }
            }
        }
    }
}

#[gpui::test]
fn native_owner_loss_and_replacement_immediately_revoke_observed_empty_shelf_facts(cx: &mut TestAppContext) {
    use crate::runtime::owner::OwnerState;
    for (keys, empty) in [("g r", "Rests on nothing"), ("g u", "Nothing here uses it yet")] {
        for replacement in [false, true] {
            let (mut rig, gate) = fact_rig(cx, toml());
            into_the_sidebar(&mut rig);
            rig.keys(keys);
            assert!(shelf_native_labels(&mut rig).iter().any(|label| label == empty), "current completed fixture establishes actual absence");
            if replacement {
                let current = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
                gate.publish(OwnerState::Ready { key: current.with_generation(current.generation().saturating_add(1)), mode: crate::model::ServiceMode::Attached });
            } else {
                gate.publish(OwnerState::Failed("The relationship source stopped answering.".into()));
            }
            let labels = shelf_native_labels(&mut rig);
            assert!(!labels.iter().any(|label| label == empty), "old facts cannot assert current absence before the watcher runs: {labels:?}");
            assert!(labels.iter().any(|label| label.contains("earlier reading is retained")), "retained evidence must be disclosed: {labels:?}");
            assert!(labels.iter().any(|label| label.contains("not serving") || label.contains("current reading failed")), "owner loss has a native reason: {labels:?}");
        }
    }
}

#[gpui::test]
fn native_used_by_lens_is_visit_local_and_forward_restores_the_mounted_tab(cx: &mut TestAppContext) {
    use crate::navigation::presentation::ShelfLens;
    let mut rig = open(cx);
    rig.cx.update(|window, _| window.set_a11y_forced(true));
    click_text(&mut rig, "Used by");
    let before = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().reading.current.clone());
    assert_eq!(before.presentation.controls().shelf.lens, ShelfLens::UsedBy);
    let selected = super::tests::native_bounds(&mut rig, "Tab", "Used by", true).expect("native Used by tab");
    assert!(selected.size.width > px(0.0));
    rig.go(Intent::Navigate(Route::World));
    rig.keys("secondary-[");
    assert_eq!(rig.route(), toml());
    assert!(shelf_texts(&mut rig).iter().any(|text| text.0 == "toml_pin"), "Back restores actual dependent rows");
    rig.keys("secondary-]");
    rig.keys("secondary-[");
    assert_eq!(rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().reading.current.id), before.id);
    assert!(shelf_texts(&mut rig).iter().any(|text| text.0 == "toml_pin"), "Forward/Back preserves Used by, not Contents");
    rig.go(Intent::Navigate(Route::Orbit(crate::navigation::OrbitRoute::Home)));
    rig.go(Intent::Navigate(toml()));
    assert_ne!(rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().reading.current.id), before.id);
    assert_eq!(rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().reading.current.presentation.controls().shelf.lens), ShelfLens::Contents,
        "a fresh visit to the same package has independent intent");
}

/// The library's guidance is a semantic row, even when it needs several
/// lines. The following row must begin below its actually measured paint.
#[gpui::test]
fn mounted_library_guidance_wraps_without_covering_the_next_row(cx: &mut TestAppContext) {
    use crate::navigation::presentation::{ReadingChange, ShelfLens};
    use facet::probe::TextOverflow;

    const NOTE: &str = "Library usage relationships are not indexed. Open a saved project's dependency tree to inspect its packages.";
    for width in [1440.0, 390.0] {
        let mut rig = super::tests::rig(cx, Some(Route::Orbit(crate::navigation::OrbitRoute::Home)), width, 700.0);
        let visit = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().reading.current.id);
        rig.go(Intent::SetReading { visit, change: ReadingChange::ShelfLens(ShelfLens::UsedBy) });
        let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
        for percent in [100, 150, 200] {
            rig.go(Intent::ZoomTo { display: display.clone(), percent });
            let first = super::fit_tests::painted(&mut rig);
            if !first.texts.iter().any(|text| text.content == NOTE) {
                let toggle = first.targets.iter().find(|target| target.key == "tb-shelf")
                    .expect("native shelf toggle when the shelf is compact").bounds.clone();
                rig.cx.simulate_click(point(px(toggle.x + toggle.width / 2.0), px(toggle.y + toggle.height / 2.0)), Modifiers::default());
                rig.settle();
            }
            for theme in [crate::model::AppearancePreference::Abyss, crate::model::AppearancePreference::Glacier] {
                rig.go(Intent::SetAppearance(theme));
                rig.cx.update(|_, cx| cx.set_global(gpui::TextTrace));
                let ledger = super::fit_tests::painted(&mut rig);
                let note = ledger.texts.iter().find(|text| text.content == NOTE)
                    .expect("full library guidance is in the painted shelf");
                assert_eq!(note.overflow, TextOverflow::Wrap, "{width}px at {percent}%");
                assert!(note.natural_width > note.bounds.width && note.bounds.height > note.line_height * 1.5,
                    "guidance needs multiple measured lines at {width}px and {percent}%: {note:?}");
                let next = ledger.texts.iter().filter(|text| text.scroll_ancestors.iter().any(|key| key == "shelf-rows")
                    && text.bounds.y > note.bounds.y + 1.0 && text.content != NOTE)
                    .min_by(|a, b| a.bounds.y.total_cmp(&b.bounds.y))
                    .expect("a semantic row follows the guidance");
                assert!(note.bounds.y + note.bounds.height <= next.bounds.y + 1.0,
                    "wrapped guidance overlaps the following row at {width}px and {percent}%: {note:?}, {next:?}");
                let painted = rig.cx.update(|window, _| window.painted_texts().iter()
                    .find(|text| text.text.as_ref() == NOTE).cloned())
                    .expect("the complete suffix 'inspect its packages.' is painted as one wrapped paragraph");
                let viewport = ledger.scrolls.iter().find(|sample| sample.key == "shelf-rows")
                    .expect("the note's measured list viewport");
                assert!(f32::from(painted.bounds.size.height) >= note.bounds.height - note.line_height * 0.5,
                    "every wrapped line, including the recovery suffix, must survive native paint clipping");
                assert!(f32::from(painted.bounds.bottom()) <= viewport.viewport.y + viewport.viewport.height + 1.0,
                    "the guidance's painted recovery suffix remains visible inside the list");
            }
        }
    }
}

#[gpui::test]
fn measured_shelf_wheel_offset_survives_back_and_keyboard_reveals_a_row(cx: &mut TestAppContext) {
    let mut rig = open(cx);
    click_text(&mut rig, "Types");
    assert!(row_names(&mut rig).iter().any(|name| name == "RelationLabel"),
        "the real fold inserts its semantic children before scrolling");
    super::fit_tests::resize(&mut rig, 1440.0, 360.0);
    let read_scroll = |rig: &mut Rig| {
        let ledger = super::fit_tests::painted(rig);
        ledger.scrolls.into_iter().find(|sample| sample.key == "shelf-rows")
            .expect("the measured shelf publishes its own viewport and extent")
    };
    let before = read_scroll(&mut rig);
    assert!(before.content.height > before.viewport.height + 100.0,
        "the fixture must have a scrollable shelf: {before:?}");
    let at = point(px(before.viewport.x + before.viewport.width / 2.0),
        px(before.viewport.y + before.viewport.height / 2.0));
    rig.cx.simulate_scroll(at, point(px(0.0), px(-120.0)));
    rig.settle();
    let scrolled = read_scroll(&mut rig);
    let displacement = scrolled.offset.expect("measured list publishes displacement").y;
    assert!(displacement < -20.0, "native wheel did not move the list: {scrolled:?}");
    let remembered = rig.graph.store.read_with(rig.cx, |store, _| {
        store.snapshot().session().reading.current.presentation.controls().shelf.offset.pixels().1
    });
    assert!((remembered - displacement).abs() <= 1.0,
        "the reading visit must save the measured list's actual displacement");

    rig.go(Intent::Navigate(Route::World));
    rig.keys("secondary-[");
    assert_eq!(rig.route(), toml());
    let restored = read_scroll(&mut rig);
    let actual = restored.offset.expect("restored measured offset").y;
    assert!((actual - remembered).abs() <= 2.0,
        "Back must restore the prior visit's shelf position: {remembered} → {actual}");

    into_the_sidebar(&mut rig);
    rig.keys("down down down down down down");
    let ledger = super::fit_tests::painted(&mut rig);
    let viewport = ledger.scrolls.iter().find(|sample| sample.key == "shelf-rows")
        .expect("measured shelf scroll probe").viewport.clone();
    let focused = ledger.targets.iter().find(|target| target.scroll_ancestors.iter().any(|key| key == "shelf-rows")
        && target.state.focused && target.state.focusable)
        .expect("keyboard focus is on a mounted shelf row");
    assert!(focused.bounds.y < viewport.y + viewport.height
        && focused.bounds.y + focused.bounds.height > viewport.y,
        "the focused row must be revealed in the measured viewport: {focused:?}, {viewport:?}");
}
