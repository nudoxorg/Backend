//! Defines args behavior for `interface-cli`, whose purpose is to project the one shared local library onto a command line.
//! This module owns the args invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! The whole grammar: argv in, one typed plan or one usage error that still holds the offending token.
//!
//! ```text
//! nudox [GLOBAL]… <subcommand> [operand] [FLAG]… [GLOBAL]…
//! ```
//!
//! Globals are accepted on either side of the subcommand because a person who has already typed
//! the command and then wants JSON should not have to move the cursor. Every rejection names the
//! exact token that caused it: a usage error that says only "invalid arguments" makes the reader
//! re-read their own command line, which is work the parser already did.

use std::path::PathBuf;

use compiler_ir::LinkKind;
use interface_core::PackageUrl;
use interface_documents::Direction;
use interface_identity::{
    ALL_ECOSYSTEMS, Address, ContentKey, KindTag, PackageCoordinate, PackageVersion,
    ecosystem_tag, parse_ecosystem_tag,
};
use interface_library::{
    COMMANDS, CommandId, CommandSpec, ContextLines, CreateProject, DependentsRequest,
    DetailRequest, ExploreLimit, ExploreRequest, ExploreSort, FollowKey, FollowRequest,
    LockfileBinding, MemberChange, Opener, OwnerHandle, OwnerRequest, PageLocator, PageNumber,
    ProjectHue, ProjectName, ProjectSelector, ReleasesRequest, SyncCompile, SyncRequest,
    TreeCloseRequest, TreeCloseScope, TreeNodeId, TreeOpenRequest, TreeSubject, spec_named,
};
use interface_search::{
    Cursor, Depth, KindSet, Lane, LaneSet, QueryText, QueryTextError, ResultLimit,
    parse_relation,
};

/// How a reply is written to standard output.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Format {
    /// Rendered for a person at a terminal.
    #[default]
    Human,
    /// Rendered for an agent, through the shared Markdown renderer.
    Markdown,
    /// Rendered for a program, as one JSON object.
    Json,
}

/// Whether colour is permitted at all.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ColourChoice {
    /// Colour when standard output is a terminal and the environment allows it.
    #[default]
    Auto,
    /// Never emit an escape code.
    Never,
}

/// Whether this process brings a compiler to the library.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Attach {
    /// Open the production compiler host.
    #[default]
    Production,
    /// Open read-only; adds are refused with a retained operand.
    Detached,
}

/// Every option that applies to any subcommand.
#[derive(Clone, Debug, Default)]
pub(crate) struct Globals {
    /// Output projection.
    pub(crate) format: Format,
    /// Colour permission.
    pub(crate) colour: ColourChoice,
    /// Explicit workspace root, overriding the environment.
    pub(crate) root: Option<PathBuf>,
    /// Compiler policy.
    pub(crate) attach: Attach,
    /// Whether help was requested.
    pub(crate) help: bool,
}

/// One parsed command line.
#[derive(Debug)]
pub(crate) struct Invocation {
    /// Options that apply to any subcommand.
    pub(crate) globals: Globals,
    /// What the caller asked for.
    pub(crate) plan: Plan,
}

/// What the caller asked for, before the shelf is consulted.
///
/// `search` keeps its `--in` operands as the exact text that was typed: resolving `cargo:serde` to
/// a pinned coordinate needs the shelf, and the parser refuses to read durable state.
#[derive(Debug)]
pub(crate) enum Plan {
    /// Print the full help.
    Help,
    /// Print the one-screen dashboard, which is what a bare `nudox` means.
    Dashboard,
    /// List the shelf.
    Packages,
    /// Add one package.
    Add {
        /// Validated package URL the compiler accepts.
        url: PackageUrl,
        /// Exact spelling the caller typed.
        spelled: String,
    },
    /// Remove one package.
    Remove {
        /// Pinned coordinate.
        coordinate: PackageCoordinate,
        /// Exact spelling the caller typed.
        spelled: String,
    },
    /// Show one page.
    Show {
        /// How the page is named.
        locator: PageLocator,
        /// Exact spelling the caller typed.
        spelled: String,
    },
    /// Show one outline.
    Outline {
        /// Pinned coordinate.
        coordinate: PackageCoordinate,
        /// Exact spelling the caller typed.
        spelled: String,
    },
    /// Resolve free text.
    Resolve {
        /// Exact spelling the caller typed.
        spelled: String,
    },
    /// Multi-lane search.
    Search(SearchPlan),
    /// Relation traversal.
    Graph(GraphPlan),
    /// Capability health.
    Health,
    /// Index search over the local registry index.
    IndexSearch {
        /// Validated search text.
        query: interface_library::ExploreQuery,
        /// Page size.
        limit: interface_library::ExploreLimit,
        /// Exact spelling the caller typed.
        spelled: String,
    },
    /// One package's recorded versions.
    PackageVersions {
        /// Validated registry package name.
        name: interface_library::ExplorePackageName,
        /// Exact spelling the caller typed.
        spelled: String,
    },
    /// One package's latest version and history.
    PackageProfile {
        /// Validated registry package name.
        name: interface_library::ExplorePackageName,
        /// Exact spelling the caller typed.
        spelled: String,
    },
    /// One declaration's source excerpt.
    Source {
        /// How the declaration is named.
        locator: PageLocator,
        /// Context lines either side.
        context: ContextLines,
        /// Exact spelling the caller typed.
        spelled: String,
    },
    /// Symbols related to one declaration.
    Related {
        /// How the declaration is named.
        locator: PageLocator,
        /// Most rows.
        limit: ResultLimit,
        /// Exact spelling the caller typed.
        spelled: String,
    },
    /// Browse or search the registries.
    Explore(ExploreRequest),
    /// One registry package's profile.
    Package {
        /// Validated request.
        request: DetailRequest,
        /// Exact spelling the caller typed.
        spelled: String,
    },
    /// One registry package's dependents.
    Dependents {
        /// Validated request.
        request: DependentsRequest,
        /// Exact spelling the caller typed.
        spelled: String,
    },
    /// One owner's packages.
    Owner {
        /// Validated request.
        request: OwnerRequest,
        /// Exact spelling the caller typed.
        spelled: String,
    },
    /// Follow one package.
    Subscribe {
        /// Validated request.
        request: FollowRequest,
        /// Exact spelling the caller typed.
        spelled: String,
    },
    /// Stop following one package.
    Unsubscribe {
        /// What to stop following.
        key: FollowKey,
        /// Exact spelling the caller typed.
        spelled: String,
    },
    /// Every followed package.
    Subscriptions,
    /// New releases.
    Releases {
        /// Whether to mark them seen.
        request: ReleasesRequest,
    },
    /// Every project folder.
    Projects,
    /// Create a folder.
    ProjectCreate {
        /// Validated request.
        request: CreateProject,
        /// Exact spelling the caller typed.
        spelled: String,
    },
    /// Delete a folder.
    ProjectDelete {
        /// Which folder.
        selector: ProjectSelector,
        /// Exact spelling the caller typed.
        spelled: String,
    },
    /// Add a member.
    ProjectAdd {
        /// Validated change.
        change: MemberChange,
        /// Exact spelling the caller typed.
        spelled: String,
    },
    /// Remove a member.
    ProjectRemove {
        /// Validated change.
        change: MemberChange,
        /// Exact spelling the caller typed.
        spelled: String,
    },
    /// Reconcile a folder with its lockfile.
    ProjectSync {
        /// Validated request.
        request: SyncRequest,
        /// Exact spelling the caller typed.
        spelled: String,
    },
    /// The session tree.
    Tree,
    /// Open a subject in the tree.
    TreeOpen {
        /// Validated request.
        request: TreeOpenRequest,
        /// Exact spelling the caller typed.
        spelled: String,
    },
    /// Close a node or branch.
    TreeClose {
        /// Validated request.
        request: TreeCloseRequest,
        /// Exact spelling the caller typed.
        spelled: String,
    },
}

impl Plan {
    /// Registry name, for JSON envelopes and usage errors.
    pub(crate) const fn command_name(&self) -> &'static str {
        match self {
            Self::Help | Self::Dashboard => "help",
            Self::Packages => "packages",
            Self::Add { .. } => "add",
            Self::Remove { .. } => "remove",
            Self::Show { .. } => "show",
            Self::Outline { .. } => "outline",
            Self::Resolve { .. } => "resolve",
            Self::Search(_) => "search",
            Self::Graph(_) => "graph",
            Self::Health => "health",
            Self::IndexSearch { .. } => "index-search",
            Self::PackageVersions { .. } => "package-versions",
            Self::PackageProfile { .. } => "package-profile",
            Self::Source { .. } => "source",
            Self::Related { .. } => "related",
            Self::Explore(_) => "explore",
            Self::Package { .. } => "package",
            Self::Dependents { .. } => "dependents",
            Self::Owner { .. } => "owner",
            Self::Subscribe { .. } => "subscribe",
            Self::Unsubscribe { .. } => "unsubscribe",
            Self::Subscriptions => "subscriptions",
            Self::Releases { .. } => "releases",
            Self::Projects => "projects",
            Self::ProjectCreate { .. } => "project-create",
            Self::ProjectDelete { .. } => "project-delete",
            Self::ProjectAdd { .. } => "project-add",
            Self::ProjectRemove { .. } => "project-remove",
            Self::ProjectSync { .. } => "project-sync",
            Self::Tree => "tree",
            Self::TreeOpen { .. } => "tree-open",
            Self::TreeClose { .. } => "tree-close",
        }
    }

    /// The exact operand the caller typed, when there was one.
    pub(crate) fn subject(&self) -> Option<&str> {
        match self {
            Self::Help | Self::Dashboard | Self::Packages | Self::Health => None,
            Self::Add { spelled, .. }
            | Self::Remove { spelled, .. }
            | Self::Show { spelled, .. }
            | Self::Outline { spelled, .. }
            | Self::Resolve { spelled } => Some(spelled),
            Self::Search(plan) => Some(&plan.spelled),
            Self::Graph(plan) => Some(&plan.spelled),
            Self::IndexSearch { spelled, .. }
            | Self::PackageVersions { spelled, .. }
            | Self::PackageProfile { spelled, .. }
            | Self::Source { spelled, .. }
            | Self::Related { spelled, .. }
            | Self::Package { spelled, .. }
            | Self::Dependents { spelled, .. }
            | Self::Owner { spelled, .. }
            | Self::Subscribe { spelled, .. }
            | Self::Unsubscribe { spelled, .. }
            | Self::ProjectCreate { spelled, .. }
            | Self::ProjectDelete { spelled, .. }
            | Self::ProjectAdd { spelled, .. }
            | Self::ProjectRemove { spelled, .. }
            | Self::ProjectSync { spelled, .. }
            | Self::TreeOpen { spelled, .. }
            | Self::TreeClose { spelled, .. } => Some(spelled),
            Self::Explore(request) => request.query.as_ref().map(|query| query.as_str()),
            Self::Subscriptions | Self::Releases { .. } | Self::Projects | Self::Tree => None,
        }
    }
}

/// A search, with its scope still spelled the way the caller typed it.
#[derive(Debug)]
pub(crate) struct SearchPlan {
    /// Bounded query text.
    pub(crate) text: QueryText,
    /// `--in` operands, exactly as typed.
    pub(crate) scope: Vec<String>,
    /// Admitted kinds.
    pub(crate) kinds: KindSet,
    /// Requested lanes.
    pub(crate) lanes: LaneSet,
    /// Page size.
    pub(crate) limit: ResultLimit,
    /// Continuation.
    pub(crate) cursor: Option<Cursor>,
    /// Exact query spelling.
    pub(crate) spelled: String,
}

/// A traversal.
#[derive(Debug)]
pub(crate) struct GraphPlan {
    /// Start node.
    pub(crate) source: interface_search::GraphSource,
    /// Restrict to one relation.
    pub(crate) kind: Option<LinkKind>,
    /// Traversal direction.
    pub(crate) direction: Direction,
    /// Hops.
    pub(crate) depth: Depth,
    /// Edge budget.
    pub(crate) limit: ResultLimit,
    /// Exact address spelling.
    pub(crate) spelled: String,
}

/// Why a command line was refused, always with the token that caused it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UsageError {
    /// Closed cause.
    pub(crate) kind: UsageKind,
    /// The exact offending token, never paraphrased.
    pub(crate) token: String,
    /// Subcommand in scope when the token was read.
    pub(crate) command: Option<&'static str>,
}

/// Closed usage rejection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum UsageKind {
    /// The first operand was not a registry name.
    UnknownCommand {
        /// Registry names closest to what was typed.
        nearest: Vec<&'static str>,
    },
    /// The flag is not accepted here.
    UnknownFlag,
    /// The flag needs a value and none followed.
    MissingValue,
    /// The subcommand needs an operand.
    MissingOperand {
        /// What the operand is called in help.
        operand: &'static str,
    },
    /// A second operand was supplied where one is accepted.
    UnexpectedOperand,
    /// The value is not one this flag accepts.
    BadValue {
        /// Every accepted spelling, or the candidates that were ambiguous.
        accepted: Vec<String>,
    },
    /// The operand did not parse, with the typed cause retained.
    BadOperand {
        /// Reader-facing spelling of the exact cause.
        cause: String,
    },
    /// Two flags cannot both be given.
    ConflictingFlags {
        /// The flag already in force.
        other: String,
    },
}

impl UsageKind {
    /// Stable slug, the same word the human sees and the JSON envelope carries.
    pub(crate) const fn slug(&self) -> &'static str {
        match self {
            Self::UnknownCommand { .. } => "unknown-command",
            Self::UnknownFlag => "unknown-flag",
            Self::MissingValue => "missing-value",
            Self::MissingOperand { .. } => "missing-operand",
            Self::UnexpectedOperand => "unexpected-operand",
            Self::BadValue { .. } => "bad-value",
            Self::BadOperand { .. } => "bad-operand",
            Self::ConflictingFlags { .. } => "conflicting-flags",
        }
    }
}

/// One accepted flag of one subcommand.
struct FlagSpec {
    name: &'static str,
    value: bool,
}

const SEARCH_FLAGS: [FlagSpec; 5] = [
    FlagSpec { name: "--in", value: true },
    FlagSpec { name: "--kinds", value: true },
    FlagSpec { name: "--lanes", value: true },
    FlagSpec { name: "--limit", value: true },
    FlagSpec { name: "--cursor", value: true },
];

const GRAPH_FLAGS: [FlagSpec; 5] = [
    FlagSpec { name: "--kind", value: true },
    FlagSpec { name: "--incoming", value: false },
    FlagSpec { name: "--outgoing", value: false },
    FlagSpec { name: "--depth", value: true },
    FlagSpec { name: "--limit", value: true },
];

const INDEX_SEARCH_FLAGS: [FlagSpec; 1] =
    [FlagSpec { name: "--limit", value: true }];

const CONTEXT_FLAGS: [FlagSpec; 1] = [FlagSpec { name: "--context", value: true }];
const LIMIT_FLAGS: [FlagSpec; 1] = [FlagSpec { name: "--limit", value: true }];
const EXPLORE_FLAGS: [FlagSpec; 4] = [
    FlagSpec { name: "--ecosystem", value: true },
    FlagSpec { name: "--sort", value: true },
    FlagSpec { name: "--page", value: true },
    FlagSpec { name: "--limit", value: true },
];
const PAGED_FLAGS: [FlagSpec; 2] = [
    FlagSpec { name: "--page", value: true },
    FlagSpec { name: "--limit", value: true },
];
const ECOSYSTEM_FLAGS: [FlagSpec; 1] = [FlagSpec { name: "--ecosystem", value: true }];
const PROJECT_FLAGS: [FlagSpec; 1] = [FlagSpec { name: "--project", value: true }];
const SEEN_FLAGS: [FlagSpec; 1] = [FlagSpec { name: "--seen", value: false }];
const PROJECT_CREATE_FLAGS: [FlagSpec; 2] = [
    FlagSpec { name: "--lockfile", value: true },
    FlagSpec { name: "--hue", value: true },
];
const COMPILE_FLAGS: [FlagSpec; 1] = [FlagSpec { name: "--compile", value: false }];
const TREE_OPEN_FLAGS: [FlagSpec; 4] = [
    FlagSpec { name: "--parent", value: true },
    FlagSpec { name: "--as", value: true },
    FlagSpec { name: "--title", value: true },
    FlagSpec { name: "--no-focus", value: false },
];
const BRANCH_FLAGS: [FlagSpec; 1] = [FlagSpec { name: "--branch", value: false }];

const fn flags_of(id: CommandId) -> &'static [FlagSpec] {
    match id {
        CommandId::Search => &SEARCH_FLAGS,
        CommandId::Graph => &GRAPH_FLAGS,
        CommandId::IndexSearch => &INDEX_SEARCH_FLAGS,
        CommandId::Source => &CONTEXT_FLAGS,
        CommandId::Related => &LIMIT_FLAGS,
        CommandId::Explore => &EXPLORE_FLAGS,
        CommandId::Dependents => &PAGED_FLAGS,
        CommandId::Owner => &ECOSYSTEM_FLAGS,
        CommandId::Subscribe | CommandId::ProjectAdd | CommandId::ProjectRemove => &PROJECT_FLAGS,
        CommandId::Releases => &SEEN_FLAGS,
        CommandId::ProjectCreate => &PROJECT_CREATE_FLAGS,
        CommandId::ProjectSync => &COMPILE_FLAGS,
        CommandId::TreeOpen => &TREE_OPEN_FLAGS,
        CommandId::TreeClose => &BRANCH_FLAGS,
        _ => &[],
    }
}

/// The operand name each subcommand reports when it is missing one.
const fn operand_of(id: CommandId) -> Option<&'static str> {
    match id {
        CommandId::Packages | CommandId::Health => None,
        CommandId::Add => Some("<package>"),
        CommandId::Remove | CommandId::Outline => Some("<coordinate>"),
        CommandId::Show | CommandId::Graph => Some("<address>"),
        CommandId::Resolve => Some("<text>"),
        CommandId::Search => Some("<query>"),
        CommandId::IndexSearch => Some("<query>"),
        CommandId::PackageVersions | CommandId::PackageProfile => Some("<package>"),
        CommandId::Source | CommandId::Related => Some("<address>"),
        CommandId::Explore
        | CommandId::Subscriptions
        | CommandId::Releases
        | CommandId::Projects
        | CommandId::Tree => None,
        CommandId::Package
        | CommandId::Dependents
        | CommandId::Subscribe
        | CommandId::Unsubscribe => Some("<ecosystem:name>"),
        CommandId::Owner => Some("<handle>"),
        CommandId::ProjectCreate => Some("<name>"),
        CommandId::ProjectDelete | CommandId::ProjectSync => Some("<project>"),
        CommandId::ProjectAdd | CommandId::ProjectRemove => Some("<coordinate>"),
        CommandId::TreeOpen => Some("<subject>"),
        CommandId::TreeClose => Some("<node>"),
    }
}

/// One flag and the value that followed it.
struct Flag {
    name: &'static str,
    value: Option<String>,
}

struct Scan {
    items: Vec<String>,
    index: usize,
    literal: bool,
}

impl Scan {
    fn next(&mut self) -> Option<String> {
        let item = self.items.get(self.index).cloned();
        if item.is_some() {
            self.index = self.index.saturating_add(1);
        }
        item
    }
}

/// Parses one command line.
pub(crate) fn parse(arguments: Vec<String>) -> Result<Invocation, UsageError> {
    let mut scan = Scan { items: arguments, index: 0, literal: false };
    let mut globals = Globals::default();
    let mut spec: Option<CommandSpec> = None;
    let mut operands: Vec<String> = Vec::new();
    let mut flags: Vec<Flag> = Vec::new();
    while let Some(token) = scan.next() {
        if !scan.literal && token == "--" {
            scan.literal = true;
        } else if !scan.literal && is_option(&token) {
            read_option(&token, &mut scan, &mut globals, spec, &mut flags)?;
        } else if spec.is_none() {
            spec = Some(subcommand(&token)?);
        } else {
            operands.push(token);
        }
    }
    if globals.help {
        return Ok(Invocation { globals, plan: Plan::Help });
    }
    let Some(spec) = spec else {
        return Ok(Invocation { globals, plan: Plan::Dashboard });
    };
    let plan = build(spec, operands, &flags)?;
    Ok(Invocation { globals, plan })
}

fn is_option(token: &str) -> bool {
    token == "-h" || (token.starts_with('-') && token.len() > 1)
}

fn subcommand(token: &str) -> Result<CommandSpec, UsageError> {
    spec_named(token).ok_or_else(|| UsageError {
        kind: UsageKind::UnknownCommand { nearest: nearest_commands(token) },
        token: token.to_owned(),
        command: None,
    })
}

/// Registry names sharing the longest leading run with what was typed.
fn nearest_commands(token: &str) -> Vec<&'static str> {
    let mut scored: Vec<(usize, &'static str)> = COMMANDS
        .into_iter()
        .map(|row| (shared_prefix(row.name, token), row.name))
        .collect();
    scored.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(right.1)));
    scored
        .into_iter()
        .filter(|(score, _)| *score > 0)
        .take(3)
        .map(|(_, name)| name)
        .collect()
}

fn shared_prefix(left: &str, right: &str) -> usize {
    left.chars()
        .zip(right.chars())
        .take_while(|(left, right)| left == right)
        .count()
}

/// Reads one option: a global wherever it appears, otherwise a flag of the subcommand in scope.
fn read_option(
    token: &str,
    scan: &mut Scan,
    globals: &mut Globals,
    spec: Option<CommandSpec>,
    flags: &mut Vec<Flag>,
) -> Result<(), UsageError> {
    let (name, inline) = match token.split_once('=') {
        Some((name, value)) => (name, Some(value.to_owned())),
        None => (token, None),
    };
    let command = spec.map(|spec| spec.name);
    if read_global(name, inline.clone(), scan, globals, command)? {
        return Ok(());
    }
    let Some(spec) = spec else {
        return Err(UsageError { kind: UsageKind::UnknownFlag, token: name.to_owned(), command });
    };
    let Some(flag) = flags_of(spec.id).iter().find(|flag| flag.name == name) else {
        return Err(UsageError { kind: UsageKind::UnknownFlag, token: name.to_owned(), command });
    };
    let value = if flag.value {
        Some(value_for(name, inline, scan, command)?)
    } else {
        None
    };
    flags.push(Flag { name: flag.name, value });
    Ok(())
}

fn read_global(
    name: &str,
    inline: Option<String>,
    scan: &mut Scan,
    globals: &mut Globals,
    command: Option<&'static str>,
) -> Result<bool, UsageError> {
    match name {
        "-h" | "--help" => globals.help = true,
        "--no-color" => globals.colour = ColourChoice::Never,
        "--detached" => globals.attach = Attach::Detached,
        "--root" => globals.root = Some(PathBuf::from(value_for(name, inline, scan, command)?)),
        "--format" => {
            let value = value_for(name, inline, scan, command)?;
            globals.format = match value.as_str() {
                "human" => Format::Human,
                "markdown" => Format::Markdown,
                "json" => Format::Json,
                _ => {
                    return Err(UsageError {
                        kind: UsageKind::BadValue {
                            accepted: ["human", "markdown", "json"]
                                .into_iter()
                                .map(str::to_owned)
                                .collect(),
                        },
                        token: value,
                        command,
                    });
                }
            };
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn value_for(
    name: &str,
    inline: Option<String>,
    scan: &mut Scan,
    command: Option<&'static str>,
) -> Result<String, UsageError> {
    if let Some(value) = inline {
        return Ok(value);
    }
    scan.next().ok_or_else(|| UsageError {
        kind: UsageKind::MissingValue,
        token: name.to_owned(),
        command,
    })
}

fn build(spec: CommandSpec, operands: Vec<String>, flags: &[Flag]) -> Result<Plan, UsageError> {
    let mut operands = operands.into_iter();
    let first = operands.next();
    if let Some(extra) = operands.next() {
        return Err(UsageError {
            kind: UsageKind::UnexpectedOperand,
            token: extra,
            command: Some(spec.name),
        });
    }
    if spec.id == CommandId::Explore {
        return explore_plan(spec, first, flags);
    }
    let operand = match (operand_of(spec.id), first) {
        (Some(_), Some(operand)) => Some(operand),
        (Some(name), None) => {
            return Err(UsageError {
                kind: UsageKind::MissingOperand { operand: name },
                token: spec.name.to_owned(),
                command: Some(spec.name),
            });
        }
        (None, Some(extra)) => {
            return Err(UsageError {
                kind: UsageKind::UnexpectedOperand,
                token: extra,
                command: Some(spec.name),
            });
        }
        (None, None) => None,
    };
    match spec.id {
        CommandId::Packages => Ok(Plan::Packages),
        CommandId::Health => Ok(Plan::Health),
        CommandId::Add => add_plan(spec, operand.unwrap_or_default()),
        CommandId::Remove => coordinate_plan(spec, operand.unwrap_or_default(), false),
        CommandId::Outline => coordinate_plan(spec, operand.unwrap_or_default(), true),
        CommandId::Resolve => Ok(Plan::Resolve { spelled: operand.unwrap_or_default() }),
        CommandId::Show => show_plan(spec, operand.unwrap_or_default()),
        CommandId::Search => search_plan(spec, operand.unwrap_or_default(), flags),
        CommandId::Graph => graph_plan(spec, operand.unwrap_or_default(), flags),
        CommandId::IndexSearch => {
            index_search_plan(spec, operand.unwrap_or_default(), flags)
        }
        CommandId::PackageVersions => {
            package_name_plan(spec, operand.unwrap_or_default(), false)
        }
        CommandId::PackageProfile => package_name_plan(spec, operand.unwrap_or_default(), true),
        CommandId::Source => source_plan(spec, operand.unwrap_or_default(), flags),
        CommandId::Related => related_plan(spec, operand.unwrap_or_default(), flags),
        CommandId::Explore => explore_plan(spec, operand, flags),
        CommandId::Package => package_plan(spec, operand.unwrap_or_default()),
        CommandId::Dependents => dependents_plan(spec, operand.unwrap_or_default(), flags),
        CommandId::Owner => owner_plan(spec, operand.unwrap_or_default(), flags),
        CommandId::Subscribe => subscribe_plan(spec, operand.unwrap_or_default(), flags),
        CommandId::Unsubscribe => unsubscribe_plan(spec, operand.unwrap_or_default()),
        CommandId::Subscriptions => Ok(Plan::Subscriptions),
        CommandId::Releases => Ok(Plan::Releases {
            request: ReleasesRequest {
                mark_seen: flags.iter().any(|flag| flag.name == "--seen"),
            },
        }),
        CommandId::Projects => Ok(Plan::Projects),
        CommandId::ProjectCreate => project_create_plan(spec, operand.unwrap_or_default(), flags),
        CommandId::ProjectDelete => project_delete_plan(spec, operand.unwrap_or_default()),
        CommandId::ProjectAdd => member_change_plan(spec, operand.unwrap_or_default(), flags, true),
        CommandId::ProjectRemove => {
            member_change_plan(spec, operand.unwrap_or_default(), flags, false)
        }
        CommandId::ProjectSync => project_sync_plan(spec, operand.unwrap_or_default(), flags),
        CommandId::Tree => Ok(Plan::Tree),
        CommandId::TreeOpen => tree_open_plan(spec, operand.unwrap_or_default(), flags),
        CommandId::TreeClose => tree_close_plan(spec, operand.unwrap_or_default(), flags),
    }
}

/// `add` accepts a package URL as a manifest spells it or a coordinate as an address spells it,
/// because both are things a reader has in front of them; only one of the two reaches the compiler.
fn add_plan(spec: CommandSpec, spelled: String) -> Result<Plan, UsageError> {
    let text = if spelled.starts_with("pkg:") {
        spelled.clone()
    } else {
        PackageCoordinate::parse(&spelled)
            .map_err(|cause| UsageError {
                kind: UsageKind::BadOperand {
                    cause: interface_library::render::text::coordinate_parse_detail(cause)
                        .to_owned(),
                },
                token: spelled.clone(),
                command: Some(spec.name),
            })?
            .package_url_text()
    };
    let url = PackageUrl::try_from(text).map_err(|rejected| UsageError {
        kind: UsageKind::BadOperand {
            cause: format!(
                "the {} of the package url is not spelled correctly",
                interface_library::render::common::package_url_slug(rejected.error)
            ),
        },
        token: spelled.clone(),
        command: Some(spec.name),
    })?;
    Ok(Plan::Add { url, spelled })
}

fn coordinate_plan(spec: CommandSpec, spelled: String, outline: bool) -> Result<Plan, UsageError> {
    let coordinate = PackageCoordinate::parse(&spelled).map_err(|cause| UsageError {
        kind: UsageKind::BadOperand {
            cause: interface_library::render::text::coordinate_parse_detail(cause).to_owned(),
        },
        token: spelled.clone(),
        command: Some(spec.name),
    })?;
    Ok(if outline {
        Plan::Outline { coordinate, spelled }
    } else {
        Plan::Remove { coordinate, spelled }
    })
}

/// A bare key names a declaration across every loaded package; anything else is an address.
fn show_plan(spec: CommandSpec, spelled: String) -> Result<Plan, UsageError> {
    if let Ok((key, _)) = ContentKey::parse(&spelled) {
        return Ok(Plan::Show { locator: PageLocator::Key(key), spelled });
    }
    let address = Address::parse(&spelled).map_err(|cause| UsageError {
        kind: UsageKind::BadOperand {
            cause: interface_library::render::text::address_parse_detail(cause),
        },
        token: spelled.clone(),
        command: Some(spec.name),
    })?;
    Ok(Plan::Show { locator: PageLocator::Address(address), spelled })
}

fn search_plan(spec: CommandSpec, spelled: String, flags: &[Flag]) -> Result<Plan, UsageError> {
    let text = QueryText::new(&spelled).map_err(|cause| UsageError {
        kind: UsageKind::BadOperand { cause: query_detail(cause) },
        token: spelled.clone(),
        command: Some(spec.name),
    })?;
    let mut plan = SearchPlan {
        text,
        scope: Vec::new(),
        kinds: KindSet::BROWSABLE,
        lanes: LaneSet::ALL,
        limit: ResultLimit::default(),
        cursor: None,
        spelled,
    };
    for flag in flags {
        let value = flag.value.clone().unwrap_or_default();
        match flag.name {
            "--in" => plan.scope.extend(split_list(&value)),
            "--kinds" => plan.kinds = parse_kinds(&value, spec)?,
            "--lanes" => plan.lanes = parse_lanes(&value, spec)?,
            "--limit" => plan.limit = ResultLimit::clamped(parse_number(flag.name, &value, spec)?),
            "--cursor" => plan.cursor = Some(Cursor(parse_number(flag.name, &value, spec)?)),
            _ => {}
        }
    }
    Ok(Plan::Search(plan))
}

fn graph_plan(spec: CommandSpec, spelled: String, flags: &[Flag]) -> Result<Plan, UsageError> {
    let source = graph_source(spec, &spelled)?;
    let mut kind = None;
    let mut direction: Option<(Direction, String)> = None;
    let mut depth = Depth::ONE;
    let mut limit = ResultLimit::default();
    for flag in flags {
        let value = flag.value.clone().unwrap_or_default();
        match flag.name {
            "--kind" => {
                let (link, way) = parse_relation(&value).ok_or_else(|| UsageError {
                    kind: UsageKind::BadValue { accepted: relation_spellings() },
                    token: value.clone(),
                    command: Some(spec.name),
                })?;
                kind = Some(link);
                direction = claim_direction(direction, way, &value, spec)?;
            }
            "--incoming" => {
                direction = claim_direction(direction, Direction::Incoming, flag.name, spec)?;
            }
            "--outgoing" => {
                direction = claim_direction(direction, Direction::Outgoing, flag.name, spec)?;
            }
            "--depth" => depth = Depth::clamped(parse_number(flag.name, &value, spec)?),
            "--limit" => limit = ResultLimit::clamped(parse_number(flag.name, &value, spec)?),
            _ => {}
        }
    }
    Ok(Plan::Graph(GraphPlan {
        source,
        kind,
        direction: direction.map_or(Direction::Outgoing, |(way, _)| way),
        depth,
        limit,
        spelled,
    }))
}

fn graph_source(
    spec: CommandSpec,
    spelled: &str,
) -> Result<interface_search::GraphSource, UsageError> {
    if let Ok((key, _)) = ContentKey::parse(spelled) {
        return Ok(interface_search::GraphSource::Key(key));
    }
    Address::parse(spelled)
        .map(interface_search::GraphSource::Address)
        .map_err(|cause| UsageError {
            kind: UsageKind::BadOperand {
                cause: interface_library::render::text::address_parse_detail(cause),
            },
            token: spelled.to_owned(),
            command: Some(spec.name),
        })
}

/// Two flags that both name a direction are a contradiction, not a last-one-wins.
fn claim_direction(
    held: Option<(Direction, String)>,
    wanted: Direction,
    token: &str,
    spec: CommandSpec,
) -> Result<Option<(Direction, String)>, UsageError> {
    match held {
        Some((way, other)) if way != wanted => Err(UsageError {
            kind: UsageKind::ConflictingFlags { other },
            token: token.to_owned(),
            command: Some(spec.name),
        }),
        _ => Ok(Some((wanted, token.to_owned()))),
    }
}

fn relation_spellings() -> Vec<String> {
    interface_search::LINK_KINDS
        .into_iter()
        .flat_map(|kind| {
            [Direction::Outgoing, Direction::Incoming]
                .into_iter()
                .map(move |way| interface_search::relation_label(kind, way).as_str().to_owned())
        })
        .collect()
}

fn split_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

fn parse_kinds(value: &str, spec: CommandSpec) -> Result<KindSet, UsageError> {
    let mut kinds = KindSet::EMPTY;
    for item in split_list(value) {
        let kind = KindTag::parse(&item).ok_or_else(|| UsageError {
            kind: UsageKind::BadValue { accepted: kind_spellings() },
            token: item.clone(),
            command: Some(spec.name),
        })?;
        kinds = kinds.with(kind);
    }
    if kinds.is_empty() {
        return Err(UsageError {
            kind: UsageKind::BadValue { accepted: kind_spellings() },
            token: value.to_owned(),
            command: Some(spec.name),
        });
    }
    Ok(kinds)
}

fn kind_spellings() -> Vec<String> {
    compiler_ir_vocabulary::EntityKind::ALL
        .into_iter()
        .map(|kind| KindTag::of(kind).as_str().to_owned())
        .collect()
}

fn parse_lanes(value: &str, spec: CommandSpec) -> Result<LaneSet, UsageError> {
    let mut lanes = LaneSet::EMPTY;
    for item in split_list(value) {
        let lane = Lane::ALL
            .into_iter()
            .find(|lane| lane.label() == item)
            .ok_or_else(|| UsageError {
                kind: UsageKind::BadValue { accepted: lane_spellings() },
                token: item.clone(),
                command: Some(spec.name),
            })?;
        lanes = lanes.with(lane);
    }
    if lanes.iter().next().is_none() {
        return Err(UsageError {
            kind: UsageKind::BadValue { accepted: lane_spellings() },
            token: value.to_owned(),
            command: Some(spec.name),
        });
    }
    Ok(lanes)
}

fn lane_spellings() -> Vec<String> {
    Lane::ALL.into_iter().map(|lane| lane.label().to_owned()).collect()
}

fn parse_number<Number: core::str::FromStr>(
    name: &str,
    value: &str,
    spec: CommandSpec,
) -> Result<Number, UsageError> {
    value.parse::<Number>().map_err(|_| UsageError {
        kind: UsageKind::BadValue { accepted: vec![format!("{name} takes a whole number")] },
        token: value.to_owned(),
        command: Some(spec.name),
    })
}

fn query_detail(cause: QueryTextError) -> String {
    match cause {
        QueryTextError::Empty => "the query is empty".to_owned(),
        QueryTextError::TooLong { observed, maximum } => {
            format!("the query is {observed} bytes; at most {maximum} are accepted")
        }
    }
}

/// `index-search` takes bounded query text and an optional page size.
fn index_search_plan(
    spec: CommandSpec,
    spelled: String,
    flags: &[Flag],
) -> Result<Plan, UsageError> {
    let query = interface_library::ExploreQuery::new(&spelled).map_err(|cause| UsageError {
        kind: UsageKind::BadOperand { cause: explore_query_detail(cause) },
        token: spelled.clone(),
        command: Some(spec.name),
    })?;
    let mut limit = interface_library::ExploreLimit::default();
    for flag in flags {
        if flag.name == "--limit" {
            let value = flag.value.clone().unwrap_or_default();
            limit = interface_library::ExploreLimit::clamped(parse_number(flag.name, &value, spec)?);
        }
    }
    Ok(Plan::IndexSearch { query, limit, spelled })
}

/// `package-versions` and `package-profile` take one validated registry package name.
fn package_name_plan(
    spec: CommandSpec,
    spelled: String,
    profile: bool,
) -> Result<Plan, UsageError> {
    let name = interface_library::ExplorePackageName::new(&spelled).map_err(|cause| UsageError {
        kind: UsageKind::BadOperand { cause: package_name_detail(cause) },
        token: spelled.clone(),
        command: Some(spec.name),
    })?;
    Ok(if profile {
        Plan::PackageProfile { name, spelled }
    } else {
        Plan::PackageVersions { name, spelled }
    })
}

fn explore_query_detail(cause: interface_library::ExploreQueryError) -> String {
    match cause {
        interface_library::ExploreQueryError::Empty => "the query is empty".to_owned(),
        interface_library::ExploreQueryError::TooLong { observed, maximum } => {
            format!("the query is {observed} bytes; at most {maximum} are accepted")
        }
    }
}

fn package_name_detail(cause: interface_library::ExplorePackageNameError) -> String {
    match cause {
        interface_library::ExplorePackageNameError::Empty => "the package name is empty".to_owned(),
        interface_library::ExplorePackageNameError::TooLong { observed, maximum } => {
            format!("the package name is {observed} bytes; at most {maximum} are accepted")
        }
    }
}

// ── the rewritten surface's rows ────────────────────────────────────────────────────────────────

/// `source` and `related` take the same locator `show` does.
fn locator_of(spec: CommandSpec, spelled: &str) -> Result<PageLocator, UsageError> {
    if let Ok((key, _)) = ContentKey::parse(spelled) {
        return Ok(PageLocator::Key(key));
    }
    Address::parse(spelled)
        .map(PageLocator::Address)
        .map_err(|cause| UsageError {
            kind: UsageKind::BadOperand {
                cause: interface_library::render::text::address_parse_detail(cause),
            },
            token: spelled.to_owned(),
            command: Some(spec.name),
        })
}

fn source_plan(spec: CommandSpec, spelled: String, flags: &[Flag]) -> Result<Plan, UsageError> {
    let locator = locator_of(spec, &spelled)?;
    let mut context = ContextLines::default();
    for flag in flags {
        if flag.name == "--context" {
            let value = flag.value.clone().unwrap_or_default();
            context = ContextLines::clamped(parse_number(flag.name, &value, spec)?);
        }
    }
    Ok(Plan::Source {
        locator,
        context,
        spelled,
    })
}

fn related_plan(spec: CommandSpec, spelled: String, flags: &[Flag]) -> Result<Plan, UsageError> {
    let locator = locator_of(spec, &spelled)?;
    let mut limit = ResultLimit::default();
    for flag in flags {
        if flag.name == "--limit" {
            let value = flag.value.clone().unwrap_or_default();
            limit = ResultLimit::clamped(parse_number(flag.name, &value, spec)?);
        }
    }
    Ok(Plan::Related {
        locator,
        limit,
        spelled,
    })
}

fn ecosystem_flag(
    spec: CommandSpec,
    flags: &[Flag],
) -> Result<Option<interface_core::PackageEcosystem>, UsageError> {
    let Some(flag) = flags.iter().find(|flag| flag.name == "--ecosystem") else {
        return Ok(None);
    };
    let value = flag.value.clone().unwrap_or_default();
    parse_ecosystem_tag(&value).map(Some).ok_or_else(|| UsageError {
        kind: UsageKind::BadValue {
            accepted: ecosystem_spellings(),
        },
        token: value,
        command: Some(spec.name),
    })
}

fn ecosystem_spellings() -> Vec<String> {
    ALL_ECOSYSTEMS
        .into_iter()
        .map(|ecosystem| ecosystem_tag(ecosystem).as_str().to_owned())
        .collect()
}

fn page_flag(spec: CommandSpec, flags: &[Flag]) -> Result<PageNumber, UsageError> {
    let Some(flag) = flags.iter().find(|flag| flag.name == "--page") else {
        return Ok(PageNumber::FIRST);
    };
    let value = flag.value.clone().unwrap_or_default();
    Ok(PageNumber::clamped(parse_number(flag.name, &value, spec)?))
}

fn explore_limit_flag(spec: CommandSpec, flags: &[Flag]) -> Result<ExploreLimit, UsageError> {
    let Some(flag) = flags.iter().find(|flag| flag.name == "--limit") else {
        return Ok(ExploreLimit::default());
    };
    let value = flag.value.clone().unwrap_or_default();
    Ok(ExploreLimit::clamped(parse_number(flag.name, &value, spec)?))
}

/// `explore` takes an optional query and browses the top of the sort without one.
fn explore_plan(spec: CommandSpec, spelled: Option<String>, flags: &[Flag]) -> Result<Plan, UsageError> {
    let query = match spelled.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => Some(interface_library::ExploreQuery::new(text).map_err(|cause| UsageError {
            kind: UsageKind::BadOperand {
                cause: explore_query_detail(cause),
            },
            token: text.to_owned(),
            command: Some(spec.name),
        })?),
        None => None,
    };
    let mut sort = ExploreSort::default();
    for flag in flags {
        if flag.name == "--sort" {
            let value = flag.value.clone().unwrap_or_default();
            sort = ExploreSort::parse(&value).ok_or_else(|| UsageError {
                kind: UsageKind::BadValue {
                    accepted: ExploreSort::ALL
                        .into_iter()
                        .map(|sort| sort.name().to_owned())
                        .collect(),
                },
                token: value,
                command: Some(spec.name),
            })?;
        }
    }
    Ok(Plan::Explore(ExploreRequest {
        ecosystem: ecosystem_flag(spec, flags)?,
        query,
        sort,
        page: page_flag(spec, flags)?,
        limit: explore_limit_flag(spec, flags)?,
    }))
}

/// `ecosystem:name` with an optional `@version`, as every registry row is spelled.
fn follow_key_of(spec: CommandSpec, spelled: &str) -> Result<(FollowKey, Option<PackageVersion>), UsageError> {
    FollowKey::parse_pinned(spelled).map_err(|cause| UsageError {
        kind: UsageKind::BadOperand {
            cause: follow_key_detail(cause),
        },
        token: spelled.to_owned(),
        command: Some(spec.name),
    })
}

fn follow_key_detail(cause: interface_library::FollowKeyError) -> String {
    match cause {
        interface_library::FollowKeyError::MissingEcosystem => {
            "spell the package as ecosystem:name, such as cargo:serde".to_owned()
        }
        interface_library::FollowKeyError::UnknownEcosystem => format!(
            "the ecosystem must be one of {}",
            ecosystem_spellings().join(" ")
        ),
        interface_library::FollowKeyError::Name(cause) => {
            interface_library::render::text::coordinate_parse_detail(cause).to_owned()
        }
    }
}

fn explore_name_of(spec: CommandSpec, key: &FollowKey) -> Result<interface_library::ExplorePackageName, UsageError> {
    interface_library::ExplorePackageName::new(key.name.as_str()).map_err(|cause| UsageError {
        kind: UsageKind::BadOperand {
            cause: package_name_detail(cause),
        },
        token: key.name.as_str().to_owned(),
        command: Some(spec.name),
    })
}

fn package_plan(spec: CommandSpec, spelled: String) -> Result<Plan, UsageError> {
    let (key, version) = follow_key_of(spec, &spelled)?;
    let name = explore_name_of(spec, &key)?;
    Ok(Plan::Package {
        request: DetailRequest {
            ecosystem: key.ecosystem,
            name,
            version,
        },
        spelled,
    })
}

fn dependents_plan(spec: CommandSpec, spelled: String, flags: &[Flag]) -> Result<Plan, UsageError> {
    let (key, _) = follow_key_of(spec, &spelled)?;
    let name = explore_name_of(spec, &key)?;
    Ok(Plan::Dependents {
        request: DependentsRequest {
            ecosystem: key.ecosystem,
            name,
            page: page_flag(spec, flags)?,
            limit: explore_limit_flag(spec, flags)?,
        },
        spelled,
    })
}

/// `owner` takes `handle` or `ecosystem:handle`; `--ecosystem` narrows a bare handle.
fn owner_plan(spec: CommandSpec, spelled: String, flags: &[Flag]) -> Result<Plan, UsageError> {
    let (ecosystem, handle) = match spelled.split_once(':') {
        Some((tag, handle)) if parse_ecosystem_tag(tag).is_some() => {
            (parse_ecosystem_tag(tag), handle)
        }
        _ => (ecosystem_flag(spec, flags)?, spelled.trim_start_matches('@')),
    };
    let handle = OwnerHandle::new(handle).map_err(|cause| UsageError {
        kind: UsageKind::BadOperand {
            cause: match cause {
                interface_library::OwnerHandleError::Empty => "the handle is empty".to_owned(),
                interface_library::OwnerHandleError::TooLong { observed, maximum } => {
                    format!("the handle is {observed} bytes; at most {maximum} are accepted")
                }
                interface_library::OwnerHandleError::Character => {
                    "the handle carries whitespace".to_owned()
                }
            },
        },
        token: spelled.clone(),
        command: Some(spec.name),
    })?;
    Ok(Plan::Owner {
        request: OwnerRequest { ecosystem, handle },
        spelled,
    })
}

fn project_selector_of(spec: CommandSpec, spelled: &str) -> Result<ProjectSelector, UsageError> {
    ProjectSelector::parse(spelled).map_err(|cause| UsageError {
        kind: UsageKind::BadOperand {
            cause: project_name_detail(cause),
        },
        token: spelled.to_owned(),
        command: Some(spec.name),
    })
}

fn project_name_detail(cause: interface_library::ProjectNameError) -> String {
    match cause {
        interface_library::ProjectNameError::Empty => "the project name is empty".to_owned(),
        interface_library::ProjectNameError::TooLong { observed, maximum } => {
            format!("the project name is {observed} bytes; at most {maximum} are accepted")
        }
        interface_library::ProjectNameError::Character => {
            "the project name carries a control character".to_owned()
        }
    }
}

fn project_flag(spec: CommandSpec, flags: &[Flag]) -> Result<Option<ProjectSelector>, UsageError> {
    let Some(flag) = flags.iter().find(|flag| flag.name == "--project") else {
        return Ok(None);
    };
    let value = flag.value.clone().unwrap_or_default();
    project_selector_of(spec, &value).map(Some)
}

fn subscribe_plan(spec: CommandSpec, spelled: String, flags: &[Flag]) -> Result<Plan, UsageError> {
    let (key, _) = follow_key_of(spec, &spelled)?;
    Ok(Plan::Subscribe {
        request: FollowRequest {
            key,
            project: project_flag(spec, flags)?,
        },
        spelled,
    })
}

fn unsubscribe_plan(spec: CommandSpec, spelled: String) -> Result<Plan, UsageError> {
    let (key, _) = follow_key_of(spec, &spelled)?;
    Ok(Plan::Unsubscribe { key, spelled })
}

fn project_create_plan(spec: CommandSpec, spelled: String, flags: &[Flag]) -> Result<Plan, UsageError> {
    let name = ProjectName::new(&spelled).map_err(|cause| UsageError {
        kind: UsageKind::BadOperand {
            cause: project_name_detail(cause),
        },
        token: spelled.clone(),
        command: Some(spec.name),
    })?;
    let mut binding = None;
    let mut hue = None;
    for flag in flags {
        let value = flag.value.clone().unwrap_or_default();
        match flag.name {
            "--lockfile" => {
                let path = std::path::PathBuf::from(&value);
                let absolute = if path.is_absolute() {
                    path
                } else {
                    std::env::current_dir().map_or(path.clone(), |cwd| cwd.join(path))
                };
                binding = Some(LockfileBinding::new(absolute).map_err(|cause| UsageError {
                    kind: UsageKind::BadValue {
                        accepted: match cause {
                            interface_library::ProjectError::UnknownLockfile { .. } => {
                                interface_library::LockfileKind::ALL
                                    .into_iter()
                                    .map(|kind| kind.file_name().to_owned())
                                    .collect()
                            }
                            _ => vec![cause.detail()],
                        },
                    },
                    token: value.clone(),
                    command: Some(spec.name),
                })?);
            }
            "--hue" => {
                hue = Some(ProjectHue::parse(&value).ok_or_else(|| UsageError {
                    kind: UsageKind::BadValue {
                        accepted: ProjectHue::ALL
                            .into_iter()
                            .map(|hue| hue.name().to_owned())
                            .collect(),
                    },
                    token: value.clone(),
                    command: Some(spec.name),
                })?);
            }
            _ => {}
        }
    }
    Ok(Plan::ProjectCreate {
        request: CreateProject { name, binding, hue },
        spelled,
    })
}

fn project_delete_plan(spec: CommandSpec, spelled: String) -> Result<Plan, UsageError> {
    let selector = project_selector_of(spec, &spelled)?;
    Ok(Plan::ProjectDelete { selector, spelled })
}

/// `project-add` and `project-remove` take the package as the operand and the folder as
/// `--project`, because the package is the thing a reader has just copied.
fn member_change_plan(
    spec: CommandSpec,
    spelled: String,
    flags: &[Flag],
    add: bool,
) -> Result<Plan, UsageError> {
    let coordinate = PackageCoordinate::parse(&spelled).map_err(|cause| UsageError {
        kind: UsageKind::BadOperand {
            cause: interface_library::render::text::coordinate_parse_detail(cause).to_owned(),
        },
        token: spelled.clone(),
        command: Some(spec.name),
    })?;
    let Some(selector) = project_flag(spec, flags)? else {
        return Err(UsageError {
            kind: UsageKind::MissingValue,
            token: "--project".to_owned(),
            command: Some(spec.name),
        });
    };
    let change = MemberChange {
        selector,
        coordinate,
    };
    Ok(if add {
        Plan::ProjectAdd { change, spelled }
    } else {
        Plan::ProjectRemove { change, spelled }
    })
}

fn project_sync_plan(spec: CommandSpec, spelled: String, flags: &[Flag]) -> Result<Plan, UsageError> {
    let selector = project_selector_of(spec, &spelled)?;
    let compile = if flags.iter().any(|flag| flag.name == "--compile") {
        SyncCompile::Missing
    } else {
        SyncCompile::Never
    };
    Ok(Plan::ProjectSync {
        request: SyncRequest { selector, compile },
        spelled,
    })
}

fn tree_open_plan(spec: CommandSpec, spelled: String, flags: &[Flag]) -> Result<Plan, UsageError> {
    let mut parent = None;
    let mut focus = true;
    let mut title = None;
    let mut kind: Option<String> = None;
    for flag in flags {
        let value = flag.value.clone().unwrap_or_default();
        match flag.name {
            "--parent" => {
                parent = Some(TreeNodeId::parse(&value).ok_or_else(|| UsageError {
                    kind: UsageKind::BadValue {
                        accepted: vec!["a node identity from `nudox tree`".to_owned()],
                    },
                    token: value.clone(),
                    command: Some(spec.name),
                })?);
            }
            "--no-focus" => focus = false,
            "--title" => title = Some(interface_documents::Text::new(value.clone())),
            "--as" => kind = Some(value.clone()),
            _ => {}
        }
    }
    let text = match kind {
        Some(kind) => format!("{kind} {spelled}"),
        None => spelled.clone(),
    };
    let subject = TreeSubject::infer(&text).map_err(|cause| UsageError {
        kind: UsageKind::BadOperand {
            cause: format!("{cause:?}"),
        },
        token: spelled.clone(),
        command: Some(spec.name),
    })?;
    Ok(Plan::TreeOpen {
        request: TreeOpenRequest {
            subject,
            parent,
            opener: Opener::Cli,
            focus,
            title,
        },
        spelled,
    })
}

fn tree_close_plan(spec: CommandSpec, spelled: String, flags: &[Flag]) -> Result<Plan, UsageError> {
    let node = TreeNodeId::parse(&spelled).ok_or_else(|| UsageError {
        kind: UsageKind::BadOperand {
            cause: "a node identity from `nudox tree`".to_owned(),
        },
        token: spelled.clone(),
        command: Some(spec.name),
    })?;
    let scope = if flags.iter().any(|flag| flag.name == "--branch") {
        TreeCloseScope::Branch
    } else {
        TreeCloseScope::Node
    };
    Ok(Plan::TreeClose {
        request: TreeCloseRequest { node, scope },
        spelled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(items: &[&str]) -> Result<Invocation, UsageError> {
        parse(items.iter().map(|item| (*item).to_owned()).collect())
    }

    fn plan_name(items: &[&str]) -> Option<&'static str> {
        parsed(items).ok().map(|invocation| invocation.plan.command_name())
    }

    fn search_of(items: &[&str]) -> Option<SearchPlan> {
        match parsed(items).ok()?.plan {
            Plan::Search(plan) => Some(plan),
            _ => None,
        }
    }

    fn graph_of(items: &[&str]) -> Option<GraphPlan> {
        match parsed(items).ok()?.plan {
            Plan::Graph(plan) => Some(plan),
            _ => None,
        }
    }

    fn rejection(items: &[&str]) -> Option<UsageError> {
        parsed(items).err()
    }

    /// One operand that satisfies each registry row, so the whole registry is exercised.
    fn operand(id: CommandId) -> Vec<&'static str> {
        match id {
            CommandId::Packages | CommandId::Health => Vec::new(),
            CommandId::Add | CommandId::Remove | CommandId::Outline => {
                vec!["cargo:serde@1.0.196"]
            }
            CommandId::Show | CommandId::Graph => {
                vec!["cargo:serde@1.0.196::de::Deserializer"]
            }
            CommandId::Resolve | CommandId::Search | CommandId::IndexSearch => vec!["deserialize"],
            CommandId::PackageVersions | CommandId::PackageProfile => vec!["serde"],
            CommandId::Source | CommandId::Related => {
                vec!["cargo:serde@1.0.196::de::Deserializer"]
            }
            CommandId::Explore
            | CommandId::Subscriptions
            | CommandId::Releases
            | CommandId::Projects
            | CommandId::Tree => Vec::new(),
            CommandId::Package
            | CommandId::Dependents
            | CommandId::Subscribe
            | CommandId::Unsubscribe => vec!["cargo:serde"],
            CommandId::Owner => vec!["dtolnay"],
            CommandId::ProjectCreate => vec!["backend"],
            CommandId::ProjectDelete | CommandId::ProjectSync => vec!["backend"],
            CommandId::ProjectAdd | CommandId::ProjectRemove => {
                vec!["cargo:serde@1.0.196", "--project", "backend"]
            }
            CommandId::TreeOpen => vec!["cargo:serde@1.0.196"],
            CommandId::TreeClose => vec!["7"],
        }
    }

    #[test]
    fn every_registry_row_parses_from_its_own_name() {
        for row in COMMANDS {
            let mut items = vec![row.name];
            items.extend(operand(row.id));
            assert_eq!(plan_name(&items), Some(row.name), "{} did not parse", row.name);
        }
    }

    #[test]
    fn flag_values_accept_both_spellings() {
        let spaced = search_of(&["search", "map", "--limit", "5", "--lanes", "exact,names"]);
        let joined = search_of(&["search", "map", "--limit=5", "--lanes=exact,names"]);
        assert_eq!(spaced.as_ref().map(|plan| plan.limit.get()), Some(5));
        assert_eq!(joined.as_ref().map(|plan| plan.limit.get()), Some(5));
        assert_eq!(
            spaced.map(|plan| plan.lanes),
            joined.map(|plan| plan.lanes),
            "the two spellings must produce the same lane set"
        );
    }

    #[test]
    fn globals_are_accepted_on_either_side_of_the_subcommand() {
        let before = parsed(&["--format", "json", "packages"]).ok();
        let after = parsed(&["packages", "--format=json"]).ok();
        assert_eq!(before.map(|one| one.globals.format), Some(Format::Json));
        assert_eq!(after.map(|one| one.globals.format), Some(Format::Json));
    }

    #[test]
    fn a_literal_separator_stops_option_parsing() {
        assert_eq!(
            parsed(&["search", "--", "--lanes"]).ok().and_then(|one| match one.plan {
                Plan::Search(plan) => Some(plan.spelled),
                _ => None,
            }),
            Some("--lanes".to_owned())
        );
    }

    #[test]
    fn every_usage_error_retains_its_offending_token() {
        let cases: [(&[&str], &str, &str); 8] = [
            (&["pakages"], "unknown-command", "pakages"),
            (&["packages", "--wat"], "unknown-flag", "--wat"),
            (&["search", "map", "--limit"], "missing-value", "--limit"),
            (&["show"], "missing-operand", "show"),
            (&["packages", "extra"], "unexpected-operand", "extra"),
            (&["search", "map", "--lanes", "vibes"], "bad-value", "vibes"),
            (&["outline", "serde"], "bad-operand", "serde"),
            (
                &["graph", "cargo:serde@1.0.196::de", "--incoming", "--outgoing"],
                "conflicting-flags",
                "--outgoing",
            ),
        ];
        for (items, slug, token) in cases {
            let error = rejection(items);
            assert_eq!(
                error.as_ref().map(|error| error.kind.slug()),
                Some(slug),
                "{items:?} did not produce {slug}"
            );
            assert_eq!(error.map(|error| error.token), Some(token.to_owned()));
        }
    }

    #[test]
    fn an_unknown_command_offers_the_registry_names_nearest_to_it() {
        let error = rejection(&["searh", "map"]);
        assert_eq!(
            error.and_then(|error| match error.kind {
                UsageKind::UnknownCommand { nearest } => Some(nearest),
                _ => None,
            }),
            Some(vec!["search", "show", "source"]),
            "every registry row sharing a leading run is offered, longest run first"
        );
    }

    #[test]
    fn a_bad_value_lists_every_accepted_spelling() {
        let error = rejection(&["search", "map", "--kinds", "widget"]);
        let accepted = error.and_then(|error| match error.kind {
            UsageKind::BadValue { accepted } => Some(accepted),
            _ => None,
        });
        assert_eq!(
            accepted.as_ref().map(|accepted| accepted.contains(&"struct".to_owned())),
            Some(true)
        );
    }

    #[test]
    fn graph_direction_defaults_to_outgoing_and_follows_an_incoming_relation() {
        assert_eq!(
            graph_of(&["graph", "cargo:serde@1.0.196::de"]).map(|plan| plan.direction),
            Some(Direction::Outgoing)
        );
        assert_eq!(
            graph_of(&["graph", "cargo:serde@1.0.196::de", "--kind", "called-by"])
                .map(|plan| plan.direction),
            Some(Direction::Incoming)
        );
    }

    #[test]
    fn add_accepts_a_package_url_and_a_coordinate_alike() {
        let from_url = parsed(&["add", "pkg:cargo/serde@1.0.196"]).ok().and_then(|one| {
            match one.plan {
                Plan::Add { url, .. } => Some(url.as_ref().to_owned()),
                _ => None,
            }
        });
        let from_coordinate = parsed(&["add", "cargo:serde@1.0.196"]).ok().and_then(|one| {
            match one.plan {
                Plan::Add { url, .. } => Some(url.as_ref().to_owned()),
                _ => None,
            }
        });
        assert_eq!(from_url, Some("pkg:cargo/serde@1.0.196".to_owned()));
        assert_eq!(from_coordinate, from_url);
    }

    #[test]
    fn a_bare_nudox_asks_for_the_dashboard_and_dash_h_asks_for_help() {
        assert!(matches!(parsed(&[]).ok().map(|one| one.plan), Some(Plan::Dashboard)));
        assert!(matches!(parsed(&["-h"]).ok().map(|one| one.plan), Some(Plan::Help)));
        assert!(matches!(
            parsed(&["search", "--help"]).ok().map(|one| one.plan),
            Some(Plan::Help)
        ));
    }
}
