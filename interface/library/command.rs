//! Defines command behavior for `interface-library`, whose purpose is to own the one shared local library every surface reads, adds to, and searches.
//! This module owns the command invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! The one closed command vocabulary every surface projects: CLI subcommands, MCP tools, and the
//! GUI palette are three spellings of this enum, and [`crate::Library::execute`] is the one dispatch.

use interface_core::PackageUrl;
use interface_documents::{Outline, Page, ProjectionLimits};
use interface_identity::PackageCoordinate;
use interface_search::{GraphRequest, GraphTerminal, SearchRequest, SearchTerminal};

use crate::{
    AddOutcome, ExploreError, ExploreLimit, ExplorePackageName, ExploreQuery, Health, IndexSearchPage,
    PackageProfile, PackageVersionRows, PageError, PageLocator, RemoveOutcome, Resolution,
    ResolveError, Shelf, ShelfError,
};

/// Stable identity of one command, shared by every surface's registry.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CommandId {
    /// List the shelf.
    Packages,
    /// Add one package.
    Add,
    /// Remove one package.
    Remove,
    /// Show one declaration page.
    Show,
    /// Show one package outline.
    Outline,
    /// Resolve free text to declarations.
    Resolve,
    /// Multi-lane search.
    Search,
    /// Relation traversal.
    Graph,
    /// Capability health.
    Health,
    /// Index search over the local registry index.
    IndexSearch,
    /// One package's recorded versions.
    PackageVersions,
    /// One package's latest version and history.
    PackageProfile,
}

/// Registry row: the words every surface uses for one command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandSpec {
    /// Stable identity.
    pub id: CommandId,
    /// Snake-case name used by the CLI subcommand and the MCP tool.
    pub name: &'static str,
    /// Title-case label used by the GUI palette.
    pub title: &'static str,
    /// One-sentence description shared verbatim by every surface.
    pub description: &'static str,
    /// Whether the command mutates the shelf.
    pub mutation: Mutation,
}

/// Whether a command changes durable state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mutation {
    /// Read-only.
    Read,
    /// Writes the shelf and bumps the epoch.
    Write,
}

/// The closed registry in stable display order.
pub const COMMANDS: [CommandSpec; 12] = [
    CommandSpec {
        id: CommandId::Packages,
        name: "packages",
        title: "Packages",
        description: "List every package on the shelf with its status and declaration counts.",
        mutation: Mutation::Read,
    },
    CommandSpec {
        id: CommandId::Add,
        name: "add",
        title: "Add Package",
        description: "Compile, publish, and index one pinned package so every surface can read it.",
        mutation: Mutation::Write,
    },
    CommandSpec {
        id: CommandId::Remove,
        name: "remove",
        title: "Remove Package",
        description: "Remove one package and its derived indexes from the shelf.",
        mutation: Mutation::Write,
    },
    CommandSpec {
        id: CommandId::Show,
        name: "show",
        title: "Show Symbol",
        description: "Render one declaration page: signature, documentation, members, relations, and source.",
        mutation: Mutation::Read,
    },
    CommandSpec {
        id: CommandId::Outline,
        name: "outline",
        title: "Package Outline",
        description: "Render one package's containment tree.",
        mutation: Mutation::Read,
    },
    CommandSpec {
        id: CommandId::Resolve,
        name: "resolve",
        title: "Resolve Address",
        description: "Turn a readable address into exact declarations, reporting ambiguity instead of guessing.",
        mutation: Mutation::Read,
    },
    CommandSpec {
        id: CommandId::Search,
        name: "search",
        title: "Search",
        description: "Search exact keys, names, relations, and vectors together; every lane reports its coverage.",
        mutation: Mutation::Read,
    },
    CommandSpec {
        id: CommandId::Graph,
        name: "graph",
        title: "Relations",
        description: "Traverse calls, implementations, references, and other relations from one declaration.",
        mutation: Mutation::Read,
    },
    CommandSpec {
        id: CommandId::Health,
        name: "health",
        title: "Health",
        description: "Report every capability as ready, unreachable, unconfigured, or detached.",
        mutation: Mutation::Read,
    },
    CommandSpec {
        id: CommandId::IndexSearch,
        name: "index-search",
        title: "Index Search",
        description: "Search every package the local registry index knows by name prefix and term.",
        mutation: Mutation::Read,
    },
    CommandSpec {
        id: CommandId::PackageVersions,
        name: "package-versions",
        title: "Package Versions",
        description:
            "List every version of one package the index recorded, with checksum and yanked state.",
        mutation: Mutation::Read,
    },
    CommandSpec {
        id: CommandId::PackageProfile,
        name: "package-profile",
        title: "Package Profile",
        description: "Show one package's latest version and full version history from the index.",
        mutation: Mutation::Read,
    },
];

/// Finds one registry row.
#[must_use]
pub fn spec(id: CommandId) -> CommandSpec {
    COMMANDS[id as usize]
}

/// Finds one registry row by its shared name.
#[must_use]
pub fn spec_named(name: &str) -> Option<CommandSpec> {
    COMMANDS.into_iter().find(|row| row.name == name)
}

/// One fully typed command from any surface.
#[derive(Debug)]
pub enum Command {
    /// List the shelf.
    Packages,
    /// Add one package.
    Add {
        /// Validated package URL.
        url: PackageUrl,
    },
    /// Remove one package.
    Remove {
        /// Pinned coordinate.
        coordinate: PackageCoordinate,
    },
    /// Show one page.
    Show {
        /// How the page is named.
        locator: PageLocator,
        /// Projection budgets.
        limits: ProjectionLimits,
    },
    /// Show one outline.
    Outline {
        /// Pinned coordinate.
        coordinate: PackageCoordinate,
    },
    /// Resolve free text.
    Resolve {
        /// Text as typed.
        text: Box<str>,
    },
    /// Multi-lane search.
    Search(SearchRequest),
    /// Relation traversal.
    Graph(GraphRequest),
    /// Capability health.
    Health,
    /// Index search over the local registry index.
    IndexSearch {
        /// Validated search text.
        query: ExploreQuery,
        /// Page size.
        limit: ExploreLimit,
    },
    /// One package's recorded versions.
    PackageVersions {
        /// Validated registry package name.
        name: ExplorePackageName,
    },
    /// One package's latest version and history.
    PackageProfile {
        /// Validated registry package name.
        name: ExplorePackageName,
    },
}

impl Command {
    /// Registry identity of this command.
    #[must_use]
    pub const fn id(&self) -> CommandId {
        match self {
            Self::Packages => CommandId::Packages,
            Self::Add { .. } => CommandId::Add,
            Self::Remove { .. } => CommandId::Remove,
            Self::Show { .. } => CommandId::Show,
            Self::Outline { .. } => CommandId::Outline,
            Self::Resolve { .. } => CommandId::Resolve,
            Self::Search(_) => CommandId::Search,
            Self::Graph(_) => CommandId::Graph,
            Self::Health => CommandId::Health,
            Self::IndexSearch { .. } => CommandId::IndexSearch,
            Self::PackageVersions { .. } => CommandId::PackageVersions,
            Self::PackageProfile { .. } => CommandId::PackageProfile,
        }
    }
}

/// The typed answer to one command; renderers project this and nothing else.
#[derive(Debug)]
pub enum Reply {
    /// Shelf read.
    Packages(Result<Shelf, ShelfError>),
    /// Add terminal.
    Added(AddOutcome),
    /// Remove terminal.
    Removed(RemoveOutcome),
    /// Page or its exact failure.
    Page(Result<Page, PageError>),
    /// Outline or its exact failure.
    Outline(Result<Outline, PageError>),
    /// Resolution or its exact failure.
    Resolved(Result<Resolution, ResolveError>),
    /// Search terminal; lanes carry their own coverage, so this cannot fail as a whole.
    Searched(SearchTerminal),
    /// Graph terminal or the source's exact failure.
    Graphed(Result<GraphTerminal, PageError>),
    /// Health rows.
    Health(Health),
    /// Index search page or its exact failure.
    IndexSearched(Result<IndexSearchPage, ExploreError>),
    /// Version rows or their exact failure.
    Versions(Result<PackageVersionRows, ExploreError>),
    /// Package profile or its exact failure.
    Profiled(Result<PackageProfile, ExploreError>),
}

impl Reply {
    /// Registry identity of the command that produced this reply.
    #[must_use]
    pub const fn id(&self) -> CommandId {
        match self {
            Self::Packages(_) => CommandId::Packages,
            Self::Added(_) => CommandId::Add,
            Self::Removed(_) => CommandId::Remove,
            Self::Page(_) => CommandId::Show,
            Self::Outline(_) => CommandId::Outline,
            Self::Resolved(_) => CommandId::Resolve,
            Self::Searched(_) => CommandId::Search,
            Self::Graphed(_) => CommandId::Graph,
            Self::Health(_) => CommandId::Health,
            Self::IndexSearched(_) => CommandId::IndexSearch,
            Self::Versions(_) => CommandId::PackageVersions,
            Self::Profiled(_) => CommandId::PackageProfile,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_order_matches_identity_discriminants() {
        for (index, row) in COMMANDS.iter().enumerate() {
            assert_eq!(row.id as usize, index);
            assert_eq!(spec(row.id), *row);
            assert_eq!(spec_named(row.name), Some(*row));
        }
    }
}
