//! The package page through the real shell: what it paints (the probe
//! ledger, never the model it was built from), what a module opens into,
//! what a click or a key does, and what it says when its source is missing
//! or read. Page data is the shell test fixture's dossier; the source on disk
//! is a small crate written to a temp directory and installed the way a
//! read would land.

#![allow(clippy::expect_used, clippy::panic)]

use super::target::PageTarget;
use crate::model::pages::PackageRef;
use crate::model::source_facts::{self, Reading};
use crate::navigation::Intent;
use crate::shell::anatomy_tests::{install, package_route, painted};
use crate::shell::tests::{PACKAGE, Rig, dossier, page_route, rig};
use facet::probe::Ledger;
use gpui::{Modifiers, TestAppContext, point, px};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The words painted under a key containing `part`, in paint order.
fn said(ledger: &Ledger, part: &str) -> Vec<String> {
    ledger
        .texts
        .iter()
        .filter(|t| t.key.contains(part))
        .map(|t| t.content.clone())
        .collect()
}

fn has(ledger: &Ledger, part: &str) -> bool {
    !said(ledger, part).is_empty()
}

struct ReadmeActions;

impl crate::runtime::reads::PageReader for ReadmeActions {
    fn read(
        &mut self,
        request: &crate::runtime::reads::ReadRequest,
        context: &crate::runtime::reads::ReadContext<'_>,
    ) -> Result<crate::model::pages::PageValue, crate::model::pages::ReadFailure> {
        use crate::model::local_package::{ReadmeHeading, ReadmeLink};
        use crate::model::pages::{Known, PageValue};
        let crate::runtime::reads::ReadRequest::Package(package) = request else {
            return crate::shell::tests::Fixture.read(request, context);
        };
        let mut about = dossier();
        about.package = package.clone();
        if *package == dossier().package {
            let source: Arc<str> = Arc::from(
                "# Present\n\n## Café\n\n[Open serde](pkg:cargo/serde@1.0.229)\n\n[Unavailable](javascript:alert(1))\n\n[Return to Café](#caf%C3%A9)\n",
            );
            let offset = source.find("## Café").expect("heading offset");
            let link = |label: &str, destination: &str| ReadmeLink {
                label: Arc::from(label),
                destination: Arc::from(destination),
                local_file: None,
                line: None,
            };
            about.readme_markdown = Known::Known(source);
            about.readme_links = Known::Known(Arc::from([
                link("Open serde", "pkg:cargo/serde@1.0.229"),
                link("Unavailable", "javascript:alert(1)"),
                link("Return to Café", "#caf%C3%A9"),
            ]));
            about.readme_headings = Known::Known(Arc::from([ReadmeHeading {
                slug: Arc::from("café"),
                element_id: Arc::from(format!("readme-heading-{offset}")),
                title: Arc::from("Café"),
                level: 2,
            }]));
        }
        Ok(PageValue::Package(about))
    }
}

#[gpui::test]
fn readme_link_keyboard_back_restores_exact_row_and_unavailable_row_stays_actionable(
    cx: &mut TestAppContext,
) {
    let pool = crate::runtime::reads::ReadPool::start(1, |_| ReadmeActions).expect("pool");
    let mut rig =
        crate::shell::tests::rig_with_reads(cx, Some(package_route()), 320.0, 900.0, pool);
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    let ledger = painted(&mut rig);
    assert!(
        ledger
            .targets
            .iter()
            .any(|target| target.key == "readme-link-0")
    );
    assert!(
        ledger
            .targets
            .iter()
            .any(|target| target.key == "readme-link-1")
    );
    walk_to(&mut rig, "readme-link-0", 200);
    rig.keys("enter");
    assert!(format!("{:?}", rig.route()).contains("serde@1.0.229"));
    rig.keys("cmd-[");
    assert_eq!(rig.route(), package_route());
    assert_eq!(focused(&mut rig).as_deref(), Some("readme-link-0"));
    walk_to(&mut rig, "readme-link-1", 200);
    let before = rig.route();
    rig.keys("enter");
    assert_eq!(
        rig.route(),
        before,
        "the unsupported scheme cannot navigate"
    );
    let ledger = painted(&mut rig);
    assert!(
        ledger
            .texts
            .iter()
            .any(|text| text.content.contains("unsupported or invalid address"))
    );
}

#[test]
fn package_hero_stacks_when_text_scale_leaves_no_readable_side_column() {
    assert!(super::hero_stacks(px(320.0), 2.0));
    assert!(super::hero_stacks(px(390.0), 2.0));
    assert!(super::hero_stacks(px(390.0), 1.0));
    assert!(!super::hero_stacks(px(900.0), 2.0));
}

/// The centre of the first text painted under a key containing `part`.
fn centre(ledger: &Ledger, part: &str) -> (f32, f32) {
    let text = ledger
        .texts
        .iter()
        .find(|t| t.key.contains(part))
        .unwrap_or_else(|| {
            panic!(
                "nothing painted under `{part}`: {:?}",
                ledger.texts.iter().map(|t| &t.key).collect::<Vec<_>>()
            )
        });
    (
        text.bounds.x + text.bounds.width / 2.0,
        text.bounds.y + text.bounds.height / 2.0,
    )
}

fn click(rig: &mut Rig, at: (f32, f32)) {
    rig.cx
        .simulate_click(point(px(at.0), px(at.1)), Modifiers::default());
    rig.settle();
}

fn move_to(rig: &mut Rig, at: (f32, f32)) {
    rig.cx
        .simulate_mouse_move(point(px(at.0), px(at.1)), None, Modifiers::default());
    rig.settle();
}

/// A crate on disk that says a few things about itself.
fn krate() -> PathBuf {
    // One directory per call: tests run in parallel, and one that rewrites a
    // crate under another's feet reads an empty manifest.
    static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local/scratch")
        .join(format!("nudox-folio-{}-{call}-present", std::process::id()));
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
    write(
        "src/lib.rs",
        "//! How one symbol page reads.\npub mod glyph;\n",
    );
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
    read_and_open_at(rig, &krate());
}

/// Installs what reading the crate at `dir` said, and goes to the package page.
fn read_and_open_at(rig: &mut Rig, dir: &Path) {
    let facts =
        source_facts::read(dir, &std::collections::HashMap::new(), None).expect("the crate reads");
    let package = package();
    rig.cx.update(|_, cx| {
        facet::probe::enable(cx);
        source_facts::install(&package, Reading::Ready(Arc::new(facts)), cx);
    });
    rig.go(Intent::Navigate(package_route()));
}

#[gpui::test]
fn a_page_whose_source_is_missing_says_so_instead_of_inventing_facts(cx: &mut TestAppContext) {
    let mut rig = rig(cx, None, 1440.0, 900.0);
    install(&mut rig);
    rig.cx.update(|_, cx| {
        source_facts::install(
            &package(),
            Reading::Absent("Its source is not on this machine.".into()),
            cx,
        );
    });
    rig.go(Intent::Navigate(package_route()));
    let ledger = painted(&mut rig);
    let heads = said(&ledger, "heads-words");
    assert_eq!(
        heads,
        [
            "Its source is not on this machine. Nothing is read for build scripts, network, files and unsafe."
        ],
        "{heads:?}"
    );
    let weight = said(&ledger, "weight-words");
    assert_eq!(
        weight,
        ["Its source is not on this machine. Nothing is read for lines of code."],
        "{weight:?}"
    );
    assert_eq!(
        said(&ledger, "advisories-word"),
        ["yours"],
        "a project of yours is not checked against feeds of published releases"
    );
    assert_eq!(said(&ledger, "licence-verdict"), ["Permissive"]);
    assert!(
        !has(&ledger, "weight-note") && !has(&ledger, "heads-note"),
        "nothing is labelled `from its source` when nothing was read from it"
    );
    assert!(
        !has(&ledger, "berg") && !has(&ledger, "features"),
        "no berg and no features bar without a manifest"
    );
}

#[gpui::test]
fn the_outline_is_the_territory_its_modules_are_regions_and_a_module_opens_into_cards_that_say_badges_never_code(
    cx: &mut TestAppContext,
) {
    let mut rig = rig(cx, Some(package_route()), 1440.0, 900.0);
    install(&mut rig);
    let ledger = painted(&mut rig);
    // The fixture puts every name in `glyph.rs`, so the outline is one module of six names.
    assert_eq!(
        said(&ledger, "present-names").first().map(String::as_str),
        Some("6"),
        "six public names"
    );
    assert_eq!(said(&ledger, "shingles-region-"), ["glyph"]);
    let said_lines = rig.said();
    assert!(
        !said_lines
            .iter()
            .any(|line| line == "Start here" || line == "what you hold"),
        "no fixture-ranked tour: {said_lines:#?}"
    );
    click(&mut rig, centre(&ledger, "shingles-region-glyph"));
    let ledger = painted(&mut rig);
    let names: Vec<String> = ledger
        .texts
        .iter()
        .filter(|t| t.key.contains("module-card-") && t.key.ends_with("-name"))
        .map(|t| t.content.clone())
        .collect();
    assert_eq!(
        names,
        [
            "Identity",
            "RelationLabel",
            "RelationDirection",
            "KindGlyph",
            "relation_label",
            "Outline"
        ],
        "the recorded order, one card each"
    );
    let kinds: Vec<String> = ledger
        .texts
        .iter()
        .filter(|t| t.key.contains("module-card-") && t.key.ends_with("-kind"))
        .map(|t| t.content.clone())
        .collect();
    assert_eq!(kinds, ["struct", "enum", "enum", "struct", "fn", "struct"]);
    assert!(
        !ledger.texts.iter().any(|t| t.content.contains("pub struct")
            || t.content.contains("pub enum")
            || t.content.contains('{')),
        "a card says what a name is, not its code"
    );
    assert!(
        has(&ledger, "rail-close-name"),
        "the rail offers a way back"
    );
}

#[gpui::test]
fn a_card_is_a_door_to_its_exact_indexed_page(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(package_route()), 1440.0, 900.0);
    install(&mut rig);
    let ledger = painted(&mut rig);
    click(&mut rig, centre(&ledger, "shingles-region-glyph"));
    let ledger = painted(&mut rig);
    let key = format!(
        "pkg-card-{}",
        crate::shell::tests::coordinate("RelationLabel")
    );
    let card = ledger
        .targets
        .iter()
        .find(|target| target.key == key)
        .unwrap_or_else(|| panic!("no card door `{key}`"))
        .bounds
        .clone();
    click(
        &mut rig,
        (card.x + card.width / 2.0, card.y + card.height / 2.0),
    );
    assert_eq!(rig.route(), page_route("RelationLabel"));
}

#[gpui::test]
fn j_walks_the_modules_enter_opens_one_and_j_then_enter_opens_a_name(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(package_route()), 1440.0, 900.0);
    install(&mut rig);
    let _ = painted(&mut rig);
    rig.keys("j");
    let (_, focused) = rig
        .shell
        .read_with(rig.cx, |shell, cx| shell.focus_state(cx));
    assert_eq!(
        focused.as_deref(),
        Some("pkg-module-glyph"),
        "j lands on the first module"
    );
    rig.keys("enter");
    let ledger = painted(&mut rig);
    assert_eq!(
        rig.route(),
        package_route(),
        "opening a module stays on the page"
    );
    assert!(
        has(&ledger, "module-card-0-name"),
        "the module's cards are out"
    );
    rig.keys("j");
    let (_, focused) = rig
        .shell
        .read_with(rig.cx, |shell, cx| shell.focus_state(cx));
    assert_eq!(
        focused.as_deref().map(|id| id.starts_with("pkg-card-")),
        Some(true),
        "j now walks the cards: {focused:?}"
    );
    rig.keys("enter");
    assert!(
        matches!(rig.route(), crate::navigation::Route::Symbol(_)),
        "enter on a card opens its page: {:?}",
        rig.route()
    );
}

#[gpui::test]
fn what_the_source_says_is_read_from_it_and_labelled_so(cx: &mut TestAppContext) {
    let mut rig = rig(cx, None, 1440.0, 900.0);
    read_and_open(&mut rig);
    let ledger = painted(&mut rig);
    // Heads-up: it starts a program (a warning) and has one unsafe block.
    assert_eq!(said(&ledger, "heads-label"), ["HEADS-UP"]);
    assert_eq!(
        said(&ledger, "heads-accent"),
        ["1"],
        "one warning: the program it starts"
    );
    assert_eq!(said(&ledger, "heads-note"), ["from its source"]);
    // Weight: a crate with no dependencies is all above the water.
    assert_eq!(said(&ledger, "weight-note"), ["from its source"]);
    assert!(
        said(&ledger, "weight-caption")
            .iter()
            .any(|c| c.starts_with("0 packages beneath")
                && (c.contains("default features") || c.contains("in your lock"))),
        "{:?}",
        said(&ledger, "weight-caption")
    );
    // The features bar reads its manifest: nothing on by default.
    assert_eq!(said(&ledger, "features-label"), ["Features"]);
    assert_eq!(said(&ledger, "features-of"), ["of 1 on"]);
    // Names take their declarations and first sentences from the source.
    assert_eq!(said(&ledger, "shingles-region-"), ["glyph"]);
    click(&mut rig, centre(&ledger, "shingles-region-glyph"));
    let ledger = painted(&mut rig);
    let docs: Vec<String> = ledger
        .texts
        .iter()
        .filter(|t| t.key.contains("module-card-") && t.key.ends_with("-doc"))
        .map(|t| t.content.clone())
        .collect();
    assert!(
        docs.iter().any(|d| d == "Names a relation"),
        "the first sentence of `RelationLabel`'s doc: {docs:?}"
    );
    assert!(
        !docs.iter().any(|d| d == "undocumented") || docs.len() > 1,
        "{docs:?}"
    );
}

#[gpui::test]
fn the_heads_up_hand_fans_out_and_a_click_opens_the_evidence_read_from_source(
    cx: &mut TestAppContext,
) {
    let mut rig = rig(cx, None, 1440.0, 900.0);
    read_and_open(&mut rig);
    let ledger = painted(&mut rig);
    let (x, y) = centre(&ledger, "heads-label");
    move_to(&mut rig, (x, y + 27.0));
    rig.frame(400);
    let ledger = painted(&mut rig);
    let words = said(&ledger, "heads-word-");
    assert!(
        words.iter().any(|w| w == "Starts programs") && words.iter().any(|w| w == "Unsafe blocks"),
        "hovering spreads the hand into words: {words:?}"
    );
    click(&mut rig, (x, y + 27.0));
    rig.frame(400);
    let ledger = painted(&mut rig);
    let evidence: Vec<&str> = ledger
        .texts
        .iter()
        .map(|t| t.content.as_str())
        .filter(|c| c.starts_with("src/glyph.rs:"))
        .collect();
    assert!(
        !evidence.is_empty(),
        "the sheet names the file and line of what it found: {:?}",
        ledger.texts.iter().map(|t| &t.content).collect::<Vec<_>>()
    );
    assert!(
        ledger
            .texts
            .iter()
            .any(|t| t.content.contains("Command::new(\"true\")")),
        "and shows the line"
    );
}

// ------------------------------------------------------------------ widths

use crate::shell::fit_tests::{findings, painted as painted_at, resize};

/// Where every word of the folio is, by key: what a frame says about layout.
fn folio_layout(ledger: &Ledger) -> Vec<(String, [f32; 4])> {
    let mut out: Vec<(String, [f32; 4])> = ledger
        .texts
        .iter()
        .filter(|t| {
            t.key.contains("folio-") || t.key.starts_with("name:") || t.key.starts_with("mk-")
        })
        .map(|t| {
            (
                t.key.clone(),
                [t.bounds.x, t.bounds.y, t.bounds.width, t.bounds.height],
            )
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// The window is dragged narrow, wide, narrower and back: the page is what
/// a window born at the final size would draw (the layout decisions are a
/// function of the room and of the mode held through a band, never of the
/// route the window took).
#[gpui::test]
fn a_page_after_a_resize_storm_equals_a_fresh_page(cx: &mut TestAppContext) {
    let mut stormed = rig(cx, None, 1440.0, 900.0);
    read_and_open(&mut stormed);
    for (width, height) in [
        (800.0, 600.0),
        (2560.0, 1440.0),
        (1024.0, 700.0),
        (1440.0, 900.0),
    ] {
        resize(&mut stormed, width, height);
    }
    let after = folio_layout(&painted_at(&mut stormed));
    let mut fresh = rig(cx, None, 1440.0, 900.0);
    read_and_open(&mut fresh);
    let born = folio_layout(&painted_at(&mut fresh));
    assert!(!born.is_empty(), "the fresh page painted nothing");
    assert_eq!(
        after.iter().map(|(k, _)| k).collect::<Vec<_>>(),
        born.iter().map(|(k, _)| k).collect::<Vec<_>>(),
        "the same words are on the page"
    );
    for ((key, was), (_, is)) in after.iter().zip(&born) {
        for (axis, (a, b)) in was.iter().zip(is).enumerate() {
            assert!(
                (a - b).abs() <= 0.75,
                "`{key}` sits differently after a resize storm (axis {axis}): {was:?} against a fresh window's {is:?}"
            );
        }
    }
}

/// A window dragged across the crest's edge (four cells in a row become two
/// by two, and the crest grows by a row): the blocks under it glide down to
/// their new place instead of jumping the crest's change of height in one
/// frame (FLUID-C: every block under a changed one moved by the change in
/// one frame). Each frame the modules' heading moves by a glide's step at
/// most, and it lands where a window born at that width puts it.
#[gpui::test]
fn the_blocks_under_the_crest_glide_when_its_cells_change_rows(cx: &mut TestAppContext) {
    let mut rig = rig(cx, None, 1600.0, 900.0);
    read_and_open(&mut rig);
    let heading = |ledger: &Ledger| {
        ledger
            .texts
            .iter()
            .find(|t| t.key.contains("names-words"))
            .map(|t| t.bounds.y)
    };
    let before = heading(&painted_at(&mut rig)).expect("the modules' heading is drawn");
    rig.cx.simulate_resize(gpui::size(px(1100.0), px(900.0)));
    let mut ys = vec![before];
    for _ in 0..40 {
        rig.frame(16);
        rig.cx.update(|_, cx| facet::probe::enable(cx));
        let _ = rig.cx.update(|_, cx| facet::probe::take(cx));
        rig.repaint();
        let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
        if let Some(y) = heading(&ledger) {
            ys.push(y);
        }
    }
    rig.settle();
    let landed = heading(&painted_at(&mut rig)).expect("the heading is drawn");
    assert!(
        (landed - before).abs() > 40.0,
        "the crest changed rows between 1600 and 1100 px, so the heading moved: {before} -> {landed}"
    );
    let worst = ys
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).abs())
        .fold(0.0_f32, f32::max);
    assert!(
        worst <= (landed - before).abs() * 0.6,
        "the heading jumped {worst:.1} px in one frame on its way from {before:.1} to {landed:.1}: {ys:?}"
    );
    let mut fresh = rig_at(cx, 1100.0);
    let born = heading(&painted_at(&mut fresh)).expect("drawn");
    assert!(
        (born - landed).abs() <= 0.75,
        "it lands where a fresh 1100 px window puts it: {landed} vs {born}"
    );
}

fn rig_at(cx: &mut TestAppContext, width: f32) -> Rig {
    let mut rig = rig(cx, None, width, 900.0);
    read_and_open(&mut rig);
    rig
}

/// From a phone to a big screen nothing of the folio hangs past the window's
/// sides, is cut mid-glyph, or lies on other words. (Below the fold is the
/// reader's to scroll; only the sides are held to the window.)
#[gpui::test]
fn the_page_holds_together_at_every_width(cx: &mut TestAppContext) {
    let mut rig = rig(cx, None, 1440.0, 900.0);
    read_and_open(&mut rig);
    for (width, height) in [
        (1440.0, 900.0),
        (320.0, 640.0),
        (360.0, 740.0),
        (390.0, 844.0),
        (430.0, 932.0),
        (480.0, 800.0),
        (600.0, 800.0),
        (768.0, 1024.0),
        (800.0, 600.0),
        (980.0, 700.0),
        (1024.0, 700.0),
        (1280.0, 800.0),
        (1920.0, 1080.0),
        (2560.0, 1440.0),
    ] {
        resize(&mut rig, width, height);
        let ledger = painted_at(&mut rig);
        let mine = |key: &str| {
            key.contains("folio-") || key.starts_with("name:") || key.starts_with("mk-")
        };
        let ours: Vec<String> = findings(&ledger, width, height)
            .into_iter()
            .filter(|f| {
                f.rule != "edge"
                    && (f.what.contains("[folio-")
                        || f.what.contains("[name:")
                        || f.what.contains("[mk-"))
            })
            .map(|f| format!("{}: {}", f.rule, f.what))
            .collect();
        assert!(
            ours.is_empty(),
            "at {width} px the page has {} problems: {ours:#?}",
            ours.len()
        );
        for text in ledger
            .texts
            .iter()
            .filter(|t| mine(&t.key) && !t.content.trim().is_empty())
        {
            assert!(
                text.bounds.x >= -0.5 && text.bounds.x + text.bounds.width <= width + 0.5,
                "at {width} px `{}` runs past the window's side: x {:.0}, {:.0} wide [{}]",
                text.content,
                text.bounds.x,
                text.bounds.width,
                text.key
            );
        }
    }
}

// ------------------------------------------------------------------ releases

/// A local crate whose manifest happens to use registry-looking coordinates.
/// The path, rather than its name and version, remains its package identity.
fn local_toml_named_root() -> PathBuf {
    static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local/scratch")
        .join(format!("nudox-folio-{}-{call}-toml", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).expect("dir");
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"toml\"\nversion = \"0.8.23\"\nlicense = \"MIT\"\n",
    )
    .expect("manifest");
    std::fs::write(dir.join("src/lib.rs"), "//! toml.\npub struct Value;\n").expect("lib");
    dir
}

fn route_for_package(package: &PackageRef) -> crate::navigation::Route {
    crate::navigation::Route::Package(crate::navigation::PackageRoute {
        project: None,
        package: crate::core::PackageId::new(package.as_str()).expect("package route"),
        lane: crate::navigation::PackageLane::Overview,
        selected: None,
        at: None,
    })
}

struct PackageAtRequest;

impl crate::runtime::reads::PageReader for PackageAtRequest {
    fn read(
        &mut self,
        request: &crate::runtime::reads::ReadRequest,
        context: &crate::runtime::reads::ReadContext<'_>,
    ) -> Result<crate::model::pages::PageValue, crate::model::pages::ReadFailure> {
        let crate::runtime::reads::ReadRequest::Package(package) = request else {
            return crate::shell::tests::Fixture.read(request, context);
        };
        let mut about = dossier();
        about.package = package.clone();
        Ok(crate::model::pages::PageValue::Package(about))
    }
}

struct RegistryPackagePage;

impl crate::runtime::reads::PageReader for RegistryPackagePage {
    fn read(
        &mut self,
        request: &crate::runtime::reads::ReadRequest,
        context: &crate::runtime::reads::ReadContext<'_>,
    ) -> Result<crate::model::pages::PageValue, crate::model::pages::ReadFailure> {
        let crate::runtime::reads::ReadRequest::Package(package) = request else {
            return crate::shell::tests::Fixture.read(request, context);
        };
        let mut about = dossier();
        about.package = package.clone();
        let name = package.display_name().to_owned();
        let entry = |version: &str| crate::model::pages::VersionEntry {
            package: PackageRef::parse(&format!("pkg:cargo/{name}@{version}"))
                .expect("release package"),
            version: Arc::from(version),
            standing: crate::model::pages::Standing::Available,
            current: package.version() == Some(version),
        };
        about.versions = crate::model::pages::Known::Known(Arc::from([
            entry("1.1.6"),
            entry("1.0.0"),
            entry("0.8.23"),
        ]));
        Ok(crate::model::pages::PageValue::Package(about))
    }
}

/// The years of the ticker's axis, as painted.
fn years(ledger: &Ledger) -> Vec<String> {
    ledger
        .texts
        .iter()
        .filter(|t| {
            t.key.contains("-ticker-") && t.content.len() == 4 && t.content.starts_with("20")
        })
        .map(|t| t.content.clone())
        .collect()
}

/// A local package whose manifest says `toml 0.8.23` is not a registry
/// package. A registry cache with that release cannot supply its ticker by
/// manifest name and version alone.
#[gpui::test]
fn a_local_root_does_not_borrow_a_registry_ticker_by_manifest_identity(cx: &mut TestAppContext) {
    let dir = local_toml_named_root();
    let package = PackageRef::parse(dir.to_str().expect("local path")).expect("local package");
    let route = route_for_package(&package);
    let pool = crate::runtime::reads::ReadPool::start(1, |_| PackageAtRequest).expect("pool");
    let mut rig = crate::shell::tests::rig_with_reads(cx, Some(route), 1440.0, 900.0, pool);
    let facts = source_facts::read(&dir, &std::collections::HashMap::new(), None)
        .expect("the local source reads");
    rig.cx.update(|_, cx| {
        facet::probe::enable(cx);
        source_facts::install(&package, Reading::Ready(Arc::new(facts)), cx);
        crate::runtime::fixture_releases::install(cx);
    });
    let ledger = painted(&mut rig);
    assert!(
        years(&ledger).is_empty(),
        "a local manifest cannot borrow registry years: {:?}",
        ledger.texts.iter().map(|t| &t.key).collect::<Vec<_>>()
    );
    assert!(!has(&ledger, "pkg-release-"), "there is no registry release door");
    assert!(
        !ledger
            .targets
            .iter()
            .any(|t| t.key.contains("folio-") && t.key.ends_with("-back")),
        "on the pin there is no way back to show"
    );
}

/// Reading an exact registry package at another release says so even when the
/// local owner has no release-history response; the banner is not the ticker's.
#[gpui::test]
fn the_past_says_so_and_offers_the_way_back_with_or_without_a_ticker(cx: &mut TestAppContext) {
    let pinned = PackageRef::parse("pkg:cargo/toml@0.8.23").expect("pinned package");
    let pool = crate::runtime::reads::ReadPool::start(1, |_| PackageAtRequest).expect("pool");
    let mut rig = crate::shell::tests::rig_with_reads(
        cx,
        Some(route_for_package(&pinned)),
        1440.0,
        900.0,
        pool,
    );
    rig.cx.update(|_, cx| {
        facet::probe::enable(cx);
        crate::runtime::releases::install_test_unavailable(
            &pinned,
            "0.3.0",
            "this case deliberately has no release history",
            cx,
        );
    });
    rig.go(Intent::SetRelease(Some(
        crate::navigation::ReleaseId::new("0.3.0").expect("release"),
    )));
    let ledger = painted(&mut rig);
    assert!(
        years(&ledger).is_empty(),
        "the unavailable-history note is not a release ticker: {:?}",
        ledger.texts.iter().map(|t| &t.key).collect::<Vec<_>>()
    );
    assert!(
        !ledger.targets.iter().any(|target| target.key.starts_with("pkg-release-")),
        "there is no release door without exact release history: {:?}",
        ledger.targets.iter().map(|target| &target.key).collect::<Vec<_>>()
    );
    assert_eq!(
        said(&ledger, "ticker-note").first().map(String::as_str),
        Some("Release history unavailable: this case deliberately has no release history"),
        "the exact pinned-package/viewed-release request received its unavailable fixture"
    );
    assert_eq!(
        said(&ledger, "-reading").first().map(String::as_str),
        Some("Reading"),
        "the banner says the page is in the past"
    );
    assert_eq!(
        said(&ledger, "-at").first().map(String::as_str),
        Some("0.3.0")
    );
    let back = ledger
        .targets
        .iter()
        .find(|t| t.key.contains("folio-") && t.key.ends_with("-back"))
        .unwrap_or_else(|| {
            panic!(
                "the way back is a button: {:?}",
                ledger.targets.iter().map(|t| &t.key).collect::<Vec<_>>()
            )
        });
    click(
        &mut rig,
        (
            back.bounds.x + back.bounds.width / 2.0,
            back.bounds.y + back.bounds.height / 2.0,
        ),
    );
    assert!(
        matches!(rig.route(), crate::navigation::Route::Package(route) if route.at.is_none()),
        "the way back returns to the pin: {:?}",
        rig.route()
    );
}

/// The four crest cells' label rows, top to bottom: which cell sits on which line.
fn crest_rows(ledger: &Ledger) -> Vec<Vec<&'static str>> {
    let mut at: Vec<(f32, &'static str)> = Vec::new();
    for (part, name) in [
        ("licence-label", "licence"),
        ("heads-label", "heads"),
        ("weight-label", "weight"),
        ("advisories-label", "advisories"),
    ] {
        if let Some(text) = ledger
            .texts
            .iter()
            .find(|t| t.key.contains("folio-") && t.key.ends_with(part))
        {
            at.push((text.bounds.y, name));
        }
    }
    at.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut rows: Vec<(f32, Vec<&'static str>)> = Vec::new();
    for (y, name) in at {
        match rows.last_mut() {
            Some((top, row)) if (y - *top).abs() < 2.0 => row.push(name),
            _ => rows.push((y, vec![name])),
        }
    }
    rows.into_iter().map(|(_, row)| row).collect()
}

/// The crest is four cells in a row, two by two, or one under the other, and
/// nothing else, at every width: never three and an orphan (the cells sum to
/// their row, and a wrapped fourth used to drop onto a row of its own), and
/// never fewer rows in a narrower window than in a wider one.
#[gpui::test]
fn the_crest_is_whole_rows_at_every_width_and_never_more_rows_when_wider(cx: &mut TestAppContext) {
    let mut rig = rig(cx, None, 1440.0, 900.0);
    read_and_open(&mut rig);
    let mut previous_rows = usize::MAX;
    // Every width R-Folio saw go wrong (1440-1600 orphaned a fourth cell; 720 was one column wide of two), and each side of the arrangement's edges.
    for width in [
        300, 360, 430, 460, 480, 560, 640, 720, 800, 960, 1000, 1024, 1152, 1280, 1360, 1440, 1500,
        1600, 1700, 1920, 2560,
    ] {
        resize(&mut rig, width as f32, 900.0);
        let rows = crest_rows(&painted_at(&mut rig));
        let shape: Vec<usize> = rows.iter().map(Vec::len).collect();
        assert!(
            matches!(shape.as_slice(), [4] | [2, 2] | [1, 1, 1, 1]),
            "at {width} px the crest is {rows:?}"
        );
        assert!(
            rows.len() <= previous_rows,
            "at {width} px the crest has {} rows, more than at a narrower window ({previous_rows}): {rows:?}",
            rows.len()
        );
        previous_rows = rows.len();
    }
}

// ---------------------------------------------------------------- keyboard

/// Presses `key` (`j` walks on, `k` walks back) until the shell's focus stands
/// on `id` (at most `limit` presses); the walk clamps at the ends of the list.
fn walk_by(rig: &mut Rig, key: &str, id: &str, limit: usize) {
    for _ in 0..limit {
        let (_, focused) = rig
            .shell
            .read_with(rig.cx, |shell, cx| shell.focus_state(cx));
        if focused.as_deref() == Some(id) {
            return;
        }
        rig.keys(key);
    }
    let (zone, focused) = rig
        .shell
        .read_with(rig.cx, |shell, cx| shell.focus_state(cx));
    assert_eq!(
        focused.as_deref(),
        Some(id),
        "{limit} presses of `{key}` never reached `{id}` (zone {zone:?})"
    );
}

/// Walks on (`j`) to `id`.
fn walk_to(rig: &mut Rig, id: &str, limit: usize) {
    walk_by(rig, "j", id, limit);
}

/// Walks back (`k`) to `id`.
fn walk_back_to(rig: &mut Rig, id: &str, limit: usize) {
    walk_by(rig, "k", id, limit);
}

fn focused(rig: &mut Rig) -> Option<String> {
    rig.shell
        .read_with(rig.cx, |shell, cx| shell.focus_state(cx))
        .1
        .map(|id| id.to_string())
}

/// The crest's cells and the releases are on the keyboard's
/// list after the territory (a reader walks what the page is about first),
/// and Enter does on each what a click does.
#[gpui::test]
fn the_crest_and_releases_are_doors_and_enter_does_what_a_click_does(cx: &mut TestAppContext) {
    let mut rig = rig(cx, None, 1440.0, 900.0);
    read_and_open(&mut rig);
    // The territory first.
    rig.keys("j");
    assert_eq!(
        focused(&mut rig).as_deref(),
        Some("pkg-module-glyph"),
        "the first door is the territory's"
    );
    // Then the licence: Enter holds the stamp unfolded, Enter again lets it go.
    walk_to(&mut rig, "pkg-licence", 6);
    rig.keys("enter");
    assert!(
        has(&painted(&mut rig), "licence-line")
            || said(&painted(&mut rig), "licence")
                .iter()
                .any(|w| w.contains("keep the notice")),
        "Enter unfolded the licence stamp"
    );
    rig.keys("enter");
    rig.settle();
    rig.frame(400);
    assert!(
        !said(&painted(&mut rig), "licence")
            .iter()
            .any(|w| w.contains("keep the notice")),
        "Enter again folded it"
    );
    // The heads-up hand: Enter opens the sheet of evidence.
    walk_to(&mut rig, "pkg-heads", 3);
    rig.keys("enter");
    rig.frame(500);
    let sheet = painted(&mut rig);
    assert!(
        sheet
            .texts
            .iter()
            .any(|t| t.content.starts_with("src/glyph.rs:")),
        "Enter on the hand opened the evidence sheet"
    );
    rig.keys("escape");
    rig.frame(400);
    // The weight: Enter opens the berg (the crate has no dependencies, so an empty one).
    walk_to(&mut rig, "pkg-weight", 4);
}

/// A module opened folds on Esc, on the page, before the shell does anything
/// of its own; a second Esc leaves the page alone.
#[gpui::test]
fn escape_folds_the_open_module_first(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(package_route()), 1440.0, 900.0);
    install(&mut rig);
    let _ = painted(&mut rig);
    rig.keys("j");
    rig.keys("enter");
    assert!(
        has(&painted(&mut rig), "module-card-0-name"),
        "the module is open"
    );
    rig.keys("escape");
    rig.frame(300);
    let ledger = painted(&mut rig);
    assert!(!has(&ledger, "module-card-0-name"), "Esc folded the module");
    assert!(
        has(&ledger, "shingles-region-glyph"),
        "and the territory is back"
    );
    assert_eq!(rig.route(), package_route(), "Esc folded; it did not leave");
}

/// The release history's pin and newest entry are doors: Enter travels to the
/// selected exact release, and the page says which one it is reading.
#[gpui::test]
fn the_releases_are_doors_and_enter_travels_to_one(cx: &mut TestAppContext) {
    let package = PackageRef::parse("pkg:cargo/toml@0.8.23").expect("pinned package");
    let pool = crate::runtime::reads::ReadPool::start(1, |_| RegistryPackagePage).expect("pool");
    let mut rig = crate::shell::tests::rig_with_reads(
        cx,
        Some(route_for_package(&package)),
        1440.0,
        900.0,
        pool,
    );
    rig.cx.update(|_, cx| {
        crate::runtime::fixture_releases::install(cx);
        facet::probe::enable(cx);
    });
    let _ = painted(&mut rig);
    walk_to(&mut rig, "pkg-release-newest", 40);
    rig.keys("enter");
    assert!(
        matches!(rig.route(), crate::navigation::Route::Package(route) if route.at.is_some()),
        "Enter on the newest release travelled to it: {:?}",
        rig.route()
    );
    assert!(
        has(&painted(&mut rig), "-reading"),
        "the page says it is reading another release"
    );
}

// -------------------------------------------------------------------- berg

/// A crate that needs `toml`, so its weight iceberg has blocks; `None` when
/// this machine's registry has no `toml` to read.
fn krate_needing_toml() -> Option<PathBuf> {
    static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    crate::model::source_facts::registry::pick("toml", "0.8", None)?;
    let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local/scratch")
        .join(format!("nudox-folio-{}-{call}-heavy", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).expect("dir");
    std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"heavy\"\nversion = \"0.1.0\"\nlicense = \"MIT\"\n[dependencies]\ntoml = \"0.8\"\n").expect("manifest");
    std::fs::write(dir.join("src/lib.rs"), "//! Heavy.\npub struct Value;\n").expect("lib");
    Some(dir)
}

/// The berg's blocks, as painted.
fn berg_blocks(ledger: &Ledger) -> usize {
    ledger
        .bounds
        .iter()
        .filter(|b| b.key.contains("-berg-block-"))
        .count()
}

/// The top edge of the names line, below the berg: what it pushes down when it opens.
fn below_the_berg(ledger: &Ledger) -> f32 {
    ledger
        .texts
        .iter()
        .find(|t| t.key.ends_with("-names"))
        .map_or(f32::NAN, |t| t.bounds.y)
}

/// The berg drops in when the weight is opened and lifts out when it is
/// closed: the page below it moves down over frames and never back on the
/// way (no overshoot flicker), the blocks are doors only while it is open
/// (a lifting berg is not on the keyboard's list), and closed again the page
/// is exactly where it began.
#[gpui::test]
fn the_berg_drops_in_and_lifts_out_and_its_doors_go_with_it(cx: &mut TestAppContext) {
    let Some(dir) = krate_needing_toml() else {
        return;
    };
    let mut rig = rig(cx, None, 1440.0, 900.0);
    let facts =
        source_facts::read(&dir, &std::collections::HashMap::new(), None).expect("the crate reads");
    if facts.berg.blocks.is_empty() {
        return;
    }
    let package = package();
    rig.cx.update(|_, cx| {
        facet::probe::enable(cx);
        source_facts::install(&package, Reading::Ready(Arc::new(facts)), cx);
    });
    rig.go(Intent::Navigate(package_route()));
    let closed = painted(&mut rig);
    assert_eq!(berg_blocks(&closed), 0, "the berg is folded away at first");
    let before = below_the_berg(&closed);
    assert!(
        before.is_finite(),
        "the features line is painted: {:?}",
        closed.texts.iter().map(|t| &t.key).collect::<Vec<_>>()
    );
    // Open it by the pointer and watch it arrive frame by frame.
    let at = centre(&closed, "weight-label");
    rig.cx
        .simulate_click(point(px(at.0), px(at.1)), Modifiers::default());
    let mut ys = Vec::new();
    for _ in 0..30 {
        rig.frame(16);
        let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
        ys.push((berg_blocks(&ledger), below_the_berg(&ledger)));
    }
    let settled = ys.last().copied().expect("frames");
    assert!(settled.0 > 0, "the berg opened: {ys:?}");
    assert!(
        settled.1 > before + 40.0,
        "and the page below it moved down ({before} to {}): {ys:?}",
        settled.1
    );
    assert!(
        ys.iter()
            .any(|(_, y)| *y > before + 1.0 && *y < settled.1 - 1.0),
        "it took frames to arrive, it did not cut: {ys:?}"
    );
    assert!(
        ys.windows(2).all(|pair| pair[1].1 >= pair[0].1 - 0.5),
        "it never moved back on the way down: {ys:?}"
    );
    // Open, the blocks are on the keyboard's list.
    walk_to(&mut rig, &PageTarget::Block(0).id(), 30);
    // Fold it by the keyboard and watch it lift out.
    walk_back_to(&mut rig, "pkg-weight", 120);
    rig.cx.simulate_keystrokes("enter");
    let mut ys = Vec::new();
    for frame in 0..30 {
        rig.frame(16);
        let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
        ys.push((berg_blocks(&ledger), below_the_berg(&ledger)));
        if frame == 1 {
            // Lifting out, the berg is still on the page but no longer on the
            // keyboard's list: `j` from the weight goes past it.
            assert!(
                berg_blocks(&ledger) > 0,
                "the berg is still lifting out: {ys:?}"
            );
            rig.cx.simulate_keystrokes("j");
            let now = focused(&mut rig);
            assert!(
                now.as_deref()
                    .is_some_and(|id| !id.starts_with("pkg-block-")),
                "a lifting berg has no doors, `j` went to {now:?}"
            );
        }
    }
    assert!(
        ys.iter().any(|(blocks, _)| *blocks > 0)
            && ys.last().is_some_and(|(blocks, _)| *blocks == 0),
        "the berg was still there a moment, then gone: {ys:?}"
    );
    assert!(
        ys.windows(2).all(|pair| pair[1].1 <= pair[0].1 + 0.5),
        "it never moved back down on the way up: {ys:?}"
    );
    let last = ys.last().expect("frames").1;
    assert!(
        (last - before).abs() < 1.0,
        "folded, the page is where it began ({before} vs {last}): {ys:?}"
    );
    // Its doors left with it.
    rig.keys("k");
    let ids: Vec<String> = (0..12)
        .filter_map(|_| {
            rig.keys("j");
            focused(&mut rig)
        })
        .collect();
    assert!(
        !ids.iter().any(|id| id.starts_with("pkg-block-")),
        "a folded berg has no doors: {ids:?}"
    );
}

// ------------------------------------------------------------------ flight

/// Where the flying stones are (their bounds), by index.
fn stone(ledger: &Ledger, index: usize) -> Option<&facet::probe::BoundsSample> {
    ledger
        .bounds
        .iter()
        .find(|b| b.key.ends_with(&format!("-flight-stone-{index}")))
}

/// Where flying stone `index` is going.
fn mark(ledger: &Ledger, index: usize) -> Option<&facet::probe::BoundsSample> {
    ledger
        .bounds
        .iter()
        .find(|b| b.key.ends_with(&format!("-flight-mark-{index}")))
}

/// How far the centre of `from` is from the centre of `to`.
fn gap(from: &facet::probe::BoundsSample, to: &facet::probe::BoundsSample) -> f32 {
    (from.x + from.width / 2.0 - (to.x + to.width / 2.0))
        .hypot(from.y + from.height / 2.0 - (to.y + to.height / 2.0))
}

/// A module opens by carrying its shingles to its cards: one stone in the
/// air per name, each nearer its card than the frame before it (the spring
/// may lean over the mark, never turn back), none once they have landed,
/// and the cards there to be read.
#[gpui::test]
fn opening_a_module_carries_each_shingle_to_its_card_and_lands(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(package_route()), 1440.0, 900.0);
    install(&mut rig);
    let ledger = painted(&mut rig);
    let at = centre(&ledger, "shingles-region-glyph");
    rig.cx
        .simulate_click(point(px(at.0), px(at.1)), Modifiers::default());
    let mut frames: Vec<Ledger> = Vec::new();
    for _ in 0..80 {
        rig.frame(16);
        frames.push(rig.cx.update(|_, cx| facet::probe::take(cx)));
    }
    let landed = frames.last().expect("frames");
    let flying: Vec<&Ledger> = frames
        .iter()
        .filter(|frame| stone(frame, 0).is_some())
        .collect();
    assert!(
        flying.len() >= 4,
        "the shingles took frames to arrive: {} frames in the air",
        flying.len()
    );
    assert!(
        frames
            .iter()
            .filter(|frame| stone(frame, 0).is_some())
            .all(|frame| (0..6).all(|index| stone(frame, index).is_some())),
        "one stone per name, six, in every frame of the flight"
    );
    assert!(
        stone(landed, 0).is_none(),
        "and none in the air once they have landed"
    );
    assert!(
        has(landed, "module-card-0-name"),
        "the cards are there to be read"
    );
    let distances: Vec<f32> = flying
        .iter()
        .filter_map(|frame| Some(gap(stone(frame, 0)?, mark(frame, 0)?)))
        .collect();
    let (first, last) = (distances[0], *distances.last().expect("distances"));
    assert!(
        first > 15.0,
        "the shingle starts away from its card ({first}px): {distances:?}"
    );
    assert!(
        last < first * 0.35,
        "and is over it by the last frame in the air ({first} to {last}): {distances:?}"
    );
    assert!(
        distances.windows(2).all(|pair| pair[1] <= pair[0] + 1.5),
        "never turning back on the way: {distances:?}"
    );
    // Folding it (Esc) leaves nothing in the air.
    rig.keys("escape");
    rig.frame(16);
    let folded = rig.cx.update(|_, cx| facet::probe::take(cx));
    assert!(
        stone(&folded, 0).is_none(),
        "nothing flies when a module folds"
    );
}

// --------------------------------------------------------------- structure

fn region(name: &str, placement: super::data::Placement) -> super::data::ModuleData {
    super::data::ModuleData {
        name: name.to_owned().into(),
        placement,
        doc: None,
        items: Vec::new(),
    }
}

/// Names the index placed in no module say so; a package of several modules,
/// or of one it recorded, is what it is.
#[test]
fn names_the_index_placed_in_no_module_are_called_that_not_drawn_as_a_module() {
    use super::data::{Placement, Structure, structure};
    assert_eq!(
        structure(&[region("present", Placement::Gathered)], None),
        Structure::Gathered
    );
    assert_eq!(
        structure(&[region("lib", Placement::Recorded)], None),
        Structure::Modules,
        "one recorded module, and no source to say otherwise"
    );
    assert_eq!(
        structure(
            &[
                region("a", Placement::Recorded),
                region("b", Placement::Gathered)
            ],
            None
        ),
        Structure::Modules,
        "several regions are several regions"
    );
    assert_eq!(structure(&[], None), Structure::Modules);
}

/// A crate that keeps its modules private and re-exports at the root has one
/// region, and the header says why (a flat API by design), not "1 module".
#[gpui::test]
fn a_crate_whose_modules_are_private_says_its_names_are_all_at_the_root(cx: &mut TestAppContext) {
    let mut rig = rig(cx, None, 1440.0, 900.0);
    let dir = krate();
    std::fs::write(
        dir.join("src/lib.rs"),
        "//! How one symbol page reads.\nmod glyph;\npub use glyph::{Identity, KindGlyph, Outline, RelationDirection, RelationLabel, relation_label};\n",
    )
    .expect("lib");
    read_and_open_at(&mut rig, &dir);
    let ledger = painted(&mut rig);
    assert_eq!(
        said(&ledger, "shingles-region-"),
        ["lib"],
        "every public name is at the root: {:?}",
        ledger.texts.iter().map(|t| &t.key).collect::<Vec<_>>()
    );
    assert_eq!(
        said(&ledger, "-names").first().map(String::as_str),
        Some("6")
    );
    assert!(
        said(&ledger, "names-words")
            .iter()
            .any(|w| w.contains("all at the root")),
        "the header says so: {:?}",
        said(&ledger, "names-words")
    );
    let hidden: Vec<String> = ledger
        .texts
        .iter()
        .filter(|t| t.key.ends_with("-hidden"))
        .map(|t| t.content.clone())
        .collect();
    assert_eq!(hidden, ["1"], "through one private module");
    assert_eq!(said(&ledger, "hidden-words"), ["private module"]);
    assert!(
        !has(&ledger, "modules-words"),
        "it does not claim `1 module`: {:?}",
        said(&ledger, "modules-words")
    );
}

/// A project's dependency is indexed as the tree its lock pins (a local root
/// in the cargo cache, named and versioned from its manifest), not under a
/// registry address: the dependency mark links to that release when the
/// library holds it (J1: toml_pin's `toml` read as text, not a link).
#[test]
fn a_dependency_links_to_the_release_the_library_holds() {
    use crate::model::pages::{Dependency, DependencyScope, IndexedPackage, Readiness};
    let home = std::env::var("HOME").unwrap_or_default();
    let tree = |name: &str| {
        let release = crate::model::release::Release::from_stem(name).expect("release stem");
        let package = PackageRef::parse(&format!(
            "{home}/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/{name}"
        ))
        .expect("a tree");
        IndexedPackage {
            package: package.clone(),
            name: Arc::from(name),
            readiness: Readiness::Ready,
            verified_registry_release: Some(
                crate::model::pages::VerifiedRegistryRelease::from_checked_release(
                    package,
                    PackageRef::parse(&release.purl()).expect("exact registry package"),
                    Arc::from("test-authority"),
                    7,
                ),
            ),
        }
    };
    // Trees this machine's cargo cache holds (the journeys read the same).
    let library = [
        tree("toml-0.8.23"),
        tree("toml-0.5.11"),
        tree("serde-1.0.229"),
    ];
    let wants = |name: &str, requirement: &str, resolved: Option<&str>| Dependency {
        name: Arc::from(name),
        requirement: Arc::from(requirement),
        scope: DependencyScope::Runtime,
        optional: false,
        resolved: resolved.map(|purl| PackageRef::parse(purl).expect("a purl")),
    };
    let linked = |dependency: &Dependency| {
        super::in_the_library_for(dependency, &library, "test-authority", 7).map(|package| {
            package
                .as_str()
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_owned()
        })
    };
    assert_eq!(
        linked(&wants("toml", "0.8.23", None)),
        None,
        "a requirement string does not prove a resolved destination"
    );
    assert_eq!(
        linked(&wants("toml", "0.5", Some("pkg:cargo/toml@0.5.11"))).as_deref(),
        Some("toml-0.5.11"),
        "the release the resolver chose"
    );
    assert_eq!(
        linked(&wants("toml", "0.5", Some("pkg:npm/toml@0.5.11"))),
        None,
        "the same name and version under npm is not the resolved Cargo package"
    );
    assert_eq!(
        linked(&wants(
            "toml",
            "0.5",
            Some("pkg:cargo/toml@0.5.11?repository_url=https%3A%2F%2Fother.example"),
        )),
        None,
        "a qualified package from another registry origin cannot alias this unqualified release"
    );
    assert_eq!(
        linked(&wants("serde", "1", None)),
        None,
        "a unique display name without resolver evidence is unresolved"
    );
    assert_eq!(
        linked(&wants("winnow", "0.7", None)),
        None,
        "a package the library does not hold has no link here"
    );
    assert_eq!(
        super::in_the_library_for(
            &wants("toml", "0.5", Some("pkg:cargo/toml@0.5.11")),
            &library,
            "another-authority",
            7,
        ),
        None,
        "a proof from a previous registry authority cannot be reused"
    );
    assert_eq!(
        super::in_the_library_for(
            &wants("toml", "0.5", Some("pkg:cargo/toml@0.5.11")),
            &library,
            "test-authority",
            8,
        ),
        None,
        "a proof from a replaced registry composition cannot be reused"
    );
    let duplicate = [tree("toml-0.5.11"), tree("toml-0.5.11")];
    assert_eq!(
        super::in_the_library_for(
            &wants("toml", "0.5", Some("pkg:cargo/toml@0.5.11")),
            &duplicate,
            "test-authority",
            7,
        ),
        None,
        "two exact candidate roots are ambiguous"
    );
    let mut copied_receipt = tree("toml-0.5.11");
    copied_receipt.package = PackageRef::parse("/different-owner/cache/toml-0.5.11")
        .expect("copied local root");
    assert_eq!(
        super::in_the_library_for(
            &wants("toml", "0.5", Some("pkg:cargo/toml@0.5.11")),
            &[copied_receipt],
            "test-authority",
            7,
        ),
        None,
        "a receipt copied onto another local root cannot prove that root"
    );
}

/// A test-only owner receipt for one exact registry tree. This keeps the
/// keyboard journey positive without teaching production to infer authority
/// from a dependency's display name or version.
fn serde_registry_tree() -> crate::model::pages::IndexedPackage {
    use crate::model::pages::{IndexedPackage, Readiness, VerifiedRegistryRelease};
    let package = PackageRef::parse("/fixture/registry/src/index.test/serde-1.0.229")
        .expect("local registry tree");
    let release = PackageRef::parse("pkg:cargo/serde@1.0.229").expect("exact release");
    IndexedPackage {
        package: package.clone(),
        name: Arc::from("serde"),
        readiness: Readiness::Ready,
        verified_registry_release: Some(VerifiedRegistryRelease::from_checked_release(
            package,
            release,
            Arc::from("test-authority"),
            7,
        )),
    }
}

/// Serves the fixture dossier and one exact worker-admitted registry tree:
/// `serde`, whose release the resolver chose, and unresolved `winnow`.
struct Depends {
    indexed: crate::model::pages::IndexedPackage,
}

impl crate::runtime::reads::PageReader for Depends {
    fn read(
        &mut self,
        request: &crate::runtime::reads::ReadRequest,
        context: &crate::runtime::reads::ReadContext<'_>,
    ) -> Result<crate::model::pages::PageValue, crate::model::pages::ReadFailure> {
        use crate::model::pages::{Dependency, DependencyScope, Known, PageValue};
        if matches!(request, crate::runtime::reads::ReadRequest::Orbit) {
            return Ok(PageValue::Orbit(crate::model::pages::OrbitModel {
                indexed: Known::Known(Arc::from([self.indexed.clone()])),
                projects: Known::Known(Arc::from([])),
                explore: Known::Unknown(crate::model::pages::Gap::new(
                    crate::model::pages::GapReason::NotServed,
                    "",
                )),
                tree: Known::Unknown(crate::model::pages::Gap::new(
                    crate::model::pages::GapReason::NotServed,
                    "",
                )),
            }));
        }
        let crate::runtime::reads::ReadRequest::Package(package) = request else {
            return crate::shell::tests::Fixture.read(request, context);
        };
        let mut about = dossier();
        about.package = package.clone();
        if *package == dossier().package {
            let wants = |name: &str, requirement: &str, resolved: Option<&str>| Dependency {
                name: Arc::from(name),
                requirement: Arc::from(requirement),
                scope: DependencyScope::Runtime,
                optional: false,
                resolved: resolved.map(|purl| PackageRef::parse(purl).expect("a purl")),
            };
            about.dependencies = Known::Known(Arc::from([
                wants("serde", "1", Some("pkg:cargo/serde@1.0.229")),
                wants("winnow", "0.7", None),
            ]));
        }
        Ok(PageValue::Package(about))
    }
}

/// A dependency the hero names is a door when it goes somewhere: the
/// keyboard reaches it, Enter opens the release it names as a click does,
/// and ⌘[ comes back with the keyboard standing on it. A name with no place
/// to go is not a door (dead end #15).
#[gpui::test]
fn a_dependency_that_goes_somewhere_is_a_door_and_back_stands_on_it(cx: &mut TestAppContext) {
    let _registry = super::use_registry_binding_for_test("test-authority", 7);
    let indexed = serde_registry_tree();
    let expected_destination = indexed.package.as_str().to_owned();
    let pool = crate::runtime::reads::ReadPool::start(1, move |_| Depends {
        indexed: indexed.clone(),
    })
    .expect("pool");
    let mut rig =
        crate::shell::tests::rig_with_reads(cx, Some(package_route()), 1440.0, 900.0, pool);
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    let ledger = painted(&mut rig);
    assert_eq!(
        said(&ledger, "mk-deps")
            .iter()
            .filter(|w| *w != "on")
            .collect::<Vec<_>>(),
        ["serde", "winnow"],
        "the hero names both"
    );
    let doors: Vec<&str> = ledger
        .targets
        .iter()
        .map(|t| t.key.as_str())
        .filter(|key| key.starts_with("pkg-dep-"))
        .collect();
    assert_eq!(
        doors,
        ["pkg-dep-serde"],
        "only the dependency with a place to go is a door"
    );
    let door = ledger
        .targets
        .iter()
        .find(|t| t.key == "pkg-dep-serde")
        .expect("the door")
        .bounds
        .clone();
    assert!(door.height >= 24.0, "a door a pointer can hit: {door:?}");
    walk_to(&mut rig, "pkg-dep-serde", 24);
    rig.keys("enter");
    let opened = rig.route();
    assert!(
        format!("{opened:?}").contains(&expected_destination),
        "Enter opened the exact indexed tree admitted by the test receipt: {opened:?}"
    );
    rig.keys("cmd-[");
    assert_eq!(rig.route(), package_route(), "⌘[ came back");
    assert_eq!(
        focused(&mut rig).as_deref(),
        Some("pkg-dep-serde"),
        "and the keyboard stands on the door it left by"
    );
}

/// Back puts the keyboard on the door the page was left by, and the focus
/// bevel comes back on it as a new bevel: it does not fly in from where it
/// last stood on the other page (J1 saw it step from a stale place).
#[gpui::test]
fn after_back_the_focus_bevel_comes_back_on_the_door_not_flying_in(cx: &mut TestAppContext) {
    let _registry = super::use_registry_binding_for_test("test-authority", 7);
    let indexed = serde_registry_tree();
    let pool = crate::runtime::reads::ReadPool::start(1, move |_| Depends {
        indexed: indexed.clone(),
    })
    .expect("pool");
    let mut rig =
        crate::shell::tests::rig_with_reads(cx, Some(package_route()), 1440.0, 900.0, pool);
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    let _ = painted(&mut rig);
    walk_to(&mut rig, "pkg-dep-serde", 24);
    rig.keys("enter");
    // On the other page the bevel stands somewhere else.
    rig.keys("j");
    for _ in 0..40 {
        rig.frame(16);
    }
    let _ = rig.cx.update(|_, cx| facet::probe::take(cx));
    rig.keys("cmd-[");
    for _ in 0..40 {
        rig.frame(16);
    }
    assert_eq!(
        focused(&mut rig).as_deref(),
        Some("pkg-dep-serde"),
        "back stands on the door"
    );
    let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
    let glow: Vec<&facet::probe::TrackSample> = ledger
        .tracks
        .iter()
        .filter(|track| track.key == "reader.glow-y")
        .collect();
    let born = glow
        .iter()
        .position(|sample| sample.kind == facet::probe::TrackKind::Snap)
        .unwrap_or_else(|| panic!("the bevel came back as a designed start: {glow:#?}"));
    let door = ledger
        .targets
        .iter()
        .find(|target| target.key == "pkg-dep-serde")
        .expect("the door is painted")
        .bounds
        .clone();
    assert!(
        (glow[born].value - door.y).abs() < 0.5,
        "it is born on the door: {} vs {}",
        glow[born].value,
        door.y
    );
    assert!(
        glow[born..]
            .iter()
            .all(|sample| !sample.live && (sample.value - door.y).abs() < 0.5),
        "and never flies: {:#?}",
        &glow[born..]
    );
}
