//! One authoritative product command registry shared by every surface.

use crate::CommandId;

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
    pub mutation: CommandMutation,
    /// Which part of the product the command serves.
    pub domain: CommandDomain,
}

impl CommandSpec {
    /// Reports whether this registry row is represented by the closed
    /// [`crate::SurfaceCommand`] transport contract.
    #[must_use]
    pub const fn is_surface(self) -> bool {
        matches!(
            self.id,
            CommandId::Read
                | CommandId::Diff
                | CommandId::Explore
                | CommandId::Package
                | CommandId::Dependents
                | CommandId::Owner
                | CommandId::IndexSearch
                | CommandId::PackageVersions
                | CommandId::SemanticVersions
                | CommandId::SelectSemanticVersion
                | CommandId::PackageProfile
                | CommandId::Subscribe
                | CommandId::Unsubscribe
                | CommandId::Subscriptions
                | CommandId::Releases
                | CommandId::Projects
                | CommandId::ProjectCreate
                | CommandId::ProjectDelete
                | CommandId::ProjectAdd
                | CommandId::ProjectRemove
                | CommandId::ProjectSync
                | CommandId::Tree
                | CommandId::TreeOpen
                | CommandId::TreeClose
        )
    }
}

/// Whether a command changes durable state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandMutation {
    /// Read-only.
    Read,
    /// Writes durable state and bumps the epoch.
    Write,
}

/// Which part of the product one command serves, so a palette can group rows and a status line
/// can blame the right capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandDomain {
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
pub const COMMANDS: [CommandSpec; 35] = [
    CommandSpec {
        id: CommandId::Packages,
        name: "packages",
        title: "Packages",
        description: "List every package on the shelf with its status and declaration counts.",
        mutation: CommandMutation::Read,
        domain: CommandDomain::Library,
    },
    CommandSpec {
        id: CommandId::Add,
        name: "add",
        title: "Add Package",
        description: "Compile, publish, and index one pinned package so every surface can read it.",
        mutation: CommandMutation::Write,
        domain: CommandDomain::Library,
    },
    CommandSpec {
        id: CommandId::Remove,
        name: "remove",
        title: "Remove Package",
        description: "Remove one package and its derived indexes from the shelf.",
        mutation: CommandMutation::Write,
        domain: CommandDomain::Library,
    },
    CommandSpec {
        id: CommandId::Show,
        name: "show",
        title: "Show Symbol",
        description: "Render one declaration page: signature, documentation, members, relations, and source.",
        mutation: CommandMutation::Read,
        domain: CommandDomain::Library,
    },
    CommandSpec {
        id: CommandId::Source,
        name: "source",
        title: "Show Source",
        description: "Read the source text of one declaration with a few lines of context either side.",
        mutation: CommandMutation::Read,
        domain: CommandDomain::Library,
    },
    CommandSpec {
        id: CommandId::Related,
        name: "related",
        title: "Related Symbols",
        description: "List the symbols related to one declaration through its links, its siblings, and the vector space.",
        mutation: CommandMutation::Read,
        domain: CommandDomain::Library,
    },
    CommandSpec {
        id: CommandId::Read,
        name: "read",
        title: "Read Pages",
        description: "Render several declaration pages in one call, each answered or refused on its own.",
        mutation: CommandMutation::Read,
        domain: CommandDomain::Library,
    },
    CommandSpec {
        id: CommandId::Diff,
        name: "diff",
        title: "Diff Versions",
        description: "Compare two versions of one package: declarations added, removed, and changed in signature.",
        mutation: CommandMutation::Read,
        domain: CommandDomain::Library,
    },
    CommandSpec {
        id: CommandId::Outline,
        name: "outline",
        title: "Package Outline",
        description: "Render one package's containment tree.",
        mutation: CommandMutation::Read,
        domain: CommandDomain::Library,
    },
    CommandSpec {
        id: CommandId::Resolve,
        name: "resolve",
        title: "Resolve Address",
        description: "Turn a readable address into exact declarations, reporting ambiguity instead of guessing.",
        mutation: CommandMutation::Read,
        domain: CommandDomain::Library,
    },
    CommandSpec {
        id: CommandId::Search,
        name: "search",
        title: "Search",
        description: "Search exact keys, names, relations, and vectors together; every lane reports its coverage.",
        mutation: CommandMutation::Read,
        domain: CommandDomain::Library,
    },
    CommandSpec {
        id: CommandId::Graph,
        name: "graph",
        title: "Relations",
        description: "Traverse calls, implementations, references, and other relations from one declaration.",
        mutation: CommandMutation::Read,
        domain: CommandDomain::Library,
    },
    CommandSpec {
        id: CommandId::Health,
        name: "health",
        title: "Health",
        description: "Report every capability as ready, unreachable, unconfigured, or detached.",
        mutation: CommandMutation::Read,
        domain: CommandDomain::System,
    },
    CommandSpec {
        id: CommandId::Explore,
        name: "explore",
        title: "Explore Packages",
        description: "Browse or search the package registries across every ecosystem, sorted and paged.",
        mutation: CommandMutation::Read,
        domain: CommandDomain::Registry,
    },
    CommandSpec {
        id: CommandId::Package,
        name: "package",
        title: "Package Details",
        description: "Show one registry package: readme, versions, owners, dependencies, dependents, downloads, and how to install it.",
        mutation: CommandMutation::Read,
        domain: CommandDomain::Registry,
    },
    CommandSpec {
        id: CommandId::Dependents,
        name: "dependents",
        title: "Dependents",
        description: "List the packages that depend on one registry package, most downloaded first.",
        mutation: CommandMutation::Read,
        domain: CommandDomain::Registry,
    },
    CommandSpec {
        id: CommandId::Owner,
        name: "owner",
        title: "Owner",
        description: "List every package one owner publishes, across ecosystems.",
        mutation: CommandMutation::Read,
        domain: CommandDomain::Registry,
    },
    CommandSpec {
        id: CommandId::IndexSearch,
        name: "index-search",
        title: "Index Search",
        description: "Search every package the local registry index knows by name prefix and term.",
        mutation: CommandMutation::Read,
        domain: CommandDomain::Registry,
    },
    CommandSpec {
        id: CommandId::PackageVersions,
        name: "package-versions",
        title: "Package Versions",
        description: "List every version of one package the index recorded, with checksum and yanked state.",
        mutation: CommandMutation::Read,
        domain: CommandDomain::Registry,
    },
    CommandSpec {
        id: CommandId::SemanticVersions,
        name: "semantic-versions",
        title: "Semantic Versions",
        description: "List immutable compiler generations retained for one exact package.",
        mutation: CommandMutation::Read,
        domain: CommandDomain::Library,
    },
    CommandSpec {
        id: CommandId::SelectSemanticVersion,
        name: "select-semantic-version",
        title: "Select Semantic Version",
        description: "Select one exact retained compiler generation for local product projection.",
        mutation: CommandMutation::Write,
        domain: CommandDomain::Library,
    },
    CommandSpec {
        id: CommandId::PackageProfile,
        name: "package-profile",
        title: "Package Profile",
        description: "Show one package's latest version and full version history from the index.",
        mutation: CommandMutation::Read,
        domain: CommandDomain::Registry,
    },
    CommandSpec {
        id: CommandId::Subscribe,
        name: "subscribe",
        title: "Subscribe",
        description: "Follow one package for new releases, optionally filing it into a project folder.",
        mutation: CommandMutation::Write,
        domain: CommandDomain::Home,
    },
    CommandSpec {
        id: CommandId::Unsubscribe,
        name: "unsubscribe",
        title: "Unsubscribe",
        description: "Stop following one package.",
        mutation: CommandMutation::Write,
        domain: CommandDomain::Home,
    },
    CommandSpec {
        id: CommandId::Subscriptions,
        name: "subscriptions",
        title: "Subscriptions",
        description: "List every followed package with how many releases you have not seen.",
        mutation: CommandMutation::Read,
        domain: CommandDomain::Home,
    },
    CommandSpec {
        id: CommandId::Releases,
        name: "releases",
        title: "Releases",
        description: "List new versions of followed packages since you last looked, optionally marking them seen.",
        mutation: CommandMutation::Write,
        domain: CommandDomain::Home,
    },
    CommandSpec {
        id: CommandId::Projects,
        name: "projects",
        title: "Projects",
        description: "List every project folder with its members and lockfile binding.",
        mutation: CommandMutation::Read,
        domain: CommandDomain::Home,
    },
    CommandSpec {
        id: CommandId::ProjectCreate,
        name: "project-create",
        title: "New Project",
        description: "Create a project folder, optionally bound to a lockfile it stays in sync with.",
        mutation: CommandMutation::Write,
        domain: CommandDomain::Home,
    },
    CommandSpec {
        id: CommandId::ProjectDelete,
        name: "project-delete",
        title: "Delete Project",
        description: "Delete a project folder; its subscriptions are kept.",
        mutation: CommandMutation::Write,
        domain: CommandDomain::Home,
    },
    CommandSpec {
        id: CommandId::ProjectAdd,
        name: "project-add",
        title: "Add to Project",
        description: "Put one pinned package into a project folder.",
        mutation: CommandMutation::Write,
        domain: CommandDomain::Home,
    },
    CommandSpec {
        id: CommandId::ProjectRemove,
        name: "project-remove",
        title: "Remove from Project",
        description: "Take one package out of a project folder.",
        mutation: CommandMutation::Write,
        domain: CommandDomain::Home,
    },
    CommandSpec {
        id: CommandId::ProjectSync,
        name: "project-sync",
        title: "Sync Project",
        description: "Reconcile a project folder with its lockfile, optionally compiling members not yet on the shelf.",
        mutation: CommandMutation::Write,
        domain: CommandDomain::Home,
    },
    CommandSpec {
        id: CommandId::Tree,
        name: "tree",
        title: "Session Tree",
        description: "Show the tree of every subject opened from any surface, nested under what it was opened from.",
        mutation: CommandMutation::Read,
        domain: CommandDomain::Session,
    },
    CommandSpec {
        id: CommandId::TreeOpen,
        name: "tree-open",
        title: "Open in Tree",
        description: "Open a package, page, registry entry, owner, gallery, or search in the session tree under a parent node.",
        mutation: CommandMutation::Write,
        domain: CommandDomain::Session,
    },
    CommandSpec {
        id: CommandId::TreeClose,
        name: "tree-close",
        title: "Close in Tree",
        description: "Close one session tree node, or a whole branch.",
        mutation: CommandMutation::Write,
        domain: CommandDomain::Session,
    },
];

/// Finds one registry row.
#[must_use]
pub fn command_spec(id: CommandId) -> CommandSpec {
    let index = match id {
        CommandId::Packages => 0,
        CommandId::Add => 1,
        CommandId::Remove => 2,
        CommandId::Document | CommandId::Show => 3,
        CommandId::Source => 4,
        CommandId::Related => 5,
        CommandId::Read => 6,
        CommandId::Diff => 7,
        CommandId::Outline => 8,
        CommandId::Name | CommandId::Resolve => 9,
        CommandId::Search => 10,
        CommandId::Graph | CommandId::GraphQuery => 11,
        CommandId::Health | CommandId::Revision => 12,
        CommandId::Explore => 13,
        CommandId::Package => 14,
        CommandId::Dependents => 15,
        CommandId::Owner => 16,
        CommandId::IndexSearch => 17,
        CommandId::PackageVersions => 18,
        CommandId::SemanticVersions => 19,
        CommandId::SelectSemanticVersion => 20,
        CommandId::PackageProfile => 21,
        CommandId::Subscribe => 22,
        CommandId::Unsubscribe => 23,
        CommandId::Subscriptions => 24,
        CommandId::Releases => 25,
        CommandId::Projects => 26,
        CommandId::ProjectCreate => 27,
        CommandId::ProjectDelete => 28,
        CommandId::ProjectAdd => 29,
        CommandId::ProjectRemove => 30,
        CommandId::ProjectSync => 31,
        CommandId::Tree => 32,
        CommandId::TreeOpen => 33,
        CommandId::TreeClose => 34,
    };
    COMMANDS[index]
}

/// Finds one registry row by its shared name.
#[must_use]
pub fn command_spec_named(name: &str) -> Option<CommandSpec> {
    COMMANDS.into_iter().find(|row| row.name == name)
}
