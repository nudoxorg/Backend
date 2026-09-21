//! Immutable package projections for the desktop registry surface.
//!
//! Every value in this module is either copied from an admitted engine reply or
//! carries an explicit coverage state. The module has no preview catalog,
//! generated package metadata, clock-based values, or registry URL builders.

use super::registry::{Dependents, Loadable, Package, PackageRow, Spelling, size_label};
use backend_library::{
    AdvisoryPackageDto, DependencyFacts, PackageDependencyRecord, PackageReference,
    RegistryDownloadCount, RegistryEcosystem, RegistryFactAvailability, RegistryReleaseStanding,
};
use backend_present::Fault;
use backend_present::Language;

// ------------------------------------------------------------ availability --

/// Coverage of a package fact.
///
/// The value is deliberately shared by every section and card. A ready empty
/// collection is still Recorded; the other variants describe why a fact is
/// absent from an admitted answer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Provenance {
    /// The engine answered and the value is exactly what it published.
    Recorded,
    /// The configured feed does not publish this fact.
    NotRecorded,
    /// The source explicitly does not support this fact.
    Unsupported,
    /// The source should provide this fact, but it could not be obtained.
    Unavailable,
    /// A source answered with data older than the current frontier.
    Stale,
    /// The source did not establish coverage for this fact.
    Unknown,
}

impl Provenance {
    /// Returns the coverage sentence shown beside an absent section.
    pub(crate) const fn sentence(self) -> Option<&'static str> {
        match self {
            Self::Recorded => None,
            Self::NotRecorded => Some("This registry feed does not record this information."),
            Self::Unsupported => Some("This registry feed does not support this information."),
            Self::Unavailable => Some("This registry fact is currently unavailable."),
            Self::Stale => Some("This registry fact is stale."),
            Self::Unknown => Some("Coverage for this registry fact is unknown."),
        }
    }

    /// Returns whether the value should be rendered as an absent fact.
    ///
    /// All absence states use the same typed rendering policy.
    pub(crate) const fn is_not_recorded(self) -> bool {
        !matches!(self, Self::Recorded)
    }
}

/// One package fact and the typed coverage of the answer that produced it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Section<T> {
    state: Loadable<T>,
    provenance: Provenance,
}

impl<T> Section<T> {
    /// Creates a section backed by the engine answer.
    pub(crate) const fn recorded(state: Loadable<T>) -> Self {
        Self {
            state,
            provenance: Provenance::Recorded,
        }
    }

    /// Creates a section with explicit coverage.
    pub(crate) const fn with_provenance(state: Loadable<T>, provenance: Provenance) -> Self {
        Self { state, provenance }
    }

    /// Creates a not-recorded ready value.
    pub(crate) const fn not_recorded(value: T) -> Self {
        Self::with_provenance(Loadable::Ready(value), Provenance::NotRecorded)
    }

    /// Creates an unsupported ready value.
    pub(crate) const fn unsupported(value: T) -> Self {
        Self::with_provenance(Loadable::Ready(value), Provenance::Unsupported)
    }

    /// Creates an unavailable ready value.
    pub(crate) const fn unavailable(value: T) -> Self {
        Self::with_provenance(Loadable::Ready(value), Provenance::Unavailable)
    }

    /// Creates a stale ready value.
    pub(crate) const fn stale(value: T) -> Self {
        Self::with_provenance(Loadable::Ready(value), Provenance::Stale)
    }

    /// Creates an unknown-coverage ready value.
    pub(crate) const fn unknown(value: T) -> Self {
        Self::with_provenance(Loadable::Ready(value), Provenance::Unknown)
    }

    /// Returns the request state.
    pub(crate) const fn state(&self) -> &Loadable<T> {
        &self.state
    }

    /// Returns this section's typed coverage.
    pub(crate) const fn provenance(&self) -> Provenance {
        self.provenance
    }

    /// Alias for consumers that use the coverage vocabulary directly.
    pub(crate) const fn availability(&self) -> Provenance {
        self.provenance
    }

    /// Returns the answer when one arrived.
    pub(crate) const fn ready(&self) -> Option<&T> {
        self.state.ready()
    }
}

// ----------------------------------------------------------------- dates --

/// A calendar day, represented without a timezone or fabricated clock value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Stamp {
    year: i64,
    month: u8,
    day: u8,
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

impl Stamp {
    /// Constructs a date from an epoch-day value.
    pub(crate) fn of_day(day: i64) -> Self {
        let (year, month, day) = civil_from_days(day);
        Self { year, month, day }
    }

    /// Returns an ISO date.
    pub(crate) fn iso(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// Returns epoch days for ordering.
    pub(crate) fn day(self) -> i64 {
        days_from_civil(self.year, i64::from(self.month), i64::from(self.day))
    }

    /// Returns a compact human date.
    pub(crate) fn spelled(self) -> String {
        let at = usize::from(self.month.saturating_sub(1));
        let name = MONTHS.get(at).copied().unwrap_or("Jan");
        format!("{} {name} {}", self.day, self.year)
    }
}

fn civil_from_days(days: i64) -> (i64, u8, u8) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let of_era = shifted - era * 146_097;
    let year_of_era = (of_era - of_era / 1460 + of_era / 36_524 - of_era / 146_096) / 365;
    let of_year = of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * of_year + 2) / 153;
    let day = of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, narrow(month), narrow(day))
}

fn narrow(value: i64) -> u8 {
    u8::try_from(value).unwrap_or(1)
}

const fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month = if month > 2 { month - 3 } else { month + 9 };
    let of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + of_year;
    era * 146_097 + of_era - 719_468
}

// ----------------------------------------------------------- package facts --

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LinkKind {
    Repository,
    Homepage,
    Documentation,
}

impl LinkKind {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Repository => "Repository",
            Self::Homepage => "Homepage",
            Self::Documentation => "Documentation",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Link {
    kind: LinkKind,
    url: String,
}

impl Link {
    pub(crate) const fn kind(&self) -> LinkKind {
        self.kind
    }

    pub(crate) fn url(&self) -> &str {
        &self.url
    }
}

/// Package identity metadata. Optional fields remain empty when the registry
/// record does not publish them.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Precis {
    description: String,
    keywords: Vec<String>,
    license: Option<String>,
    links: Vec<Link>,
}

impl Precis {
    pub(crate) fn description(&self) -> &str {
        &self.description
    }

    pub(crate) fn keywords(&self) -> &[String] {
        &self.keywords
    }

    pub(crate) fn license(&self) -> Option<&str> {
        self.license.as_deref()
    }

    pub(crate) fn links(&self) -> &[Link] {
        &self.links
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Standing {
    Published,
    Yanked,
    Deprecated,
    Unlisted,
    Retracted,
    Removed,
}

impl Standing {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Published => "published",
            Self::Yanked => "yanked",
            Self::Deprecated => "deprecated",
            Self::Unlisted => "unlisted",
            Self::Retracted => "retracted",
            Self::Removed => "removed",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Release {
    coordinate: String,
    version: String,
    bytes: u64,
    published: Option<Stamp>,
    standing: Standing,
    advisory: AdvisoryPackageDto,
}

impl Release {
    pub(crate) fn coordinate(&self) -> &str {
        &self.coordinate
    }

    pub(crate) fn version(&self) -> &str {
        &self.version
    }

    pub(crate) fn size(&self) -> String {
        size_label(self.bytes)
    }

    pub(crate) const fn published(&self) -> Option<Stamp> {
        self.published
    }

    pub(crate) const fn standing(&self) -> Standing {
        self.standing
    }

    pub(crate) const fn advisory(&self) -> &AdvisoryPackageDto {
        &self.advisory
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Tally {
    week: Stamp,
    count: u64,
}

impl Tally {
    pub(crate) const fn week(&self) -> Stamp {
        self.week
    }

    pub(crate) const fn count(&self) -> u64 {
        self.count
    }
}

/// Precision of a cumulative registry download observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DownloadPrecision {
    Exact,
    Approximate,
}

/// Download observations. Cumulative and weekly coverage are kept separate so
/// an absent window is never represented by fabricated zeroes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Downloads {
    history: Section<Vec<Tally>>,
    total: Option<u64>,
    recent: Option<u64>,
    precision: Option<DownloadPrecision>,
}

impl Downloads {
    /// Returns the admitted weekly history, when the source publishes it.
    pub(crate) fn weekly(&self) -> &[Tally] {
        self.history.ready().map_or(&[], Vec::as_slice)
    }

    pub(crate) fn counts(&self) -> Vec<u64> {
        self.weekly().iter().map(Tally::count).collect()
    }

    /// Returns the cumulative observation, when one was published.
    pub(crate) const fn total(&self) -> Option<u64> {
        self.total
    }

    /// Returns the recent observation, when the source publishes that window.
    pub(crate) const fn recent(&self) -> Option<u64> {
        self.recent
    }

    /// Returns coverage for the weekly history independently of cumulative counts.
    pub(crate) const fn history(&self) -> &Section<Vec<Tally>> {
        &self.history
    }

    /// Returns whether the cumulative observation is exact or approximate.
    pub(crate) const fn precision(&self) -> Option<DownloadPrecision> {
        self.precision
    }

    pub(crate) fn span(&self) -> Option<(Stamp, Stamp)> {
        Some((self.weekly().first()?.week(), self.weekly().last()?.week()))
    }
}

impl Default for Downloads {
    fn default() -> Self {
        Self {
            history: Section::unsupported(Vec::new()),
            total: None,
            recent: None,
            precision: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Role {
    Required,
    Optional,
    Development,
    Build,
    Peer,
}

impl Role {
    pub(crate) const fn tag(self) -> Option<&'static str> {
        match self {
            Self::Required => None,
            Self::Optional => Some("optional"),
            Self::Development => Some("dev"),
            Self::Build => Some("build"),
            Self::Peer => Some("peer"),
        }
    }
}

/// One outgoing edge as admitted by the package graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Dependency {
    name: String,
    requirement: String,
    ecosystem: RegistryEcosystem,
    role: Role,
    resolved: Option<String>,
}

impl Dependency {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn requirement(&self) -> &str {
        &self.requirement
    }

    pub(crate) const fn ecosystem(&self) -> RegistryEcosystem {
        self.ecosystem
    }

    pub(crate) const fn role(&self) -> Role {
        self.role
    }

    pub(crate) fn resolved(&self) -> Option<&str> {
        self.resolved.as_deref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Dependent {
    coordinate: String,
    name: String,
    version: String,
    ecosystem: RegistryEcosystem,
    downloads: Option<u64>,
}

impl Dependent {
    pub(crate) fn coordinate(&self) -> &str {
        &self.coordinate
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn version(&self) -> &str {
        &self.version
    }

    pub(crate) const fn ecosystem(&self) -> RegistryEcosystem {
        self.ecosystem
    }

    pub(crate) const fn downloads(&self) -> Option<u64> {
        self.downloads
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Reverse {
    Recorded(Vec<Dependent>),
    NotRecorded(String),
}

impl Default for Reverse {
    fn default() -> Self {
        Self::Recorded(Vec::new())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Seat {
    Publisher,
    Maintainer,
}

impl Seat {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Publisher => "publisher",
            Self::Maintainer => "maintainer",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Owner {
    handle: String,
    seat: Seat,
}

impl Owner {
    pub(crate) fn handle(&self) -> &str {
        &self.handle
    }

    pub(crate) const fn seat(&self) -> Seat {
        self.seat
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct History {
    versions: u64,
    latest: Option<String>,
}

impl History {
    pub(crate) const fn versions(&self) -> u64 {
        self.versions
    }

    pub(crate) fn latest(&self) -> Option<&str> {
        self.latest.as_deref()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Fence {
    Shell,
    Source(Language),
    Manifest,
}

impl Fence {
    pub(crate) const fn tag(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::Manifest => "manifest",
            Self::Source(language) => crate::theme::language::label(language),
        }
    }

    pub(crate) const fn language(self) -> Option<Language> {
        match self {
            Self::Shell | Self::Manifest => None,
            Self::Source(language) => Some(language),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Rank {
    Title,
    Section,
    Subsection,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ReadmeBlock {
    Heading { rank: Rank, text: String },
    Paragraph(String),
    Code { fence: Fence, text: String },
    List(Vec<String>),
}

// ----------------------------------------------------------- the dossier --

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Dossier {
    coordinate: String,
    spelling: Spelling,
    precis: Section<Precis>,
    releases: Section<Vec<Release>>,
    history: Section<History>,
    downloads: Section<Downloads>,
    dependencies: Section<Vec<Dependency>>,
    dependents: Section<Reverse>,
    owners: Section<Vec<Owner>>,
    readme: Section<Vec<ReadmeBlock>>,
}

impl Dossier {
    pub(crate) fn coordinate(&self) -> &str {
        &self.coordinate
    }

    pub(crate) const fn spelling(&self) -> &Spelling {
        &self.spelling
    }

    pub(crate) const fn precis(&self) -> &Section<Precis> {
        &self.precis
    }

    pub(crate) const fn releases(&self) -> &Section<Vec<Release>> {
        &self.releases
    }

    pub(crate) const fn history(&self) -> &Section<History> {
        &self.history
    }

    pub(crate) const fn downloads(&self) -> &Section<Downloads> {
        &self.downloads
    }

    pub(crate) const fn dependencies(&self) -> &Section<Vec<Dependency>> {
        &self.dependencies
    }

    pub(crate) const fn dependents(&self) -> &Section<Reverse> {
        &self.dependents
    }

    pub(crate) const fn owners(&self) -> &Section<Vec<Owner>> {
        &self.owners
    }

    pub(crate) fn route_request(&self, route: PackageRoute) -> Option<PackageRouteRequest> {
        PackageReference::parse(self.coordinate.clone())
            .ok()
            .map(|package| PackageRouteRequest { package, route })
    }

    pub(crate) const fn readme(&self) -> &Section<Vec<ReadmeBlock>> {
        &self.readme
    }

    pub(crate) fn pinned(&self) -> Option<&Release> {
        let releases = self.releases.ready()?;
        releases
            .iter()
            .find(|release| release.version() == self.spelling.version())
            .or_else(|| releases.first())
    }

    pub(crate) const fn ecosystem(&self) -> Option<RegistryEcosystem> {
        self.spelling.ecosystem()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PackageRoute {
    Documentation,
    Source,
    Code,
    Search,
}

impl PackageRoute {
    pub(crate) const fn key(self) -> &'static str {
        match self {
            Self::Documentation => "package-docs",
            Self::Source => "package-source",
            Self::Code => "package-code",
            Self::Search => "package-search",
        }
    }

    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::Documentation => "Documentation",
            Self::Source => "Source",
            Self::Code => "Code",
            Self::Search => "Search",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PackageRouteRequest {
    package: PackageReference,
    route: PackageRoute,
}

impl PackageRouteRequest {
    pub(crate) const fn package(&self) -> &PackageReference {
        &self.package
    }

    pub(crate) const fn route(&self) -> PackageRoute {
        self.route
    }
}

/// Assembles one package from only the live gathered store state.
pub(crate) fn assemble(coordinate: &str, state: &Loadable<Box<Package>>) -> Dossier {
    let spelling = Spelling::of(coordinate);
    Dossier {
        coordinate: coordinate.to_owned(),
        spelling,
        precis: Section::not_recorded(Precis::default()),
        releases: releases_section(state),
        history: history_section(state),
        downloads: downloads_section(state),
        dependencies: dependencies_section(state),
        dependents: reverse_section(state),
        owners: Section::not_recorded(Vec::new()),
        readme: Section::not_recorded(Vec::new()),
    }
}

/// Projects cumulative download telemetry while preserving missing coverage.
fn downloads_section(state: &Loadable<Box<Package>>) -> Section<Downloads> {
    match engine(state, Package::record) {
        Reached::Pending => Section::recorded(Loadable::Loading),
        Reached::Faulted(fault) => Section::recorded(Loadable::Faulted(fault)),
        Reached::Answered(rows) => {
            let Some(row) = rows.first() else {
                return Section::not_recorded(Downloads::default());
            };
            match row.downloads() {
                RegistryDownloadCount::Exact(value) | RegistryDownloadCount::Approximate(value) => {
                    // The package feed has no weekly window. Keep the
                    // cumulative fact, but make the usage-series limitation
                    // explicit so recent/weekly numbers cannot be mistaken for
                    // recorded zeros.
                    Section::with_provenance(
                        Loadable::Ready(Downloads {
                            history: Section::unsupported(Vec::new()),
                            total: Some(*value),
                            recent: None,
                            precision: Some(match row.downloads() {
                                RegistryDownloadCount::Exact(_) => DownloadPrecision::Exact,
                                RegistryDownloadCount::Approximate(_) => {
                                    DownloadPrecision::Approximate
                                }
                                RegistryDownloadCount::Unavailable(_) => unreachable!(),
                            }),
                        }),
                        Provenance::Recorded,
                    )
                }
                RegistryDownloadCount::Unavailable(coverage) => Section::with_provenance(
                    Loadable::Ready(Downloads::default()),
                    registry_coverage(*coverage),
                ),
            }
        }
    }
}

/// Projects the version command. An empty admitted answer remains Recorded
/// empty; a pinned package row is used only when the version endpoint itself
/// returned no rows and the exact package endpoint did return one.
fn releases_section(state: &Loadable<Box<Package>>) -> Section<Vec<Release>> {
    match engine(state, Package::versions) {
        Reached::Pending => Section::recorded(Loadable::Loading),
        Reached::Faulted(fault) => Section::recorded(Loadable::Faulted(fault)),
        Reached::Answered(rows) if rows.is_empty() => exact(state).map_or_else(
            || Section::recorded(Loadable::Ready(Vec::new())),
            |only| Section::recorded(Loadable::Ready(only)),
        ),
        Reached::Answered(rows) => {
            Section::recorded(Loadable::Ready(rows.iter().map(release_of).collect()))
        }
    }
}

fn exact(state: &Loadable<Box<Package>>) -> Option<Vec<Release>> {
    match engine(state, Package::record) {
        Reached::Answered(rows) if !rows.is_empty() => Some(rows.iter().map(release_of).collect()),
        Reached::Answered(_) | Reached::Pending | Reached::Faulted(_) => None,
    }
}

fn dependencies_section(state: &Loadable<Box<Package>>) -> Section<Vec<Dependency>> {
    match engine(state, Package::dependencies) {
        Reached::Pending => Section::recorded(Loadable::Loading),
        Reached::Faulted(fault) => Section::recorded(Loadable::Faulted(fault)),
        Reached::Answered(DependencyFacts::Known(rows)) => {
            Section::recorded(Loadable::Ready(rows.iter().map(dependency_of).collect()))
        }
        Reached::Answered(DependencyFacts::Unknown(_)) => Section::unknown(Vec::new()),
        Reached::Answered(DependencyFacts::Unavailable(_)) => Section::unavailable(Vec::new()),
    }
}

fn reverse_section(state: &Loadable<Box<Package>>) -> Section<Reverse> {
    match engine(state, Package::dependents) {
        Reached::Pending => Section::recorded(Loadable::Loading),
        Reached::Faulted(fault) => Section::recorded(Loadable::Faulted(fault)),
        Reached::Answered(Dependents::NotRecorded(reason)) => Section::with_provenance(
            Loadable::Ready(Reverse::NotRecorded(reason.clone())),
            Provenance::NotRecorded,
        ),
        Reached::Answered(Dependents::Recorded(rows)) => Section::recorded(Loadable::Ready(
            Reverse::Recorded(rows.iter().map(dependent_of).collect()),
        )),
    }
}

fn history_section(state: &Loadable<Box<Package>>) -> Section<History> {
    match engine(state, Package::profile) {
        Reached::Pending => Section::recorded(Loadable::Loading),
        Reached::Faulted(fault) => Section::recorded(Loadable::Faulted(fault)),
        Reached::Answered(profile) => Section::recorded(Loadable::Ready(History {
            versions: profile.versions(),
            latest: profile.latest().map(|row| row.version().to_owned()),
        })),
    }
}

enum Reached<'a, T> {
    Pending,
    Faulted(Box<Fault>),
    Answered(&'a T),
}

fn engine<'a, T>(
    state: &'a Loadable<Box<Package>>,
    section: fn(&'a Package) -> &'a Loadable<T>,
) -> Reached<'a, T> {
    let package = match state {
        Loadable::Idle | Loadable::Loading => return Reached::Pending,
        Loadable::Faulted(fault) => return Reached::Faulted(fault.clone()),
        Loadable::Ready(package) => package,
    };
    match section(package) {
        Loadable::Idle | Loadable::Loading => Reached::Pending,
        Loadable::Faulted(fault) => Reached::Faulted(fault.clone()),
        Loadable::Ready(value) => Reached::Answered(value),
    }
}

fn release_of(row: &PackageRow) -> Release {
    Release {
        coordinate: row.coordinate().to_owned(),
        version: row.version().to_owned(),
        bytes: row.bytes(),
        // RegistryPackageRecord has no publication date. None is the typed
        // absence consumed by the view; using the current date would be false.
        published: None,
        standing: match row.release_standing() {
            backend_library::RegistryReleaseStanding::Available => Standing::Published,
            backend_library::RegistryReleaseStanding::Yanked => Standing::Yanked,
            backend_library::RegistryReleaseStanding::Deprecated => Standing::Deprecated,
            backend_library::RegistryReleaseStanding::Unlisted => Standing::Unlisted,
            backend_library::RegistryReleaseStanding::Retracted => Standing::Retracted,
            backend_library::RegistryReleaseStanding::Removed => Standing::Removed,
        },
        advisory: row.advisory().clone(),
    }
}

fn dependency_of(row: &PackageDependencyRecord) -> Dependency {
    let target = &row.target;
    let role = match row.scope {
        backend_library::DependencyScope::Runtime => Role::Required,
        backend_library::DependencyScope::Optional => Role::Optional,
        backend_library::DependencyScope::Development => Role::Development,
        backend_library::DependencyScope::Build => Role::Build,
        backend_library::DependencyScope::Peer => Role::Peer,
    };
    Dependency {
        name: target.name.as_str().to_owned(),
        requirement: target.requirement.as_str().to_owned(),
        ecosystem: target.ecosystem,
        role,
        resolved: target
            .resolved
            .as_ref()
            .map(|value| value.as_str().to_owned()),
    }
}

fn dependent_of(row: &PackageRow) -> Dependent {
    Dependent {
        coordinate: row.coordinate().to_owned(),
        name: row.name().to_owned(),
        version: row.version().to_owned(),
        ecosystem: row.ecosystem(),
        // Dependents replies publish the edge only. No popularity is inferred
        // from archive bytes or another package's current telemetry.
        downloads: None,
    }
}

fn registry_coverage(coverage: RegistryFactAvailability) -> Provenance {
    match coverage {
        RegistryFactAvailability::NotRecorded => Provenance::NotRecorded,
        RegistryFactAvailability::Unsupported => Provenance::Unsupported,
        RegistryFactAvailability::Unavailable => Provenance::Unavailable,
        RegistryFactAvailability::Stale => Provenance::Stale,
        RegistryFactAvailability::Unknown => Provenance::Unknown,
    }
}

// ---------------------------------------------------------- browse cards --

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Card {
    blurb: String,
    keywords: Vec<String>,
    license: Option<String>,
    released: Option<Stamp>,
    standing: RegistryReleaseStanding,
    advisory: AdvisoryPackageDto,
    downloads: DownloadFact,
}

/// Card download coverage, backed by the same availability abstraction used by
/// package sections.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DownloadFact {
    downloads: Option<Downloads>,
    provenance: Provenance,
}

impl DownloadFact {
    pub(crate) fn reported(downloads: Downloads) -> Self {
        Self {
            downloads: Some(downloads),
            provenance: Provenance::Recorded,
        }
    }

    pub(crate) fn unavailable(provenance: Provenance) -> Self {
        Self {
            downloads: None,
            provenance,
        }
    }

    pub(crate) fn total(&self) -> Option<u64> {
        self.downloads.as_ref().and_then(Downloads::total)
    }

    pub(crate) fn counts(&self) -> Vec<u64> {
        self.downloads
            .as_ref()
            .map_or_else(Vec::new, Downloads::counts)
    }

    /// Returns the source's exact or approximate qualifier, when a count is present.
    pub(crate) const fn precision(&self) -> Option<DownloadPrecision> {
        match &self.downloads {
            Some(downloads) => downloads.precision(),
            None => None,
        }
    }

    pub(crate) const fn provenance(&self) -> Provenance {
        self.provenance
    }
}

impl Card {
    pub(crate) fn blurb(&self) -> &str {
        &self.blurb
    }

    pub(crate) fn keywords(&self) -> &[String] {
        self.keywords.get(..3).unwrap_or(&self.keywords)
    }

    pub(crate) fn license(&self) -> Option<&str> {
        self.license.as_deref()
    }

    pub(crate) const fn released(&self) -> Option<Stamp> {
        self.released
    }

    /// Returns the package release standing copied from the catalog row.
    pub(crate) const fn standing(&self) -> RegistryReleaseStanding {
        self.standing
    }

    /// Returns the versioned advisory projection copied from the catalog row.
    pub(crate) const fn advisory(&self) -> &AdvisoryPackageDto {
        &self.advisory
    }

    pub(crate) const fn downloads(&self) -> &DownloadFact {
        &self.downloads
    }

    pub(crate) fn weight(&self) -> Option<u64> {
        self.downloads.total()
    }

    pub(crate) fn freshness(&self) -> Option<i64> {
        self.released.map(Stamp::day)
    }
}

/// Projects one admitted catalog row. Fields absent from the row are kept
/// absent; the card never derives a README, description, license, date, owner,
/// link, or install command from a package name.
pub(crate) fn live_card(row: &PackageRow) -> Card {
    let downloads = match row.downloads() {
        RegistryDownloadCount::Exact(value) | RegistryDownloadCount::Approximate(value) => {
            DownloadFact::reported(Downloads {
                history: Section::unsupported(Vec::new()),
                total: Some(*value),
                recent: None,
                precision: Some(match row.downloads() {
                    RegistryDownloadCount::Exact(_) => DownloadPrecision::Exact,
                    RegistryDownloadCount::Approximate(_) => DownloadPrecision::Approximate,
                    RegistryDownloadCount::Unavailable(_) => unreachable!(),
                }),
            })
        }
        RegistryDownloadCount::Unavailable(coverage) => {
            DownloadFact::unavailable(registry_coverage(*coverage))
        }
    };
    Card {
        blurb: "description not recorded".to_owned(),
        keywords: Vec::new(),
        license: None,
        released: None,
        standing: row.release_standing(),
        advisory: row.advisory().clone(),
        downloads,
    }
}

/// Returns a compact human count.
pub(crate) fn tally_label(count: u64) -> String {
    const THOUSAND: u64 = 1_000;
    const MILLION: u64 = 1_000_000;
    if count < THOUSAND {
        return count.to_string();
    }
    if count < MILLION {
        let whole = count / THOUSAND;
        return format!("{whole}.{}K", count % THOUSAND / 100);
    }
    let whole = count / MILLION;
    format!("{whole}.{}M", count % MILLION / 100_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::registry::Loadable;

    #[test]
    fn absence_states_are_shared_by_sections_and_cards() {
        assert!(Provenance::Unsupported.is_not_recorded());
        assert!(Provenance::Unavailable.is_not_recorded());
        assert!(Provenance::Stale.is_not_recorded());
        assert!(Provenance::Unknown.is_not_recorded());
        assert_eq!(
            Section::unsupported(Vec::<String>::new()).availability(),
            Provenance::Unsupported
        );
        assert_eq!(
            DownloadFact::unavailable(Provenance::Unknown).provenance(),
            Provenance::Unknown
        );
    }

    #[test]
    fn route_request_keeps_the_admitted_package_identity() {
        let dossier = assemble("pkg:cargo/serde@1.0.0", &Loadable::Loading);
        let request = dossier
            .route_request(PackageRoute::Source)
            .expect("a pinned package is an admitted route identity");
        assert_eq!(request.package().as_str(), "pkg:cargo/serde@1.0.0");
        assert_eq!(request.route(), PackageRoute::Source);
    }

    #[test]
    fn date_conversion_round_trips_epoch_days() {
        for day in [0_i64, 1, 59, 60, 20_000, 20_710] {
            assert_eq!(Stamp::of_day(day).day(), day);
        }
    }
}
