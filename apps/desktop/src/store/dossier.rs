//! Everything a registry can say about one package, as one typed value.
//! Each section states where it came from, so a stand-in can never pass as a fact.
//! Nothing here reaches the service; a dossier is assembled from a reply.
//!
//! The production dossier is a projection of the live registry reply. Every
//! section carries a [`Provenance`]: either the engine answered and the
//! section holds exactly what it said, or the configured feed does not publish
//! that fact and the page says so. Test-only fixtures exercise the full visual
//! state matrix without being compiled into the application.
//!
//! A `Recorded` section never contains a fabricated field — where the feed is
//! silent the field is `None` and the page draws "not recorded" rather than
//! something plausible. Download telemetry keeps an explicit missing state so
//! a real zero cannot be confused with an unavailable count.

use super::registry::{Dependents, Loadable, Package, PackageRow, Spelling, size_label};
use backend_library::{
    PackageReference, RegistryDownloadCount, RegistryEcosystem, RegistrySecurityStanding,
};
use backend_present::Fault;
use backend_present::Language;

/// How many weeks of download history a usage series carries.
#[cfg(test)]
pub(crate) const WEEKS: usize = 26;

/// How many trailing weeks the "recent" download figure covers (90 days).
#[cfg(test)]
const RECENT_WEEKS: usize = 13;

/// Days in one week, for stepping a weekly series back in time.
#[cfg(test)]
const WEEK: i64 = 7;

/// The day the newest sample week ends on: a Monday, so every week aligns.
#[cfg(test)]
const ANCHOR: i64 = days_from_civil(2026, 9, 14);

// ------------------------------------------------------------ provenance --

/// Where one section of a dossier came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Provenance {
    /// The engine answered, and the section holds exactly what it said.
    Recorded,
    /// The configured feed does not publish this section.
    NotRecorded,
    /// The engine published nothing here, so a labelled sample stands in.
    #[cfg(test)]
    Sample(Stand),
}

/// Why a sample is standing in for a recorded fact.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Stand {
    /// No surface command publishes this fact yet.
    Unpublished,
    /// A command exists, and the local index answered nothing under this name.
    Silent,
}

impl Provenance {
    /// Returns the tag a section head draws, when the section is a sample.
    pub(crate) const fn tag(self) -> Option<&'static str> {
        match self {
            Self::Recorded | Self::NotRecorded => None,
            #[cfg(test)]
            Self::Sample(_) => Some("sample"),
        }
    }

    /// Returns the sentence that explains a sample in the feed's own terms.
    pub(crate) const fn sentence(self) -> Option<&'static str> {
        match self {
            Self::Recorded => None,
            Self::NotRecorded => Some("This registry feed does not publish this information."),
            #[cfg(test)]
            Self::Sample(Stand::Unpublished) => {
                Some("No surface command publishes this fact yet — these are sample values.")
            }
            #[cfg(test)]
            Self::Sample(Stand::Silent) => {
                Some("The local index recorded nothing here — these are sample values.")
            }
        }
    }

    /// Returns whether the section is a stand-in rather than a fact.
    #[cfg(test)]
    pub(crate) const fn is_sample(self) -> bool {
        matches!(self, Self::Sample(_))
    }

    /// Returns whether the section is a stand-in rather than a fact.
    #[cfg(not(test))]
    pub(crate) const fn is_sample(self) -> bool {
        false
    }

    /// Returns whether the section is a truthful absence from the feed.
    pub(crate) const fn is_not_recorded(self) -> bool {
        matches!(self, Self::NotRecorded)
    }
}

/// One dossier section: its request state, and where that state came from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Section<T> {
    state: Loadable<T>,
    provenance: Provenance,
}

impl<T> Section<T> {
    /// Returns a section holding exactly what the engine answered.
    pub(crate) const fn recorded(state: Loadable<T>) -> Self {
        Self {
            state,
            provenance: Provenance::Recorded,
        }
    }

    /// Returns a section holding a sample, because nothing publishes the fact.
    #[cfg(test)]
    pub(crate) const fn unpublished(value: T) -> Self {
        Self {
            state: Loadable::Ready(value),
            provenance: Provenance::Sample(Stand::Unpublished),
        }
    }

    /// Returns a section whose source has no field for this fact.
    pub(crate) const fn not_recorded(value: T) -> Self {
        Self {
            state: Loadable::Ready(value),
            provenance: Provenance::NotRecorded,
        }
    }

    /// Returns a section holding a sample, because the feed answered nothing.
    #[cfg(test)]
    pub(crate) const fn silent(value: T) -> Self {
        Self {
            state: Loadable::Ready(value),
            provenance: Provenance::Sample(Stand::Silent),
        }
    }

    /// Returns the request state this section is in.
    pub(crate) const fn state(&self) -> &Loadable<T> {
        &self.state
    }

    /// Returns where this section's contents came from.
    pub(crate) const fn provenance(&self) -> Provenance {
        self.provenance
    }

    /// Returns the answer, when one arrived.
    pub(crate) const fn ready(&self) -> Option<&T> {
        self.state.ready()
    }
}

// ----------------------------------------------------------------- dates --

/// A calendar day, as a date a reader can read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Stamp {
    year: i64,
    month: u8,
    day: u8,
}

/// The three-letter month names, in order.
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

impl Stamp {
    /// Returns the calendar day this many days after 1970-01-01.
    pub(crate) fn of_day(day: i64) -> Self {
        let (year, month, day) = civil_from_days(day);
        Self { year, month, day }
    }

    /// Returns the ISO spelling a dense version row draws.
    pub(crate) fn iso(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// Returns this day as a count of days since 1970-01-01.
    pub(crate) fn day(self) -> i64 {
        days_from_civil(self.year, i64::from(self.month), i64::from(self.day))
    }

    /// Returns the spelled form a page header draws.
    pub(crate) fn spelled(self) -> String {
        let at = usize::from(self.month.saturating_sub(1));
        let name = MONTHS.get(at).copied().unwrap_or("Jan");
        format!("{} {name} {}", self.day, self.year)
    }
}

/// Returns the calendar day of a count of days since 1970-01-01.
///
/// This is Hinnant's `civil_from_days`: exact for every day in the proleptic
/// Gregorian calendar, with no table and no leap-year branch.
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

/// Narrows a calendar component that is known to be small.
fn narrow(value: i64) -> u8 {
    u8::try_from(value).unwrap_or(1)
}

/// Returns the count of days from 1970-01-01 to one calendar day.
const fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month = if month > 2 { month - 3 } else { month + 9 };
    let of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + of_year;
    era * 146_097 + of_era - 719_468
}

// ----------------------------------------------------------- the sections --

/// Which page one external link opens.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LinkKind {
    /// The source repository.
    Repository,
    /// The project's own home page.
    Homepage,
    /// The published API documentation.
    Documentation,
}

impl LinkKind {
    /// Returns the words the link button shows.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Repository => "Repository",
            Self::Homepage => "Homepage",
            Self::Documentation => "Documentation",
        }
    }
}

/// One external address a package publishes about itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Link {
    kind: LinkKind,
    url: String,
}

impl Link {
    /// Returns which page this link opens.
    pub(crate) const fn kind(&self) -> LinkKind {
        self.kind
    }

    /// Returns the exact address.
    pub(crate) fn url(&self) -> &str {
        &self.url
    }
}

/// The one-paragraph identity of a package: what it is, and where it lives.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Precis {
    description: String,
    keywords: Vec<String>,
    license: Option<String>,
    links: Vec<Link>,
}

impl Precis {
    /// Returns the one-line description.
    pub(crate) fn description(&self) -> &str {
        &self.description
    }

    /// Returns the publisher's own keywords.
    pub(crate) fn keywords(&self) -> &[String] {
        &self.keywords
    }

    /// Returns the SPDX licence expression, when one is published.
    pub(crate) fn license(&self) -> Option<&str> {
        self.license.as_deref()
    }

    /// Returns every external address, in reading order.
    pub(crate) fn links(&self) -> &[Link] {
        &self.links
    }
}

/// Whether a release is still offered.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Standing {
    /// The release is offered.
    Published,
    /// The publisher withdrew this release.
    Yanked,
    /// The publisher recommends replacing this release.
    Deprecated,
    /// The release is addressable but omitted from normal listings.
    Unlisted,
    /// The ecosystem policy retracts this release.
    Retracted,
    /// The release disappeared from the upstream feed.
    Removed,
}

impl Standing {
    /// Returns the short status shown beside a release.
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

/// One release of a package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Release {
    coordinate: String,
    version: String,
    bytes: u64,
    published: Option<Stamp>,
    standing: Standing,
    security: RegistrySecurityStanding,
}

impl Release {
    /// Returns the pinned coordinate this release opens.
    pub(crate) fn coordinate(&self) -> &str {
        &self.coordinate
    }

    /// Returns the immutable version.
    pub(crate) fn version(&self) -> &str {
        &self.version
    }

    /// Returns the short human size of the verified archive.
    pub(crate) fn size(&self) -> String {
        size_label(self.bytes)
    }

    /// Returns the publication day, when the feed recorded one.
    pub(crate) const fn published(&self) -> Option<Stamp> {
        self.published
    }

    /// Returns whether this release is still offered.
    pub(crate) const fn standing(&self) -> Standing {
        self.standing
    }

    /// Returns the advisory evaluation recorded for this release.
    pub(crate) const fn security(&self) -> RegistrySecurityStanding {
        self.security
    }
}

/// One week of downloads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Tally {
    week: Stamp,
    count: u64,
}

impl Tally {
    /// Returns the Monday the week ends on.
    pub(crate) const fn week(&self) -> Stamp {
        self.week
    }

    /// Returns the downloads counted in that week.
    pub(crate) const fn count(&self) -> u64 {
        self.count
    }
}

/// How often a package is fetched: the series, the lifetime, and the window.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Downloads {
    weekly: Vec<Tally>,
    total: u64,
    recent: u64,
}

impl Downloads {
    /// Returns every week, oldest first, as a chart consumes them.
    pub(crate) fn weekly(&self) -> &[Tally] {
        &self.weekly
    }

    /// Returns just the counts, oldest first, for a sparkline.
    pub(crate) fn counts(&self) -> Vec<u64> {
        self.weekly.iter().map(Tally::count).collect()
    }

    /// Returns the lifetime download count.
    pub(crate) const fn total(&self) -> u64 {
        self.total
    }

    /// Returns the downloads in the trailing ninety days.
    pub(crate) const fn recent(&self) -> u64 {
        self.recent
    }

    /// Returns the first and last week of the series, when it has any.
    pub(crate) fn span(&self) -> Option<(Stamp, Stamp)> {
        Some((self.weekly.first()?.week(), self.weekly.last()?.week()))
    }
}

/// What a dependency is used for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Role {
    /// Required to build and run.
    Required,
    /// Required only when a feature selects it.
    Optional,
    /// Required only to run the package's own tests.
    Development,
}

impl Role {
    /// Returns the chip a dependency row draws, when the role needs one.
    pub(crate) const fn tag(self) -> Option<&'static str> {
        match self {
            Self::Required => None,
            Self::Optional => Some("optional"),
            Self::Development => Some("dev"),
        }
    }
}

/// One declared dependency of a package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Dependency {
    name: String,
    requirement: String,
    ecosystem: RegistryEcosystem,
    role: Role,
    resolved: String,
}

impl Dependency {
    /// Returns the dependency's registry-native name.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Returns the version requirement exactly as it was declared.
    pub(crate) fn requirement(&self) -> &str {
        &self.requirement
    }

    /// Returns the registry this dependency is published in.
    pub(crate) const fn ecosystem(&self) -> RegistryEcosystem {
        self.ecosystem
    }

    /// Returns what the dependency is used for.
    pub(crate) const fn role(&self) -> Role {
        self.role
    }

    /// Returns the pinned coordinate a click on this row opens.
    pub(crate) fn resolved(&self) -> &str {
        &self.resolved
    }
}

/// One package that depends on this one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Dependent {
    coordinate: String,
    name: String,
    version: String,
    ecosystem: RegistryEcosystem,
    downloads: Option<u64>,
}

impl Dependent {
    /// Returns the pinned coordinate this row opens.
    pub(crate) fn coordinate(&self) -> &str {
        &self.coordinate
    }

    /// Returns the registry-native name.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Returns the pinned version.
    pub(crate) fn version(&self) -> &str {
        &self.version
    }

    /// Returns the registry this dependent is published in.
    pub(crate) const fn ecosystem(&self) -> RegistryEcosystem {
        self.ecosystem
    }

    /// Returns the download weight a sample roster is ordered by.
    ///
    /// A recorded dependent has none: the feed publishes the edge, not the
    /// popularity, so a recorded roster keeps the feed's own order instead of
    /// being sorted by a number nobody published.
    pub(crate) const fn downloads(&self) -> Option<u64> {
        self.downloads
    }
}

/// What the feed said about reverse dependencies.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Reverse {
    /// The feed publishes this fact, and these packages depend on it.
    Recorded(Vec<Dependent>),
    /// The feed does not publish this fact, and said why.
    NotRecorded(String),
}

impl Default for Reverse {
    fn default() -> Self {
        Self::Recorded(Vec::new())
    }
}

/// What one publisher does for a package.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Seat {
    /// Holds the publishing rights.
    Publisher,
    /// Reviews and releases.
    Maintainer,
}

impl Seat {
    /// Returns the word an owner row draws.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Publisher => "publisher",
            Self::Maintainer => "maintainer",
        }
    }
}

/// One publisher handle recorded against a package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Owner {
    handle: String,
    seat: Seat,
}

impl Owner {
    /// Returns the registry handle.
    pub(crate) fn handle(&self) -> &str {
        &self.handle
    }

    /// Returns what this owner does.
    pub(crate) const fn seat(&self) -> Seat {
        self.seat
    }
}

/// How many releases the feed recorded, and which one is newest.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct History {
    versions: u64,
    latest: Option<String>,
}

impl History {
    /// Returns how many versions exist under this package name.
    pub(crate) const fn versions(&self) -> u64 {
        self.versions
    }

    /// Returns the newest recorded version, when one exists.
    pub(crate) fn latest(&self) -> Option<&str> {
        self.latest.as_deref()
    }
}

/// The language a code fence in a README is set in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Fence {
    /// A shell transcript: an install line, a command.
    Shell,
    /// Source in one of the languages this product reads.
    Source(Language),
    /// A manifest or configuration excerpt.
    Manifest,
}

impl Fence {
    /// Returns the tag drawn on the code well.
    pub(crate) const fn tag(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::Manifest => "manifest",
            Self::Source(language) => crate::theme::language::label(language),
        }
    }

    /// Returns the language whose hue the well is tinted with, if any.
    pub(crate) const fn language(self) -> Option<Language> {
        match self {
            Self::Shell | Self::Manifest => None,
            Self::Source(language) => Some(language),
        }
    }
}

/// How loud a README heading is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Rank {
    /// The document's own title.
    Title,
    /// A top-level section.
    Section,
    /// A subsection.
    Subsection,
}

/// One block of a rendered README.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ReadmeBlock {
    /// A heading at one rank.
    Heading {
        /// How loud the heading is.
        rank: Rank,
        /// The heading text.
        text: String,
    },
    /// A paragraph of prose.
    Paragraph(String),
    /// A fenced code block.
    Code {
        /// The language the fence named.
        fence: Fence,
        /// The exact source inside the fence.
        text: String,
    },
    /// A bulleted list.
    List(Vec<String>),
}

// ----------------------------------------------------------- the dossier --

/// Everything one package page draws, section by section.
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
    /// Returns the exact coordinate this dossier answers for.
    pub(crate) fn coordinate(&self) -> &str {
        &self.coordinate
    }

    /// Returns the coordinate split into the parts a header draws.
    pub(crate) const fn spelling(&self) -> &Spelling {
        &self.spelling
    }

    /// Returns what the package says it is.
    pub(crate) const fn precis(&self) -> &Section<Precis> {
        &self.precis
    }

    /// Returns every release, newest first.
    pub(crate) const fn releases(&self) -> &Section<Vec<Release>> {
        &self.releases
    }

    /// Returns the recorded version count and the newest version.
    pub(crate) const fn history(&self) -> &Section<History> {
        &self.history
    }

    /// Returns how often the package is fetched.
    pub(crate) const fn downloads(&self) -> &Section<Downloads> {
        &self.downloads
    }

    /// Returns what the package declares it needs.
    pub(crate) const fn dependencies(&self) -> &Section<Vec<Dependency>> {
        &self.dependencies
    }

    /// Returns what depends on the package.
    pub(crate) const fn dependents(&self) -> &Section<Reverse> {
        &self.dependents
    }

    /// Returns who publishes the package.
    pub(crate) const fn owners(&self) -> &Section<Vec<Owner>> {
        &self.owners
    }

    /// Returns the local index request for a package route, when this dossier
    /// carries an admitted package coordinate.
    pub(crate) fn route_request(&self, route: PackageRoute) -> Option<PackageRouteRequest> {
        PackageReference::parse(self.coordinate.clone())
            .ok()
            .map(|package| PackageRouteRequest { package, route })
    }

    /// Returns the README, as blocks.
    pub(crate) const fn readme(&self) -> &Section<Vec<ReadmeBlock>> {
        &self.readme
    }

    /// Returns the release the coordinate pinned, or the newest one.
    pub(crate) fn pinned(&self) -> Option<&Release> {
        let releases = self.releases.ready()?;
        releases
            .iter()
            .find(|release| release.version() == self.spelling.version())
            .or_else(|| releases.first())
    }

    /// Returns the ecosystem the coordinate named, when it named a known one.
    pub(crate) const fn ecosystem(&self) -> Option<RegistryEcosystem> {
        self.spelling.ecosystem()
    }
}

/// An internal package capability supplied by the local registry/semantic
/// index. These routes never derive an upstream URL from a package name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PackageRoute {
    /// Render the package's admitted documentation records.
    Documentation,
    /// Render versioned source records.
    Source,
    /// Render declarations from the semantic index.
    Code,
    /// Search declarations within the package's admitted index.
    Search,
}

impl PackageRoute {
    /// Returns the stable UI key used to unfold this internal route.
    pub(crate) const fn key(self) -> &'static str {
        match self {
            Self::Documentation => "package-docs",
            Self::Source => "package-source",
            Self::Code => "package-code",
            Self::Search => "package-search",
        }
    }

    /// Returns the route's reader-facing title.
    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::Documentation => "Documentation",
            Self::Source => "Source",
            Self::Code => "Code",
            Self::Search => "Search",
        }
    }
}

/// The typed handoff between the package surface and local index/reader
/// providers. A provider consumes the admitted package identity and route;
/// the GUI never manufactures an external docs or source address.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PackageRouteRequest {
    package: PackageReference,
    route: PackageRoute,
}

impl PackageRouteRequest {
    /// Returns the exact package identity a local provider must query.
    pub(crate) const fn package(&self) -> &PackageReference {
        &self.package
    }

    /// Returns the requested internal capability.
    pub(crate) const fn route(&self) -> PackageRoute {
        self.route
    }
}

/// Returns the dossier for one coordinate, given what the live registry read
/// answered. Every section is either backed by the service or explicitly says
/// that the configured feed does not publish that fact.
pub(crate) fn assemble(coordinate: &str, state: &Loadable<Box<Package>>) -> Dossier {
    #[cfg(test)]
    {
        assemble_fixture(coordinate, state)
    }
    #[cfg(not(test))]
    {
        assemble_live(coordinate, state)
    }
}

fn assemble_live(coordinate: &str, state: &Loadable<Box<Package>>) -> Dossier {
    let spelling = Spelling::of(coordinate);
    Dossier {
        coordinate: coordinate.to_owned(),
        spelling,
        precis: Section::not_recorded(Precis::default()),
        releases: releases_section(state),
        history: history_section(state),
        downloads: downloads_section(state),
        dependencies: Section::not_recorded(Vec::new()),
        dependents: reverse_section(state),
        owners: Section::not_recorded(Vec::new()),
        readme: Section::not_recorded(Vec::new()),
    }
}

#[cfg(test)]
fn assemble_fixture(coordinate: &str, state: &Loadable<Box<Package>>) -> Dossier {
    let mut dossier = sample(coordinate);
    let sampled_releases = dossier.releases.ready().cloned().unwrap_or_default();
    let sampled_reverse = dossier.dependents.ready().cloned().unwrap_or_default();
    let sampled_history = dossier.history.ready().cloned().unwrap_or_default();
    dossier.releases = releases_fixture_section(state, sampled_releases);
    dossier.dependents = reverse_fixture_section(state, sampled_reverse);
    dossier.history = history_fixture_section(state, sampled_history);
    dossier
}

/// Maps the one cumulative download observation into the shared usage model.
/// Weekly history is a separate registry capability and remains absent when
/// the feed only publishes a cumulative count.
fn downloads_section(state: &Loadable<Box<Package>>) -> Section<Downloads> {
    match engine(state, Package::record) {
        Reached::Pending => Section::recorded(Loadable::Loading),
        Reached::Faulted(fault) => Section::recorded(Loadable::Faulted(fault)),
        Reached::Answered(rows) => {
            let Some(row) = rows.first() else {
                return Section::not_recorded(Downloads::default());
            };
            let total = match row.downloads() {
                RegistryDownloadCount::Exact(value) | RegistryDownloadCount::Approximate(value) => {
                    *value
                }
                RegistryDownloadCount::NotReported(_) => {
                    return Section::not_recorded(Downloads::default());
                }
            };
            Section::recorded(Loadable::Ready(Downloads {
                weekly: Vec::new(),
                total,
                recent: 0,
            }))
        }
    }
}

/// Returns the versions section, recorded when the index listed any.
fn releases_section(state: &Loadable<Box<Package>>) -> Section<Vec<Release>> {
    match engine(state, Package::versions) {
        Reached::Pending => Section::recorded(Loadable::Loading),
        Reached::Faulted(fault) => Section::recorded(Loadable::Faulted(fault)),
        Reached::Answered(rows) if rows.is_empty() => exact(state).map_or_else(
            || Section::not_recorded(Vec::new()),
            |only| Section::recorded(Loadable::Ready(only)),
        ),
        Reached::Answered(rows) => {
            Section::recorded(Loadable::Ready(rows.iter().map(release_of).collect()))
        }
    }
}

#[cfg(test)]
fn releases_fixture_section(
    state: &Loadable<Box<Package>>,
    sampled: Vec<Release>,
) -> Section<Vec<Release>> {
    match releases_section(state).provenance() {
        Provenance::NotRecorded if !sampled.is_empty() => Section::silent(sampled),
        _ => releases_section(state),
    }
}

/// Returns the one release the exact-package read holds, when it holds any.
///
/// A local index that records a package it fetched but lists no version
/// history still answers `Package` for the pinned coordinate, and one recorded
/// release is worth more than a sampled ladder of nine.
fn exact(state: &Loadable<Box<Package>>) -> Option<Vec<Release>> {
    match engine(state, Package::record) {
        Reached::Answered(rows) if !rows.is_empty() => Some(rows.iter().map(release_of).collect()),
        Reached::Answered(_) | Reached::Pending | Reached::Faulted(_) => None,
    }
}

/// Returns the dependents section, recorded when the feed answered at all.
fn reverse_section(state: &Loadable<Box<Package>>) -> Section<Reverse> {
    match engine(state, Package::dependents) {
        Reached::Pending => Section::recorded(Loadable::Loading),
        Reached::Faulted(fault) => Section::recorded(Loadable::Faulted(fault)),
        Reached::Answered(Dependents::NotRecorded(reason)) => {
            Section::not_recorded(Reverse::NotRecorded(reason.clone()))
        }
        Reached::Answered(Dependents::Recorded(rows)) if rows.is_empty() => {
            Section::not_recorded(Reverse::Recorded(Vec::new()))
        }
        Reached::Answered(Dependents::Recorded(rows)) => Section::recorded(Loadable::Ready(
            Reverse::Recorded(rows.iter().map(dependent_of).collect()),
        )),
    }
}

#[cfg(test)]
fn reverse_fixture_section(state: &Loadable<Box<Package>>, sampled: Reverse) -> Section<Reverse> {
    let section = reverse_section(state);
    match section.state() {
        Loadable::Ready(Reverse::NotRecorded(_)) => Section::recorded(section.state().clone()),
        _ if section.provenance().is_not_recorded() => Section::silent(sampled),
        _ => section,
    }
}

/// Returns the history section, recorded when the profile named a release.
fn history_section(state: &Loadable<Box<Package>>) -> Section<History> {
    match engine(state, Package::profile) {
        Reached::Pending => Section::recorded(Loadable::Loading),
        Reached::Faulted(fault) => Section::recorded(Loadable::Faulted(fault)),
        Reached::Answered(profile) => match profile.latest() {
            None => Section::not_recorded(History::default()),
            Some(latest) => Section::recorded(Loadable::Ready(History {
                versions: profile.versions(),
                latest: Some(latest.version().to_owned()),
            })),
        },
    }
}

#[cfg(test)]
fn history_fixture_section(state: &Loadable<Box<Package>>, sampled: History) -> Section<History> {
    match history_section(state).provenance() {
        Provenance::NotRecorded if sampled.latest().is_some() => Section::silent(sampled),
        _ => history_section(state),
    }
}

/// What one engine-backed section of a gathered package read amounts to.
enum Reached<'a, T> {
    /// The read has not answered yet.
    Pending,
    /// The read failed, and the fault says how.
    Faulted(Box<Fault>),
    /// The read answered exactly this.
    Answered(&'a T),
}

/// Returns one gathered section's state, flattening the outer read into it.
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

/// Returns one recorded release: exact fields, and `None` where the feed is silent.
fn release_of(row: &PackageRow) -> Release {
    Release {
        coordinate: row.coordinate().to_owned(),
        version: row.version().to_owned(),
        bytes: row.bytes(),
        published: None,
        standing: match row.release_standing() {
            backend_library::RegistryReleaseStanding::Available => Standing::Published,
            backend_library::RegistryReleaseStanding::Yanked => Standing::Yanked,
            backend_library::RegistryReleaseStanding::Deprecated => Standing::Deprecated,
            backend_library::RegistryReleaseStanding::Unlisted => Standing::Unlisted,
            backend_library::RegistryReleaseStanding::Retracted => Standing::Retracted,
            backend_library::RegistryReleaseStanding::Removed => Standing::Removed,
        },
        security: row.security(),
    }
}

/// Returns one recorded dependent, with no download weight invented for it.
fn dependent_of(row: &PackageRow) -> Dependent {
    Dependent {
        coordinate: row.coordinate().to_owned(),
        name: row.name().to_owned(),
        version: row.version().to_owned(),
        ecosystem: row.ecosystem(),
        downloads: None,
    }
}

// ---------------------------------------------------------- browse cards --

/// The facts a browse card draws beside the recorded ones.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Card {
    blurb: String,
    keywords: Vec<String>,
    license: Option<String>,
    released: Option<Stamp>,
    downloads: DownloadFact,
    #[cfg(test)]
    sampled: bool,
}

/// The closed set of download coverage a registry card can carry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum DownloadFact {
    /// The registry published this cumulative/series value, including zero.
    Reported(Downloads),
    /// The registry did not publish download telemetry for this row.
    NotReported,
}

impl DownloadFact {
    /// Returns the lifetime count only when the feed reported one.
    pub(crate) const fn total(&self) -> Option<u64> {
        match self {
            Self::Reported(downloads) => Some(downloads.total()),
            Self::NotReported => None,
        }
    }

    /// Returns the recorded series, empty when the feed has no telemetry.
    pub(crate) fn counts(&self) -> Vec<u64> {
        match self {
            Self::Reported(downloads) => downloads.counts(),
            Self::NotReported => Vec::new(),
        }
    }
}

#[cfg(test)]
mod download_fact_tests {
    use super::{DownloadFact, Downloads};

    #[test]
    fn zero_downloads_stay_distinct_from_unreported_telemetry() {
        assert_eq!(
            DownloadFact::Reported(Downloads::default()).total(),
            Some(0)
        );
        assert_eq!(DownloadFact::NotReported.total(), None);
    }
}

#[cfg(test)]
mod route_request_tests {
    use super::{PackageRoute, sample};

    #[test]
    fn internal_route_handoff_keeps_the_pinned_package_identity() {
        let dossier = sample("pkg:cargo/serde@1.0.0");
        let request = dossier
            .route_request(PackageRoute::Source)
            .expect("the pinned package is an admitted route identity");

        assert_eq!(request.package().as_str(), "pkg:cargo/serde@1.0.0");
        assert_eq!(request.route(), PackageRoute::Source);
    }
}

impl Card {
    /// Returns the one-line description.
    pub(crate) fn blurb(&self) -> &str {
        &self.blurb
    }

    /// Returns at most three keywords, which is what a card has room for.
    pub(crate) fn keywords(&self) -> &[String] {
        self.keywords.get(..3).unwrap_or(&self.keywords)
    }

    /// Returns the licence expression, when the feed publishes one.
    pub(crate) fn license(&self) -> Option<&str> {
        self.license.as_deref()
    }

    /// Returns the day the pinned release was published.
    pub(crate) const fn released(&self) -> Option<Stamp> {
        self.released
    }

    /// Returns the download series and totals.
    pub(crate) const fn downloads(&self) -> &DownloadFact {
        &self.downloads
    }

    /// Returns whether this card comes from a test/preview fixture.
    #[cfg(test)]
    pub(crate) const fn is_sampled(&self) -> bool {
        self.sampled
    }

    /// Returns whether this card comes from a test/preview fixture.
    #[cfg(not(test))]
    pub(crate) const fn is_sampled(&self) -> bool {
        false
    }

    /// Returns the lifetime downloads a grid sorts by.
    pub(crate) const fn weight(&self) -> Option<u64> {
        self.downloads.total()
    }

    /// Returns the day a grid sorts by, as days since the epoch.
    pub(crate) fn freshness(&self) -> i64 {
        self.released.map_or(i64::MIN, Stamp::day)
    }
}

/// Returns the sample card one browse row draws.
///
/// The card is a projection of the same sample dossier the package page
/// assembles, so the licence, the series, and the date on a card are the exact
/// values the page it opens will state. A card built from its own sequence
/// would drift from the page, and a reader who noticed would be right to stop
/// trusting both.
#[cfg(test)]
pub(crate) fn card(coordinate: &str) -> Card {
    let dossier = sample(coordinate);
    let precis = dossier.precis().ready();
    Card {
        blurb: precis
            .map(|precis| precis.description().to_owned())
            .unwrap_or_default(),
        keywords: precis
            .map(|precis| precis.keywords().to_vec())
            .unwrap_or_default(),
        license: precis
            .and_then(|precis| precis.license())
            .map(str::to_owned),
        released: dossier.pinned().and_then(Release::published),
        downloads: DownloadFact::Reported(dossier.downloads().ready().cloned().unwrap_or_default()),
        sampled: true,
    }
}

/// Projects only the facts a live registry row actually publishes.
pub(crate) fn live_card(row: &PackageRow) -> Card {
    let downloads = match row.downloads() {
        RegistryDownloadCount::Exact(value) | RegistryDownloadCount::Approximate(value) => {
            DownloadFact::Reported(Downloads {
                weekly: Vec::new(),
                total: *value,
                recent: 0,
            })
        }
        RegistryDownloadCount::NotReported(_) => DownloadFact::NotReported,
    };
    Card {
        blurb: String::new(),
        keywords: Vec::new(),
        license: None,
        released: None,
        downloads,
        #[cfg(test)]
        sampled: false,
    }
}

/// Returns a short human count: `934`, `12.4K`, `3.1M`.
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

// -------------------------------------------------- test-only fixtures --

/// Returns the whole sample dossier one coordinate implies.
#[cfg(test)]
pub(crate) fn sample(coordinate: &str) -> Dossier {
    let spelling = Spelling::of(coordinate);
    let ecosystem = spelling.ecosystem().unwrap_or(RegistryEcosystem::Cargo);
    let mut draw = Draw::of(spelling.name());
    let mut shape = Draw::shaping(spelling.name());
    let keywords = keywords(&mut draw);
    let topic = keywords.first().cloned();
    let precis = Precis {
        description: blurb(&mut draw, topic.as_deref()),
        keywords,
        license: license(&mut draw, &mut shape),
        links: links(&mut draw, ecosystem, spelling.name()),
    };
    let releases = releases(
        &mut draw,
        &mut shape,
        spelling.name(),
        spelling.version(),
        ecosystem,
    );
    Dossier {
        coordinate: coordinate.to_owned(),
        history: Section::unpublished(history_of(&releases)),
        readme: Section::unpublished(readme(&mut shape, ecosystem, &spelling, &precis)),
        precis: Section::unpublished(precis),
        releases: Section::unpublished(releases),
        downloads: Section::unpublished(downloads(&mut draw)),
        dependencies: Section::unpublished(dependencies(&mut draw, &mut shape, ecosystem)),
        dependents: Section::unpublished(reverse(&mut draw, &mut shape, ecosystem)),
        owners: Section::unpublished(owners(&mut draw, spelling.name())),
        spelling,
    }
}

/// A deterministic sequence of choices drawn from one package name.
///
/// This is a mixing function, not a random number generator: one name yields
/// one sequence in every process and every test run, which is what makes a
/// sample dossier a fixture rather than noise.
#[cfg(test)]
struct Draw(u64);

/// The FNV-1a offset basis.
#[cfg(test)]
const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// The FNV-1a prime.
#[cfg(test)]
const PRIME: u64 = 0x0000_0100_0000_01b3;
/// The golden-ratio constant that advances the sequence.
#[cfg(test)]
const GOLDEN: u64 = 0x9e37_79b9_7f4a_7c15;

#[cfg(test)]
impl Draw {
    /// Seeds the sequence that decides which facts a sample omits.
    ///
    /// Absences are drawn from their own seed so that adding a field to the
    /// model cannot silently change which packages ship a README: the shape of
    /// a sample is a property of the package name, not of the order this
    /// module happens to build its sections in.
    fn shaping(text: &str) -> Self {
        Self::of(&format!("{text}\u{1}shape"))
    }

    /// Seeds the sequence from one package name.
    fn of(text: &str) -> Self {
        Self(text.bytes().fold(OFFSET, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(PRIME)
        }))
    }

    /// Returns the next value below `bound`, advancing the sequence.
    fn below(&mut self, bound: u64) -> u64 {
        self.0 = self.0.wrapping_mul(PRIME) ^ GOLDEN;
        if bound == 0 {
            return 0;
        }
        (self.0 >> 11) % bound
    }

    /// Returns the next value in an inclusive range.
    fn between(&mut self, low: u64, high: u64) -> u64 {
        low.saturating_add(self.below(high.saturating_sub(low).saturating_add(1)))
    }

    /// Returns one of the given options.
    fn pick<'a>(&mut self, options: &[&'a str]) -> Option<&'a str> {
        let bound = u64::try_from(options.len()).unwrap_or(1);
        let at = usize::try_from(self.below(bound)).unwrap_or(0);
        options.get(at).copied()
    }
}

/// The subject vocabulary a sample description draws from.
#[cfg(test)]
const TOPICS: [&str; 14] = [
    "serialization",
    "async runtime",
    "http client",
    "parser",
    "logging",
    "command line",
    "testing",
    "compression",
    "date and time",
    "templating",
    "validation",
    "cryptography",
    "graph",
    "caching",
];

/// The adjectival keywords a sample adds beside its subject.
#[cfg(test)]
const TAGS: [&str; 12] = [
    "no-std",
    "zero-copy",
    "derive",
    "async",
    "typed",
    "streaming",
    "wasm",
    "tracing",
    "fast",
    "minimal",
    "batteries-included",
    "macros",
];

/// The licence expressions a sample publishes.
#[cfg(test)]
const LICENSES: [&str; 6] = [
    "MIT OR Apache-2.0",
    "MIT",
    "Apache-2.0",
    "BSD-3-Clause",
    "MPL-2.0",
    "ISC",
];

/// Returns the subject keyword and up to three tags beside it.
#[cfg(test)]
fn keywords(draw: &mut Draw) -> Vec<String> {
    let mut all = vec![draw.pick(&TOPICS).unwrap_or("utilities").to_owned()];
    for _ in 0..draw.below(4) {
        if let Some(tag) = draw.pick(&TAGS)
            && !all.iter().any(|held| held == tag)
        {
            all.push(tag.to_owned());
        }
    }
    all
}

/// Returns the one-line description a card and a header draw.
#[cfg(test)]
fn blurb(draw: &mut Draw, topic: Option<&str>) -> String {
    let topic = topic.unwrap_or("general purpose");
    match draw.below(4) {
        0 => format!("A small, dependency-light {topic} library."),
        1 => format!("Ergonomic {topic} primitives with a stable public API."),
        2 => format!("The {topic} layer, with no macros and no global state."),
        _ => format!("A batteries-included {topic} toolkit."),
    }
}

/// Returns the licence, or nothing for the packages that publish none.
#[cfg(test)]
fn license(draw: &mut Draw, shape: &mut Draw) -> Option<String> {
    if shape.below(9) == 0 {
        return None;
    }
    draw.pick(&LICENSES).map(str::to_owned)
}

/// Returns the external addresses a sample publishes.
#[cfg(test)]
fn links(draw: &mut Draw, ecosystem: RegistryEcosystem, name: &str) -> Vec<Link> {
    let slug = name.trim_start_matches('@').replace('/', "-");
    let mut links = vec![Link {
        kind: LinkKind::Repository,
        url: format!("https://github.com/{slug}/{slug}"),
    }];
    if draw.below(3) != 0 {
        links.push(Link {
            kind: LinkKind::Homepage,
            url: format!("https://{slug}.dev"),
        });
    }
    if draw.below(2) == 0 {
        links.push(Link {
            kind: LinkKind::Documentation,
            url: documentation(ecosystem, name),
        });
    }
    links
}

/// Returns the documentation address one registry publishes packages at.
#[cfg(test)]
fn documentation(ecosystem: RegistryEcosystem, name: &str) -> String {
    match ecosystem {
        RegistryEcosystem::Cargo => format!("https://docs.rs/{name}"),
        RegistryEcosystem::Npm => format!("https://www.npmjs.com/package/{name}"),
        RegistryEcosystem::Pypi => format!("https://pypi.org/project/{name}/"),
        RegistryEcosystem::Maven => format!("https://javadoc.io/doc/{name}"),
        RegistryEcosystem::Nuget => format!("https://www.nuget.org/packages/{name}"),
        RegistryEcosystem::Golang => format!("https://pkg.go.dev/{name}"),
        RegistryEcosystem::Cpp => format!("https://conan.io/center/recipes/{name}"),
    }
}

/// Returns the sample release history, newest first.
///
/// The coordinate's own version is always the newest entry and the ladder is
/// stepped *down* from that version's own numbers, so the picker never lists a
/// higher version below the one the coordinate pinned. A version with no
/// numeric prefix — a date stamp, a hash — cannot be stepped, so the ladder
/// starts from drawn numbers instead and the pinned spelling stays on top.
#[cfg(test)]
fn releases(
    draw: &mut Draw,
    shape: &mut Draw,
    name: &str,
    pinned: &str,
    ecosystem: RegistryEcosystem,
) -> Vec<Release> {
    let count = draw.between(3, 9);
    let withdrawn = if shape.below(5) == 0 {
        Some(shape.below(count))
    } else {
        None
    };
    let mut numbers = numbers_of(pinned)
        .unwrap_or_else(|| (draw.between(0, 4), draw.between(0, 14), draw.between(0, 9)));
    let mut day = ANCHOR.saturating_sub(i64::try_from(draw.between(3, 120)).unwrap_or(30));
    let mut bytes = draw.between(9_000, 4_000_000);
    let mut out = Vec::new();
    for at in 0..count {
        let version = match (at, pinned.is_empty()) {
            (0, false) => pinned.to_owned(),
            _ => format!("{}.{}.{}", numbers.0, numbers.1, numbers.2),
        };
        out.push(Release {
            coordinate: format!("pkg:{}/{name}@{version}", ecosystem.as_str()),
            version,
            bytes,
            published: Some(Stamp::of_day(day)),
            standing: if withdrawn == Some(at) {
                Standing::Yanked
            } else {
                Standing::Published
            },
            security: RegistrySecurityStanding::Unassessed,
        });
        numbers = step_down(numbers, draw);
        day = day.saturating_sub(i64::try_from(draw.between(9, 160)).unwrap_or(30));
        bytes = bytes.saturating_sub(bytes / 11).max(1_024);
    }
    out
}

/// Returns the three version numbers one spelling opens with, when it has them.
#[cfg(test)]
fn numbers_of(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version
        .trim_start_matches('v')
        .split(['.', '-', '+'])
        .map_while(|part| part.parse::<u64>().ok());
    let major = parts.next()?;
    Some((major, parts.next().unwrap_or(0), parts.next().unwrap_or(0)))
}

/// Returns the version numbers of the release before this one.
#[cfg(test)]
fn step_down(numbers: (u64, u64, u64), draw: &mut Draw) -> (u64, u64, u64) {
    let (major, minor, patch) = numbers;
    if patch > 0 {
        return (major, minor, patch.saturating_sub(1));
    }
    if minor > 0 {
        return (major, minor.saturating_sub(1), draw.between(0, 6));
    }
    if major > 0 {
        return (
            major.saturating_sub(1),
            draw.between(1, 12),
            draw.between(0, 6),
        );
    }
    (0, 0, 0)
}

/// Returns the version count and newest version of a sample history.
#[cfg(test)]
fn history_of(releases: &[Release]) -> History {
    History {
        versions: u64::try_from(releases.len()).unwrap_or(0),
        latest: releases.first().map(|release| release.version().to_owned()),
    }
}

/// Returns a sample weekly download series with a lifetime and a window.
///
/// One in three packages is drawn rising, one flat, one falling, because a
/// usage chart that only ever slopes up teaches a reader to ignore it.
#[cfg(test)]
fn downloads(draw: &mut Draw) -> Downloads {
    let base = draw.between(400, 900_000);
    let trend = draw.below(3);
    let mut weekly = Vec::with_capacity(WEEKS);
    for week in 0..WEEKS {
        let index = u64::try_from(week).unwrap_or(0);
        let shape = match trend {
            0 => 55_u64.saturating_add(index.saturating_mul(4)),
            2 => 160_u64.saturating_sub(index.saturating_mul(3)),
            _ => 100,
        };
        let wobble = draw.between(88, 112);
        let count = base
            .saturating_mul(shape)
            .saturating_mul(wobble)
            .saturating_div(10_000)
            .max(1);
        let back = i64::try_from(WEEKS.saturating_sub(week).saturating_sub(1)).unwrap_or(0);
        weekly.push(Tally {
            week: Stamp::of_day(ANCHOR.saturating_sub(back.saturating_mul(WEEK))),
            count,
        });
    }
    let lifetime = weekly.iter().map(Tally::count).sum::<u64>();
    Downloads {
        recent: weekly
            .iter()
            .rev()
            .take(RECENT_WEEKS)
            .map(Tally::count)
            .sum(),
        total: lifetime.saturating_mul(draw.between(4, 30)),
        weekly,
    }
}

/// Returns the package names one registry's sample dependencies are drawn from.
#[cfg(test)]
const fn vocabulary(ecosystem: RegistryEcosystem) -> &'static [&'static str] {
    match ecosystem {
        RegistryEcosystem::Cargo => &[
            "serde",
            "tokio",
            "anyhow",
            "thiserror",
            "clap",
            "tracing",
            "regex",
            "itertools",
            "bytes",
            "rayon",
        ],
        RegistryEcosystem::Npm => &[
            "zod",
            "typescript",
            "vitest",
            "esbuild",
            "chalk",
            "commander",
            "rxjs",
            "date-fns",
            "@types/node",
            "undici",
        ],
        RegistryEcosystem::Pypi => &[
            "requests",
            "attrs",
            "pydantic",
            "click",
            "httpx",
            "numpy",
            "rich",
            "pytest",
            "typing-extensions",
            "packaging",
        ],
        RegistryEcosystem::Maven => &[
            "com.google.guava:guava",
            "org.slf4j:slf4j-api",
            "com.fasterxml.jackson.core:jackson-databind",
            "org.junit.jupiter:junit-jupiter",
            "io.netty:netty-buffer",
        ],
        RegistryEcosystem::Nuget => &[
            "Newtonsoft.Json",
            "Serilog",
            "Polly",
            "AutoMapper",
            "FluentAssertions",
            "Dapper",
        ],
        RegistryEcosystem::Golang => &[
            "github.com/spf13/cobra",
            "golang.org/x/sync",
            "github.com/stretchr/testify",
            "google.golang.org/protobuf",
            "github.com/rs/zerolog",
        ],
        RegistryEcosystem::Cpp => &["fmt", "spdlog", "catch2", "abseil", "zlib", "boost"],
    }
}

/// Returns the sample dependency set, which is empty for some packages.
#[cfg(test)]
fn dependencies(
    draw: &mut Draw,
    shape: &mut Draw,
    ecosystem: RegistryEcosystem,
) -> Vec<Dependency> {
    let names = vocabulary(ecosystem);
    let count = shape.below(9);
    let mut out: Vec<Dependency> = Vec::new();
    for _ in 0..count {
        let Some(name) = draw.pick(names) else {
            continue;
        };
        if out.iter().any(|held| held.name() == name) {
            continue;
        }
        let numbers = (draw.between(0, 6), draw.between(0, 20), draw.between(0, 12));
        out.push(Dependency {
            requirement: requirement(ecosystem, numbers),
            resolved: format!(
                "pkg:{}/{name}@{}.{}.{}",
                ecosystem.as_str(),
                numbers.0,
                numbers.1,
                numbers.2
            ),
            role: match draw.below(6) {
                0 => Role::Optional,
                1 => Role::Development,
                _ => Role::Required,
            },
            name: name.to_owned(),
            ecosystem,
        });
    }
    out
}

/// Returns a version requirement spelled the way one registry spells them.
#[cfg(test)]
fn requirement(ecosystem: RegistryEcosystem, numbers: (u64, u64, u64)) -> String {
    let (major, minor, patch) = numbers;
    match ecosystem {
        RegistryEcosystem::Cargo => format!("^{major}.{minor}"),
        RegistryEcosystem::Npm => format!("^{major}.{minor}.{patch}"),
        RegistryEcosystem::Pypi => format!(">={major}.{minor},<{}", major.saturating_add(1)),
        RegistryEcosystem::Maven | RegistryEcosystem::Nuget => {
            format!("[{major}.{minor}.{patch},)")
        }
        RegistryEcosystem::Golang => format!("v{major}.{minor}.{patch}"),
        RegistryEcosystem::Cpp => format!("{major}.{minor}.{patch}"),
    }
}

/// Returns the sample reverse dependencies, ordered by their download weight.
///
/// One registry in eight publishes no reverse index at all, and some packages
/// simply have no dependents; both are states the page must be able to draw.
#[cfg(test)]
fn reverse(draw: &mut Draw, shape: &mut Draw, ecosystem: RegistryEcosystem) -> Reverse {
    if shape.below(8) == 0 {
        return Reverse::NotRecorded(
            "the configured feed publishes no reverse dependency index".to_owned(),
        );
    }
    let names = vocabulary(ecosystem);
    let count = shape.below(15);
    let mut rows: Vec<Dependent> = Vec::new();
    for _ in 0..count {
        let Some(name) = draw.pick(names) else {
            continue;
        };
        if rows.iter().any(|held| held.name() == name) {
            continue;
        }
        let version = format!(
            "{}.{}.{}",
            draw.between(0, 8),
            draw.between(0, 20),
            draw.below(9)
        );
        rows.push(Dependent {
            coordinate: format!("pkg:{}/{name}@{version}", ecosystem.as_str()),
            name: name.to_owned(),
            version,
            ecosystem,
            downloads: Some(draw.between(120, 9_000_000)),
        });
    }
    rows.sort_by_key(|row| std::cmp::Reverse(row.downloads().unwrap_or(0)));
    Reverse::Recorded(rows)
}

/// The synthetic publisher handles a sample owner roster draws from.
#[cfg(test)]
const HANDLES: [&str; 8] = [
    "ferris", "octo", "quill", "vellum", "atlas", "harbor", "lumen", "cinder",
];

/// Returns the sample owner roster: a publisher, and sometimes maintainers.
#[cfg(test)]
fn owners(draw: &mut Draw, name: &str) -> Vec<Owner> {
    let slug = name.trim_start_matches('@').replace('/', "-");
    let mut out = vec![Owner {
        handle: format!("{slug}-team"),
        seat: Seat::Publisher,
    }];
    for _ in 0..draw.below(3) {
        if let Some(handle) = draw.pick(&HANDLES)
            && !out.iter().any(|held| held.handle() == handle)
        {
            out.push(Owner {
                handle: handle.to_owned(),
                seat: Seat::Maintainer,
            });
        }
    }
    out
}

/// Returns the language a registry's packages are written in.
///
/// This mirrors the hue mapping the views use for an ecosystem tag; the store
/// cannot reach the view layer, and the mapping is one closed match either way.
#[cfg(test)]
const fn language_of(ecosystem: RegistryEcosystem) -> Language {
    match ecosystem {
        RegistryEcosystem::Cargo => Language::Rust,
        RegistryEcosystem::Npm => Language::TypeScript,
        RegistryEcosystem::Pypi => Language::Python,
        RegistryEcosystem::Maven => Language::Java,
        RegistryEcosystem::Nuget => Language::CSharp,
        RegistryEcosystem::Golang => Language::Go,
        RegistryEcosystem::Cpp => Language::Cxx,
    }
}

/// Returns the sample README, which one package in seven does not have.
#[cfg(test)]
fn readme(
    shape: &mut Draw,
    ecosystem: RegistryEcosystem,
    spelling: &Spelling,
    precis: &Precis,
) -> Vec<ReadmeBlock> {
    if shape.below(7) == 0 {
        return Vec::new();
    }
    let name = spelling.name();
    let mut blocks = vec![
        ReadmeBlock::Heading {
            rank: Rank::Title,
            text: name.to_owned(),
        },
        ReadmeBlock::Paragraph(format!(
            "{} It is built for reading: every public item carries documentation, and the \
             examples below are compiled as part of the test suite.",
            precis.description()
        )),
        ReadmeBlock::Heading {
            rank: Rank::Section,
            text: "Install".to_owned(),
        },
        ReadmeBlock::Code {
            fence: Fence::Shell,
            text: install_line(ecosystem, name),
        },
        ReadmeBlock::Heading {
            rank: Rank::Section,
            text: "Usage".to_owned(),
        },
        ReadmeBlock::Code {
            fence: Fence::Source(language_of(ecosystem)),
            text: usage_snippet(ecosystem, name),
        },
    ];
    blocks.extend(readme_tail(shape, ecosystem, precis));
    blocks
}

/// Returns the closing blocks of a sample README: features, notes, licence.
#[cfg(test)]
fn readme_tail(draw: &mut Draw, ecosystem: RegistryEcosystem, precis: &Precis) -> Vec<ReadmeBlock> {
    let mut blocks = vec![
        ReadmeBlock::Heading {
            rank: Rank::Subsection,
            text: "What you get".to_owned(),
        },
        ReadmeBlock::List(
            precis
                .keywords()
                .iter()
                .map(|keyword| format!("{keyword}, without a build script"))
                .chain(std::iter::once(
                    "one dependency-free core, tested on every supported release".to_owned(),
                ))
                .collect(),
        ),
    ];
    if draw.below(2) == 0 {
        blocks.push(ReadmeBlock::Heading {
            rank: Rank::Section,
            text: "Configuration".to_owned(),
        });
        blocks.push(ReadmeBlock::Code {
            fence: Fence::Manifest,
            text: manifest_snippet(ecosystem).to_owned(),
        });
    }
    blocks.push(ReadmeBlock::Paragraph(format!(
        "Licensed under {}.",
        precis
            .license()
            .unwrap_or("terms the publisher did not state")
    )));
    blocks
}

/// Returns the one line that installs a package from one registry.
#[cfg(test)]
fn install_line(ecosystem: RegistryEcosystem, name: &str) -> String {
    match ecosystem {
        RegistryEcosystem::Cargo => format!("cargo add {name}"),
        RegistryEcosystem::Npm => format!("npm install {name}"),
        RegistryEcosystem::Pypi => format!("pip install {name}"),
        RegistryEcosystem::Maven => format!("mvn dependency:get -Dartifact={name}:LATEST"),
        RegistryEcosystem::Nuget => format!("dotnet add package {name}"),
        RegistryEcosystem::Golang => format!("go get {name}@latest"),
        RegistryEcosystem::Cpp => format!("conan install --requires={name}/latest"),
    }
}

/// Returns a short usage snippet in one registry's own language.
#[cfg(test)]
fn usage_snippet(ecosystem: RegistryEcosystem, name: &str) -> String {
    let symbol = name
        .rsplit(['/', ':', '.'])
        .next()
        .unwrap_or(name)
        .replace('-', "_");
    match ecosystem {
        RegistryEcosystem::Cargo => {
            format!(
                "use {symbol}::Reader;\n\nlet reader = Reader::open(\"input\")?;\nfor item in reader {{\n    println!(\"{{item}}\");\n}}"
            )
        }
        RegistryEcosystem::Npm => {
            format!(
                "import {{ read }} from \"{name}\";\n\nconst items = await read(\"input\");\nconsole.log(items.length);"
            )
        }
        RegistryEcosystem::Pypi => {
            format!("from {symbol} import read\n\nitems = read(\"input\")\nprint(len(items))")
        }
        RegistryEcosystem::Maven | RegistryEcosystem::Nuget => {
            format!("var reader = new {symbol}Reader();\nvar items = reader.Read(\"input\");")
        }
        RegistryEcosystem::Golang => {
            format!("items, err := {symbol}.Read(\"input\")\nif err != nil {{\n    return err\n}}")
        }
        RegistryEcosystem::Cpp => {
            format!("#include <{symbol}/reader.hpp>\n\nauto items = {symbol}::read(\"input\");")
        }
    }
}

/// Returns a manifest excerpt in one registry's own configuration format.
#[cfg(test)]
const fn manifest_snippet(ecosystem: RegistryEcosystem) -> &'static str {
    match ecosystem {
        RegistryEcosystem::Cargo => "[features]\ndefault = [\"std\"]\nstd = []",
        RegistryEcosystem::Npm => "{\n  \"type\": \"module\",\n  \"sideEffects\": false\n}",
        RegistryEcosystem::Pypi => "[tool.package]\nstrict = true\ncache = \".cache\"",
        RegistryEcosystem::Maven => "<configuration>\n  <strict>true</strict>\n</configuration>",
        RegistryEcosystem::Nuget => {
            "<PropertyGroup>\n  <Nullable>enable</Nullable>\n</PropertyGroup>"
        }
        RegistryEcosystem::Golang => "go 1.23\n\nrequire (\n    // see go.sum\n)",
        RegistryEcosystem::Cpp => "set(CMAKE_CXX_STANDARD 20)",
    }
}
