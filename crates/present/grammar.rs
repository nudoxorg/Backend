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

mod table;

pub use table::GRAMMARS;

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
    /// A canonical forge repository URL with an explicit revision.
    ForgeCoordinate,
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
            Self::ForgeCoordinate => "FORGE",
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
            CommandId::Advisory
            | CommandId::Packages
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
            | CommandId::ForgeReference
            | CommandId::Dependents
            | CommandId::Dependencies
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
            | CommandId::ForgeAdd
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
