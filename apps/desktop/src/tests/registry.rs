//! Pure tests for the registry browse store. Owned by the browse lane.
//! Every assertion here is about a rendered word, a slug, or an exact order.
//! Nothing in this file opens a window or reaches the local service.
//!
//! The defects these are written to catch are all silent. A stale reply that
//! installs anyway still draws a full list — of the wrong packages. A paging
//! rule that stops resetting still draws rows — of page four under a fresh
//! query. An add button that reads the shelf wrongly still draws a button —
//! offering to index something already on the shelf. A version list sorted as
//! text still lists every version — with 2.9.0 above 2.10.0. None of those
//! fail a build and none of them are visible in a count, so every assertion
//! below compares the exact string or the exact sequence a reader would see.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "a test states its property directly and aborts on a broken fixture"
)]

use crate::presentation::fault::operand_spelling;
use crate::store::registry::{
    Loadable, PackageRow, Query, Spelling, Standing, add_standing, install_page, package_of,
    size_label, version_rank,
};
use backend_library::{
    PackageReference, ProductText, RegistryDownloadCount, RegistryEcosystem, RegistryPackageRecord,
    RegistryReleaseStanding, SurfaceCommand, SurfaceReply,
};
use backend_present::{
    CauseSlug, Fault, FaultSlug, Identity, KeyTag, Readiness, RowCount, Shelf, ShelfEntry,
};

// ------------------------------------------------------------- fixtures --

fn record(
    ecosystem: RegistryEcosystem,
    name: &str,
    version: &str,
    bytes: u64,
) -> RegistryPackageRecord {
    let coordinate = format!("pkg:{}/{name}@{version}", ecosystem.as_str());
    RegistryPackageRecord {
        coordinate: PackageReference::parse(coordinate).expect("a canonical package url"),
        ecosystem,
        name: ProductText::new(name).expect("a registry name"),
        version: ProductText::new(version).expect("a pinned version"),
        bytes,
        standing: RegistryReleaseStanding::Available,
        downloads: RegistryDownloadCount::NotReported(
            ProductText::new("unsupported").expect("reason"),
        ),
        facts_version: [0; 32],
        advisory: backend_library::AdvisoryPackageDto::unknown(),
    }
}

fn explored(records: Vec<RegistryPackageRecord>) -> SurfaceReply {
    SurfaceReply::Explored(records.into_boxed_slice())
}

/// Returns the exact line one browse row would draw, in row order.
fn lines(rows: &[PackageRow]) -> Vec<String> {
    rows.iter()
        .map(|row| {
            format!(
                "{} {} {} {} {}",
                row.ecosystem().as_str(),
                row.name(),
                row.version(),
                row.size(),
                row.coordinate()
            )
        })
        .collect()
}

fn shelf_fault() -> Fault {
    package_of("/Users/reader/project").expect_err("a folder is not a registry package")
}

fn shelf() -> Shelf {
    let entries = vec![
        ShelfEntry::new(Identity::parse("pkg:cargo/ready@1.0.0"), Readiness::Ready),
        ShelfEntry::new(
            Identity::parse("pkg:cargo/indexing@1.0.0"),
            Readiness::Indexing {
                rows: RowCount::new(12),
            },
        ),
        ShelfEntry::new(
            Identity::parse("pkg:cargo/requested@1.0.0"),
            Readiness::Requested,
        ),
        ShelfEntry::new(
            Identity::parse("pkg:cargo/failed@1.0.0"),
            Readiness::Failed {
                fault: shelf_fault(),
            },
        ),
    ];
    Shelf::new(KeyTag::from_key(&[0_u8; 32]), entries)
}

// ----------------------------------------------------------- the query --

#[test]
fn an_empty_field_browses_the_catalog_and_text_searches_the_index() {
    let browsing = Query::default();
    match browsing.command().expect("an empty field is admissible") {
        SurfaceCommand::Explore { query, limit } => {
            assert!(query.is_none(), "browsing carries no filter");
            assert_eq!(limit, 24);
        }
        other => panic!("an empty field must browse, not {other:?}"),
    }
    match browsing
        .with_text("  serde  ")
        .command()
        .expect("typed text is admissible")
    {
        SurfaceCommand::IndexSearch { query, limit } => {
            assert_eq!(query.as_str(), "serde");
            assert_eq!(limit, 24);
        }
        other => panic!("typed text must search the index, not {other:?}"),
    }
}

#[test]
fn paging_widens_the_row_budget_and_stops_at_the_engines_bound() {
    let mut query = Query::default();
    assert_eq!(query.limit(), 24);
    assert!(query.can_advance());
    query = query.advanced();
    assert_eq!(query.limit(), 48);
    for _ in 0..40 {
        query = query.advanced();
    }
    assert_eq!(query.limit(), 256, "paging never exceeds the reply bound");
    assert!(!query.can_advance(), "the last page offers no further page");
    match query.command().expect("the capped query is admissible") {
        SurfaceCommand::Explore { limit, .. } => assert_eq!(limit, 256),
        other => panic!("the capped query must still browse, not {other:?}"),
    }
}

#[test]
fn new_text_and_a_new_scope_both_return_to_the_first_page() {
    let deep = Query::default().advanced().advanced();
    assert_eq!(
        deep.limit(),
        72,
        "the third page asks for three pages of rows"
    );
    assert_eq!(
        deep.with_text("axum").limit(),
        24,
        "fresh text answers a new question and asks for one page again"
    );
    assert_eq!(
        deep.with_ecosystem(Some(RegistryEcosystem::Npm)).limit(),
        24,
        "a new scope answers a new question and starts again"
    );
    let scoped = deep
        .with_text("axum")
        .with_ecosystem(Some(RegistryEcosystem::Cargo));
    assert_eq!(scoped.text(), "axum", "scoping keeps what was typed");
    assert_eq!(scoped.ecosystem(), Some(RegistryEcosystem::Cargo));
}

// --------------------------------------------------------- generations --

#[test]
fn a_reply_issued_before_the_newest_question_never_installs() {
    let reply = explored(vec![record(
        RegistryEcosystem::Cargo,
        "serde",
        "1.0.0",
        2048,
    )]);
    assert!(
        install_page(3, 4, None, &reply).is_none(),
        "a reply for the previous keystroke must be dropped, not drawn"
    );
    let installed = install_page(4, 4, None, &reply).expect("the newest reply installs");
    let rows = installed.ready().expect("the installed page holds rows");
    assert_eq!(
        lines(rows),
        vec!["cargo serde 1.0.0 2 KB pkg:cargo/serde@1.0.0".to_owned()]
    );
}

#[test]
fn an_ecosystem_scope_drops_every_row_from_another_registry() {
    let reply = explored(vec![
        record(RegistryEcosystem::Cargo, "serde", "1.0.0", 2048),
        record(RegistryEcosystem::Npm, "zod", "3.23.8", 1_048_576),
        record(RegistryEcosystem::Pypi, "attrs", "24.2.0", 512),
    ]);
    let scoped =
        install_page(1, 1, Some(RegistryEcosystem::Npm), &reply).expect("a scoped reply installs");
    let rows = scoped.ready().expect("the scoped page holds rows");
    assert_eq!(
        lines(rows),
        vec!["npm zod 3.23.8 1 MB pkg:npm/zod@3.23.8".to_owned()]
    );
    let unscoped = install_page(1, 1, None, &reply).expect("an unscoped reply installs everything");
    let every = unscoped.ready().expect("the unscoped page holds rows");
    assert_eq!(
        every
            .iter()
            .map(|row| row.name().to_owned())
            .collect::<Vec<_>>(),
        vec!["serde".to_owned(), "zod".to_owned(), "attrs".to_owned()]
    );
}

#[test]
fn an_answer_of_nothing_is_an_answer_rather_than_a_pending_read() {
    let empty =
        install_page(1, 1, None, &explored(Vec::new())).expect("an empty catalog still answers");
    assert!(
        !empty.is_pending(),
        "an answered-nothing page must not reserve skeletons forever"
    );
    assert!(empty.ready().expect("rows").is_empty());
}

// ----------------------------------------------------------- loadable --

#[test]
fn a_loadable_states_exactly_which_of_the_four_things_it_is() {
    let idle: Loadable<Vec<PackageRow>> = Loadable::Idle;
    assert!(idle.is_idle() && idle.is_pending());
    assert!(idle.ready().is_none(), "nothing asked for holds no answer");

    let loading: Loadable<Vec<PackageRow>> = Loadable::Loading;
    assert!(
        !loading.is_idle(),
        "a read in flight has already been asked"
    );
    assert!(loading.is_pending());

    let reply = explored(vec![record(
        RegistryEcosystem::Cargo,
        "memchr",
        "2.7.4",
        90_112,
    )]);
    let ready = install_page(1, 1, None, &reply).expect("a page installs");
    assert!(!ready.is_pending() && !ready.is_idle());
    assert_eq!(
        ready
            .ready()
            .and_then(|rows| rows.first())
            .map(PackageRow::name),
        Some("memchr")
    );

    let faulted: Loadable<Vec<PackageRow>> = Loadable::Faulted(Box::new(shelf_fault()));
    assert!(!faulted.is_pending() && faulted.ready().is_none());
    match &faulted {
        Loadable::Faulted(fault) => assert_eq!(
            fault.slug(),
            FaultSlug::Usage,
            "a failed browse carries the typed fault, never a string"
        ),
        other => panic!("a faulted browse must hold a fault, not {other:?}"),
    }
}

// -------------------------------------------------------- on the shelf --

#[test]
fn the_shelf_alone_decides_what_the_add_affordance_offers() {
    let shelf = shelf();
    assert_eq!(
        add_standing(&shelf, "pkg:cargo/ready@1.0.0").label(),
        "On shelf ✓"
    );
    assert_eq!(
        add_standing(&shelf, "pkg:cargo/indexing@1.0.0").label(),
        "Adding…"
    );
    assert_eq!(
        add_standing(&shelf, "pkg:cargo/requested@1.0.0").label(),
        "Adding…"
    );
    assert_eq!(
        add_standing(&shelf, "pkg:cargo/failed@1.0.0").label(),
        "Re-index"
    );
    assert_eq!(add_standing(&shelf, "pkg:cargo/serde@1.0.0").label(), "Add");
}

#[test]
fn only_an_absent_or_failed_row_offers_a_press() {
    let shelf = shelf();
    assert!(!add_standing(&shelf, "pkg:cargo/ready@1.0.0").is_actionable());
    assert!(!add_standing(&shelf, "pkg:cargo/indexing@1.0.0").is_actionable());
    assert!(add_standing(&shelf, "pkg:cargo/failed@1.0.0").is_actionable());
    assert!(add_standing(&shelf, "pkg:cargo/serde@1.0.0").is_actionable());
    assert_eq!(Standing::OnShelf.label(), "On shelf ✓");
}

#[test]
fn a_version_pinned_differently_is_not_the_row_on_the_shelf() {
    let shelf = shelf();
    assert_eq!(
        add_standing(&shelf, "pkg:cargo/ready@2.0.0").label(),
        "Add",
        "the shelf holds an exact pinned coordinate, not a package name"
    );
}

// --------------------------------------------------------- coordinates --

#[test]
fn a_page_only_asks_about_a_pinned_package_url() {
    assert_eq!(
        package_of("pkg:cargo/memchr@2.7.4")
            .expect("a pinned purl is admitted")
            .as_str(),
        "pkg:cargo/memchr@2.7.4"
    );

    let folder = package_of("/Users/reader/project").expect_err("a folder is refused");
    assert_eq!(folder.slug(), FaultSlug::Usage);
    assert_eq!(folder.cause().slug(), CauseSlug::Malformed);
    assert_eq!(
        folder.cause().sentence(),
        "a registry page needs a package URL, such as pkg:cargo/serde@1.0.0"
    );
    assert_eq!(operand_spelling(folder.operand()), "/Users/reader/project");

    let unpinned = package_of("pkg:cargo/memchr").expect_err("an unpinned purl is refused");
    assert_eq!(
        unpinned.cause().sentence(),
        "a package URL must pin a version with @, such as pkg:cargo/serde@1.0.0"
    );
}

#[test]
fn a_coordinate_splits_into_the_parts_a_header_draws() {
    let scoped = Spelling::of("pkg:npm/@tanstack/query-core@5.62.8");
    assert_eq!(scoped.ecosystem(), Some(RegistryEcosystem::Npm));
    assert_eq!(
        scoped.name(),
        "@tanstack/query-core",
        "a scoped npm name keeps its slash"
    );
    assert_eq!(scoped.version(), "5.62.8");
    assert_eq!(scoped.label(), "@tanstack/query-core@5.62.8");

    let plain = Spelling::of("pkg:cargo/memchr@2.7.4");
    assert_eq!(plain.ecosystem(), Some(RegistryEcosystem::Cargo));
    assert_eq!(plain.name(), "memchr");
    assert_eq!(plain.label(), "memchr@2.7.4");

    let folder = Spelling::of("/Users/reader/project");
    assert!(
        folder.ecosystem().is_none(),
        "a folder names no registry ecosystem"
    );
    assert_eq!(folder.version(), "");
}

// ------------------------------------------------------------ ordering --

#[test]
fn versions_order_newest_first_by_number_rather_than_by_spelling() {
    let mut ordered = vec!["2.9.0", "2.10.0", "10.0.0", "2.10.0-rc1", "1.0.0"];
    ordered.sort_by_key(|version| version_rank(version));
    assert_eq!(
        ordered,
        vec!["10.0.0", "2.10.0", "2.10.0-rc1", "2.9.0", "1.0.0"]
    );
}

#[test]
fn a_size_is_drawn_in_the_largest_unit_that_still_states_a_whole_number() {
    assert_eq!(size_label(0), "0 B");
    assert_eq!(size_label(512), "512 B");
    assert_eq!(size_label(1024), "1 KB");
    assert_eq!(size_label(90_112), "88 KB");
    assert_eq!(size_label(5_242_880), "5 MB");
}

// ================================================================ dossier ==
//
// The defects below are the ones a dossier can hide. A section that silently
// swaps a sample in for a recorded answer looks identical to one that read the
// engine — unless the tag is asserted. A release ladder generated independently
// of the pinned version still lists seven versions — with 3.4.8 *below* 2.7.4.
// A card built from its own draw sequence still shows a licence — a different
// one from the page it opens. None of those fail a build, so every assertion
// here compares the exact rendered string, the exact order, or the exact tag.

use crate::store::dossier::{
    self, Dossier, Fence, History, Precis, Rank, ReadmeBlock, Release, Reverse, Stamp,
    Standing as ReleaseStanding,
};
use crate::store::registry::{Listing, Ordering, Package, arrange, gathered};
use crate::ui::chart;
use backend_library::RegistryMetadata;

/// The coordinate the package preview scene opens, and the demo shelf holds.
const MEMCHR: &str = "pkg:cargo/memchr@2.7.4";

fn versions_reply(records: Vec<RegistryPackageRecord>) -> SurfaceReply {
    SurfaceReply::PackageVersions(records.into_boxed_slice())
}

fn package_reply(records: Vec<RegistryPackageRecord>) -> SurfaceReply {
    SurfaceReply::Package(records.into_boxed_slice())
}

fn recorded_dependents(records: Vec<RegistryPackageRecord>) -> SurfaceReply {
    SurfaceReply::Dependents(RegistryMetadata::Recorded(records.into_boxed_slice()))
}

fn unrecorded_dependents(reason: &str) -> SurfaceReply {
    SurfaceReply::Dependents(RegistryMetadata::NotRecorded(
        ProductText::new(reason).expect("a reason the feed gave"),
    ))
}

fn profile_reply(latest: Option<RegistryPackageRecord>, versions: u64) -> SurfaceReply {
    SurfaceReply::PackageProfile { latest, versions }
}

/// Returns the gathered package one set of four replies produces, as read.
fn read(
    record: &SurfaceReply,
    versions: &SurfaceReply,
    dependents: &SurfaceReply,
    profile: &SurfaceReply,
) -> Loadable<Box<Package>> {
    Loadable::Ready(Box::new(gathered(record, versions, dependents, profile)))
}

/// Returns the exact line one version row draws, in row order.
fn release_lines(dossier: &Dossier) -> Vec<String> {
    dossier
        .releases()
        .ready()
        .map(|releases| {
            releases
                .iter()
                .map(|release| {
                    format!(
                        "{}|{}|{}|{}",
                        release.version(),
                        release
                            .published()
                            .map_or_else(|| "date not recorded".to_owned(), Stamp::iso),
                        release.size(),
                        match release.standing() {
                            ReleaseStanding::Published => "published",
                            ReleaseStanding::Yanked => "yanked",
                            ReleaseStanding::Deprecated => "deprecated",
                            ReleaseStanding::Unlisted => "unlisted",
                            ReleaseStanding::Retracted => "retracted",
                            ReleaseStanding::Removed => "removed",
                        }
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Returns the names of a roster, in the order a page would list them.
fn dependent_names(dossier: &Dossier) -> Vec<String> {
    match dossier.dependents().ready() {
        Some(Reverse::Recorded(rows)) => rows.iter().map(|row| row.name().to_owned()).collect(),
        Some(Reverse::NotRecorded(_)) | None => Vec::new(),
    }
}

fn names(listings: &[Listing]) -> Vec<String> {
    listings
        .iter()
        .map(|listing| listing.row().name().to_owned())
        .collect()
}

// --------------------------------------------------------- determinism --

#[test]
fn a_sample_dossier_is_a_pure_function_of_the_package_name() {
    let once = dossier::sample(MEMCHR);
    let twice = dossier::sample(MEMCHR);
    assert_eq!(once, twice, "one coordinate names exactly one sample");
    assert_eq!(
        once.precis().ready().map(Precis::description),
        Some("A small, dependency-light caching library."),
        "the sample description is fixed by the name, not by the frame"
    );
    let other_version = dossier::sample("pkg:cargo/memchr@2.6.2");
    assert_eq!(
        other_version.precis().ready().and_then(Precis::license),
        once.precis().ready().and_then(Precis::license),
        "a licence belongs to the package, so two versions state the same one"
    );
}

#[test]
fn a_browse_card_states_exactly_what_the_page_it_opens_states() {
    let card = dossier::card(MEMCHR);
    let page = dossier::sample(MEMCHR);
    assert_eq!(card.blurb(), "A small, dependency-light caching library.");
    assert_eq!(card.license(), Some("MIT OR Apache-2.0"));
    assert_eq!(
        card.license(),
        page.precis().ready().and_then(Precis::license),
        "a card that disagreed with its page would teach a reader to trust neither"
    );
    assert_eq!(
        card.downloads().total(),
        page.downloads().ready().map(dossier::Downloads::total),
    );
    assert_eq!(
        card.released().map(Stamp::iso),
        page.pinned().and_then(Release::published).map(Stamp::iso),
        "the date on a card is the pinned release's own date"
    );
    assert_eq!(
        card.released().map(Stamp::iso),
        Some("2026-08-15".to_owned())
    );
}

#[test]
fn every_sample_section_says_that_it_is_a_sample() {
    let dossier = dossier::sample(MEMCHR);
    for tag in [
        dossier.precis().provenance().tag(),
        dossier.releases().provenance().tag(),
        dossier.history().provenance().tag(),
        dossier.downloads().provenance().tag(),
        dossier.dependencies().provenance().tag(),
        dossier.dependents().provenance().tag(),
        dossier.owners().provenance().tag(),
        dossier.readme().provenance().tag(),
    ] {
        assert_eq!(tag, Some("sample"), "an unlabelled stand-in is a lie");
    }
    assert_eq!(
        dossier.downloads().provenance().sentence(),
        Some("No surface command publishes this fact yet — these are sample values.")
    );
}

// ------------------------------------------------------- sampled shapes --

#[test]
fn a_sample_release_ladder_descends_from_the_version_the_coordinate_pinned() {
    let dossier = dossier::sample(MEMCHR);
    assert_eq!(
        release_lines(&dossier),
        vec![
            "2.7.4|2026-08-15|3 MB|published".to_owned(),
            "2.7.3|2026-07-31|2 MB|published".to_owned(),
            "2.7.2|2026-05-03|2 MB|published".to_owned(),
            "2.7.1|2026-03-04|2 MB|yanked".to_owned(),
            "2.7.0|2025-12-06|2 MB|published".to_owned(),
            "2.6.3|2025-11-26|1 MB|published".to_owned(),
            "2.6.2|2025-11-05|1 MB|published".to_owned(),
        ]
    );
    assert_eq!(
        dossier.pinned().map(Release::version),
        Some("2.7.4"),
        "the release the coordinate pinned is the one the header draws"
    );
    assert_eq!(
        dossier.history().ready().and_then(History::latest),
        Some("2.7.4")
    );
}

#[test]
fn a_sample_readme_exercises_every_block_a_reader_can_meet() {
    let dossier = dossier::sample(MEMCHR);
    let blocks = dossier
        .readme()
        .ready()
        .expect("memchr ships a sample readme");
    let shapes: Vec<String> = blocks
        .iter()
        .map(|block| match block {
            ReadmeBlock::Heading { rank, text } => format!("{}:{text}", rank_word(*rank)),
            ReadmeBlock::Paragraph(_) => "paragraph".to_owned(),
            ReadmeBlock::Code { fence, .. } => format!("code:{}", fence.tag()),
            ReadmeBlock::List(items) => format!("list:{}", items.len()),
        })
        .collect();
    assert_eq!(
        shapes,
        vec![
            "title:memchr".to_owned(),
            "paragraph".to_owned(),
            "section:Install".to_owned(),
            "code:shell".to_owned(),
            "section:Usage".to_owned(),
            "code:Rust".to_owned(),
            "subsection:What you get".to_owned(),
            "list:4".to_owned(),
            "section:Configuration".to_owned(),
            "code:manifest".to_owned(),
            "paragraph".to_owned(),
        ]
    );
    assert!(
        blocks.iter().any(|block| matches!(
            block,
            ReadmeBlock::Code { fence: Fence::Shell, text } if text == "cargo add memchr"
        )),
        "a cargo package installs with cargo, not with npm"
    );
}

const fn rank_word(rank: Rank) -> &'static str {
    match rank {
        Rank::Title => "title",
        Rank::Section => "section",
        Rank::Subsection => "subsection",
    }
}

#[test]
fn a_sample_dependent_roster_is_ordered_by_the_weight_it_publishes() {
    let dossier = dossier::sample(MEMCHR);
    let rows = match dossier.dependents().ready() {
        Some(Reverse::Recorded(rows)) => rows.clone(),
        other => panic!("memchr's sample records dependents, not {other:?}"),
    };
    assert_eq!(
        rows.first().map(|row| row.name().to_owned()),
        Some("anyhow".to_owned()),
        "the heaviest dependent leads the roster"
    );
    let weights: Vec<u64> = rows
        .iter()
        .filter_map(dossier::Dependent::downloads)
        .collect();
    assert_eq!(
        weights.len(),
        rows.len(),
        "a sampled roster weighs every row"
    );
    assert!(
        weights.windows(2).all(|pair| pair.first() >= pair.last()),
        "a roster ordered by weight never climbs: {weights:?}"
    );
}

#[test]
fn every_sample_shape_a_page_can_draw_is_reachable_by_naming_a_package() {
    let mut without_readme = 0_u32;
    let mut without_license = 0_u32;
    let mut with_a_yanked_release = 0_u32;
    let mut without_a_reverse_index = 0_u32;
    let mut without_dependents = 0_u32;
    let mut without_dependencies = 0_u32;
    for at in 0..60_u32 {
        let dossier = dossier::sample(&format!("pkg:cargo/p{at}@1.0.0"));
        if dossier.readme().ready().is_some_and(Vec::is_empty) {
            without_readme += 1;
        }
        if dossier
            .precis()
            .ready()
            .is_some_and(|precis| precis.license().is_none())
        {
            without_license += 1;
        }
        if dossier.releases().ready().is_some_and(|releases| {
            releases
                .iter()
                .any(|release| release.standing() == ReleaseStanding::Yanked)
        }) {
            with_a_yanked_release += 1;
        }
        match dossier.dependents().ready() {
            Some(Reverse::NotRecorded(_)) => without_a_reverse_index += 1,
            Some(Reverse::Recorded(rows)) if rows.is_empty() => without_dependents += 1,
            _ => (),
        }
        if dossier.dependencies().ready().is_some_and(Vec::is_empty) {
            without_dependencies += 1;
        }
    }
    let census = [
        ("no readme", without_readme),
        ("no licence", without_license),
        ("a yanked release", with_a_yanked_release),
        ("no reverse index", without_a_reverse_index),
        ("no dependents", without_dependents),
        ("no dependencies", without_dependencies),
    ];
    for (shape, found) in census {
        assert!(
            found > 0,
            "no package in sixty draws {shape}, so that branch of the page is dead code"
        );
    }
}

#[test]
fn every_coordinate_a_sample_offers_is_one_a_page_can_open() {
    for at in 0..40_u32 {
        let dossier = dossier::sample(&format!("pkg:npm/q{at}@2.0.0"));
        for release in dossier.releases().ready().into_iter().flatten() {
            package_of(release.coordinate()).expect("a sampled release names a pinned package url");
        }
        for dependency in dossier.dependencies().ready().into_iter().flatten() {
            package_of(dependency.resolved())
                .expect("a sampled dependency resolves to a pinned package url");
        }
        if let Some(Reverse::Recorded(rows)) = dossier.dependents().ready() {
            for row in rows {
                package_of(row.coordinate())
                    .expect("a sampled dependent names a pinned package url");
            }
        }
    }
}

// --------------------------------------------- recorded over sampled --

#[test]
fn a_recorded_version_list_replaces_the_sample_and_invents_no_date() {
    let records = vec![
        record(RegistryEcosystem::Cargo, "memchr", "2.7.4", 90_112),
        record(RegistryEcosystem::Cargo, "memchr", "2.10.0", 92_160),
        record(RegistryEcosystem::Cargo, "memchr", "2.9.0", 88_064),
    ];
    let state = read(
        &package_reply(records.clone()),
        &versions_reply(records.clone()),
        &recorded_dependents(Vec::new()),
        &profile_reply(records.first().cloned(), 3),
    );
    let dossier = dossier::assemble(MEMCHR, &state);
    assert_eq!(
        dossier.releases().provenance().tag(),
        None,
        "a recorded section wears no sample tag"
    );
    assert_eq!(
        release_lines(&dossier),
        vec![
            "2.10.0|date not recorded|90 KB|published".to_owned(),
            "2.9.0|date not recorded|86 KB|published".to_owned(),
            "2.7.4|date not recorded|88 KB|published".to_owned(),
        ]
    );
    assert_eq!(
        dossier.pinned().map(Release::version),
        Some("2.7.4"),
        "the pinned release is found in the recorded list, not the newest one"
    );
}

#[test]
fn an_exact_package_record_stands_in_when_the_index_lists_no_versions() {
    let only = record(RegistryEcosystem::Cargo, "memchr", "2.7.4", 90_112);
    let state = read(
        &package_reply(vec![only.clone()]),
        &versions_reply(Vec::new()),
        &recorded_dependents(Vec::new()),
        &profile_reply(Some(only), 1),
    );
    let dossier = dossier::assemble(MEMCHR, &state);
    assert_eq!(dossier.releases().provenance().tag(), None);
    assert_eq!(
        release_lines(&dossier),
        vec!["2.7.4|date not recorded|88 KB|published".to_owned()]
    );
}

#[test]
fn a_silent_index_falls_back_to_a_sample_that_says_the_index_was_silent() {
    let state = read(
        &package_reply(Vec::new()),
        &versions_reply(Vec::new()),
        &recorded_dependents(Vec::new()),
        &profile_reply(None, 0),
    );
    let dossier = dossier::assemble(MEMCHR, &state);
    assert_eq!(dossier.releases().provenance().tag(), Some("sample"));
    assert_eq!(
        dossier.releases().provenance().sentence(),
        Some("The local index recorded nothing here — these are sample values.")
    );
    assert_eq!(
        release_lines(&dossier).first().map(String::as_str),
        Some("2.7.4|2026-08-15|3 MB|published"),
        "the sample ladder stands behind the index's silence"
    );
    assert_eq!(dossier.dependents().provenance().tag(), Some("sample"));
    assert_eq!(dossier.history().provenance().tag(), Some("sample"));
}

#[test]
fn the_feeds_own_words_survive_into_the_dependents_section() {
    let state = read(
        &package_reply(Vec::new()),
        &versions_reply(Vec::new()),
        &unrecorded_dependents("this feed publishes no reverse index"),
        &profile_reply(None, 0),
    );
    let dossier = dossier::assemble(MEMCHR, &state);
    assert_eq!(dossier.dependents().provenance().tag(), None);
    match dossier.dependents().ready() {
        Some(Reverse::NotRecorded(reason)) => {
            assert_eq!(reason, "this feed publishes no reverse index");
        }
        other => panic!("a feed that publishes no reverse index must say so, not {other:?}"),
    }
}

#[test]
fn recorded_dependents_keep_the_feeds_order_and_carry_no_invented_weight() {
    let rows = vec![
        record(RegistryEcosystem::Cargo, "ripgrep", "14.1.1", 4096),
        record(RegistryEcosystem::Cargo, "aho-corasick", "1.1.3", 2048),
        record(RegistryEcosystem::Cargo, "regex", "1.11.1", 8192),
    ];
    let state = read(
        &package_reply(Vec::new()),
        &versions_reply(Vec::new()),
        &recorded_dependents(rows),
        &profile_reply(None, 0),
    );
    let dossier = dossier::assemble(MEMCHR, &state);
    assert_eq!(dossier.dependents().provenance().tag(), None);
    assert_eq!(
        dependent_names(&dossier),
        vec![
            "ripgrep".to_owned(),
            "aho-corasick".to_owned(),
            "regex".to_owned(),
        ]
    );
    let weights: Vec<Option<u64>> = match dossier.dependents().ready() {
        Some(Reverse::Recorded(rows)) => rows.iter().map(dossier::Dependent::downloads).collect(),
        other => panic!("a recorded roster holds rows, not {other:?}"),
    };
    assert_eq!(
        weights,
        vec![None, None, None],
        "the feed publishes the edge, not the popularity, so no row carries a weight"
    );
}

#[test]
fn a_read_in_flight_reserves_the_engine_sections_and_draws_the_samples() {
    let pending = dossier::assemble(MEMCHR, &Loadable::Loading);
    assert!(pending.releases().state().is_pending());
    assert!(pending.dependents().state().is_pending());
    assert!(pending.history().state().is_pending());
    assert!(
        pending.readme().ready().is_some(),
        "a sampled section does not wait for a read it never made"
    );
    assert!(pending.downloads().ready().is_some());
    assert!(pending.dependencies().ready().is_some());
    assert!(
        release_lines(&pending).is_empty(),
        "a pending version list draws no rows at all"
    );
}

#[test]
fn a_faulted_read_carries_the_engines_own_fault_into_every_engine_section() {
    let faulted = Loadable::Faulted(Box::new(shelf_fault()));
    let dossier = dossier::assemble(MEMCHR, &faulted);
    for state in [
        matches!(dossier.releases().state(), Loadable::Faulted(_)),
        matches!(dossier.dependents().state(), Loadable::Faulted(_)),
        matches!(dossier.history().state(), Loadable::Faulted(_)),
    ] {
        assert!(state, "an engine section shows the engine's failure");
    }
    match dossier.releases().state() {
        Loadable::Faulted(fault) => assert_eq!(fault.slug(), FaultSlug::Usage),
        other => panic!("a faulted section holds the typed fault, not {other:?}"),
    }
    assert!(
        dossier.readme().ready().is_some(),
        "a failed version read does not erase the README beside it"
    );
}

// ------------------------------------------------------ the browse grid --

#[test]
fn the_grid_arranges_a_loaded_page_without_asking_the_engine_again() {
    let reply = explored(vec![
        record(RegistryEcosystem::Cargo, "serde", "1.0.0", 2048),
        record(RegistryEcosystem::Npm, "zod", "3.23.8", 1_048_576),
        record(RegistryEcosystem::Pypi, "attrs", "24.2.0", 512),
        record(RegistryEcosystem::Cargo, "memchr", "2.7.4", 90_112),
    ]);
    let installed = install_page(1, 1, None, &reply).expect("a page installs");
    let rows = installed.ready().expect("the page holds rows").clone();
    let mut listings: Vec<Listing> = rows
        .iter()
        .map(|row| Listing::new(row.clone(), dossier::card(row.coordinate())))
        .collect();

    arrange(&mut listings, Ordering::Relevance);
    assert_eq!(
        names(&listings),
        vec![
            "serde".to_owned(),
            "zod".to_owned(),
            "attrs".to_owned(),
            "memchr".to_owned(),
        ],
        "relevance is the engine's own order, untouched"
    );

    arrange(&mut listings, Ordering::Downloads);
    assert_eq!(
        names(&listings),
        vec![
            "serde".to_owned(),
            "memchr".to_owned(),
            "attrs".to_owned(),
            "zod".to_owned(),
        ]
    );

    arrange(&mut listings, Ordering::Fresh);
    assert_eq!(
        names(&listings),
        vec![
            "memchr".to_owned(),
            "zod".to_owned(),
            "attrs".to_owned(),
            "serde".to_owned(),
        ]
    );
}

#[test]
fn the_sort_controls_say_which_of_them_rank_by_a_sampled_number() {
    assert_eq!(
        Ordering::ALL.map(Ordering::label),
        ["relevance", "downloads", "recently updated"]
    );
    assert_eq!(
        Ordering::ALL.map(Ordering::sentence),
        [
            "in the order the index answered",
            "by sampled downloads",
            "by sampled release date",
        ],
        "a sampled ranking says so in the sentence, not only in a chip"
    );
    assert!(
        !Ordering::Relevance.is_sampled(),
        "the engine's order is the engine's"
    );
    assert!(Ordering::Downloads.is_sampled());
    assert!(Ordering::Fresh.is_sampled());
    assert_eq!(Ordering::default(), Ordering::Relevance);
}

// -------------------------------------------------- numbers and charts --

#[test]
fn a_download_count_keeps_one_digit_of_detail_at_every_magnitude() {
    assert_eq!(dossier::tally_label(0), "0");
    assert_eq!(dossier::tally_label(934), "934");
    assert_eq!(dossier::tally_label(1_000), "1.0K");
    assert_eq!(dossier::tally_label(12_450), "12.4K");
    assert_eq!(dossier::tally_label(999_999), "999.9K");
    assert_eq!(dossier::tally_label(3_100_000), "3.1M");
    assert_eq!(dossier::tally_label(648_340_200), "648.3M");
}

#[test]
fn a_day_is_the_day_it_says_it_is() {
    assert_eq!(Stamp::of_day(0).iso(), "1970-01-01");
    assert_eq!(Stamp::of_day(20_000).iso(), "2024-10-04");
    assert_eq!(Stamp::of_day(20_000).spelled(), "4 Oct 2024");
    for day in [0_i64, 1, 59, 60, 20_000, 20_710] {
        assert_eq!(
            Stamp::of_day(day).day(),
            day,
            "a calendar day and its count are the same fact"
        );
    }
}

#[test]
fn a_chart_column_never_vanishes_and_never_overflows_its_box() {
    assert_eq!(
        chart::column(0, 100, 20),
        1,
        "a quiet week is a trough, not a gap"
    );
    assert_eq!(chart::column(50, 100, 20), 10);
    assert_eq!(chart::column(100, 100, 20), 20);
    assert_eq!(chart::column(1, 1_000_000, 80), 1);
    assert_eq!(
        chart::column(u64::MAX, 1, 20),
        20,
        "no column leaves its box"
    );
    assert_eq!(
        chart::column(5, 0, 20),
        20,
        "an empty series has no peak to scale by"
    );
}

#[test]
fn a_share_is_a_whole_percent_that_cannot_exceed_the_whole() {
    assert_eq!(chart::share(0, 0), 0);
    assert_eq!(chart::share(1, 3), 33);
    assert_eq!(chart::share(2, 3), 66);
    assert_eq!(chart::share(5, 5), 100);
    assert_eq!(
        chart::share(9, 5),
        100,
        "a part larger than its whole is still a full bar"
    );
    assert_eq!(chart::share(u64::MAX, 1), 100);
}
