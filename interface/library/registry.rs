//! Defines registry behavior for `interface-library`, whose purpose is to own the one shared local library every surface reads, adds to, and searches.
//! This module owns the registry invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! The package-exploration contract: cards, profiles, owners, and dependents as every surface shows them.
//!
//! Nothing here fetches. These are the values the registry service answers with and every
//! surface renders; a fact a registry cannot supply is a typed absence (`None`, an empty slice, a
//! [`RegistryError::Unsupported`] row), never a plausible default. Every page carries its
//! [`Provenance`] so a reader always knows whether a number came from the network a moment ago or
//! from the cache an hour ago.

use core::{fmt, num::NonZeroU32};

use interface_core::PackageEcosystem;
use interface_documents::{Count, Text};
use interface_identity::{PackageCoordinate, PackageName, PackageVersion, ecosystem_tag};

use crate::{ExploreLimit, ExplorePackageName, ExploreQuery, Timestamp};

/// Longest URL any registry record retains.
pub const MAX_URL_BYTES: usize = 2048;
/// Longest owner handle any registry record retains.
pub const MAX_OWNER_HANDLE_BYTES: usize = 128;
/// Highest page a surface may ask for; beyond it a registry's own paging has long stopped.
pub const MAX_PAGE_NUMBER: u32 = 500;
/// Most cards one explore page carries.
pub const MAX_EXPLORE_CARDS: usize = 100;

/// One-based page ordinal, bounded so a runaway pager cannot spin a registry.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PageNumber(NonZeroU32);

impl PageNumber {
    /// The first page.
    pub const FIRST: Self = Self(NonZeroU32::MIN);

    /// Admits one page ordinal, clamping into `1..=MAX_PAGE_NUMBER`.
    #[must_use]
    pub const fn clamped(value: u32) -> Self {
        let value = if value == 0 {
            1
        } else if value > MAX_PAGE_NUMBER {
            MAX_PAGE_NUMBER
        } else {
            value
        };
        match NonZeroU32::new(value) {
            Some(page) => Self(page),
            None => Self::FIRST,
        }
    }

    /// The ordinal.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }

    /// The page after this one, saturating at the bound.
    #[must_use]
    pub const fn next(self) -> Self {
        Self::clamped(self.0.get().saturating_add(1))
    }

    /// The page before this one, saturating at the first.
    #[must_use]
    pub const fn previous(self) -> Self {
        Self::clamped(self.0.get().saturating_sub(1))
    }
}

impl Default for PageNumber {
    fn default() -> Self {
        Self::FIRST
    }
}

/// A validated `http(s)` URL a registry handed back.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Url(Box<str>);

/// Exact URL admission failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UrlError {
    /// The text did not start with `http://` or `https://`.
    Scheme,
    /// The text exceeds the fixed budget.
    TooLong {
        /// Observed bytes.
        observed: usize,
        /// Accepted bytes.
        maximum: usize,
    },
    /// The text carries whitespace or a control byte.
    Character,
}

impl Url {
    /// Admits one absolute web URL.
    ///
    /// # Errors
    ///
    /// Rejects a non-web scheme, an oversized spelling, or embedded whitespace.
    pub fn new(text: &str) -> Result<Self, UrlError> {
        let trimmed = text.trim();
        if !(trimmed.starts_with("https://") || trimmed.starts_with("http://")) {
            return Err(UrlError::Scheme);
        }
        if trimmed.len() > MAX_URL_BYTES {
            return Err(UrlError::TooLong {
                observed: trimmed.len(),
                maximum: MAX_URL_BYTES,
            });
        }
        if trimmed.bytes().any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control()) {
            return Err(UrlError::Character);
        }
        Ok(Self(trimmed.into()))
    }

    /// The exact URL text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Url {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Where a reply's facts came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Origin {
    /// Fetched from the registry for this reply.
    Live,
    /// Served from the durable cache beneath the workspace root.
    Cached,
}

/// How fresh a reply is, so a surface never presents an hour-old number as this instant's.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Provenance {
    /// Live or cached.
    pub origin: Origin,
    /// When the facts were fetched from the registry.
    pub fetched_at: Timestamp,
    /// Whether the cache considers the facts older than its freshness window.
    pub stale: bool,
}

/// One download count.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DownloadCount(pub u64);

impl DownloadCount {
    /// Compact reader spelling: `12.4M`, `980k`, `312`.
    #[must_use]
    pub fn compact(self) -> String {
        let value = self.0;
        if value >= 1_000_000_000 {
            format!("{}.{}B", value / 1_000_000_000, (value % 1_000_000_000) / 100_000_000)
        } else if value >= 1_000_000 {
            format!("{}.{}M", value / 1_000_000, (value % 1_000_000) / 100_000)
        } else if value >= 10_000 {
            format!("{}k", value / 1_000)
        } else {
            value.to_string()
        }
    }
}

/// The recent window one registry reports downloads over.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DownloadWindow {
    /// Last day.
    Day,
    /// Last week.
    Week,
    /// Last month.
    Month,
    /// Last ninety days, as crates.io reports.
    Quarter,
}

impl DownloadWindow {
    /// The reader-facing word.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::Quarter => "90 days",
        }
    }
}

/// Download facts one registry reported.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Downloads {
    /// All-time downloads when the registry counts them.
    pub total: Option<DownloadCount>,
    /// Downloads over the recent window when the registry counts them.
    pub recent: Option<DownloadCount>,
    /// The window `recent` covers.
    pub window: DownloadWindow,
}

/// How a gallery page is ordered.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum ExploreSort {
    /// Most downloaded first.
    #[default]
    Downloads,
    /// Most recently updated first.
    RecentlyUpdated,
    /// Best match for the query first; falls back to downloads without a query.
    Relevance,
    /// Alphabetical.
    Name,
}

impl ExploreSort {
    /// Every sort in display order.
    pub const ALL: [Self; 4] = [
        Self::Downloads,
        Self::RecentlyUpdated,
        Self::Relevance,
        Self::Name,
    ];

    /// The stable word every surface accepts and emits.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Downloads => "downloads",
            Self::RecentlyUpdated => "updated",
            Self::Relevance => "relevance",
            Self::Name => "name",
        }
    }

    /// The reader-facing label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Downloads => "Most downloaded",
            Self::RecentlyUpdated => "Recently updated",
            Self::Relevance => "Best match",
            Self::Name => "Name",
        }
    }

    /// Parses the stable word.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|sort| sort.name() == text)
    }
}

/// One gallery page request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExploreRequest {
    /// Restrict to one ecosystem; `None` spans all seven.
    pub ecosystem: Option<PackageEcosystem>,
    /// Free text to match; `None` browses the top of the sort.
    pub query: Option<ExploreQuery>,
    /// Ordering.
    pub sort: ExploreSort,
    /// Which page.
    pub page: PageNumber,
    /// Cards per page.
    pub limit: ExploreLimit,
}

impl ExploreRequest {
    /// The top of every ecosystem by downloads: what the gallery opens on.
    #[must_use]
    pub fn top() -> Self {
        Self {
            ecosystem: None,
            query: None,
            sort: ExploreSort::Downloads,
            page: PageNumber::FIRST,
            limit: ExploreLimit::default(),
        }
    }
}

/// One package as a card: enough to decide whether to open it, and nothing that needs a second
/// request to draw.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryCard {
    /// Ecosystem.
    pub ecosystem: PackageEcosystem,
    /// Exact registry name, namespace included.
    pub name: PackageName,
    /// Newest version of any kind.
    pub latest: Option<PackageVersion>,
    /// Newest version without a pre-release tag, which the add button defaults to.
    pub latest_stable: Option<PackageVersion>,
    /// One-line description as the registry publishes it.
    pub description: Option<Text>,
    /// Download facts.
    pub downloads: Option<Downloads>,
    /// When the latest version was published.
    pub updated_at: Option<Timestamp>,
    /// SPDX expression or the registry's own spelling.
    pub license: Option<Text>,
    /// Registry keywords, in registry order.
    pub keywords: Box<[Text]>,
    /// Source repository.
    pub repository: Option<Url>,
    /// The pinned coordinate already on the local shelf, when one is.
    pub on_shelf: Option<PackageCoordinate>,
}

impl RegistryCard {
    /// The version an add button offers: latest stable, else latest.
    #[must_use]
    pub fn offered_version(&self) -> Option<&PackageVersion> {
        self.latest_stable.as_ref().or(self.latest.as_ref())
    }

    /// The coordinate an add button compiles, when a version is offered.
    #[must_use]
    pub fn offered_coordinate(&self) -> Option<PackageCoordinate> {
        let version = self.offered_version()?;
        PackageCoordinate::new(self.ecosystem, self.name.as_str(), version.as_str()).ok()
    }
}

/// One gallery page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExplorePage {
    /// The request answered, echoed so a surface can page from it.
    pub request: ExploreRequest,
    /// Cards in the requested order.
    pub cards: Box<[RegistryCard]>,
    /// Total matches when the registry reports one.
    pub total: Option<Count>,
    /// Whether the next page exists.
    pub has_more: bool,
    /// Freshness.
    pub provenance: Provenance,
}

/// A validated owner handle as the registry spells it.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct OwnerHandle(Box<str>);

/// Exact owner-handle admission failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OwnerHandleError {
    /// Nothing but whitespace was supplied.
    Empty,
    /// The text exceeds the fixed budget.
    TooLong {
        /// Observed bytes.
        observed: usize,
        /// Accepted bytes.
        maximum: usize,
    },
    /// The text carries whitespace or a control byte.
    Character,
}

impl OwnerHandle {
    /// Admits one handle.
    ///
    /// # Errors
    ///
    /// Rejects empty, oversized, or whitespace-bearing text.
    pub fn new(text: &str) -> Result<Self, OwnerHandleError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(OwnerHandleError::Empty);
        }
        if trimmed.len() > MAX_OWNER_HANDLE_BYTES {
            return Err(OwnerHandleError::TooLong {
                observed: trimmed.len(),
                maximum: MAX_OWNER_HANDLE_BYTES,
            });
        }
        if trimmed.bytes().any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control()) {
            return Err(OwnerHandleError::Character);
        }
        Ok(Self(trimmed.into()))
    }

    /// The exact handle.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for OwnerHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Whether an owner is a person or a group.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OwnerKind {
    /// One person.
    User,
    /// A team or organisation.
    Team,
}

/// One publisher.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Owner {
    /// The registry the handle belongs to.
    pub ecosystem: PackageEcosystem,
    /// Exact handle.
    pub handle: OwnerHandle,
    /// Display name when the registry has one.
    pub display: Option<Text>,
    /// Person or team.
    pub kind: OwnerKind,
    /// Avatar image.
    pub avatar: Option<Url>,
    /// Profile page.
    pub url: Option<Url>,
}

/// One owner-page request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnerRequest {
    /// Restrict to one ecosystem; `None` asks every registry that knows the handle.
    pub ecosystem: Option<PackageEcosystem>,
    /// Exact handle.
    pub handle: OwnerHandle,
}

/// Everything one owner publishes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnerPage {
    /// The owner as the first registry that knew them spelled them.
    pub owner: Owner,
    /// Their packages, most downloaded first.
    pub cards: Box<[RegistryCard]>,
    /// Ecosystems that could not answer, with why.
    pub faults: Box<[(PackageEcosystem, RegistryError)]>,
    /// Freshness.
    pub provenance: Provenance,
}

/// One published version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryVersion {
    /// Exact spelling.
    pub version: PackageVersion,
    /// Publication instant.
    pub published_at: Option<Timestamp>,
    /// Withdrawn by the publisher; still installable by pin, never offered.
    pub yanked: bool,
    /// Carries a pre-release tag.
    pub prerelease: bool,
    /// Downloads of this version alone.
    pub downloads: Option<DownloadCount>,
    /// Licence of this version when it differs per version.
    pub license: Option<Text>,
    /// Toolchain floor (`rust-version`, `engines.node`, `requires_python`).
    pub toolchain: Option<Text>,
}

/// Which manifest section a dependency lives in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DependencyKind {
    /// Needed at run time.
    Normal,
    /// Needed only to develop or test.
    Development,
    /// Needed only to build.
    Build,
    /// Provided by the consumer.
    Peer,
}

impl DependencyKind {
    /// The reader-facing word.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Development => "dev",
            Self::Build => "build",
            Self::Peer => "peer",
        }
    }
}

/// One dependency of one version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Dependency {
    /// Ecosystem of the dependency, which is the package's own for every registry today.
    pub ecosystem: PackageEcosystem,
    /// Exact name.
    pub name: PackageName,
    /// Version requirement exactly as the manifest spells it.
    pub requirement: Text,
    /// Manifest section.
    pub kind: DependencyKind,
    /// Only pulled in by a feature.
    pub optional: bool,
    /// Platform restriction when one applies.
    pub target: Option<Text>,
    /// Features requested of the dependency.
    pub features: Box<[Text]>,
}

/// The reverse edge: who depends on this package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Dependents {
    /// Total dependents when the registry counts them.
    pub count: Option<Count>,
    /// The first page, most downloaded first.
    pub sample: Box<[RegistryCard]>,
    /// Whether `dependents` would answer a second page.
    pub has_more: bool,
}

/// One dependents-page request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependentsRequest {
    /// Ecosystem.
    pub ecosystem: PackageEcosystem,
    /// Exact name.
    pub name: ExplorePackageName,
    /// Which page.
    pub page: PageNumber,
    /// Cards per page.
    pub limit: ExploreLimit,
}

/// One page of dependents.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependentsPage {
    /// Ecosystem.
    pub ecosystem: PackageEcosystem,
    /// Exact name.
    pub name: PackageName,
    /// Cards, most downloaded first.
    pub cards: Box<[RegistryCard]>,
    /// Which page this is.
    pub page: PageNumber,
    /// Total dependents when known.
    pub total: Option<Count>,
    /// Whether the next page exists.
    pub has_more: bool,
    /// Freshness.
    pub provenance: Provenance,
}

/// Where a readme came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadmeOrigin {
    /// The registry serves the readme itself.
    Registry,
    /// Read from the source repository.
    Repository,
    /// Read from the package archive.
    Archive,
}

/// The package readme as Markdown.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Readme {
    /// Markdown source; rendered by the surface, never pre-rendered here.
    pub markdown: Text,
    /// Where it came from.
    pub origin: ReadmeOrigin,
}

/// Links a package publishes.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Links {
    /// Source repository.
    pub repository: Option<Url>,
    /// Home page.
    pub homepage: Option<Url>,
    /// Hosted documentation.
    pub documentation: Option<Url>,
}

/// The two spellings a reader copies to add a package to their own project.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Install {
    /// The shell command, as `cargo add serde@1.0.196`.
    pub command: Text,
    /// The manifest line, as `serde = "1.0.196"`.
    pub manifest: Text,
}

/// Spells the install snippets for one pinned package, in the ecosystem's own tool.
#[must_use]
pub fn install_snippet(ecosystem: PackageEcosystem, name: &str, version: &str) -> Install {
    let (command, manifest) = match ecosystem {
        PackageEcosystem::Cargo => (
            format!("cargo add {name}@{version}"),
            format!("{name} = \"{version}\""),
        ),
        PackageEcosystem::Npm => (
            format!("npm install {name}@{version}"),
            format!("\"{name}\": \"{version}\""),
        ),
        PackageEcosystem::Pypi => (
            format!("pip install {name}=={version}"),
            format!("{name}=={version}"),
        ),
        PackageEcosystem::Golang => (
            format!("go get {name}@v{}", version.trim_start_matches('v')),
            format!("require {name} v{}", version.trim_start_matches('v')),
        ),
        PackageEcosystem::Maven => {
            let (group, artifact) = name.split_once('/').unwrap_or((name, name));
            (
                format!("mvn dependency:get -Dartifact={group}:{artifact}:{version}"),
                format!(
                    "<dependency><groupId>{group}</groupId><artifactId>{artifact}</artifactId><version>{version}</version></dependency>"
                ),
            )
        }
        PackageEcosystem::Nuget => (
            format!("dotnet add package {name} --version {version}"),
            format!("<PackageReference Include=\"{name}\" Version=\"{version}\" />"),
        ),
        PackageEcosystem::Generic => (
            format!("conan install --requires={name}/{version}"),
            format!("{name}/{version}"),
        ),
    };
    Install {
        command: Text::new(command),
        manifest: Text::new(manifest),
    }
}

/// One detail-page request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DetailRequest {
    /// Ecosystem.
    pub ecosystem: PackageEcosystem,
    /// Exact name.
    pub name: ExplorePackageName,
    /// Which version's dependencies and facts to show; `None` selects latest stable.
    pub version: Option<PackageVersion>,
}

/// One package's whole profile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageDetail {
    /// The card facts.
    pub card: RegistryCard,
    /// The version the dependency list and install snippet describe.
    pub selected: PackageVersion,
    /// Every version, newest first.
    pub versions: Box<[RegistryVersion]>,
    /// Readme when one is published.
    pub readme: Option<Readme>,
    /// Publishers.
    pub owners: Box<[Owner]>,
    /// Dependencies of `selected`.
    pub dependencies: Box<[Dependency]>,
    /// Who depends on this package.
    pub dependents: Dependents,
    /// Published links.
    pub links: Links,
    /// What a reader copies to add `selected`.
    pub install: Install,
    /// Registry categories.
    pub categories: Box<[Text]>,
    /// Freshness.
    pub provenance: Provenance,
}

impl PackageDetail {
    /// The coordinate the add button compiles.
    #[must_use]
    pub fn selected_coordinate(&self) -> Option<PackageCoordinate> {
        PackageCoordinate::new(
            self.card.ecosystem,
            self.card.name.as_str(),
            self.selected.as_str(),
        )
        .ok()
    }
}

/// One thing a registry adapter can do; an adapter that cannot says so per capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegistryCapability {
    /// Browse or search the gallery.
    Explore,
    /// Profile one package.
    Detail,
    /// Serve the readme.
    Readme,
    /// List owners.
    Owners,
    /// List dependencies.
    Dependencies,
    /// List dependents.
    Dependents,
    /// Count downloads.
    Downloads,
    /// List versions.
    Versions,
}

impl RegistryCapability {
    /// The stable word.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Explore => "explore",
            Self::Detail => "detail",
            Self::Readme => "readme",
            Self::Owners => "owners",
            Self::Dependencies => "dependencies",
            Self::Dependents => "dependents",
            Self::Downloads => "downloads",
            Self::Versions => "versions",
        }
    }
}

/// Exact registry failure, with the operand retained.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegistryError {
    /// The registry could not be reached.
    Offline {
        /// Which registry.
        ecosystem: PackageEcosystem,
        /// Bounded transport description.
        detail: Box<str>,
    },
    /// The registry refused for rate.
    RateLimited {
        /// Which registry.
        ecosystem: PackageEcosystem,
        /// Seconds until a retry is welcome, when the registry said.
        retry_after: Option<u32>,
    },
    /// The registry has no such package.
    NotFound {
        /// Which registry.
        ecosystem: PackageEcosystem,
        /// Exact name asked for.
        name: ExplorePackageName,
    },
    /// The package exists but not this version.
    VersionUnknown {
        /// Which registry.
        ecosystem: PackageEcosystem,
        /// Exact name.
        name: ExplorePackageName,
        /// Exact version asked for.
        version: PackageVersion,
    },
    /// The registry answered something this build cannot read.
    Malformed {
        /// Which registry.
        ecosystem: PackageEcosystem,
        /// Bounded description.
        detail: Box<str>,
    },
    /// This registry has no adapter for the capability.
    Unsupported {
        /// Which registry.
        ecosystem: PackageEcosystem,
        /// What was asked.
        capability: RegistryCapability,
    },
    /// The durable cache beneath the workspace root failed.
    Cache {
        /// Bounded description.
        detail: Box<str>,
    },
    /// Registry access is not attached in this process or was disabled by the reader.
    Unconfigured {
        /// Bounded description.
        detail: Box<str>,
    },
}

impl RegistryError {
    /// Stable cause slug, the same word on every surface.
    #[must_use]
    pub const fn slug(&self) -> &'static str {
        match self {
            Self::Offline { .. } => "registry-offline",
            Self::RateLimited { .. } => "registry-rate-limited",
            Self::NotFound { .. } => "registry-not-found",
            Self::VersionUnknown { .. } => "registry-version-unknown",
            Self::Malformed { .. } => "registry-malformed",
            Self::Unsupported { .. } => "registry-unsupported",
            Self::Cache { .. } => "registry-cache",
            Self::Unconfigured { .. } => "registry-unconfigured",
        }
    }

    /// The exact operand that was refused, spelled as a reader would pass it back.
    #[must_use]
    pub fn operand(&self) -> String {
        match self {
            Self::Offline { ecosystem, .. }
            | Self::RateLimited { ecosystem, .. }
            | Self::Unsupported { ecosystem, .. } => ecosystem_tag(*ecosystem).as_str().to_owned(),
            Self::NotFound { ecosystem, name } => {
                format!("{}:{}", ecosystem_tag(*ecosystem).as_str(), name.as_str())
            }
            Self::VersionUnknown {
                ecosystem,
                name,
                version,
            } => format!(
                "{}:{}@{}",
                ecosystem_tag(*ecosystem).as_str(),
                name.as_str(),
                version.as_str()
            ),
            Self::Malformed { ecosystem, .. } => ecosystem_tag(*ecosystem).as_str().to_owned(),
            Self::Cache { .. } | Self::Unconfigured { .. } => String::new(),
        }
    }

    /// One line in the failure's own words.
    #[must_use]
    pub fn detail(&self) -> String {
        match self {
            Self::Offline { detail, .. } => format!("the registry could not be reached: {detail}"),
            Self::RateLimited {
                retry_after: Some(seconds),
                ..
            } => format!("the registry asked for a pause of {seconds}s"),
            Self::RateLimited {
                retry_after: None, ..
            } => "the registry asked for a pause".to_owned(),
            Self::NotFound { .. } => "the registry has no package by this name".to_owned(),
            Self::VersionUnknown { .. } => "the registry never published this version".to_owned(),
            Self::Malformed { detail, .. } => format!("the registry answered unreadably: {detail}"),
            Self::Unsupported { capability, .. } => {
                format!("no adapter serves {} for this registry", capability.label())
            }
            Self::Cache { detail } => format!("the registry cache failed: {detail}"),
            Self::Unconfigured { detail } => detail.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_numbers_are_one_based_and_bounded() {
        assert_eq!(PageNumber::clamped(0).get(), 1);
        assert_eq!(PageNumber::clamped(7).get(), 7);
        assert_eq!(PageNumber::clamped(u32::MAX).get(), MAX_PAGE_NUMBER);
        assert_eq!(PageNumber::FIRST.previous(), PageNumber::FIRST);
        assert_eq!(PageNumber::FIRST.next().get(), 2);
    }

    #[test]
    fn urls_are_web_only_and_trimmed() {
        assert_eq!(
            Url::new("  https://crates.io/crates/serde ")
                .map(|url| url.as_str().to_owned()),
            Ok("https://crates.io/crates/serde".to_owned())
        );
        assert_eq!(Url::new("ftp://x"), Err(UrlError::Scheme));
        assert_eq!(Url::new("https://a b"), Err(UrlError::Character));
    }

    #[test]
    fn download_counts_compact_the_way_a_card_reads_them() {
        assert_eq!(DownloadCount(312).compact(), "312");
        assert_eq!(DownloadCount(9_999).compact(), "9999");
        assert_eq!(DownloadCount(980_000).compact(), "980k");
        assert_eq!(DownloadCount(12_400_000).compact(), "12.4M");
        assert_eq!(DownloadCount(2_100_000_000).compact(), "2.1B");
    }

    #[test]
    fn install_snippets_speak_each_ecosystems_tool() {
        let cargo = install_snippet(PackageEcosystem::Cargo, "serde", "1.0.196");
        assert_eq!(cargo.command.as_str(), "cargo add serde@1.0.196");
        assert_eq!(cargo.manifest.as_str(), "serde = \"1.0.196\"");
        let npm = install_snippet(PackageEcosystem::Npm, "@types/node", "20.11.0");
        assert_eq!(npm.command.as_str(), "npm install @types/node@20.11.0");
        let go = install_snippet(PackageEcosystem::Golang, "github.com/spf13/cobra", "1.8.0");
        assert_eq!(go.command.as_str(), "go get github.com/spf13/cobra@v1.8.0");
        let maven = install_snippet(PackageEcosystem::Maven, "com.google.guava/guava", "33.0.0");
        assert!(maven.manifest.as_str().contains("<artifactId>guava</artifactId>"));
    }

    #[test]
    fn a_card_offers_stable_before_latest() {
        let stable = PackageVersion::new("1.2.0").ok();
        let latest = PackageVersion::new("2.0.0-beta.1").ok();
        let Some(name) = PackageName::new("tokio").ok() else {
            return;
        };
        let card = RegistryCard {
            ecosystem: PackageEcosystem::Cargo,
            name,
            latest: latest.clone(),
            latest_stable: stable.clone(),
            description: None,
            downloads: None,
            updated_at: None,
            license: None,
            keywords: Box::new([]),
            repository: None,
            on_shelf: None,
        };
        assert_eq!(card.offered_version(), stable.as_ref());
        assert_eq!(
            card.offered_coordinate().map(|coordinate| coordinate.to_string()),
            Some("cargo:tokio@1.2.0".to_owned())
        );
    }

    #[test]
    fn every_registry_error_names_its_operand_and_slug() {
        let Some(name) = ExplorePackageName::new("serde").ok() else {
            return;
        };
        let error = RegistryError::NotFound {
            ecosystem: PackageEcosystem::Cargo,
            name,
        };
        assert_eq!(error.slug(), "registry-not-found");
        assert_eq!(error.operand(), "cargo:serde");
        assert_eq!(ExploreSort::parse("updated"), Some(ExploreSort::RecentlyUpdated));
        assert_eq!(ExploreSort::parse("popular"), None);
    }
}
