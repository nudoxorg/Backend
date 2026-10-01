//! The shelf's release comb drives the route's `at=` through the real shell.

use super::tests::{Fixture, Rig, dossier, rig_with_reads};
use crate::core::PackageId;
use crate::model::pages::{Known, PackageRef, PageValue, ReadFailure, Standing, VersionEntry};
use crate::navigation::{Coordinate, PackageLane, PackageRoute, Route, SymbolRoute, View};
use crate::runtime::reads::{PageReader, ReadContext, ReadPool, ReadRequest};
use gpui::{Modifiers, TestAppContext, point, px};
use std::sync::Arc;

const PINNED: &str = "pkg:cargo/toml@0.8.23";

/// A registry package with three releases; each dossier is about the
/// release it was asked for (its `current`), as the index answers.
struct Registry;

impl PageReader for Registry {
    fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
        let ReadRequest::Package(package) = request else {
            return Fixture.read(request, context);
        };
        let mut about = dossier();
        about.package = package.clone();
        let name = package.display_name().to_owned();
        let release = |version: &str| VersionEntry {
            package: PackageRef::parse(&format!("pkg:cargo/{name}@{version}")).expect("release"),
            version: Arc::from(version),
            standing: Standing::Available,
            current: package.version() == Some(version),
        };
        about.versions = Known::Known(Arc::from([release("1.1.6"), release("1.0.0"), release("0.8.23")]));
        Ok(PageValue::Package(about))
    }
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

/// `toml::de::from_str`'s page, at the pin.
fn from_str() -> Route {
    Route::Symbol(SymbolRoute {
        project: None,
        package: PackageId::new(PINNED).expect("package"),
        id: Coordinate::new(&format!("{PINNED}::de.rs:20::from_str")).expect("coordinate"),
        at: None,
        view: View::Page,
        line: None,
        selected: None,
    })
}

fn open_at(cx: &mut TestAppContext, route: Route) -> Rig {
    let pool = ReadPool::start(1, |_| Registry).expect("pool");
    let mut rig = rig_with_reads(cx, Some(route), 1440.0, 900.0, pool);
    rig.cx.update(|_, cx| {
        crate::runtime::fixture_releases::install(cx);
        facet::probe::enable(cx);
    });
    rig.repaint();
    rig
}

fn open(cx: &mut TestAppContext) -> Rig {
    open_at(cx, toml())
}

/// Clicks the comb's right end: the newest release.
fn scrub_to_newest(rig: &mut Rig) {
    rig.repaint();
    let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
    let comb = ledger.targets.iter().find(|target| target.key == "shelf-versions").expect("the comb").bounds.clone();
    rig.cx.simulate_click(point(px(comb.x + comb.width - 2.0), px(comb.y + comb.height / 2.0)), Modifiers::default());
    rig.settle();
}

/// Away from the pin the upgrade lens appears: the shelf's line under the
/// comb compares the pin with the viewed release, and the page's section
/// names the release it upgrades to. Esc takes both away. (The counts are
/// W-Data's to prove; this is the wiring.)
#[gpui::test]
fn scrubbing_away_from_the_pin_opens_the_upgrade_lens_and_escape_closes_it(cx: &mut TestAppContext) {
    let mut rig = open_at(cx, from_str());
    let upgrade = |rig: &mut Rig| rig.shell.read_with(rig.cx, |shell, cx| shell.shelf_upgrade(cx));
    let heading = |rig: &mut Rig| {
        rig.repaint();
        let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
        ledger.texts.iter().filter(|text| text.key == "upgrade-heading").map(|text| text.content.clone()).collect::<Vec<_>>()
    };
    assert_eq!(upgrade(&mut rig), None, "at the pin there is nothing to compare");
    assert!(heading(&mut rig).is_empty());
    scrub_to_newest(&mut rig);
    assert_eq!(rig.route().at().map(|at| at.as_str().to_owned()), Some("1.1.6".to_owned()));
    assert_eq!(heading(&mut rig), ["Upgrading to 1.1.6"], "the page's section names the release");
    assert_eq!(
        upgrade(&mut rig),
        Some(("0.8.23".into(), "1.1.6+spec-1.1.0".into())),
        "the shelf's line compares the pin with the viewed release, as the release data spells them"
    );
    rig.keys("escape");
    assert_eq!(rig.route(), from_str(), "Esc returns to the pin");
    assert!(heading(&mut rig).is_empty(), "and the section is gone");
    assert_eq!(upgrade(&mut rig), None, "and so is the line");
}

/// A package the release data does not know still views the release; its
/// comb shows no lens.
#[gpui::test]
fn a_package_without_release_data_views_the_release_with_no_lens(cx: &mut TestAppContext) {
    let serde = Route::Package(PackageRoute {
        project: None,
        package: PackageId::new("pkg:cargo/serde@0.8.23").expect("package"),
        lane: PackageLane::Overview,
        selected: None,
        at: None,
    });
    let mut rig = open_at(cx, serde);
    scrub_to_newest(&mut rig);
    rig.repaint();
    assert_eq!(rig.route().at().map(|at| at.as_str().to_owned()), Some("1.1.6".to_owned()), "the release is viewed");
    let upgrade = rig.shell.read_with(rig.cx, |shell, cx| shell.shelf_upgrade(cx));
    assert_eq!(upgrade, None, "no release data, no lens");
}

#[gpui::test]
fn scrubbing_the_comb_views_a_release_and_escape_returns_to_the_pin(cx: &mut TestAppContext) {
    let mut rig = open(cx);
    let marks = |rig: &mut Rig| rig.shell.read_with(rig.cx, |shell, cx| shell.shelf_comb(cx)).expect("the comb drew");
    assert_eq!(marks(&mut rig), (Some("0.8.23".into()), None), "home: the pin, nothing else viewed");
    // The newest release sits at the comb's right end.
    scrub_to_newest(&mut rig);
    rig.repaint();
    assert_eq!(rig.route().at().map(|at| at.as_str().to_owned()), Some("1.1.6".to_owned()), "the route views 1.1.6");
    let here = rig.graph.store.read_with(rig.cx, |store, _| super::jump::here(&store.snapshot(), store).path.to_string());
    assert_eq!(here, "viewing 1.1.6 · you pin 0.8.23");
    assert_eq!(marks(&mut rig), (Some("0.8.23".into()), Some("1.1.6".into())), "the pin stays where you pin it");
    rig.keys("escape");
    assert_eq!(rig.route(), toml(), "Esc returns to the pin");
}
