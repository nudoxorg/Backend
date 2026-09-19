//! The one argument grammar behind every surface's command vocabulary.
//!
//! [`backend_library::COMMANDS`] says what the 35 product commands *are*; it
//! does not say what they take. Without a shared answer to that, the CLI grows
//! one hand-written parser per verb and the MCP grows one hand-written
//! `inputSchema` per tool, and the two drift the first time an operand is
//! added. This table is the single answer: the CLI generates its subcommand
//! grammar and its help from it, and the MCP generates its tool list, its
//! `inputSchema`s, and its enum bounds from it.
//!
//! Every row here is keyed to a registry row by name. The table is checked
//! against `COMMANDS` in this crate's tests, so a registry row without a
//! grammar — or a grammar without a registry row — fails the build's tests
//! rather than shipping a command nobody can call.

use backend_library::{
    COMMANDS, CommandDomain, CommandId, CommandMutation, CommandSpec, command_spec_named,
};
use core::fmt::Write as _;

/// What one operand means, and therefore how each surface validates it.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ArgumentKind {
    /// An exact declaration coordinate returned by a previous result.
    Coordinate,
    /// An absolute path to a project directory on this host.
    ProjectPath,
    /// A pinned package URL or an exact local package label.
    PackageReference,
    /// A version-pinned package URL.
    PackageCoordinate,
    /// Free search or name text.
    Text,
    /// A project folder selected by stable identity or unique name.
    ProjectSelector,
    /// A unique project folder name.
    ProjectName,
    /// An absolute path to a supported lockfile.
    LockfilePath,
    /// A session tree node identity.
    NodeId,
    /// Which kind of subject a session tree node displays.
    SubjectKind,
    /// A closed compiler language and dialect profile.
    LanguageProfile,
    /// An immutable compiler generation identity, as 64 hexadecimal digits.
    Generation,
    /// A bounded page size between 1 and 200.
    Limit,
    /// A boolean switch that is absent or present.
    Flag,
}

impl ArgumentKind {
    /// Returns the metavariable a help line prints.
    #[must_use]
    pub const fn metavar(self) -> &'static str {
        match self {
            Self::Coordinate => "COORDINATE",
            Self::ProjectPath => "PATH",
            Self::PackageReference => "PACKAGE",
            Self::PackageCoordinate => "PURL",
            Self::Text => "TEXT",
            Self::ProjectSelector => "PROJECT",
            Self::ProjectName => "NAME",
            Self::LockfilePath => "LOCKFILE",
            Self::NodeId => "NODE",
            Self::SubjectKind => "SUBJECT",
            Self::LanguageProfile => "PROFILE",
            Self::Generation => "GENERATION",
            Self::Limit => "COUNT",
            Self::Flag => "",
        }
    }

    /// Returns the JSON Schema primitive an MCP `inputSchema` declares.
    #[must_use]
    pub const fn json_type(self) -> &'static str {
        match self {
            Self::NodeId | Self::Limit => "integer",
            Self::Flag => "boolean",
            _ => "string",
        }
    }

    /// Returns the closed value set, when this operand has one.
    #[must_use]
    pub const fn enumeration(self) -> &'static [&'static str] {
        match self {
            Self::SubjectKind => &["package", "declaration", "explore", "search", "owner"],
            Self::LanguageProfile => &[
                "rust", "typescript", "tsx", "python", "go", "java", "csharp", "c", "cpp",
            ],
            _ => &[],
        }
    }
}

/// One operand of one command.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ArgumentSpec {
    name: &'static str,
    kind: ArgumentKind,
    required: bool,
    repeated: bool,
    help: &'static str,
}

impl ArgumentSpec {
    const fn required(name: &'static str, kind: ArgumentKind, help: &'static str) -> Self {
        Self {
            name,
            kind,
            required: true,
            repeated: false,
            help,
        }
    }

    const fn optional(name: &'static str, kind: ArgumentKind, help: &'static str) -> Self {
        Self {
            name,
            kind,
            required: false,
            repeated: false,
            help,
        }
    }

    const fn repeated(name: &'static str, kind: ArgumentKind, help: &'static str) -> Self {
        Self {
            name,
            kind,
            required: true,
            repeated: true,
            help,
        }
    }

    /// Returns the operand name shared by the CLI flag and the MCP field.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// Returns what the operand means.
    #[must_use]
    pub const fn kind(self) -> ArgumentKind {
        self.kind
    }

    /// Returns whether the operand must be supplied.
    #[must_use]
    pub const fn is_required(self) -> bool {
        self.required
    }

    /// Returns whether the operand accepts more than one value.
    #[must_use]
    pub const fn is_repeated(self) -> bool {
        self.repeated
    }

    /// Returns the one-line operand description.
    #[must_use]
    pub const fn help(self) -> &'static str {
        self.help
    }
}

/// One registry row's complete calling convention.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandGrammar {
    name: &'static str,
    tool: &'static str,
    aliases: &'static [&'static str],
    positional: &'static [ArgumentSpec],
    options: &'static [ArgumentSpec],
    when: &'static str,
}

impl CommandGrammar {
    /// Returns the registry row name, verbatim.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// Returns the MCP tool name this row is reachable as.
    #[must_use]
    pub const fn tool(self) -> &'static str {
        self.tool
    }

    /// Returns the CLI spellings kept for compatibility.
    #[must_use]
    pub const fn aliases(self) -> &'static [&'static str] {
        self.aliases
    }

    /// Returns the positional operands in grammar order.
    #[must_use]
    pub const fn positional(self) -> &'static [ArgumentSpec] {
        self.positional
    }

    /// Returns the named options.
    #[must_use]
    pub const fn options(self) -> &'static [ArgumentSpec] {
        self.options
    }

    /// Returns the sentence that says *when* to reach for this command.
    #[must_use]
    pub const fn when(self) -> &'static str {
        self.when
    }

    /// Returns the registry row this grammar belongs to.
    ///
    /// # Panics
    ///
    /// Never: the table is checked against `COMMANDS` by this crate's tests.
    #[must_use]
    pub fn spec(self) -> Option<CommandSpec> {
        command_spec_named(self.name)
    }

    /// Returns the domain this command serves.
    #[must_use]
    pub fn domain(self) -> Option<CommandDomain> {
        self.spec().map(|spec| spec.domain)
    }

    /// Returns whether this command writes durable state.
    #[must_use]
    pub fn is_write(self) -> bool {
        self.spec()
            .is_some_and(|spec| spec.mutation == CommandMutation::Write)
    }

    /// Returns whether this command takes something away.
    ///
    /// MCP's `destructiveHint` is only meaningful for a write, and the registry
    /// records *that* a row writes, not *what* it removes. The distinction is
    /// answered here, once, by an exhaustive match: a registry row added
    /// tomorrow does not compile until someone answers it for that row too.
    #[must_use]
    pub fn is_destructive(self) -> bool {
        let Some(spec) = self.spec() else {
            return false;
        };
        match spec.id {
            CommandId::Remove
            | CommandId::Unsubscribe
            | CommandId::ProjectDelete
            | CommandId::ProjectRemove
            | CommandId::TreeClose => true,
            CommandId::Packages
            | CommandId::Add
            | CommandId::Document
            | CommandId::Show
            | CommandId::Outline
            | CommandId::Name
            | CommandId::Resolve
            | CommandId::Search
            | CommandId::Graph
            | CommandId::GraphQuery
            |             CommandId::Source
            | CommandId::Related
            | CommandId::References
            | CommandId::Read
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
            | CommandId::Subscriptions
            | CommandId::Releases
            | CommandId::Projects
            | CommandId::ProjectCreate
            | CommandId::ProjectAdd
            | CommandId::ProjectSync
            | CommandId::Tree
            | CommandId::TreeOpen
            | CommandId::Health
            | CommandId::Revision => false,
        }
    }

    /// Returns the complete tool description: the registry sentence, then when
    /// an agent should reach for it.
    #[must_use]
    pub fn description(self) -> String {
        self.spec().map_or_else(
            || self.when.to_owned(),
            |spec| format!("{} {}", spec.description, self.when),
        )
    }

    /// Returns the CLI usage line, without the leading program name.
    #[must_use]
    pub fn usage(self) -> String {
        let mut line = self.name.to_owned();
        for argument in self.positional {
            let metavar = argument.kind.metavar();
            line.push(' ');
            let _ = match (argument.required, argument.repeated) {
                (_, true) => write!(line, "<{metavar}>..."),
                (true, false) => write!(line, "<{metavar}>"),
                (false, false) => write!(line, "[{metavar}]"),
            };
        }
        for option in self.options {
            line.push(' ');
            let metavar = option.kind.metavar();
            let _ = if option.kind == ArgumentKind::Flag {
                write!(line, "[--{}]", option.name)
            } else {
                write!(line, "[--{} {metavar}]", option.name)
            };
        }
        line
    }
}

/// Finds the grammar for one registry row name or CLI alias.
#[must_use]
pub fn grammar_for(name: &str) -> Option<CommandGrammar> {
    GRAMMARS
        .iter()
        .copied()
        .find(|grammar| grammar.name == name || grammar.aliases.contains(&name))
}

/// Finds the grammar one MCP tool name reaches.
#[must_use]
pub fn grammar_for_tool(tool: &str) -> Option<CommandGrammar> {
    GRAMMARS.iter().copied().find(|grammar| grammar.tool() == tool)
}

/// Returns every grammar whose registry row serves one domain.
#[must_use]
pub fn grammars_in(domain: CommandDomain) -> Vec<CommandGrammar> {
    GRAMMARS
        .iter()
        .copied()
        .filter(|grammar| grammar.domain() == Some(domain))
        .collect()
}

/// Returns every domain in stable display order.
#[must_use]
pub fn domains() -> [CommandDomain; 5] {
    [
        CommandDomain::Library,
        CommandDomain::Registry,
        CommandDomain::Home,
        CommandDomain::Session,
        CommandDomain::System,
    ]
}

/// Returns the stable lowercase name of one command domain.
#[must_use]
pub const fn domain_name(domain: CommandDomain) -> &'static str {
    match domain {
        CommandDomain::Library => "library",
        CommandDomain::Registry => "registry",
        CommandDomain::Home => "home",
        CommandDomain::Session => "session",
        CommandDomain::System => "system",
    }
}

/// Returns the number of registry rows this table must cover.
#[must_use]
pub const fn registry_size() -> usize {
    COMMANDS.len()
}

const LIMIT: ArgumentSpec = ArgumentSpec::optional(
    "limit",
    ArgumentKind::Limit,
    "Maximum rows in this page, 1 to 200.",
);

/// The calling convention of every registry row, in registry order.
pub const GRAMMARS: [CommandGrammar; 36] = [
    CommandGrammar {
        name: "packages",
        tool: "backend.packages",
        aliases: &["projects-indexed"],
        positional: &[],
        options: &[],
        when: "Call this first in a new session: it is the only way to learn which project roots exist and whether each one is ready to answer.",
    },
    CommandGrammar {
        name: "add",
        tool: "backend.index",
        aliases: &["index"],
        positional: &[ArgumentSpec::optional(
            "path",
            ArgumentKind::ProjectPath,
            "Project directory; defaults to the active project.",
        )],
        options: &[],
        when: "Use when packages does not list the project you need, or when its source changed since the last revision.",
    },
    CommandGrammar {
        name: "remove",
        tool: "backend.remove",
        aliases: &[],
        positional: &[ArgumentSpec::required(
            "path",
            ArgumentKind::ProjectPath,
            "Project directory to drop from the shelf.",
        )],
        options: &[],
        when: "Use to stop answering from a project you no longer read; its derived indexes go with it.",
    },
    CommandGrammar {
        name: "show",
        tool: "backend.document",
        aliases: &["document"],
        positional: &[ArgumentSpec::required(
            "coordinate",
            ArgumentKind::Coordinate,
            "Exact declaration coordinate from a search or outline result.",
        )],
        options: &[],
        when: "Reach for this immediately after search: it is the cheapest complete answer about one declaration.",
    },
    CommandGrammar {
        name: "source",
        tool: "backend.source",
        aliases: &[],
        positional: &[ArgumentSpec::required(
            "coordinate",
            ArgumentKind::Coordinate,
            "Exact declaration coordinate from a search or outline result.",
        )],
        options: &[],
        when: "Use when the signature and documentation are not enough and you need the code itself.",
    },
    CommandGrammar {
        name: "related",
        tool: "backend.related",
        aliases: &[],
        positional: &[ArgumentSpec::required(
            "coordinate",
            ArgumentKind::Coordinate,
            "Exact declaration coordinate to centre the neighbourhood on.",
        )],
        options: &[],
        when: "Use to widen from one declaration to its callers, implementers, and nearest neighbours in one call.",
    },
    CommandGrammar {
        name: "references",
        tool: "backend.references",
        aliases: &[],
        positional: &[ArgumentSpec::required(
            "coordinate",
            ArgumentKind::Coordinate,
            "Exact declaration coordinate whose uses are requested.",
        )],
        options: &[],
        when: "Use to answer where is this symbol used: every row carries the relation kind, the authority class, and the exact source span that proves the use.",
    },
    CommandGrammar {
        name: "read",
        tool: "backend.read",
        aliases: &[],
        positional: &[ArgumentSpec::repeated(
            "coordinate",
            ArgumentKind::Coordinate,
            "Exact declaration coordinates; each is answered or refused on its own.",
        )],
        options: &[],
        when: "Use instead of several show calls when you already know every coordinate you need.",
    },
    CommandGrammar {
        name: "diff",
        tool: "backend.diff",
        aliases: &[],
        positional: &[
            ArgumentSpec::required(
                "from",
                ArgumentKind::PackageReference,
                "Older package: a pinned purl or an exact local package label.",
            ),
            ArgumentSpec::required(
                "to",
                ArgumentKind::PackageReference,
                "Newer package: a pinned purl or an exact local package label.",
            ),
        ],
        options: &[],
        when: "Use to answer what changed between two versions without reading either one end to end.",
    },
    CommandGrammar {
        name: "outline",
        tool: "backend.outline",
        aliases: &[],
        positional: &[ArgumentSpec::optional(
            "path",
            ArgumentKind::ProjectPath,
            "Project directory; defaults to the active project.",
        )],
        options: &[],
        when: "Use to learn a package's shape before searching it, when you do not yet know what to search for.",
    },
    CommandGrammar {
        name: "resolve",
        tool: "backend.resolve",
        aliases: &["name"],
        positional: &[ArgumentSpec::required(
            "query",
            ArgumentKind::Text,
            "A readable declaration name or address.",
        )],
        options: &[LIMIT],
        when: "Use when you have a name from prose or a stack trace and need the exact coordinates it could mean.",
    },
    CommandGrammar {
        name: "search",
        tool: "backend.search",
        aliases: &[],
        positional: &[ArgumentSpec::required(
            "query",
            ArgumentKind::Text,
            "Text to find in names, signatures, and documentation.",
        )],
        options: &[LIMIT],
        when: "Start here when you do not have a coordinate; every lane reports its own coverage so a thin answer is visible as one.",
    },
    CommandGrammar {
        name: "graph",
        tool: "backend.graph",
        aliases: &[],
        positional: &[ArgumentSpec::required(
            "coordinate",
            ArgumentKind::Coordinate,
            "Exact declaration coordinate to traverse from.",
        )],
        options: &[],
        when: "Use to follow one declaration outward along calls, implementations, and references.",
    },
    CommandGrammar {
        name: "health",
        tool: "backend.status",
        aliases: &["status"],
        positional: &[],
        options: &[],
        when: "Check this before trusting an empty result: it says which capabilities are ready and which are simply not configured.",
    },
    CommandGrammar {
        name: "explore",
        tool: "backend.explore",
        aliases: &[],
        positional: &[ArgumentSpec::optional(
            "query",
            ArgumentKind::Text,
            "Case-insensitive filter; omit to browse everything.",
        )],
        options: &[LIMIT],
        when: "Use to browse the package registries rather than the compiled shelf.",
    },
    CommandGrammar {
        name: "package",
        tool: "backend.package",
        aliases: &[],
        positional: &[ArgumentSpec::required(
            "package",
            ArgumentKind::PackageReference,
            "A pinned purl or an exact local package label.",
        )],
        options: &[],
        when: "Use to decide whether to depend on a package before adding it to the shelf.",
    },
    CommandGrammar {
        name: "dependents",
        tool: "backend.dependents",
        aliases: &[],
        positional: &[ArgumentSpec::required(
            "package",
            ArgumentKind::PackageReference,
            "A pinned purl or an exact local package label.",
        )],
        options: &[],
        when: "Use to judge how widely a package is relied on before changing or replacing it.",
    },
    CommandGrammar {
        name: "owner",
        tool: "backend.owner",
        aliases: &[],
        positional: &[ArgumentSpec::required(
            "owner",
            ArgumentKind::Text,
            "Registry publisher handle.",
        )],
        options: &[],
        when: "Use to find the rest of a publisher's work when one of their packages already fits.",
    },
    CommandGrammar {
        name: "index-search",
        tool: "backend.index_search",
        aliases: &[],
        positional: &[ArgumentSpec::required(
            "query",
            ArgumentKind::Text,
            "Name prefix or term.",
        )],
        options: &[LIMIT],
        when: "Use for a name-first lookup across every package the local registry index knows, indexed or not.",
    },
    CommandGrammar {
        name: "package-versions",
        tool: "backend.package_versions",
        aliases: &[],
        positional: &[ArgumentSpec::required(
            "package",
            ArgumentKind::PackageReference,
            "A pinned purl or an exact local package label.",
        )],
        options: &[],
        when: "Use to pick an exact version to pin, with its checksum and yanked state.",
    },
    CommandGrammar {
        name: "semantic-versions",
        tool: "backend.semantic_versions",
        aliases: &[],
        positional: &[ArgumentSpec::required(
            "package",
            ArgumentKind::PackageReference,
            "A pinned purl or an exact local package label.",
        )],
        options: &[],
        when: "Use before select-semantic-version to see which compiler generations were retained.",
    },
    CommandGrammar {
        name: "select-semantic-version",
        tool: "backend.select_semantic_version",
        aliases: &[],
        positional: &[
            ArgumentSpec::required(
                "package",
                ArgumentKind::PackageReference,
                "A pinned purl or an exact local package label.",
            ),
            ArgumentSpec::required(
                "coordinate",
                ArgumentKind::PackageCoordinate,
                "Exact compiler purl for the target language lane.",
            ),
            ArgumentSpec::required(
                "profile",
                ArgumentKind::LanguageProfile,
                "Closed source language and dialect profile.",
            ),
            ArgumentSpec::required(
                "generation",
                ArgumentKind::Generation,
                "Immutable generation identity, 64 hexadecimal digits.",
            ),
        ],
        options: &[],
        when: "Use to pin answers to one exact compiler generation when a newer one disagrees.",
    },
    CommandGrammar {
        name: "package-profile",
        tool: "backend.package_profile",
        aliases: &[],
        positional: &[ArgumentSpec::required(
            "package",
            ArgumentKind::PackageReference,
            "A pinned purl or an exact local package label.",
        )],
        options: &[],
        when: "Use for the one-call summary of a package: its newest version and how many exist.",
    },
    CommandGrammar {
        name: "subscribe",
        tool: "backend.subscribe",
        aliases: &[],
        positional: &[ArgumentSpec::required(
            "package",
            ArgumentKind::PackageReference,
            "A pinned purl or an exact local package label.",
        )],
        options: &[ArgumentSpec::optional(
            "project",
            ArgumentKind::ProjectSelector,
            "Project folder to file the subscription into.",
        )],
        when: "Use to be told about new releases instead of re-checking a package by hand.",
    },
    CommandGrammar {
        name: "unsubscribe",
        tool: "backend.unsubscribe",
        aliases: &[],
        positional: &[ArgumentSpec::required(
            "package",
            ArgumentKind::PackageReference,
            "A pinned purl or an exact local package label.",
        )],
        options: &[],
        when: "Use to stop following a package without deleting the project folder it sat in.",
    },
    CommandGrammar {
        name: "subscriptions",
        tool: "backend.subscriptions",
        aliases: &[],
        positional: &[],
        options: &[],
        when: "Use to see everything you follow and how much of it you have not looked at.",
    },
    CommandGrammar {
        name: "releases",
        tool: "backend.releases",
        aliases: &[],
        positional: &[],
        options: &[ArgumentSpec::optional(
            "mark-seen",
            ArgumentKind::Flag,
            "Mark the returned newest releases as seen.",
        )],
        when: "Use as the periodic check on followed packages; pass mark-seen only when you have acted on the list.",
    },
    CommandGrammar {
        name: "projects",
        tool: "backend.projects",
        aliases: &[],
        positional: &[],
        options: &[],
        when: "Use to see the project folders that group subscriptions, with their lockfile bindings.",
    },
    CommandGrammar {
        name: "project-create",
        tool: "backend.project_create",
        aliases: &[],
        positional: &[ArgumentSpec::required(
            "name",
            ArgumentKind::ProjectName,
            "Unique project folder name, at most 64 bytes.",
        )],
        options: &[ArgumentSpec::optional(
            "lockfile",
            ArgumentKind::LockfilePath,
            "Absolute supported lockfile the folder stays in sync with.",
        )],
        when: "Use to group packages that belong to one piece of work, optionally driven by a lockfile.",
    },
    CommandGrammar {
        name: "project-delete",
        tool: "backend.project_delete",
        aliases: &[],
        positional: &[ArgumentSpec::required(
            "project",
            ArgumentKind::ProjectSelector,
            "Project folder by identity or unique name.",
        )],
        options: &[],
        when: "Use when a piece of work is finished; the subscriptions it held survive.",
    },
    CommandGrammar {
        name: "project-add",
        tool: "backend.project_add",
        aliases: &[],
        positional: &[
            ArgumentSpec::required(
                "project",
                ArgumentKind::ProjectSelector,
                "Project folder by identity or unique name.",
            ),
            ArgumentSpec::required(
                "package",
                ArgumentKind::PackageReference,
                "Pinned package to file into the folder.",
            ),
        ],
        options: &[],
        when: "Use to put one pinned package into a folder you already created.",
    },
    CommandGrammar {
        name: "project-remove",
        tool: "backend.project_remove",
        aliases: &[],
        positional: &[
            ArgumentSpec::required(
                "project",
                ArgumentKind::ProjectSelector,
                "Project folder by identity or unique name.",
            ),
            ArgumentSpec::required(
                "package",
                ArgumentKind::PackageReference,
                "Pinned package to take out of the folder.",
            ),
        ],
        options: &[],
        when: "Use to take one package out of a folder without unsubscribing from it.",
    },
    CommandGrammar {
        name: "project-sync",
        tool: "backend.project_sync",
        aliases: &[],
        positional: &[ArgumentSpec::required(
            "project",
            ArgumentKind::ProjectSelector,
            "Project folder by identity or unique name.",
        )],
        options: &[],
        when: "Use after the bound lockfile changed, to make the folder match it again.",
    },
    CommandGrammar {
        name: "tree",
        tool: "backend.tree",
        aliases: &[],
        positional: &[],
        options: &[],
        when: "Use to see what a person has open in the desktop or another session before you change it.",
    },
    CommandGrammar {
        name: "tree-open",
        tool: "backend.tree_open",
        aliases: &[],
        positional: &[
            ArgumentSpec::required(
                "subject",
                ArgumentKind::SubjectKind,
                "Which kind of subject to open.",
            ),
            ArgumentSpec::required("value", ArgumentKind::Text, "The subject's operand."),
        ],
        options: &[
            ArgumentSpec::optional("parent", ArgumentKind::NodeId, "Parent node to nest under."),
            ArgumentSpec::optional("title", ArgumentKind::Text, "Short node title."),
        ],
        when: "Use to put what you found in front of the person who asked, in the surface they are already looking at.",
    },
    CommandGrammar {
        name: "tree-close",
        tool: "backend.tree_close",
        aliases: &[],
        positional: &[ArgumentSpec::required(
            "node",
            ArgumentKind::NodeId,
            "Session tree node identity.",
        )],
        options: &[ArgumentSpec::optional(
            "branch",
            ArgumentKind::Flag,
            "Close the node's descendants too.",
        )],
        when: "Use to tidy the shared tree when a line of work is done.",
    },
];
