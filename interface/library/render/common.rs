//! Defines common behavior for `interface-library`, whose purpose is to own the one shared local library every surface reads, adds to, and searches.
//! This module owns the common invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! The rendering vocabulary both text surfaces share: relative addresses, faults, and cause slugs.
//!
//! Everything here is a rule rather than a style. Two renderers that disagree about how a target is
//! spelled relative to a page, or about which slug names a failure, are two renderers that teach a
//! reader two different products; the types in this module make that disagreement impossible.

use core::fmt::Write as _;

use compiler_ir::{Confidence, Visibility};
use compiler_vocabulary::Language;
use interface_core::{PackageCompilePhase, PackageUrlError};
use interface_documents::{Census, Symbol, Target};
use interface_identity::{PackageCoordinate, ecosystem_tag};
use interface_search::{Coverage, Degradation, LaneReport, Unavailability};

use crate::{
    AddRejection, Capability, CapabilityState, CompilePhaseProgress, ExploreCoverage, ExploreError,
    ExploreUnavailable, FollowError, ProjectError, RegistryError, ShelfFailure, SourceError,
    Timestamp, TreeError,
};

/// Everything a renderer needs that a [`crate::Reply`] does not carry.
///
/// A reply is a value the engine produced at some past instant; a rendering is read now. The
/// context is the only place that difference is allowed to enter, so a renderer stays a pure
/// function of `(reply, context)` and its tests never depend on a clock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderContext {
    /// Wall clock the reader is reading at, used for relative ages and nothing else.
    pub now: Timestamp,
}

/// The one rule for spelling a target relative to the package a page belongs to.
///
/// A page's coordinate is written exactly once, in its heading. Every target below it is spelled
/// relative to that coordinate:
///
/// * the same package — the path alone, because the coordinate is already on the page;
/// * another loaded package — `cargo:serde › de::Error[trait]`, naming the package without
///   repeating a version the shelf already lists;
/// * a package that is not loaded — `⟨cargo serde⟩ de::Error`, which says *not here*, never
///   *does not exist*.
///
/// The `›` form is deliberately not a parseable address: a reader who wants to follow it hands
/// `package::path` to `resolve`, which answers with the pinned coordinate rather than guessing a
/// version. Spelling every cross-package row with its full pinned coordinate would repeat a
/// version on every line of a relation block to save one call that is only sometimes made.
#[derive(Clone, Copy, Debug)]
pub struct RelativeAddress<'home> {
    home: &'home PackageCoordinate,
}

impl<'home> RelativeAddress<'home> {
    /// Spells targets relative to one package.
    #[must_use]
    pub const fn to(home: &'home PackageCoordinate) -> Self {
        Self { home }
    }

    /// Spells one resolved declaration.
    #[must_use]
    pub fn symbol(&self, symbol: &Symbol) -> String {
        let address = symbol.address.as_address();
        if address.path.is_root() {
            return address.package.to_string();
        }
        if address.package == *self.home {
            return address.path.to_string();
        }
        format!(
            "{}:{} › {}",
            ecosystem_tag(address.package.ecosystem).as_str(),
            address.package.name.as_str(),
            address.path
        )
    }

    /// Spells any link target, resolved or not.
    #[must_use]
    pub fn target(&self, target: &Target) -> String {
        match target {
            Target::Local(symbol) => self.symbol(symbol),
            Target::External(external) => {
                let path = if external.path.is_empty() {
                    external.display.as_str()
                } else {
                    external.path.as_str()
                };
                match &external.origin {
                    Some(origin) => match &origin.package {
                        Some(package) => format!("⟨{} {package}⟩ {path}", origin.ecosystem),
                        None => format!("⟨{}⟩ {path}", origin.ecosystem),
                    },
                    None => format!("⟨?⟩ {path}"),
                }
            }
            Target::Unresolved(text) => format!("⟨?⟩ {text}"),
        }
    }
}

/// The one call that could make progress past a fault.
///
/// A fault names an affordance, never a spelling: an agent surface renders `Add` as the exact
/// `tools/call` arguments and a terminal renders it as the exact shell command, and neither has to
/// know what the other writes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Affordance {
    /// Nothing this layer can offer; the reader is not being sent anywhere.
    None,
    /// Read the shelf.
    Packages,
    /// Read capability health.
    Health,
    /// Add one package.
    Add {
        /// Package URL or coordinate exactly as it should be passed back.
        package: String,
    },
    /// Resolve free text to candidates.
    Resolve {
        /// Text exactly as it should be passed back.
        text: String,
    },
    /// Search for text.
    Search {
        /// Query exactly as it should be passed back.
        query: String,
    },
    /// Show one address.
    Show {
        /// Address exactly as it should be passed back.
        address: String,
    },
}

/// How one surface spells an [`Affordance`].
pub trait Affordances {
    /// Spells the next call, or `None` when this surface cannot offer it.
    fn spell(&self, affordance: &Affordance) -> Option<String>;
}

/// One failure rendered as content: what, on which operand, and what to do next.
///
/// Every field is drawn from the typed failure the engine returned. A renderer that has to invent
/// any of them is rendering a failure the engine did not describe, which is the one thing this
/// layer never does.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fault {
    /// Stable cause slug, the same word on every surface.
    pub slug: &'static str,
    /// The exact operand that was refused; empty when the failure retained none.
    pub operand: String,
    /// One line in the failure's own words; empty when the slug already says everything.
    pub detail: String,
    /// The one call that could make progress.
    pub affordance: Affordance,
}

impl Fault {
    /// A fault with no retained detail.
    #[must_use]
    pub fn new(slug: &'static str, operand: impl Into<String>, affordance: Affordance) -> Self {
        Self {
            slug,
            operand: operand.into(),
            detail: String::new(),
            affordance,
        }
    }

    /// The same fault carrying one line of detail.
    #[must_use]
    pub fn detailed(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
}

/// Writes one fault in the grammar every surface shares.
///
/// ```text
/// ✗ {slug} {operand}
///   {detail}
///   → {affordance}
/// ```
pub fn write_fault(out: &mut String, fault: &Fault, affordances: &dyn Affordances) {
    out.push_str("✗ ");
    out.push_str(fault.slug);
    if !fault.operand.is_empty() {
        out.push(' ');
        out.push_str(&fault.operand);
    }
    out.push('\n');
    if !fault.detail.is_empty() {
        out.push_str("  ");
        out.push_str(&fault.detail);
        out.push('\n');
    }
    write_affordance(out, &fault.affordance, affordances);
}

/// Writes one bare affordance line, for places that offer a next call without a failure.
pub fn write_affordance(out: &mut String, affordance: &Affordance, affordances: &dyn Affordances) {
    out.push_str("  → ");
    match affordances.spell(affordance) {
        Some(spelling) => out.push_str(&spelling),
        None => out.push_str("no action available"),
    }
    out.push('\n');
}

/// The affordance that turns "this package is not on the shelf" into one exact call.
#[must_use]
pub fn add_affordance(coordinate: &PackageCoordinate) -> Affordance {
    Affordance::Add {
        package: coordinate.package_url_text(),
    }
}

/// The package URL shown wherever an empty shelf has to teach the spelling.
pub const EXAMPLE_PACKAGE_URL: &str = "pkg:cargo/serde@1.0.196";

/// Renders one lane's report as the `~lanes` signal fragment: `exact✓2`, `names◐5 3/7`,
/// `graph✗ no-index`.
#[must_use]
pub fn lane_signal(report: &LaneReport) -> String {
    let label = report.lane.label();
    let hits = report.hits.0;
    match report.coverage {
        Coverage::Complete => format!("{label}✓{hits}"),
        Coverage::Partial { searched, total } => {
            format!("{label}◐{hits} {}/{}", searched.0, total.0)
        }
        Coverage::Degraded { reason } => {
            format!("{label}◐{hits} {}", degradation_slug(reason))
        }
        Coverage::Unavailable { reason } => {
            format!("{label}✗ {}", unavailability_slug(reason))
        }
    }
}

/// Stable slug for why a lane could not run.
#[must_use]
pub const fn unavailability_slug(reason: Unavailability) -> &'static str {
    match reason {
        Unavailability::NoEmbedder => "no-embedder",
        Unavailability::NoQdrant => "no-qdrant",
        Unavailability::QdrantUnreachable => "qdrant-unreachable",
        Unavailability::NoIndex => "no-index",
        Unavailability::NoPackages => "no-packages",
        Unavailability::Cancelled => "cancelled",
        Unavailability::NotRequested => "not-requested",
    }
}

/// Stable slug for why a lane ran with reduced fidelity.
#[must_use]
pub const fn degradation_slug(reason: Degradation) -> &'static str {
    match reason {
        Degradation::MissingSegments => "missing-segments",
        Degradation::StaleProjection => "stale-projection",
        Degradation::CandidateBudget => "candidate-budget",
        Degradation::RemoteTimeout => "remote-timeout",
    }
}

/// Stable slug for why an index search covered less than everything, or nothing at all.
#[must_use]
pub const fn explore_unavailable_slug(reason: ExploreUnavailable) -> &'static str {
    match reason {
        ExploreUnavailable::EmptyIndex => "empty-index",
        ExploreUnavailable::StoreFault { slug } => slug,
        ExploreUnavailable::CatalogAbsent => "catalog-absent",
    }
}

/// Stable slug naming which explore failure a reply carried.
#[must_use]
pub const fn explore_error_slug(error: &ExploreError) -> &'static str {
    match error {
        ExploreError::QueryTooLong { .. } => "query-too-long",
        ExploreError::IndexStore { .. } => "index-store",
        ExploreError::CatalogAbsent => "catalog-absent",
        ExploreError::Catalog { .. } => "catalog",
        ExploreError::NotFound { .. } => "package-not-found",
        ExploreError::Ambiguous { .. } => "ambiguous",
    }
}

/// One line naming exactly what the index store or the catalog reported, in its own words.
#[must_use]
pub fn explore_error_detail(error: &ExploreError) -> String {
    match error {
        ExploreError::QueryTooLong { observed, maximum } => {
            format!("{observed} bytes exceeds the {maximum}-byte index-search query budget")
        }
        ExploreError::IndexStore { detail } | ExploreError::Catalog { detail } => {
            detail.as_ref().to_owned()
        }
        ExploreError::CatalogAbsent => {
            "no registry catalog exists at library/catalog.db, so the index knows no packages"
                .to_owned()
        }
        ExploreError::NotFound { package } => {
            format!("the registry catalog records no versions for {}", package.as_str())
        }
        ExploreError::Ambiguous { observed, .. } => {
            format!("{observed} registry rows share this name across ecosystems")
        }
    }
}

/// One-line coverage of an index search, in the shared signal vocabulary.
#[must_use]
pub fn explore_coverage_signal(coverage: ExploreCoverage) -> String {
    match coverage {
        ExploreCoverage::Complete => "complete".to_owned(),
        ExploreCoverage::Partial { searched, total } => format!("partial {searched}/{total}"),
        ExploreCoverage::Unavailable { reason } => {
            format!("unavailable {}", explore_unavailable_slug(reason))
        }
    }
}

/// Stable slug for why an add reached a terminal failure.
#[must_use]
pub const fn shelf_failure_slug(cause: &ShelfFailure) -> &'static str {
    match cause {
        ShelfFailure::PackageNotFound => "package-not-found",
        ShelfFailure::EcosystemRootUnavailable => "no-ecosystem-root",
        ShelfFailure::Compiler { .. } => "compiler-rejected",
        ShelfFailure::Publication => "publication-failed",
        ShelfFailure::Cancelled => "cancelled",
        ShelfFailure::Orphaned => "orphaned",
    }
}

/// Stable slug for why an add was refused before any work started.
#[must_use]
pub const fn rejection_slug(rejection: &AddRejection) -> &'static str {
    match rejection {
        AddRejection::PackageUrl { .. } => "package-url",
        AddRejection::CompilerDetached => "compiler-detached",
        AddRejection::Busy { .. } => "compiler-busy",
        AddRejection::AlreadyReady => "already-ready",
        AddRejection::ShelfFull { .. } => "shelf-full",
    }
}

/// Stable slug naming which part of a package URL was rejected.
#[must_use]
pub const fn package_url_slug(cause: PackageUrlError) -> &'static str {
    match cause {
        PackageUrlError::Empty => "empty",
        PackageUrlError::TooLong { .. } => "too-long",
        PackageUrlError::Scheme => "scheme",
        PackageUrlError::Ecosystem { .. } => "ecosystem",
        PackageUrlError::Name => "name",
        PackageUrlError::Version => "version",
        PackageUrlError::Delimiter { .. } => "delimiter",
        PackageUrlError::Character { .. } => "character",
        PackageUrlError::Escape { .. } => "escape",
        PackageUrlError::QualifierOrder { .. } => "qualifier-order",
        PackageUrlError::QualifierValue { .. } => "qualifier-value",
    }
}

/// Stable slug for which reopen step failed.
#[must_use]
pub const fn reopen_phase_slug(phase: crate::ReopenPhase) -> &'static str {
    match phase {
        crate::ReopenPhase::Binding => "binding",
        crate::ReopenPhase::Manifest => "manifest",
        crate::ReopenPhase::Bytes => "bytes",
        crate::ReopenPhase::Validate => "validate",
        crate::ReopenPhase::Authority => "authority",
    }
}

/// One line naming exactly what a projector found missing, in the projector's own coordinates.
#[must_use]
pub fn projection_detail(error: &interface_documents::ProjectionError) -> String {
    use interface_documents::{MissingPool, ProjectionError};
    match error {
        ProjectionError::MissingEntity { entity } => {
            format!("entity {entity:?} is not in the image")
        }
        ProjectionError::MissingIdentity { entity } => {
            format!("entity {entity:?} has no declaration identity, so no key can be minted")
        }
        ProjectionError::MissingPool { entity, pool } => {
            let plane = match pool {
                MissingPool::Name => "name",
                MissingPool::Members => "member list",
                MissingPool::Documentation => "documentation list",
                MissingPool::Type => "semantic type",
                MissingPool::Text => "text atom",
            };
            format!("entity {entity:?} named a {plane} the image does not hold")
        }
        ProjectionError::ParentageMismatch { entity, member } => {
            format!("member {member:?} does not agree that {entity:?} owns it")
        }
        ProjectionError::ProseBudget {
            entity,
            required,
            maximum,
        } => format!(
            "documentation for {entity:?} needs {} bytes of the {} admitted",
            required.get(),
            maximum.get()
        ),
        ProjectionError::AddressDepth { entity } => {
            format!("entity {entity:?} nests deeper than an address can spell")
        }
    }
}

/// One line naming exactly which part of an address spelling was rejected.
#[must_use]
pub fn address_parse_detail(cause: interface_identity::AddressParseError) -> String {
    use interface_identity::{AddressParseError, CoordinateParseError, PathParseError};
    match cause {
        AddressParseError::Empty => "the address was empty".to_owned(),
        AddressParseError::TooLong { observed, maximum } => {
            format!("{observed} bytes exceeds the {maximum}-byte address budget")
        }
        AddressParseError::Coordinate { cause } => match cause {
            CoordinateParseError::MissingEcosystem => {
                "no `ecosystem:` prefix; write `cargo:serde@1.0.196`".to_owned()
            }
            CoordinateParseError::MissingVersion => {
                "no `@version`; a coordinate is pinned, as in `cargo:serde@1.0.196`".to_owned()
            }
            CoordinateParseError::UnknownEcosystem { length } => format!(
                "the {length}-byte ecosystem tag is not one of cargo npm pypi go maven nuget cpp"
            ),
            CoordinateParseError::EmptyName => "the package name was empty".to_owned(),
            CoordinateParseError::EmptyVersion => "the version was empty".to_owned(),
            CoordinateParseError::Empty => "the coordinate was empty".to_owned(),
            CoordinateParseError::TooLong { observed, maximum } => {
                format!("{observed} bytes exceeds the {maximum}-byte coordinate budget")
            }
            CoordinateParseError::Character { offset, observed } => {
                format!("byte {offset} is {observed:#04x}, which cannot appear in a coordinate")
            }
        },
        AddressParseError::Path { cause } => match cause {
            PathParseError::EmptySegment { index } => {
                format!("path segment {index} was empty")
            }
            PathParseError::TooDeep { observed, maximum } => {
                format!("{observed} path segments exceeds the {maximum} admitted")
            }
        },
        AddressParseError::Key { .. } => {
            "the `#key` suffix was not thirty-two lower-hex characters".to_owned()
        }
    }
}

/// Stable slug for one capability's state.
#[must_use]
pub const fn capability_slug(state: &CapabilityState) -> &'static str {
    match state {
        CapabilityState::Ready => "ready",
        CapabilityState::Unreachable { .. } => "unreachable",
        CapabilityState::Unconfigured => "unconfigured",
        CapabilityState::Detached => "detached",
    }
}

/// The glyph every surface draws beside one capability row.
#[must_use]
pub const fn capability_glyph(state: &CapabilityState) -> &'static str {
    match state {
        CapabilityState::Ready => "✓",
        CapabilityState::Unreachable { .. } => "✗",
        CapabilityState::Unconfigured => "·",
        CapabilityState::Detached => "⊘",
    }
}

/// The capability whose health explains one lane's unavailability, when one does.
#[must_use]
pub const fn blamed_capability(reason: Unavailability) -> Option<Capability> {
    match reason {
        Unavailability::NoEmbedder => Some(Capability::Embedder),
        Unavailability::NoQdrant | Unavailability::QdrantUnreachable => Some(Capability::Vector),
        Unavailability::NoIndex => Some(Capability::Lexical),
        Unavailability::NoPackages
        | Unavailability::Cancelled
        | Unavailability::NotRequested => None,
    }
}

/// Reader-facing visibility word, or `None` when the image proved none.
#[must_use]
pub const fn visibility_label(visibility: Visibility) -> Option<&'static str> {
    match visibility {
        Visibility::Unknown => None,
        Visibility::Private => Some("private"),
        Visibility::Restricted => Some("restricted"),
        Visibility::Package => Some("package"),
        Visibility::Public => Some("public"),
    }
}

/// Reader-facing confidence word.
#[must_use]
pub const fn confidence_label(confidence: Confidence) -> &'static str {
    match confidence {
        Confidence::Syntactic => "syntactic",
        Confidence::Heuristic => "heuristic",
        Confidence::Indexed => "indexed",
        Confidence::Imported => "imported",
        Confidence::Compiler => "compiler",
    }
}

/// The fence info string for one proved source language, so a fence never guesses.
#[must_use]
pub const fn fence_tag(language: Language) -> &'static str {
    match language {
        Language::Rust => "rust",
        Language::TypeScript => "typescript",
        Language::Python => "python",
        Language::Go => "go",
        Language::Java => "java",
        Language::CSharp => "csharp",
        Language::Clang => "c",
    }
}

/// Renders the eight-dot compile journey with the entered phase filled in.
#[must_use]
pub fn phase_dots(phase: PackageCompilePhase) -> String {
    let progress = CompilePhaseProgress::of(phase);
    let filled = usize::from(progress.ordinal).saturating_add(1);
    let total = usize::from(progress.total);
    let mut dots = String::with_capacity(total * 3);
    for step in 0..total {
        dots.push_str(if step < filled { "●" } else { "○" });
    }
    dots
}

/// Most kind counts one package row spells before the row stops being scannable.
pub const MAX_CENSUS_KINDS: usize = 4;

/// Renders a package census as `fn 412 · struct 88 · trait 21`, bounded and in canonical order.
#[must_use]
pub fn census_line(census: &Census) -> String {
    let mut line = String::new();
    let mut written = 0_usize;
    let mut remaining = 0_usize;
    for row in census.kinds() {
        if written == MAX_CENSUS_KINDS {
            remaining = remaining.saturating_add(1);
            continue;
        }
        if written != 0 {
            line.push_str(" · ");
        }
        let _ = write!(
            line,
            "{} {}",
            interface_identity::KindTag::of(row.kind).as_str(),
            row.count.0
        );
        written = written.saturating_add(1);
    }
    if remaining != 0 {
        let _ = write!(line, " · +{remaining} more kinds");
    }
    line
}

/// Renders how long ago something happened, coarsely, because exact seconds are noise.
#[must_use]
pub fn relative_age(now: Timestamp, then: Timestamp) -> String {
    let seconds = now.0.saturating_sub(then.0);
    if seconds < 60 {
        return "just now".to_owned();
    }
    if seconds < 3_600 {
        return format!("{}m ago", seconds / 60);
    }
    if seconds < 86_400 {
        return format!("{}h ago", seconds / 3_600);
    }
    format!("{}d ago", seconds / 86_400)
}

#[cfg(test)]
mod tests {
    use interface_core::PackageEcosystem;

    use super::*;

    fn coordinate(name: &str, version: &str) -> PackageCoordinate {
        match PackageCoordinate::new(PackageEcosystem::Cargo, name, version) {
            Ok(coordinate) => coordinate,
            Err(_) => unreachable!("fixture coordinates are well formed"),
        }
    }

    struct NoAffordances;

    impl Affordances for NoAffordances {
        fn spell(&self, _affordance: &Affordance) -> Option<String> {
            None
        }
    }

    #[test]
    fn fault_grammar_is_three_lines_with_an_affordance() {
        let mut out = String::new();
        write_fault(
            &mut out,
            &Fault::new("package-not-on-shelf", "cargo:serde@1.0.196", Affordance::None)
                .detailed("no shelf row names this coordinate"),
            &NoAffordances,
        );
        assert_eq!(
            out,
            "✗ package-not-on-shelf cargo:serde@1.0.196\n  no shelf row names this coordinate\n  → no action available\n"
        );
    }

    #[test]
    fn ages_and_dots_stay_coarse() {
        assert_eq!(relative_age(Timestamp(7_200), Timestamp(0)), "2h ago");
        assert_eq!(relative_age(Timestamp(30), Timestamp(0)), "just now");
        assert_eq!(relative_age(Timestamp(0), Timestamp(90)), "just now");
        assert_eq!(phase_dots(PackageCompilePhase::Authority), "●●●○○○○○");
        assert_eq!(phase_dots(PackageCompilePhase::Render), "●●●●●●●●");
    }

    #[test]
    fn add_affordance_spells_the_package_url_the_compiler_accepts() {
        let Affordance::Add { package } = add_affordance(&coordinate("serde", "1.0.196")) else {
            unreachable!("add_affordance returns an add");
        };
        assert_eq!(package, "pkg:cargo/serde@1.0.196");
    }
}

/// One registry refusal as the fault every surface draws.
///
/// An unreachable or throttled registry sends the reader to `health`; an unknown name sends them
/// to `search`, because the most common cause is a package that lives under another spelling.
#[must_use]
pub fn registry_fault(error: &RegistryError) -> Fault {
    let affordance = match error {
        RegistryError::Offline { .. }
        | RegistryError::RateLimited { .. }
        | RegistryError::Cache { .. }
        | RegistryError::Unconfigured { .. } => Affordance::Health,
        RegistryError::NotFound { name, .. } => Affordance::Search {
            query: name.as_str().to_owned(),
        },
        RegistryError::VersionUnknown { .. }
        | RegistryError::Malformed { .. }
        | RegistryError::Unsupported { .. } => Affordance::None,
    };
    Fault::new(error.slug(), error.operand(), affordance).detailed(error.detail())
}

/// One subscription refusal as the fault every surface draws.
#[must_use]
pub fn follow_fault(error: &FollowError) -> Fault {
    match error {
        FollowError::Registry(inner) => registry_fault(inner),
        FollowError::Project(inner) => project_fault(inner),
        FollowError::Store { .. } | FollowError::Full { .. } => {
            Fault::new(error.slug(), String::new(), Affordance::Health).detailed(error.detail())
        }
    }
}

/// One project refusal as the fault every surface draws.
#[must_use]
pub fn project_fault(error: &ProjectError) -> Fault {
    let affordance = match error {
        ProjectError::Store { .. } | ProjectError::Full { .. } => Affordance::Health,
        ProjectError::Unknown { .. }
        | ProjectError::Duplicate { .. }
        | ProjectError::Unbound { .. }
        | ProjectError::Lockfile { .. }
        | ProjectError::UnknownLockfile { .. }
        | ProjectError::Name(_) => Affordance::None,
    };
    Fault::new(error.slug(), error.operand(), affordance).detailed(error.detail())
}

/// One session-tree refusal as the fault every surface draws.
#[must_use]
pub fn tree_fault(error: &TreeError) -> Fault {
    let affordance = match error {
        TreeError::Store { .. } | TreeError::Full { .. } => Affordance::Health,
        TreeError::UnknownNode { .. } => Affordance::None,
    };
    Fault::new(error.slug(), error.operand(), affordance).detailed(error.detail())
}

/// One source refusal as the fault every surface draws, given the page fault the surface
/// already knows how to build for the locate step.
#[must_use]
pub fn source_fault(error: &SourceError, page: impl FnOnce(&crate::PageError) -> Fault) -> Fault {
    match error {
        SourceError::Page(inner) => page(inner),
        SourceError::NoSpan { symbol } => {
            Fault::new(error.slug(), symbol.address.to_string(), Affordance::None)
                .detailed(error.detail())
        }
        SourceError::NoSourceRoot { package } => Fault::new(
            error.slug(),
            package.to_string(),
            Affordance::Add {
                package: package.to_string(),
            },
        )
        .detailed(error.detail()),
        SourceError::Unreadable { file, .. } | SourceError::SpanOutOfFile { file, .. } => {
            Fault::new(error.slug(), file.as_str(), Affordance::Health).detailed(error.detail())
        }
    }
}

/// One coverage value as the word every surface shows beside a lane: `✓`, `◐ 3/9`, `~ stale`,
/// or `✗ cause`.
#[must_use]
pub fn coverage_word(coverage: Coverage) -> String {
    match coverage {
        Coverage::Complete => "✓".to_owned(),
        Coverage::Partial { searched, total } => format!("◐ {}/{}", searched.0, total.0),
        Coverage::Degraded { reason } => format!("~ {}", degradation_slug(reason)),
        Coverage::Unavailable { reason } => format!("✗ {}", unavailability_slug(reason)),
    }
}
