//! The one argument grammar behind every surface's command vocabulary.
//!
//! [`backend_library::COMMANDS`] says what the product commands *are*; it
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
    /// Exact selected-symbol-id object in MCP; full 64 hexadecimal digits in CLI.
    SelectedSymbol,
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
    /// The requested Add compilation execution class.
    ExecutionIntent,
    /// An immutable compiler generation identity, as 64 hexadecimal digits.
    Generation,
    /// Direction around a package in the dependency graph.
    GraphDirection,
    /// A positive page size within the operand's declared domain.
    Limit,
    /// Opaque continuation token returned by a previous page.
    Cursor,
    /// A boolean switch that is absent or present.
    Flag,
    /// The exact JSON object returned by an owner-issued indexing job.
    IndexJobTicket,
    /// The exact JSON origin returned by a Cargo package README read.
    CargoPackageReadmeOrigin,
    /// The exact JSON package and tree binding returned by a ProjectTree.
    CargoPackageSourceRequest,
    /// Exact package source-membership page request JSON.
    PackageSourceMembershipRequest,
    /// Exact current compiler-outcome selector returned by package-profile.
    PackageCompilerOutcomeSelector,
    /// Exact source selection, full selected keys, and caller shape budgets.
    SemanticShapeRequest,
    /// Exact portable semantic-source guard and native byte-range request.
    SemanticSourceRequest,
    /// A non-negative progress event sequence.
    Sequence,
    /// A caller observation duration expressed in whole seconds.
    DurationSeconds,
}

impl ArgumentKind {
    /// Returns the metavariable a help line prints.
    #[must_use]
    pub const fn metavar(self) -> &'static str {
        match self {
            Self::Coordinate => "COORDINATE",
            Self::SelectedSymbol => "SELECTED_ID",
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
            Self::ExecutionIntent => "INTENT",
            Self::IndexJobTicket => "TICKET",
            Self::CargoPackageReadmeOrigin => "ORIGIN_JSON",
            Self::CargoPackageSourceRequest => "SOURCE_REQUEST_JSON",
            Self::PackageSourceMembershipRequest => "MEMBERSHIP_REQUEST_JSON",
            Self::PackageCompilerOutcomeSelector => "COMPILER_OUTCOME_JSON",
            Self::SemanticShapeRequest => "SHAPE_REQUEST_JSON",
            Self::SemanticSourceRequest => "SOURCE_REQUEST_JSON",
            Self::Sequence => "SEQUENCE",
            Self::DurationSeconds => "SECONDS",
            Self::Generation => "GENERATION",
            Self::GraphDirection => "DIRECTION",
            Self::Limit => "COUNT",
            Self::Cursor => "CURSOR",
            Self::Flag => "",
        }
    }

    /// Returns the JSON Schema primitive an MCP `inputSchema` declares.
    #[must_use]
    pub const fn json_type(self) -> &'static str {
        match self {
            Self::NodeId | Self::Limit | Self::Sequence | Self::DurationSeconds => "integer",
            Self::Flag => "boolean",
            Self::SelectedSymbol
            | Self::IndexJobTicket
            | Self::CargoPackageReadmeOrigin
            | Self::CargoPackageSourceRequest => "object",
            Self::PackageSourceMembershipRequest
            | Self::PackageCompilerOutcomeSelector
            | Self::SemanticShapeRequest
            | Self::SemanticSourceRequest => "object",
            _ => "string",
        }
    }

    /// Returns the closed value set, when this operand has one.
    #[must_use]
    pub const fn enumeration(self) -> &'static [&'static str] {
        match self {
            Self::SubjectKind => &["package", "declaration", "explore", "search", "owner"],
            Self::LanguageProfile => &[
                "rust",
                "typescript",
                "tsx",
                "python",
                "go",
                "java",
                "csharp",
                "c",
                "cpp",
            ],
            Self::ExecutionIntent => &["interactive", "background"],
            Self::GraphDirection => &["dependencies", "dependents"],
            _ => &[],
        }
    }
}

/// One positive page-bound domain shared by lowering and surface discovery.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PageLimitDomain {
    maximum: u16,
    default: u16,
}

impl PageLimitDomain {
    const STANDARD: Self = Self::new(backend_library::QueryLimit::MAX, crate::DEFAULT_LIMIT);

    // Only grammar declarations can construct domains. An invalid declaration
    // fails constant evaluation instead of advertising an uncallable range.
    const fn new(maximum: u16, default: u16) -> Self {
        assert!(default > 0 && default <= maximum);
        Self { maximum, default }
    }

    /// The inclusive lower bound for a page request.
    #[must_use]
    pub const fn minimum(self) -> u16 {
        1
    }

    /// The inclusive upper bound for a page request.
    #[must_use]
    pub const fn maximum(self) -> u16 {
        self.maximum
    }

    /// The bound used when the caller omits the option.
    #[must_use]
    pub const fn default_limit(self) -> u16 {
        self.default
    }

    /// Whether a caller's explicit bound belongs to this domain.
    #[must_use]
    pub const fn contains(self, value: u16) -> bool {
        value >= self.minimum() && value <= self.maximum
    }
}

/// The authority and wire shape of one command's continuation operand.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ContinuationDomain {
    /// A selected projection page authenticated by the surface wrapper.
    PresentationPage,
    /// A deterministic declaration-section window revalidated against its full source/content.
    DocumentSections,
    /// An opaque owner token authenticated by the existing surface wrapper.
    PresentationOwnerToken,
    /// An opaque token whose native owner authenticates its selected roots.
    NativeOwnerToken,
    /// A typed JSON cursor whose native owner verifies the exact graph guards.
    NativeOwnerJson,
}

/// One operand of one command.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ArgumentSpec {
    /// CLI option spelling and the shared lowering key.
    name: &'static str,
    /// MCP input property spelling, when it differs from the CLI name.
    json_name: &'static str,
    kind: ArgumentKind,
    required: bool,
    repeated: bool,
    help: &'static str,
    page_limit: Option<PageLimitDomain>,
    continuation: Option<ContinuationDomain>,
}

impl ArgumentSpec {
    const fn required(name: &'static str, kind: ArgumentKind, help: &'static str) -> Self {
        Self {
            name,
            json_name: name,
            kind,
            required: true,
            repeated: false,
            help,
            page_limit: None,
            continuation: None,
        }
    }

    const fn optional(name: &'static str, kind: ArgumentKind, help: &'static str) -> Self {
        Self {
            name,
            json_name: name,
            kind,
            required: false,
            repeated: false,
            help,
            page_limit: None,
            continuation: None,
        }
    }

    const fn repeated(name: &'static str, kind: ArgumentKind, help: &'static str) -> Self {
        Self {
            name,
            json_name: name,
            kind,
            required: true,
            repeated: true,
            help,
            page_limit: None,
            continuation: None,
        }
    }

    const fn optional_with_json_name(
        name: &'static str,
        json_name: &'static str,
        kind: ArgumentKind,
        help: &'static str,
    ) -> Self {
        Self {
            name,
            json_name,
            kind,
            required: false,
            repeated: false,
            help,
            page_limit: None,
            continuation: None,
        }
    }

    /// Returns the CLI option spelling and lowering key.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// Returns the MCP input property name.
    #[must_use]
    pub const fn json_name(self) -> &'static str {
        self.json_name
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

    /// Attaches the continuation authority to a cursor operand only.
    #[must_use]
    pub const fn with_continuation_domain(mut self, domain: ContinuationDomain) -> Self {
        assert!(matches!(self.kind, ArgumentKind::Cursor));
        self.continuation = Some(domain);
        self
    }

    /// The cursor's authoritative transport/domain contract.
    #[must_use]
    pub const fn continuation_domain(self) -> Option<ContinuationDomain> {
        if !matches!(self.kind, ArgumentKind::Cursor) {
            return None;
        }
        match self.continuation {
            Some(domain) => Some(domain),
            None => Some(ContinuationDomain::PresentationPage),
        }
    }

    /// The JSON input type, including the declared native cursor shape.
    #[must_use]
    pub const fn json_type(self) -> &'static str {
        if matches!(
            self.continuation_domain(),
            Some(ContinuationDomain::NativeOwnerJson)
        ) {
            "object"
        } else {
            self.kind.json_type()
        }
    }

    const fn with_page_limit(mut self, domain: PageLimitDomain) -> Self {
        assert!(matches!(self.kind, ArgumentKind::Limit));
        self.page_limit = Some(domain);
        self
    }

    /// The grammar-owned range and default for a positive page bound.
    #[must_use]
    pub const fn page_limit_domain(self) -> Option<PageLimitDomain> {
        if !matches!(self.kind, ArgumentKind::Limit) {
            return None;
        }
        match self.page_limit {
            Some(domain) => Some(domain),
            None => Some(PageLimitDomain::STANDARD),
        }
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

    /// Caller-lifetime CLI options, excluded from product/MCP arguments.
    /// They use the same generic parser and help grammar as product options.
    #[must_use]
    pub fn cli_options(self) -> &'static [ArgumentSpec] {
        // A `const` is promoted to 'static; a runtime array literal of const-fn
        // calls is a temporary (E0515).
        const ADD: &[ArgumentSpec] = &[
            ArgumentSpec::optional(
                "wait",
                ArgumentKind::Flag,
                "Wait for this exact durable operation's terminal receipt; failures return nonzero. Without this flag Add returns acceptance immediately.",
            ),
            ArgumentSpec::optional(
                "wait-timeout",
                ArgumentKind::DurationSeconds,
                "With --wait, bound observation to 0–900 seconds after acceptance (default 900). Timeout retains the operation; resume with the returned index_progress command.",
            ),
        ];
        if self.name == "add" { ADD } else { &[] }
    }

    /// The continuation domain of the explicit operand or projection-only page.
    /// Graph and Outline retain their existing presentation paging controls;
    /// these are not added to the daemon's CLI operands.
    #[must_use]
    pub fn continuation_domain(self) -> Option<ContinuationDomain> {
        self.options
            .iter()
            .find_map(|spec| spec.continuation_domain())
            .or_else(|| match self.spec()?.id {
                CommandId::Graph | CommandId::GraphQuery | CommandId::Outline => {
                    Some(ContinuationDomain::PresentationPage)
                }
                _ => None,
            })
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
            | CommandId::Source
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
            | CommandId::SemanticSource
            | CommandId::PackageSourceMembership
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
            | CommandId::Revision
            | CommandId::ProjectTree
            | CommandId::CargoPackageSourceFile
            | CommandId::CargoPackageSourceInventory
            | CommandId::CargoPackageReadme
            | CommandId::CargoPackageReadmeLink
            | CommandId::AdvisoryRefresh
            | CommandId::IndexStart
            | CommandId::IndexAwait
            | CommandId::IndexCancel
            | CommandId::IndexProgress
            | CommandId::PackageGraphPage
            | CommandId::SemanticShapes => false,
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
        self.usage_with_options(false)
    }

    /// CLI usage including caller-lifetime options that are not MCP inputs.
    #[must_use]
    pub fn cli_usage(self) -> String {
        self.usage_with_options(true)
    }

    fn usage_with_options(self, cli: bool) -> String {
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
        for option in self
            .options
            .iter()
            .chain(if cli { self.cli_options() } else { &[] })
        {
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

    /// Returns a usage line for the installed public command.
    #[must_use]
    pub fn installed_usage(self) -> String {
        format!("nudox {}", self.cli_usage())
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
    GRAMMARS
        .iter()
        .copied()
        .find(|grammar| grammar.tool() == tool)
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

const SELECTED_SYMBOL: ArgumentSpec = ArgumentSpec::optional_with_json_name(
    "selected-id",
    "selected_symbol",
    ArgumentKind::SelectedSymbol,
    "Full selected row ID from identity.semantic_data: CLI takes 64 hex digits; MCP takes the exact selected-symbol-id object. Optional coordinate asserts the selected row label. Requires current-view membership.",
);

const LIMIT: ArgumentSpec = ArgumentSpec::optional(
    "limit",
    ArgumentKind::Limit,
    "Maximum rows in this page, 1 to 200.",
);
const CURSOR: ArgumentSpec = ArgumentSpec::optional(
    "cursor",
    ArgumentKind::Cursor,
    "Opaque continuation returned by the preceding page; bound to its query and index snapshot.",
);

/// The calling convention of every registry row, in registry order.
pub const GRAMMARS: [CommandGrammar; 54] = [
    CommandGrammar {
        name: "advisory",
        tool: "backend.advisory",
        aliases: &[],
        positional: &[ArgumentSpec::required(
            "package",
            ArgumentKind::PackageReference,
            "A pinned purl or an exact local package label.",
        )],
        options: &[
            ArgumentSpec::optional(
                "override-actor",
                ArgumentKind::Text,
                "Auditable actor identity for an exceptional allow.",
            ),
            ArgumentSpec::optional(
                "override-reason",
                ArgumentKind::Text,
                "Why the blocked release is being reviewed.",
            ),
            ArgumentSpec::optional(
                "override-policy-version",
                ArgumentKind::Text,
                "Positive policy version that granted the override.",
            ),
            ArgumentSpec::optional(
                "override-expires-at",
                ArgumentKind::Text,
                "Optional Unix timestamp at which the override expires.",
            ),
        ],
        when: "Use before adding a package to inspect source IDs, aliases, affected and fixed ranges, freshness, coverage, and the typed acquisition decision.",
    },
    CommandGrammar {
        name: "packages",
        tool: "backend.packages",
        aliases: &["projects-indexed"],
        positional: &[],
        options: &[],
        when: "Call this first in a new session to learn which project roots exist and whether each one is ready to answer. Use backend.outline to inspect a package's shape or backend.search to find declarations.",
    },
    CommandGrammar {
        name: "add",
        tool: "backend.index",
        aliases: &["index"],
        positional: &[ArgumentSpec::optional(
            "path",
            ArgumentKind::ProjectPath,
            "Local project directory to index or re-index; use `.` for the current directory.",
        )],
        options: &[ArgumentSpec::optional_with_json_name(
            "execution-intent",
            "execution_intent",
            ArgumentKind::ExecutionIntent,
            "Compilation class: interactive protects local latency; background permits bounded remote calibration. Defaults to interactive.",
        )],
        when: "Pass the absolute local project directory to index or re-index it. The CLI also accepts index as an alias for add. Call it when packages does not list the project, or when its source changed. Use background execution when the request may be calibrated remotely.",
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
        positional: &[ArgumentSpec::optional(
            "coordinate",
            ArgumentKind::Coordinate,
            "Exact declaration coordinate from a search or outline result.",
        )],
        options: &[SELECTED_SYMBOL, CURSOR.with_continuation_domain(ContinuationDomain::DocumentSections)],
        when: "Read one exact declaration and bounded sections. Copy section_page.continuation into backend.document, or use its selected-id CLI continuation, to retrieve every member and relation.",
    },
    CommandGrammar {
        name: "source",
        tool: "backend.source",
        aliases: &[],
        positional: &[ArgumentSpec::optional(
            "coordinate",
            ArgumentKind::Coordinate,
            "Exact declaration coordinate from a search or outline result.",
        )],
        options: &[SELECTED_SYMBOL],
        when: "Use when the signature and documentation are not enough and you need the code itself.",
    },
    CommandGrammar {
        name: "related",
        tool: "backend.related",
        aliases: &[],
        positional: &[ArgumentSpec::optional(
            "coordinate",
            ArgumentKind::Coordinate,
            "Exact declaration coordinate to centre the neighbourhood on.",
        )],
        options: &[SELECTED_SYMBOL],
        when: "Use after you have a coordinate to see callers and callees in this package. Package-level dependents are a separate call.",
    },
    CommandGrammar {
        name: "references",
        tool: "backend.references",
        aliases: &[],
        positional: &[ArgumentSpec::required(
            "coordinate",
            ArgumentKind::Coordinate,
            "Exact declaration coordinate whose uses are requested. Selected row IDs are not supported by the references protocol.",
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
        when: "Legacy coordinate batch read. Same-coordinate aliases use its existing first-match semantics; use show with --selected-id when exact row selection matters.",
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
        options: &[LIMIT, CURSOR],
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
        options: &[LIMIT, CURSOR],
        when: "Use when you have a name from prose or a stack trace and need the exact coordinates it could mean. Reported score is a descending order key (u32::MAX - rank, zero-based), not a relevance probability; compare it only within the same query/revision.",
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
        options: &[LIMIT, CURSOR],
        when: "Start here for a question like where error handling lives. Match names, signatures, and documentation, then open a coordinate. Reported score is a descending order key (u32::MAX - rank, zero-based), not a relevance probability; compare it only within the same query/revision.",
    },
    CommandGrammar {
        name: "graph",
        tool: "backend.graph",
        aliases: &[],
        positional: &[ArgumentSpec::optional(
            "coordinate",
            ArgumentKind::Coordinate,
            "Exact declaration coordinate to traverse from.",
        )],
        options: &[SELECTED_SYMBOL, LIMIT, CURSOR],
        when: "Use to follow one declaration outward along calls and references in its package.",
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
        name: "dependencies",
        tool: "backend.dependencies",
        aliases: &[],
        positional: &[ArgumentSpec::required(
            "package",
            ArgumentKind::PackageReference,
            "A pinned purl or an exact local package label.",
        )],
        options: &[],
        when: "Use to inspect the requirements and scopes declared by one package.",
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
        options: &[LIMIT, CURSOR.with_continuation_domain(ContinuationDomain::PresentationOwnerToken)],
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
        options: &[
            LIMIT.with_page_limit(PageLimitDomain::new(backend_library::QueryLimit::MAX, backend_library::DEFAULT_SEMANTIC_VERSIONS_LIMIT)),
            CURSOR.with_continuation_domain(ContinuationDomain::NativeOwnerToken),
        ],
        when: "Use before select-semantic-version to discover exact retained compiler identities. Default requests up to eight complete descriptors with history counts and portable native semantic-source references. The first page may be smaller to fit the complete response budget; copy the returned continuation with its effective page bound.",
    },
    CommandGrammar {
        name: "semantic-shapes",
        tool: "backend.semantic_shapes",
        aliases: &[],
        positional: &[ArgumentSpec::required(
            "request",
            ArgumentKind::SemanticShapeRequest,
            "Exact portable semantic-source reference or full inline SemanticVersionRecord with mandatory source_epoch roots; full selected SymbolKey byte arrays, max_nodes and max_bytes.",
        )],
        options: &[],
        when: "Use after semantic-versions and an exact selected-key read to obtain compiler-owned member/type shapes with source/image provenance. Unavailable and unsupported remain typed facts; no source-text fallback is inferred.",
    },
    CommandGrammar {
        name: "semantic-source",
        tool: "backend.semantic_source",
        aliases: &[],
        positional: &[ArgumentSpec::required(
            "request",
            ArgumentKind::SemanticSourceRequest,
            "Owner-authenticated reference returned by semantic-versions, with range.offset and range.max_bytes. Copy each returned next offset to retrieve the same complete canonical source; omitted range starts at zero with the product text bound.",
        )],
        options: &[],
        when: "Read or resume the complete captured source through exact native byte ranges. Assemble only one source identity in order; only the complete digest-checked union is a source operand.",
    },
    CommandGrammar {
        name: "package-source-membership",
        tool: "backend.package_source_membership",
        aliases: &[],
        positional: &[ArgumentSpec::required(
            "request",
            ArgumentKind::PackageSourceMembershipRequest,
            "Exact local package plus the selected source root/version and continuation returned by the preceding page.",
        )],
        options: &[],
        when: "Use to enumerate the exact File rows selected by a local Project, with package-relative paths, parser language, content version, and optional SourceFact identity. The reply states that tsconfig, build-tag, and platform exclusion reasons were not captured.",
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
        options: &[ArgumentSpec::optional_with_json_name(
            "compiler-outcome", "compiler_outcome", ArgumentKind::PackageCompilerOutcomeSelector,
            "Use {\"kind\":\"current\"} for capture-only discovery independent of packaging metadata, or copy a retained diagnostic's read object for one full failure under its exact source/capture roots.",
        )],
        when: "Read registry version/packaging facts and, for an indexed local project, every current compiler profile outcome. Use a returned compiler-outcome selector for each retained full diagnostic; stale selectors require fresh discovery.",
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
        name: "project-tree",
        tool: "backend.project_tree",
        aliases: &[],
        positional: &[ArgumentSpec::optional(
            "path",
            ArgumentKind::ProjectPath,
            "Project directory; defaults to the active project.",
        )],
        options: &[],
        when: "Use to see what a project depends on and why: the role of each direct dependency, the path to any package, which crates are here twice, and which advisories affect it.",
    },
    CommandGrammar {
        name: "advisory-refresh",
        tool: "backend.advisory_refresh",
        aliases: &[],
        positional: &[],
        options: &[],
        when: "Use before reading a tree when the advisory sources may have moved; it reads only the sources the owner was configured with.",
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
    CommandGrammar {
        name: "forge-add",
        tool: "backend.forge_add",
        aliases: &[],
        positional: &[ArgumentSpec::required(
            "coordinate",
            ArgumentKind::ForgeCoordinate,
            "HTTPS forge repository URL with an explicit tag, branch, or commit; append #subdir for a monorepo package.",
        )],
        options: &[],
        when: "Use to acquire a project or package that is absent from the configured registries.",
    },
    CommandGrammar {
        name: "forge-reference",
        tool: "backend.forge_reference",
        aliases: &[],
        positional: &[ArgumentSpec::required(
            "coordinate",
            ArgumentKind::ForgeCoordinate,
            "The exact forge coordinate previously acquired into the local cache.",
        )],
        options: &[],
        when: "Use offline or during restart to read an exact source snapshot from the local cache.",
    },
    CommandGrammar {
        name: "package-graph",
        tool: "backend.package_graph",
        aliases: &[],
        positional: &[
            ArgumentSpec::required(
                "package",
                ArgumentKind::PackageReference,
                "Pinned package URL at the center of the graph page.",
            ),
            ArgumentSpec::required(
                "direction",
                ArgumentKind::GraphDirection,
                "Read outgoing dependencies or incoming dependents.",
            ),
        ],
        options: &[
            ArgumentSpec::optional(
                "authority",
                ArgumentKind::Text,
                "Exact source selector copied from an ambiguous dependency answer.",
            ),
            ArgumentSpec::optional(
                "limit",
                ArgumentKind::Limit,
                "Maximum rows in this package graph page.",
            )
            .with_page_limit(PageLimitDomain::new(
                backend_library::MAX_PACKAGE_GRAPH_PAGE_ROWS,
                64,
            )),
            ArgumentSpec::optional(
                "cursor",
                ArgumentKind::Cursor,
                "Exact JSON cursor object copied from package_graph_page.terminal.cursor in the preceding response.",
            ).with_continuation_domain(ContinuationDomain::NativeOwnerJson),
        ],
        when: "Use for a package's exact dependency or dependent edges, with each source authority, ecosystem, resolver scope, version requirement, and selected graph snapshot preserved.",
    },
    CommandGrammar {
        name: "cargo-source-file",
        tool: "backend.cargo_source_file",
        aliases: &[],
        positional: &[
            ArgumentSpec::required(
                "request",
                ArgumentKind::CargoPackageSourceRequest,
                "Exact package and requested/effective root binding copied from one ProjectTree reply.",
            ),
            ArgumentSpec::required(
                "path",
                ArgumentKind::Text,
                "Slash-separated package-relative source or documentation path.",
            ),
        ],
        options: &[],
        when: "Use for a bounded source or documentation file when the owner can revalidate both the Cargo source receipt and the exact ProjectTree request.",
    },
    CommandGrammar {
        name: "cargo-source-inventory",
        tool: "backend.cargo_source_inventory",
        aliases: &[],
        positional: &[ArgumentSpec::required(
            "request",
            ArgumentKind::CargoPackageSourceRequest,
            "Exact package and requested/effective root binding copied from one ProjectTree reply.",
        )],
        options: &[],
        when: "Use to list a bounded set of owner-observed source and documentation paths. Each address must be read separately; the inventory is not proof that a file is indexed.",
    },
    CommandGrammar {
        name: "cargo-package-readme",
        tool: "backend.cargo_package_readme",
        aliases: &[],
        positional: &[ArgumentSpec::required(
            "package",
            ArgumentKind::PackageReference,
            "Exact source-qualified package reference copied from the current ProjectTree row.",
        )],
        options: &[],
        when: "Read the bounded README selected by this exact package manifest under the current project-tree request; README text does not prove semantic indexing.",
    },
    CommandGrammar {
        name: "cargo-package-readme-link",
        tool: "backend.cargo_package_readme_link",
        aliases: &[],
        positional: &[
            ArgumentSpec::required(
                "origin",
                ArgumentKind::CargoPackageReadmeOrigin,
                "Exact origin object from an owner-returned cargo-package-readme result.",
            ),
            ArgumentSpec::required(
                "href",
                ArgumentKind::Text,
                "Relative Markdown href copied from that README.",
            ),
        ],
        options: &[],
        when: "Follow one relative file or fragment from the exact current README under the package or inherited workspace root; the owner revalidates the README origin and never follows external links.",
    },
    CommandGrammar {
        name: "index_start",
        tool: "backend.index_start",
        aliases: &[],
        positional: &[ArgumentSpec::required(
            "package",
            ArgumentKind::PackageReference,
            "Local package path or exact version-pinned package URL.",
        )],
        options: &[
            ArgumentSpec::optional_with_json_name(
                "execution-intent",
                "execution_intent",
                ArgumentKind::ExecutionIntent,
                "Compilation class; defaults to interactive.",
            ),
            ArgumentSpec::optional_with_json_name(
                "operation-key",
                "operation_key",
                ArgumentKind::Text,
                "Optional caller-persisted durable operation key; replay the exact same request and key after interrupted delivery.",
            ),
        ],
        when: "Use to start owner-managed indexing without holding this client open; keep the exact returned ticket for progress or cancellation.",
    },
    CommandGrammar {
        name: "index_await",
        tool: "backend.index_await",
        aliases: &[],
        positional: &[ArgumentSpec::required(
            "ticket",
            ArgumentKind::IndexJobTicket,
            "Exact JSON ticket returned by index_start; quote it as one shell argument and do not edit its id, epoch, or package.",
        )],
        options: &[],
        when: "Use from a command-line client when you want to wait for the exact job's terminal receipt instead of polling.",
    },
    CommandGrammar {
        name: "index_cancel",
        tool: "backend.index_cancel",
        aliases: &[],
        positional: &[ArgumentSpec::required(
            "ticket",
            ArgumentKind::IndexJobTicket,
            "Exact JSON ticket returned by index_start; quote it as one shell argument and do not edit its id, epoch, or package.",
        )],
        options: &[],
        when: "Use to request cancellation of one exact job; a requested status is not terminal, so keep polling its ticket.",
    },
    CommandGrammar {
        name: "index_progress",
        tool: "backend.index_progress",
        aliases: &[],
        positional: &[ArgumentSpec::optional(
            "ticket",
            ArgumentKind::IndexJobTicket,
            "Exact JSON ticket returned by index_start; quote it as one shell argument and do not edit its id, epoch, or package.",
        )],
        options: &[
            ArgumentSpec::optional_with_json_name(
                "after-sequence",
                "after_sequence",
                ArgumentKind::Sequence,
                "Return events after this sequence; use next_sequence from the previous observation.",
            ),
            ArgumentSpec::optional_with_json_name(
                "operation-key",
                "operation_key",
                ArgumentKind::Text,
                "Durable operation key returned by add; use this instead of a ticket to reconcile across owner restarts.",
            ),
        ],
        when: "Use for one immediate bounded observation of an exact ticket or durable operation key. Unknown and unresolved observations do not establish publication.",
    },
];
