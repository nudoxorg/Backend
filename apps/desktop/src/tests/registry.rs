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
    PackageReference, ProductText, RegistryDownloadCount, RegistryEcosystem,
    RegistryFactAvailability, RegistryPackageRecord, RegistryReleaseStanding, SurfaceCommand,
    SurfaceReply,
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
    let coordinate = match ecosystem {
        RegistryEcosystem::Maven => format!("pkg:maven/org.example/{name}@{version}"),
        RegistryEcosystem::Cpp => format!("pkg:generic/stable/{name}@{version}"),
        _ => format!("pkg:{}/{name}@{version}", ecosystem.as_str()),
    };
    RegistryPackageRecord {
        coordinate: PackageReference::parse(coordinate).expect("a canonical package url"),
        ecosystem,
        name: ProductText::new(name).expect("a registry name"),
        version: ProductText::new(version).expect("a pinned version"),
        bytes,
        standing: RegistryReleaseStanding::Available,
        downloads: RegistryDownloadCount::Unavailable(RegistryFactAvailability::Unsupported),
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

// ================================================================ typed package projection ==
//
// These fixtures model engine replies directly. Every test chooses the
// coverage state it intends to exercise; no package name selects a scenario.

use crate::store::dossier::{
    self, Dossier, DownloadPrecision, Downloads, Provenance, Release, Reverse, Role, Section,
};
use crate::store::registry::{Listing, Ordering, Package, arrange, gathered};
use backend_library::{
    DependencyAuthority, DependencyEvidence, DependencyFacts, DependencyScope,
    PackageDependencyRecord, PackageDependencyTarget, RegistryMetadata,
};

const MEMCHR: &str = "pkg:cargo/memchr@2.7.4";

fn record_with(
    ecosystem: RegistryEcosystem,
    name: &str,
    version: &str,
    bytes: u64,
    standing: RegistryReleaseStanding,
    downloads: RegistryDownloadCount,
    advisory: backend_library::AdvisoryPackageDto,
) -> RegistryPackageRecord {
    let coordinate = match ecosystem {
        RegistryEcosystem::Maven => format!("pkg:maven/org.example/{name}@{version}"),
        RegistryEcosystem::Cpp => format!("pkg:generic/stable/{name}@{version}"),
        _ => format!("pkg:{}/{name}@{version}", ecosystem.as_str()),
    };
    RegistryPackageRecord {
        coordinate: PackageReference::parse(coordinate).expect("a canonical package url"),
        ecosystem,
        name: ProductText::new(name).expect("a registry name"),
        version: ProductText::new(version).expect("a pinned version"),
        bytes,
        standing,
        downloads,
        facts_version: [0; 32],
        advisory,
    }
}

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

fn dependencies_reply(facts: DependencyFacts<Vec<PackageDependencyRecord>>) -> SurfaceReply {
    let facts = match facts {
        DependencyFacts::Known(rows) => DependencyFacts::Known(rows.into_boxed_slice()),
        DependencyFacts::Unknown(reason) => DependencyFacts::Unknown(reason),
        DependencyFacts::Unavailable(reason) => DependencyFacts::Unavailable(reason),
    };
    SurfaceReply::Dependencies(facts)
}

fn read(
    record: &SurfaceReply,
    versions: &SurfaceReply,
    dependencies: &SurfaceReply,
    dependents: &SurfaceReply,
    profile: &SurfaceReply,
) -> Loadable<Box<Package>> {
    Loadable::Ready(Box::new(gathered(
        record,
        versions,
        dependencies,
        dependents,
        profile,
    )))
}

fn edge(resolved: Option<&str>, scope: DependencyScope) -> PackageDependencyRecord {
    let source = PackageReference::parse(MEMCHR).expect("the source coordinate is pinned");
    let target_name = "itoa";
    let target_coordinate = resolved.map(|value| {
        PackageReference::parse(value).expect("the resolved dependency coordinate is pinned")
    });
    let target = PackageDependencyTarget::new(
        RegistryEcosystem::Cargo,
        target_name,
        "^1.0",
        target_coordinate,
    )
    .expect("a typed dependency target");
    PackageDependencyRecord::new(
        source,
        target,
        scope,
        matches!(scope, DependencyScope::Optional),
        DependencyEvidence {
            authority: DependencyAuthority::RegistryMetadata,
            frontier: [1; 32],
            provenance: [2; 32],
        },
    )
}

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
                            .map_or_else(|| "date not recorded".to_owned(), |date| date.iso()),
                        release.size(),
                        release.standing().label()
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

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

#[test]
fn every_registry_ecosystem_preserves_recorded_standing_downloads_and_advisory() {
    let ecosystems = [
        RegistryEcosystem::Cargo,
        RegistryEcosystem::Npm,
        RegistryEcosystem::Pypi,
        RegistryEcosystem::Maven,
        RegistryEcosystem::Nuget,
        RegistryEcosystem::Golang,
        RegistryEcosystem::Cpp,
    ];
    for (index, ecosystem) in ecosystems.into_iter().enumerate() {
        let downloads = RegistryDownloadCount::Exact(100 + index as u64);
        let advisory = backend_library::AdvisoryPackageDto::unknown();
        let record = record_with(
            ecosystem,
            "fixture",
            "1.2.3",
            1024 + index as u64,
            RegistryReleaseStanding::Deprecated,
            downloads.clone(),
            advisory.clone(),
        );
        let row = crate::store::registry::install_page(1, 1, None, &explored(vec![record]))
            .expect("the typed catalog answer installs")
            .ready()
            .expect("the typed catalog answer is ready")
            .first()
            .cloned()
            .expect("one fixture row");
        assert_eq!(row.ecosystem(), ecosystem);
        assert_eq!(row.release_standing(), RegistryReleaseStanding::Deprecated);
        assert_eq!(row.downloads(), &downloads);
        assert_eq!(row.advisory(), &advisory);
    }
}

#[test]
fn recorded_releases_keep_engine_versions_and_dates_remain_absent() {
    let rows = vec![
        record_with(
            RegistryEcosystem::Cargo,
            "memchr",
            "2.7.4",
            90_112,
            RegistryReleaseStanding::Available,
            RegistryDownloadCount::Exact(12),
            backend_library::AdvisoryPackageDto::unknown(),
        ),
        record_with(
            RegistryEcosystem::Cargo,
            "memchr",
            "2.10.0",
            92_160,
            RegistryReleaseStanding::Yanked,
            RegistryDownloadCount::Approximate(9),
            backend_library::AdvisoryPackageDto::unknown(),
        ),
    ];
    let state = read(
        &package_reply(rows.clone()),
        &versions_reply(rows.clone()),
        &dependencies_reply(DependencyFacts::Known(Vec::new())),
        &recorded_dependents(Vec::new()),
        &profile_reply(rows.first().cloned(), 2),
    );
    let dossier = dossier::assemble(MEMCHR, &state);
    assert_eq!(dossier.releases().provenance(), Provenance::Recorded);
    assert_eq!(
        release_lines(&dossier),
        vec![
            "2.10.0|date not recorded|90 KB|yanked".to_owned(),
            "2.7.4|date not recorded|88 KB|published".to_owned(),
        ]
    );
    assert_eq!(dossier.pinned().map(Release::version), Some("2.7.4"));
    assert_eq!(dossier.pinned().and_then(Release::published), None);
    assert_eq!(
        dossier.pinned().map(|release| release.advisory()),
        rows.first().map(|row| &row.advisory)
    );
}

#[test]
fn an_empty_version_reply_can_use_the_exact_record_without_inventing_a_release() {
    let only = record_with(
        RegistryEcosystem::Cargo,
        "memchr",
        "2.7.4",
        90_112,
        RegistryReleaseStanding::Available,
        RegistryDownloadCount::Unavailable(RegistryFactAvailability::Unsupported),
        backend_library::AdvisoryPackageDto::unknown(),
    );
    let state = read(
        &package_reply(vec![only]),
        &versions_reply(Vec::new()),
        &dependencies_reply(DependencyFacts::Known(Vec::new())),
        &recorded_dependents(Vec::new()),
        &profile_reply(None, 0),
    );
    let dossier = dossier::assemble(MEMCHR, &state);
    assert_eq!(dossier.releases().provenance(), Provenance::Recorded);
    assert_eq!(
        release_lines(&dossier),
        vec!["2.7.4|date not recorded|88 KB|published"]
    );
    assert_eq!(dossier.history().provenance(), Provenance::Recorded);
    assert_eq!(
        dossier.history().ready().map(|history| history.versions()),
        Some(0)
    );
    assert_eq!(dossier.pinned().map(Release::version), Some("2.7.4"));
}

#[test]
fn dependency_projection_preserves_requirement_scope_resolution_and_coverage() {
    let runtime = edge(Some("pkg:cargo/itoa@1.0.15"), DependencyScope::Runtime);
    let optional = edge(None, DependencyScope::Optional);
    let state = read(
        &package_reply(Vec::new()),
        &versions_reply(Vec::new()),
        &dependencies_reply(DependencyFacts::Known(vec![runtime, optional])),
        &recorded_dependents(Vec::new()),
        &profile_reply(None, 0),
    );
    let dossier = dossier::assemble(MEMCHR, &state);
    assert_eq!(dossier.dependencies().provenance(), Provenance::Recorded);
    let dependencies = dossier
        .dependencies()
        .ready()
        .expect("known dependency facts");
    assert_eq!(dependencies.len(), 2);
    assert_eq!(dependencies[0].name(), "itoa");
    assert_eq!(dependencies[0].requirement(), "^1.0");
    assert_eq!(dependencies[0].role(), Role::Required);
    assert_eq!(dependencies[0].resolved(), Some("pkg:cargo/itoa@1.0.15"));
    assert_eq!(dependencies[1].role(), Role::Optional);
    assert_eq!(dependencies[1].resolved(), None);
}

#[test]
fn dependency_unknown_and_unavailable_replies_are_explicit_coverage_states() {
    let unknown = read(
        &package_reply(Vec::new()),
        &versions_reply(Vec::new()),
        &dependencies_reply(DependencyFacts::Unknown(
            ProductText::new("dependency metadata unsupported").expect("reason"),
        )),
        &recorded_dependents(Vec::new()),
        &profile_reply(None, 0),
    );
    let unknown_dossier = dossier::assemble(MEMCHR, &unknown);
    assert_eq!(
        unknown_dossier.dependencies().provenance(),
        Provenance::Unknown
    );
    assert!(
        unknown_dossier
            .dependencies()
            .ready()
            .expect("empty unknown section")
            .is_empty()
    );

    let unavailable = read(
        &package_reply(Vec::new()),
        &versions_reply(Vec::new()),
        &dependencies_reply(DependencyFacts::Unavailable(
            ProductText::new("dependency metadata unavailable").expect("reason"),
        )),
        &recorded_dependents(Vec::new()),
        &profile_reply(None, 0),
    );
    let unavailable_dossier = dossier::assemble(MEMCHR, &unavailable);
    assert_eq!(
        unavailable_dossier.dependencies().provenance(),
        Provenance::Unavailable
    );
}

#[test]
fn dependent_coverage_distinguishes_recorded_empty_from_not_recorded() {
    let empty = read(
        &package_reply(Vec::new()),
        &versions_reply(Vec::new()),
        &dependencies_reply(DependencyFacts::Known(Vec::new())),
        &recorded_dependents(Vec::new()),
        &profile_reply(None, 0),
    );
    let empty_dossier = dossier::assemble(MEMCHR, &empty);
    assert_eq!(
        empty_dossier.dependents().provenance(),
        Provenance::Recorded
    );
    assert_eq!(dependent_names(&empty_dossier), Vec::<String>::new());

    let absent = read(
        &package_reply(Vec::new()),
        &versions_reply(Vec::new()),
        &dependencies_reply(DependencyFacts::Known(Vec::new())),
        &unrecorded_dependents("reverse index not recorded"),
        &profile_reply(None, 0),
    );
    let absent_dossier = dossier::assemble(MEMCHR, &absent);
    assert_eq!(
        absent_dossier.dependents().provenance(),
        Provenance::NotRecorded
    );
    match absent_dossier.dependents().ready() {
        Some(Reverse::NotRecorded(reason)) => assert_eq!(reason, "reverse index not recorded"),
        other => panic!("missing reverse coverage must remain typed: {other:?}"),
    }
}

#[test]
fn pending_and_faulted_engine_replies_do_not_grow_unrecorded_sections() {
    let pending = dossier::assemble(MEMCHR, &Loadable::Loading);
    assert!(pending.releases().state().is_pending());
    assert!(pending.history().state().is_pending());
    assert!(pending.downloads().state().is_pending());
    assert!(pending.dependencies().state().is_pending());
    assert!(pending.dependents().state().is_pending());
    assert_eq!(pending.precis().provenance(), Provenance::NotRecorded);
    assert_eq!(pending.owners().provenance(), Provenance::NotRecorded);
    assert_eq!(pending.readme().provenance(), Provenance::NotRecorded);
    assert!(pending.releases().ready().is_none());
    assert!(pending.downloads().ready().is_none());

    let fault = Loadable::Faulted(Box::new(shelf_fault()));
    let failed = dossier::assemble(MEMCHR, &fault);
    assert!(matches!(failed.releases().state(), Loadable::Faulted(_)));
    assert!(matches!(failed.history().state(), Loadable::Faulted(_)));
    assert!(matches!(failed.downloads().state(), Loadable::Faulted(_)));
    assert!(matches!(failed.dependencies().state(), Loadable::Faulted(_)));
    assert!(matches!(failed.dependents().state(), Loadable::Faulted(_)));
    assert!(
        failed.precis().ready().is_some(),
        "absence is explicit and stable"
    );
}

#[test]
fn download_projection_never_turns_missing_series_into_recorded_zeroes() {
    let exact = record_with(
        RegistryEcosystem::Cargo,
        "memchr",
        "2.7.4",
        90_112,
        RegistryReleaseStanding::Available,
        RegistryDownloadCount::Exact(42),
        backend_library::AdvisoryPackageDto::unknown(),
    );
    let exact_state = read(
        &package_reply(vec![exact]),
        &versions_reply(Vec::new()),
        &dependencies_reply(DependencyFacts::Known(Vec::new())),
        &recorded_dependents(Vec::new()),
        &profile_reply(None, 0),
    );
    let exact_dossier = dossier::assemble(MEMCHR, &exact_state);
    assert_eq!(exact_dossier.downloads().provenance(), Provenance::Recorded);
    let downloads = exact_dossier.downloads().ready().expect("cumulative count");
    assert_eq!(downloads.total(), Some(42));
    assert_eq!(downloads.recent(), None);
    assert_eq!(downloads.precision(), Some(DownloadPrecision::Exact));
    assert!(downloads.weekly().is_empty());
    assert_eq!(
        downloads.history().provenance(),
        Provenance::Unsupported,
        "the cumulative fact does not imply a weekly series"
    );

    let missing = record_with(
        RegistryEcosystem::Cargo,
        "memchr",
        "2.7.4",
        90_112,
        RegistryReleaseStanding::Available,
        RegistryDownloadCount::Unavailable(RegistryFactAvailability::Stale),
        backend_library::AdvisoryPackageDto::unknown(),
    );
    let missing_state = read(
        &package_reply(vec![missing]),
        &versions_reply(Vec::new()),
        &dependencies_reply(DependencyFacts::Known(Vec::new())),
        &recorded_dependents(Vec::new()),
        &profile_reply(None, 0),
    );
    let missing_dossier = dossier::assemble(MEMCHR, &missing_state);
    assert_eq!(
        missing_dossier.downloads().provenance(),
        Provenance::Stale
    );
    assert_eq!(
        missing_dossier
            .downloads()
            .ready()
            .and_then(Downloads::total),
        None
    );

    let unsupported = record_with(
        RegistryEcosystem::Cargo,
        "memchr",
        "2.7.4",
        90_112,
        RegistryReleaseStanding::Available,
        RegistryDownloadCount::Unavailable(RegistryFactAvailability::Unsupported),
        backend_library::AdvisoryPackageDto::unknown(),
    );
    let unsupported_state = read(
        &package_reply(vec![unsupported]),
        &versions_reply(Vec::new()),
        &dependencies_reply(DependencyFacts::Known(Vec::new())),
        &recorded_dependents(Vec::new()),
        &profile_reply(None, 0),
    );
    let unsupported_dossier = dossier::assemble(MEMCHR, &unsupported_state);
    assert_eq!(
        unsupported_dossier.downloads().provenance(),
        Provenance::Unsupported
    );
}

#[test]
fn card_projection_has_only_row_facts_and_explicit_absence() {
    let row_record = record_with(
        RegistryEcosystem::Npm,
        "zod",
        "3.23.8",
        1_048_576,
        RegistryReleaseStanding::Available,
        RegistryDownloadCount::Approximate(1_200),
        backend_library::AdvisoryPackageDto::unknown(),
    );
    let row = crate::store::registry::install_page(1, 1, None, &explored(vec![row_record]))
        .expect("catalog answer")
        .ready()
        .expect("catalog rows")
        .first()
        .cloned()
        .expect("one row");
    let card = dossier::live_card(&row);
    assert_eq!(card.blurb(), "description not recorded");
    assert!(card.keywords().is_empty());
    assert_eq!(card.license(), None);
    assert_eq!(card.released(), None);
    assert_eq!(
        card.standing(),
        RegistryReleaseStanding::Available,
        "card standing comes from the same release row"
    );
    assert_eq!(
        card.advisory(),
        &backend_library::AdvisoryPackageDto::unknown()
    );
    assert_eq!(card.downloads().total(), Some(1_200));
    assert_eq!(
        card.downloads().precision(),
        Some(DownloadPrecision::Approximate)
    );
    assert!(
        card.downloads().counts().is_empty(),
        "the row has no weekly series"
    );
}

#[test]
fn browse_sorting_uses_only_recorded_row_facts_and_keeps_missing_values_last() {
    let rows = vec![
        record_with(
            RegistryEcosystem::Cargo,
            "unknown",
            "1.0.0",
            1024,
            RegistryReleaseStanding::Available,
        RegistryDownloadCount::Unavailable(RegistryFactAvailability::Unknown),
            backend_library::AdvisoryPackageDto::unknown(),
        ),
        record_with(
            RegistryEcosystem::Cargo,
            "popular",
            "1.0.0",
            1024,
            RegistryReleaseStanding::Available,
            RegistryDownloadCount::Exact(900),
            backend_library::AdvisoryPackageDto::unknown(),
        ),
        record_with(
            RegistryEcosystem::Cargo,
            "approximate",
            "1.0.0",
            1024,
            RegistryReleaseStanding::Available,
            RegistryDownloadCount::Approximate(500),
            backend_library::AdvisoryPackageDto::unknown(),
        ),
    ];
    let installed =
        crate::store::registry::install_page(1, 1, None, &explored(rows)).expect("catalog answer");
    let rows = installed.ready().expect("catalog rows").clone();
    let mut listings: Vec<Listing> = rows
        .iter()
        .map(|row| Listing::new(row.clone(), dossier::live_card(row)))
        .collect();
    arrange(&mut listings, Ordering::Downloads);
    assert_eq!(names(&listings), vec!["popular", "approximate", "unknown"]);
    let mut fresh_listings: Vec<Listing> = rows
        .iter()
        .map(|row| Listing::new(row.clone(), dossier::live_card(row)))
        .collect();
    arrange(&mut fresh_listings, Ordering::Fresh);
    assert_eq!(names(&fresh_listings), vec!["unknown", "popular", "approximate"]);
}

#[test]
fn ordering_words_and_states_have_no_generated_path() {
    assert_eq!(
        Ordering::ALL.map(Ordering::sentence),
        [
            "in the order the index answered",
            "by recorded downloads",
            "by recorded release date",
        ]
    );
}

#[test]
fn coverage_mapping_is_closed_and_shared_by_sections_and_cards() {
    assert_eq!(Provenance::Recorded.sentence(), None);
    for state in [
        Provenance::NotRecorded,
        Provenance::Unsupported,
        Provenance::Unavailable,
        Provenance::Stale,
        Provenance::Unknown,
    ] {
        assert!(state.is_not_recorded());
        assert!(state.sentence().is_some());
    }
    assert_eq!(
        Section::unsupported(Vec::<String>::new()).availability(),
        Provenance::Unsupported
    );
}

#[test]
fn date_math_only_round_trips_recorded_calendar_days() {
    use crate::store::dossier::Stamp;
    assert_eq!(Stamp::of_day(0).iso(), "1970-01-01");
    assert_eq!(Stamp::of_day(20_000).iso(), "2024-10-04");
    for day in [0_i64, 1, 59, 60, 20_000, 20_710] {
        assert_eq!(Stamp::of_day(day).day(), day);
    }
}

#[test]
fn chart_scaling_is_independent_of_package_fixture_data() {
    use crate::ui::chart;
    assert_eq!(chart::column(0, 100, 20), 1);
    assert_eq!(chart::column(50, 100, 20), 10);
    assert_eq!(chart::column(u64::MAX, 1, 20), 20);
    assert_eq!(chart::share(2, 3), 66);
    assert_eq!(chart::share(9, 5), 100);
}
