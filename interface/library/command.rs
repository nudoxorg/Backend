//! Defines command behavior for `interface-library`, whose purpose is to own the one shared local library every surface reads, adds to, and searches.
//! This module owns the command invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! The one closed command vocabulary every surface projects: CLI subcommands, MCP tools, and the
//! GUI palette are three spellings of this enum, and [`crate::Library::execute`] is the one dispatch.

use interface_core::PackageUrl;
use interface_documents::{Outline, Page, ProjectionLimits};
use interface_identity::{PackageCoordinate, PackageName};
use interface_search::{GraphRequest, GraphTerminal, SearchRequest, SearchTerminal};

use crate::{
    AddOutcome, CreateProject, DependentsPage, DiffRequest, PackageDiff, ReadRequest, ReadTerminal, DependentsRequest, DetailRequest, ExploreError,
    ExploreLimit, ExplorePackageName, ExplorePage, ExploreQuery, ExploreRequest, FollowError,
    FollowKey, FollowOutcome, FollowRequest, Health, IndexSearchPage, MemberChange, OwnerPage,
    OwnerRequest, PackageDetail, PackageProfile, PackageVersionRows, PageError, PageLocator,
    Project, ProjectError, ProjectId, ProjectSelector, Projects, RegistryError, Related,
    RelatedRequest, Releases, ReleasesRequest, RemoveOutcome, Resolution, ResolveError,
    SessionTree, Shelf, ShelfError, SourceError, SourceRequest, SourceText, Subscriptions,
    SyncReport, SyncRequest, TreeCloseRequest, TreeError, TreeOpenRequest, TreeOutcome,
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
    /// Show one declaration's source.
    Source,
    /// Symbols related to one declaration.
    Related,
    /// Several declaration pages in one call.
    Read,
    /// Declaration-level diff between two versions.
    Diff,
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
    /// Browse or search registry packages.
    Explore,
    /// One registry package's profile.
    Package,
    /// One registry package's dependents.
    Dependents,
    /// One owner's packages.
    Owner,
    /// Index search over the local registry index.
    IndexSearch,
    /// One package's recorded versions.
    PackageVersions,
    /// One package's latest version and history.
    PackageProfile,
    /// Follow one package.
    Subscribe,
    /// Stop following one package.
    Unsubscribe,
    /// List followed packages.
    Subscriptions,
    /// New releases since last seen.
    Releases,
    /// List project folders.
    Projects,
    /// Create a folder.
    ProjectCreate,
    /// Delete a folder.
    ProjectDelete,
    /// Add a member to a folder.
    ProjectAdd,
    /// Remove a member from a folder.
    ProjectRemove,
    /// Reconcile a folder with its lockfile.
    ProjectSync,
    /// The session tree.
    Tree,
    /// Open a subject in the tree.
    TreeOpen,
    /// Close a node or branch.
    TreeClose,
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
    /// Whether the command mutates durable state.
    pub mutation: Mutation,
    /// Which part of the product the command serves.
    pub domain: Domain,
}

/// Whether a command changes durable state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mutation {
    /// Read-only.
    Read,
    /// Writes durable state and bumps the epoch.
    Write,
}

/// Which part of the product one command serves, so a palette can group rows and a status line
/// can blame the right capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Domain {
    /// The compiled shelf and its documentation.
    Library,
    /// Package registries and the local registry index.
    Registry,
    /// Subscriptions and project folders.
    Home,
    /// The session tree shared across surfaces.
    Session,
    /// Process and capability state.
    System,
}

/// The closed registry in stable display order.
pub const COMMANDS: [CommandSpec; 33] = [
    CommandSpec {
        id: CommandId::Packages,
        name: "packages",
        title: "Packages",
        description: "List every package on the shelf with its status and declaration counts.",
        mutation: Mutation::Read,
        domain: Domain::Library,
    },
    CommandSpec {
        id: CommandId::Add,
        name: "add",
        title: "Add Package",
        description: "Compile, publish, and index one pinned package so every surface can read it.",
        mutation: Mutation::Write,
        domain: Domain::Library,
    },
    CommandSpec {
        id: CommandId::Remove,
        name: "remove",
        title: "Remove Package",
        description: "Remove one package and its derived indexes from the shelf.",
        mutation: Mutation::Write,
        domain: Domain::Library,
    },
    CommandSpec {
        id: CommandId::Show,
        name: "show",
        title: "Show Symbol",
        description: "Render one declaration page: signature, documentation, members, relations, and source.",
        mutation: Mutation::Read,
        domain: Domain::Library,
    },
    CommandSpec {
        id: CommandId::Source,
        name: "source",
        title: "Show Source",
        description: "Read the source text of one declaration with a few lines of context either side.",
        mutation: Mutation::Read,
        domain: Domain::Library,
    },
    CommandSpec {
        id: CommandId::Related,
        name: "related",
        title: "Related Symbols",
        description: "List the symbols related to one declaration through its links, its siblings, and the vector space.",
        mutation: Mutation::Read,
        domain: Domain::Library,
    },
    CommandSpec {
        id: CommandId::Read,
        name: "read",
        title: "Read Pages",
        description: "Render several declaration pages in one call, each answered or refused on its own.",
        mutation: Mutation::Read,
        domain: Domain::Library,
    },
    CommandSpec {
        id: CommandId::Diff,
        name: "diff",
        title: "Diff Versions",
        description: "Compare two versions of one package: declarations added, removed, and changed in signature.",
        mutation: Mutation::Read,
        domain: Domain::Library,
    },
    CommandSpec {
        id: CommandId::Outline,
        name: "outline",
        title: "Package Outline",
        description: "Render one package's containment tree.",
        mutation: Mutation::Read,
        domain: Domain::Library,
    },
    CommandSpec {
        id: CommandId::Resolve,
        name: "resolve",
        title: "Resolve Address",
        description: "Turn a readable address into exact declarations, reporting ambiguity instead of guessing.",
        mutation: Mutation::Read,
        domain: Domain::Library,
    },
    CommandSpec {
        id: CommandId::Search,
        name: "search",
        title: "Search",
        description: "Search exact keys, names, relations, and vectors together; every lane reports its coverage.",
        mutation: Mutation::Read,
        domain: Domain::Library,
    },
    CommandSpec {
        id: CommandId::Graph,
        name: "graph",
        title: "Relations",
        description: "Traverse calls, implementations, references, and other relations from one declaration.",
        mutation: Mutation::Read,
        domain: Domain::Library,
    },
    CommandSpec {
        id: CommandId::Health,
        name: "health",
        title: "Health",
        description: "Report every capability as ready, unreachable, unconfigured, or detached.",
        mutation: Mutation::Read,
        domain: Domain::System,
    },
    CommandSpec {
        id: CommandId::Explore,
        name: "explore",
        title: "Explore Packages",
        description: "Browse or search the package registries across every ecosystem, sorted and paged.",
        mutation: Mutation::Read,
        domain: Domain::Registry,
    },
    CommandSpec {
        id: CommandId::Package,
        name: "package",
        title: "Package Details",
        description: "Show one registry package: readme, versions, owners, dependencies, dependents, downloads, and how to install it.",
        mutation: Mutation::Read,
        domain: Domain::Registry,
    },
    CommandSpec {
        id: CommandId::Dependents,
        name: "dependents",
        title: "Dependents",
        description: "List the packages that depend on one registry package, most downloaded first.",
        mutation: Mutation::Read,
        domain: Domain::Registry,
    },
    CommandSpec {
        id: CommandId::Owner,
        name: "owner",
        title: "Owner",
        description: "List every package one owner publishes, across ecosystems.",
        mutation: Mutation::Read,
        domain: Domain::Registry,
    },
    CommandSpec {
        id: CommandId::IndexSearch,
        name: "index-search",
        title: "Index Search",
        description: "Search every package the local registry index knows by name prefix and term.",
        mutation: Mutation::Read,
        domain: Domain::Registry,
    },
    CommandSpec {
        id: CommandId::PackageVersions,
        name: "package-versions",
        title: "Package Versions",
        description:
            "List every version of one package the index recorded, with checksum and yanked state.",
        mutation: Mutation::Read,
        domain: Domain::Registry,
    },
    CommandSpec {
        id: CommandId::PackageProfile,
        name: "package-profile",
        title: "Package Profile",
        description: "Show one package's latest version and full version history from the index.",
        mutation: Mutation::Read,
        domain: Domain::Registry,
    },
    CommandSpec {
        id: CommandId::Subscribe,
        name: "subscribe",
        title: "Subscribe",
        description: "Follow one package for new releases, optionally filing it into a project folder.",
        mutation: Mutation::Write,
        domain: Domain::Home,
    },
    CommandSpec {
        id: CommandId::Unsubscribe,
        name: "unsubscribe",
        title: "Unsubscribe",
        description: "Stop following one package.",
        mutation: Mutation::Write,
        domain: Domain::Home,
    },
    CommandSpec {
        id: CommandId::Subscriptions,
        name: "subscriptions",
        title: "Subscriptions",
        description: "List every followed package with how many releases you have not seen.",
        mutation: Mutation::Read,
        domain: Domain::Home,
    },
    CommandSpec {
        id: CommandId::Releases,
        name: "releases",
        title: "Releases",
        description: "List new versions of followed packages since you last looked, optionally marking them seen.",
        mutation: Mutation::Write,
        domain: Domain::Home,
    },
    CommandSpec {
        id: CommandId::Projects,
        name: "projects",
        title: "Projects",
        description: "List every project folder with its members and lockfile binding.",
        mutation: Mutation::Read,
        domain: Domain::Home,
    },
    CommandSpec {
        id: CommandId::ProjectCreate,
        name: "project-create",
        title: "New Project",
        description: "Create a project folder, optionally bound to a lockfile it stays in sync with.",
        mutation: Mutation::Write,
        domain: Domain::Home,
    },
    CommandSpec {
        id: CommandId::ProjectDelete,
        name: "project-delete",
        title: "Delete Project",
        description: "Delete a project folder; its subscriptions are kept.",
        mutation: Mutation::Write,
        domain: Domain::Home,
    },
    CommandSpec {
        id: CommandId::ProjectAdd,
        name: "project-add",
        title: "Add to Project",
        description: "Put one pinned package into a project folder.",
        mutation: Mutation::Write,
        domain: Domain::Home,
    },
    CommandSpec {
        id: CommandId::ProjectRemove,
        name: "project-remove",
        title: "Remove from Project",
        description: "Take one package out of a project folder.",
        mutation: Mutation::Write,
        domain: Domain::Home,
    },
    CommandSpec {
        id: CommandId::ProjectSync,
        name: "project-sync",
        title: "Sync Project",
        description: "Reconcile a project folder with its lockfile, optionally compiling members not yet on the shelf.",
        mutation: Mutation::Write,
        domain: Domain::Home,
    },
    CommandSpec {
        id: CommandId::Tree,
        name: "tree",
        title: "Session Tree",
        description: "Show the tree of every subject opened from any surface, nested under what it was opened from.",
        mutation: Mutation::Read,
        domain: Domain::Session,
    },
    CommandSpec {
        id: CommandId::TreeOpen,
        name: "tree-open",
        title: "Open in Tree",
        description: "Open a package, page, registry entry, owner, gallery, or search in the session tree under a parent node.",
        mutation: Mutation::Write,
        domain: Domain::Session,
    },
    CommandSpec {
        id: CommandId::TreeClose,
        name: "tree-close",
        title: "Close in Tree",
        description: "Close one session tree node, or a whole branch.",
        mutation: Mutation::Write,
        domain: Domain::Session,
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
    /// Show one declaration's source.
    Source(SourceRequest),
    /// Symbols related to one declaration.
    Related(RelatedRequest),
    /// Several pages in one call.
    Read(ReadRequest),
    /// Declaration-level diff between two versions.
    Diff(DiffRequest),
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
    /// Browse or search registry packages.
    Explore(ExploreRequest),
    /// One registry package's profile.
    Package(DetailRequest),
    /// One registry package's dependents.
    Dependents(DependentsRequest),
    /// One owner's packages.
    Owner(OwnerRequest),
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
    /// Follow one package.
    Subscribe(FollowRequest),
    /// Stop following one package.
    Unsubscribe {
        /// What to stop following.
        key: FollowKey,
    },
    /// List followed packages.
    Subscriptions,
    /// New releases since last seen.
    Releases(ReleasesRequest),
    /// List project folders.
    Projects,
    /// Create a folder.
    ProjectCreate(CreateProject),
    /// Delete a folder.
    ProjectDelete {
        /// Which folder.
        selector: ProjectSelector,
    },
    /// Add a member.
    ProjectAdd(MemberChange),
    /// Remove a member.
    ProjectRemove(MemberChange),
    /// Reconcile a folder with its lockfile.
    ProjectSync(SyncRequest),
    /// The session tree.
    Tree,
    /// Open a subject in the tree.
    TreeOpen(TreeOpenRequest),
    /// Close a node or branch.
    TreeClose(TreeCloseRequest),
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
            Self::Source(_) => CommandId::Source,
            Self::Related(_) => CommandId::Related,
            Self::Read(_) => CommandId::Read,
            Self::Diff(_) => CommandId::Diff,
            Self::Outline { .. } => CommandId::Outline,
            Self::Resolve { .. } => CommandId::Resolve,
            Self::Search(_) => CommandId::Search,
            Self::Graph(_) => CommandId::Graph,
            Self::Health => CommandId::Health,
            Self::Explore(_) => CommandId::Explore,
            Self::Package(_) => CommandId::Package,
            Self::Dependents(_) => CommandId::Dependents,
            Self::Owner(_) => CommandId::Owner,
            Self::IndexSearch { .. } => CommandId::IndexSearch,
            Self::PackageVersions { .. } => CommandId::PackageVersions,
            Self::PackageProfile { .. } => CommandId::PackageProfile,
            Self::Subscribe(_) => CommandId::Subscribe,
            Self::Unsubscribe { .. } => CommandId::Unsubscribe,
            Self::Subscriptions => CommandId::Subscriptions,
            Self::Releases(_) => CommandId::Releases,
            Self::Projects => CommandId::Projects,
            Self::ProjectCreate(_) => CommandId::ProjectCreate,
            Self::ProjectDelete { .. } => CommandId::ProjectDelete,
            Self::ProjectAdd(_) => CommandId::ProjectAdd,
            Self::ProjectRemove(_) => CommandId::ProjectRemove,
            Self::ProjectSync(_) => CommandId::ProjectSync,
            Self::Tree => CommandId::Tree,
            Self::TreeOpen(_) => CommandId::TreeOpen,
            Self::TreeClose(_) => CommandId::TreeClose,
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
    /// Source excerpt or its exact failure.
    Source(Result<SourceText, SourceError>),
    /// Related set or the subject's exact failure.
    Related(Result<Related, PageError>),
    /// Batched pages; every locator is answered on its own, so this cannot fail as a whole.
    Read(ReadTerminal),
    /// Diff or the exact failure of an outline.
    Diffed(Result<PackageDiff, PageError>),
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
    /// Gallery page or its exact failure.
    Explored(Result<ExplorePage, RegistryError>),
    /// Package detail or its exact failure.
    Detailed(Result<PackageDetail, RegistryError>),
    /// Dependents page or its exact failure.
    Dependents(Result<DependentsPage, RegistryError>),
    /// Owner page or its exact failure.
    Owned(Result<OwnerPage, RegistryError>),
    /// Index search page or its exact failure.
    IndexSearched(Result<IndexSearchPage, ExploreError>),
    /// Version rows or their exact failure.
    Versions(Result<PackageVersionRows, ExploreError>),
    /// Package profile or its exact failure.
    Profiled(Result<PackageProfile, ExploreError>),
    /// Subscribe terminal.
    Subscribed(Result<FollowOutcome, FollowError>),
    /// Unsubscribe terminal.
    Unsubscribed(Result<FollowOutcome, FollowError>),
    /// Subscriptions or their exact failure.
    Subscriptions(Result<Subscriptions, FollowError>),
    /// Releases feed or its exact failure.
    Releases(Result<Releases, FollowError>),
    /// Folders or their exact failure.
    Projects(Result<Projects, ProjectError>),
    /// The created folder or the exact refusal.
    ProjectCreated(Result<Project, ProjectError>),
    /// The deleted folder's identity or the exact refusal.
    ProjectDeleted(Result<ProjectId, ProjectError>),
    /// The folder after the add or the exact refusal.
    ProjectAdded(Result<Project, ProjectError>),
    /// The folder after the remove or the exact refusal.
    ProjectRemoved(Result<Project, ProjectError>),
    /// The sync report or the exact refusal.
    ProjectSynced(Result<SyncReport, ProjectError>),
    /// The tree or its exact failure.
    Tree(Result<SessionTree, TreeError>),
    /// Open terminal.
    TreeOpened(Result<TreeOutcome, TreeError>),
    /// Close terminal.
    TreeClosed(Result<TreeOutcome, TreeError>),
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
            Self::Source(_) => CommandId::Source,
            Self::Related(_) => CommandId::Related,
            Self::Read(_) => CommandId::Read,
            Self::Diffed(_) => CommandId::Diff,
            Self::Outline(_) => CommandId::Outline,
            Self::Resolved(_) => CommandId::Resolve,
            Self::Searched(_) => CommandId::Search,
            Self::Graphed(_) => CommandId::Graph,
            Self::Health(_) => CommandId::Health,
            Self::Explored(_) => CommandId::Explore,
            Self::Detailed(_) => CommandId::Package,
            Self::Dependents(_) => CommandId::Dependents,
            Self::Owned(_) => CommandId::Owner,
            Self::IndexSearched(_) => CommandId::IndexSearch,
            Self::Versions(_) => CommandId::PackageVersions,
            Self::Profiled(_) => CommandId::PackageProfile,
            Self::Subscribed(_) => CommandId::Subscribe,
            Self::Unsubscribed(_) => CommandId::Unsubscribe,
            Self::Subscriptions(_) => CommandId::Subscriptions,
            Self::Releases(_) => CommandId::Releases,
            Self::Projects(_) => CommandId::Projects,
            Self::ProjectCreated(_) => CommandId::ProjectCreate,
            Self::ProjectDeleted(_) => CommandId::ProjectDelete,
            Self::ProjectAdded(_) => CommandId::ProjectAdd,
            Self::ProjectRemoved(_) => CommandId::ProjectRemove,
            Self::ProjectSynced(_) => CommandId::ProjectSync,
            Self::Tree(_) => CommandId::Tree,
            Self::TreeOpened(_) => CommandId::TreeOpen,
            Self::TreeClosed(_) => CommandId::TreeClose,
        }
    }

    /// Whether the reply carries a typed failure rather than an answer.
    ///
    /// Every surface that has to choose an exit code, an `isError` flag, or a fault colour reads
    /// it from here, so the three cannot disagree about what counts as failure.
    #[must_use]
    pub fn is_failure(&self) -> bool {
        match self {
            Self::Health(_) | Self::Searched(_) => false,
            Self::Packages(result) => result.is_err(),
            Self::Added(outcome) => !matches!(outcome, AddOutcome::Ready { .. }),
            Self::Removed(outcome) => !matches!(outcome, RemoveOutcome::Removed),
            Self::Page(result) => result.is_err(),
            Self::Source(result) => result.is_err(),
            Self::Related(result) => result.is_err(),
            Self::Read(terminal) => terminal.pages.iter().all(|entry| entry.page.is_err()) && !terminal.pages.is_empty(),
            Self::Diffed(result) => result.is_err(),
            Self::Outline(result) => result.is_err(),
            Self::Graphed(result) => result.is_err(),
            Self::Resolved(Err(_)) => true,
            Self::Resolved(Ok(resolution)) => !matches!(resolution, Resolution::Exact(_)),
            Self::Explored(result) => result.is_err(),
            Self::Detailed(result) => result.is_err(),
            Self::Dependents(result) => result.is_err(),
            Self::Owned(result) => result.is_err(),
            Self::IndexSearched(result) => result.is_err(),
            Self::Versions(result) => result.is_err(),
            Self::Profiled(result) => result.is_err(),
            Self::Subscribed(result) | Self::Unsubscribed(result) => result.is_err(),
            Self::Subscriptions(result) => result.is_err(),
            Self::Releases(result) => result.is_err(),
            Self::Projects(result) => result.is_err(),
            Self::ProjectCreated(result)
            | Self::ProjectAdded(result)
            | Self::ProjectRemoved(result) => result.is_err(),
            Self::ProjectDeleted(result) => result.is_err(),
            Self::ProjectSynced(result) => result.is_err(),
            Self::Tree(result) => result.is_err(),
            Self::TreeOpened(result) | Self::TreeClosed(result) => result.is_err(),
        }
    }
}

/// The name of the package a command is about, when it names exactly one, for tree titles and
/// for surfaces that key state by package.
#[must_use]
pub fn package_name_of(command: &Command) -> Option<&PackageName> {
    match command {
        Command::Remove { coordinate } | Command::Outline { coordinate } => Some(&coordinate.name),
        Command::Unsubscribe { key } => Some(&key.name),
        Command::Subscribe(request) => Some(&request.key.name),
        Command::ProjectAdd(change) | Command::ProjectRemove(change) => {
            Some(&change.coordinate.name)
        }
        _ => None,
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

    #[test]
    fn every_name_is_unique_kebab_case_and_every_sentence_ends() {
        for (index, row) in COMMANDS.iter().enumerate() {
            assert!(
                row.name
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'-'),
                "{} is not kebab-case",
                row.name
            );
            assert!(row.description.ends_with('.'), "{} lacks its full stop", row.name);
            assert!(
                COMMANDS
                    .iter()
                    .skip(index + 1)
                    .all(|other| other.name != row.name && other.title != row.title),
                "{} is not unique",
                row.name
            );
        }
    }

    #[test]
    fn every_write_bumps_the_epoch_domain_wise() {
        for row in COMMANDS {
            let writes = matches!(row.mutation, Mutation::Write);
            let session_or_home = matches!(row.domain, Domain::Session | Domain::Home);
            if session_or_home && row.id != CommandId::Subscriptions && row.id != CommandId::Projects
                && row.id != CommandId::Tree
            {
                assert!(writes, "{} changes durable state and must say so", row.name);
            }
        }
    }
}
