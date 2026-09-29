//! The shelf's lenses through the real shell: Contents (at a release) · Rests
//! on · Used by list what the page already read, change the list and never
//! the page, and follow their rows to the right place.

use super::tests::{Fixture, Rig, dossier, rig_with_reads};
use crate::core::PackageId;
use crate::model::pages::{
    Dependency, DependencyScope, Known, PackageRecord, PackageRef, PageValue, ReadFailure, RecordSource, Standing,
    VersionEntry,
};
use crate::navigation::{PackageLane, PackageRoute, Route};
use crate::runtime::reads::{PageReader, ReadContext, ReadPool, ReadRequest};
use gpui::{Modifiers, TestAppContext, point, px};
use std::sync::Arc;

const PINNED: &str = "pkg:cargo/toml@0.8.23";

/// toml at its pin, with three releases, two dependencies (one resolved to an
/// indexed release, one not) and one dependent, as the index answers.
struct Registry;

impl PageReader for Registry {
    fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
        let ReadRequest::Package(package) = request else {
            return Fixture.read(request, context);
        };
        let mut about = dossier();
        about.package = package.clone();
        let name = package.display_name().to_owned();
        let release = |version: &str, standing: Standing| VersionEntry {
            package: PackageRef::parse(&format!("pkg:cargo/{name}@{version}")).expect("release"),
            version: Arc::from(version),
            standing,
            current: package.version() == Some(version),
        };
        about.versions = Known::Known(Arc::from([
            release("1.1.6", Standing::Available),
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

fn said(rig: &mut Rig) -> Vec<String> {
    shelf_texts(rig).into_iter().map(|text| text.0).collect()
}

#[gpui::test]
fn the_shelf_opens_a_book_on_its_contents_at_its_pin_with_three_lenses(cx: &mut TestAppContext) {
    let mut rig = open(cx);
    let said = said(&mut rig);
    for lens in ["Contents", "Rests on", "Used by"] {
        assert!(said.iter().any(|text| text == lens), "the {lens:?} lens is not drawn: {said:#?}");
    }
    assert!(!said.iter().any(|text| text == "Versions"), "versions are not a lens of their own: {said:#?}");
    // Contents is the outline at a release: the pin heads it, then the modules.
    let at = |words: &str| said.iter().position(|text| text == words).unwrap_or_else(|| panic!("{words:?} missing: {said:#?}"));
    assert_eq!(said.get(at("0.8.23") + 1).map(String::as_str), Some("your pin"), "{said:#?}");
    assert!(at("0.8.23") < at("glyph"), "the release heads the outline: {said:#?}");
    assert!(!said.iter().any(|text| text == "1.1.6"), "the releases stay folded until asked: {said:#?}");
}

#[gpui::test]
fn the_release_row_opens_the_releases_in_place_and_reads_the_book_at_one(cx: &mut TestAppContext) {
    let mut rig = open(cx);
    click_text(&mut rig, "0.8.23");
    let said = said(&mut rig);
    let at = |words: &str| said.iter().position(|text| text == words).unwrap_or_else(|| panic!("{words:?} missing: {said:#?}"));
    // Opened, the releases list under the head, newest first; the pin and a
    // yanked release say so; the outline stays beneath them.
    assert!(at("1.1.6") < at("0.9.0"), "newest first: {said:#?}");
    assert_eq!(said.get(at("0.9.0") + 1).map(String::as_str), Some("yanked"), "{said:#?}");
    let pins = said.iter().filter(|text| *text == "your pin").count();
    assert_eq!(pins, 2, "the head and the pinned release both say it: {said:#?}");
    assert!(at("0.9.0") < at("glyph"), "contents and releases are one list: {said:#?}");
    // Choosing a release reads the book at it, and the list folds.
    click_text(&mut rig, "1.1.6");
    let Route::Package(route) = rig.route() else { panic!("still the package") };
    assert_eq!(route.at.as_ref().map(|at| at.as_str().to_owned()), Some("1.1.6".to_owned()));
    let said = self::said(&mut rig);
    let at = |words: &str| said.iter().position(|text| text == words).unwrap_or_else(|| panic!("{words:?} missing: {said:#?}"));
    assert_eq!(said.get(at("1.1.6") + 1).map(String::as_str), Some("reading"), "the head names the release being read: {said:#?}");
    assert!(!said.iter().any(|text| text == "0.9.0"), "choosing folds the releases: {said:#?}");
}

#[gpui::test]
fn the_rests_on_lens_opens_what_it_resolved_and_says_what_it_could_not(cx: &mut TestAppContext) {
    let mut rig = open(cx);
    click_text(&mut rig, "Rests on");
    let said = said(&mut rig);
    let at = |words: &str| said.iter().position(|text| text == words).unwrap_or_else(|| panic!("{words:?} missing: {said:#?}"));
    assert_eq!(said.get(at("toml_edit") + 1).map(String::as_str), Some("0.22"), "{said:#?}");
    assert_eq!(said.get(at("indexmap") + 1).map(String::as_str), Some("2.0 · optional"), "{said:#?}");
    click_text(&mut rig, "toml_edit");
    let Route::Package(route) = rig.route() else { panic!("a package page") };
    assert_eq!(route.package.as_str(), "pkg:cargo/toml_edit@0.22.27");
    // A new book opens on its contents.
    let said = self::said(&mut rig);
    assert!(!said.iter().any(|text| text == "indexmap"), "the new book is not on toml's Rests on lens: {said:#?}");
}

#[gpui::test]
fn the_used_by_lens_lists_dependents_and_an_unknown_list_says_why(cx: &mut TestAppContext) {
    let mut rig = open(cx);
    click_text(&mut rig, "Used by");
    let said = said(&mut rig);
    assert!(said.iter().any(|text| text == "toml_pin"), "{said:#?}");
    // The plain fixture's own package is a local project: no dependents are
    // recorded, and the lens says so instead of drawing an empty list.
    let mut rig2 = super::tests::rig(cx, Some(super::tests::page_route("RelationLabel")), 1440.0, 900.0);
    rig2.cx.update(|_, cx| facet::probe::enable(cx));
    click_text(&mut rig2, "Used by");
    let said = self::said(&mut rig2);
    assert!(said.iter().any(|text| text.contains("local project")), "an honest reason, not an empty list: {said:#?}");
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
