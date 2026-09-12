//! Defines arguments behavior for `interface-mcp`, whose purpose is to serve the one shared local library to agents over MCP.
//! This module owns the arguments invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! One `tools/call` arguments object decoded into one [`Command`], or the exact field that refused.
//!
//! Decoding is strict on purpose. An unrecognised field is named and the accepted set is listed,
//! because an agent that misspells `package` as `packages` and gets a plausible empty answer will
//! believe it. Every rejection is a [`Fault`] whose affordance is the call that would have worked,
//! so a malformed call is one round trip from a correct one rather than a guessing game.

use interface_core::PackageUrl;
use interface_documents::ProjectionLimits;
use interface_identity::{Address, ContentKey, KindTag, PackageCoordinate};
use interface_documents::Text;
use interface_identity::{PackageVersion, parse_ecosystem_tag};
use interface_library::{
    Command, CommandId, ContextLines, CreateProject, DependentsRequest, DetailRequest,
    ExploreLimit, ExplorePackageName, ExploreQuery, ExploreQueryError, ExplorePackageNameError,
    ExploreRequest, ExploreSort, FollowKey, FollowRequest, LockfileBinding, MemberChange, Opener,
    OwnerHandle, OwnerRequest, PageLocator, PageNumber, ProjectHue, ProjectName, ProjectSelector,
    RelatedRequest, ReleasesRequest, SourceRequest, SyncCompile, SyncRequest, TreeCloseRequest,
    TreeCloseScope, TreeNodeId, TreeOpenRequest, TreeSubject,
    render::common::{Affordance, Fault, add_affordance},
};
use interface_search::{
    Cursor, Depth, GraphRequest, GraphSource, KindSet, Lane, LaneSet, QueryText, QueryTextError,
    ResultLimit, SearchRequest, SearchScope, parse_relation,
};
use serde_json::{Map, Value};

use crate::tools;

/// Decodes one tool call into the command the library will execute.
///
/// # Errors
///
/// Returns the exact field that was absent, malformed, or unrecognised, with the call that would
/// have worked.
pub fn decode(tool: &str, arguments: &Map<String, Value>) -> Result<Command, Fault> {
    let Some(id) = tools::command_id(tool) else {
        return Err(Fault::new("unknown-tool", tool, Affordance::None).detailed(format!(
            "this server publishes {}",
            interface_library::COMMANDS
                .map(|row| row.name)
                .join(" ")
        )));
    };
    reject_unknown_fields(id, arguments)?;
    match id {
        CommandId::Packages => Ok(Command::Packages),
        CommandId::Health => Ok(Command::Health),
        CommandId::Add => Ok(Command::Add {
            url: package_url(arguments)?,
        }),
        CommandId::Remove => Ok(Command::Remove {
            coordinate: coordinate(arguments, "package")?,
        }),
        CommandId::Outline => Ok(Command::Outline {
            coordinate: coordinate(arguments, "package")?,
        }),
        CommandId::Show => Ok(Command::Show {
            locator: locator(text(arguments, "address")?)?,
            limits: ProjectionLimits::default(),
        }),
        CommandId::Resolve => Ok(Command::Resolve {
            text: text(arguments, "text")?.into(),
        }),
        CommandId::Search => search(arguments).map(Command::Search),
        CommandId::Graph => graph(arguments).map(Command::Graph),
        CommandId::IndexSearch => {
            let (query, limit) = index_search(arguments)?;
            Ok(Command::IndexSearch { query, limit })
        }
        CommandId::PackageVersions => Ok(Command::PackageVersions {
            name: package_name(arguments)?,
        }),
        CommandId::PackageProfile => Ok(Command::PackageProfile {
            name: package_name(arguments)?,
        }),
        CommandId::Source => Ok(Command::Source(SourceRequest {
            locator: locator(text(arguments, "address")?)?,
            context: whole_number(arguments, "context")?.map_or_else(ContextLines::default, |value| {
                ContextLines::clamped(u8::try_from(value).unwrap_or(u8::MAX))
            }),
        })),
        CommandId::Related => Ok(Command::Related(RelatedRequest {
            locator: locator(text(arguments, "address")?)?,
            limit: limit(arguments)?,
        })),
        CommandId::Explore => explore(arguments).map(Command::Explore),
        CommandId::Package => {
            let (key, version) = follow_key(arguments, "package")?;
            Ok(Command::Package(DetailRequest {
                ecosystem: key.ecosystem,
                name: explore_name(&key)?,
                version,
            }))
        }
        CommandId::Dependents => {
            let (key, _) = follow_key(arguments, "package")?;
            Ok(Command::Dependents(DependentsRequest {
                ecosystem: key.ecosystem,
                name: explore_name(&key)?,
                page: page(arguments)?,
                limit: explore_limit(arguments)?,
            }))
        }
        CommandId::Owner => Ok(Command::Owner(OwnerRequest {
            ecosystem: ecosystem(arguments)?,
            handle: owner_handle(arguments)?,
        })),
        CommandId::Subscribe => {
            let (key, _) = follow_key(arguments, "package")?;
            Ok(Command::Subscribe(FollowRequest {
                key,
                project: project_selector(arguments, "project")?,
            }))
        }
        CommandId::Unsubscribe => {
            let (key, _) = follow_key(arguments, "package")?;
            Ok(Command::Unsubscribe { key })
        }
        CommandId::Subscriptions => Ok(Command::Subscriptions),
        CommandId::Releases => Ok(Command::Releases(ReleasesRequest {
            mark_seen: flag(arguments, "mark_seen")?,
        })),
        CommandId::Projects => Ok(Command::Projects),
        CommandId::ProjectCreate => Ok(Command::ProjectCreate(CreateProject {
            name: project_name(arguments)?,
            binding: lockfile(arguments)?,
            hue: hue(arguments)?,
        })),
        CommandId::ProjectDelete => Ok(Command::ProjectDelete {
            selector: project_selector(arguments, "project")?.ok_or_else(|| missing("project"))?,
        }),
        CommandId::ProjectAdd => member_change(arguments).map(Command::ProjectAdd),
        CommandId::ProjectRemove => member_change(arguments).map(Command::ProjectRemove),
        CommandId::ProjectSync => Ok(Command::ProjectSync(SyncRequest {
            selector: project_selector(arguments, "project")?.ok_or_else(|| missing("project"))?,
            compile: if flag(arguments, "compile")? {
                SyncCompile::Missing
            } else {
                SyncCompile::Never
            },
        })),
        CommandId::Tree => Ok(Command::Tree),
        CommandId::TreeOpen => tree_open(arguments).map(Command::TreeOpen),
        CommandId::TreeClose => Ok(Command::TreeClose(TreeCloseRequest {
            node: node_id(arguments, "node")?.ok_or_else(|| missing("node"))?,
            scope: if flag(arguments, "branch")? {
                TreeCloseScope::Branch
            } else {
                TreeCloseScope::Node
            },
        })),
    }
}

/// The client name this process records as the opener of every tree node it touches, until the
/// handshake supplies the client's own name.
pub const DEFAULT_CLIENT: &str = "agent";

/// The fields each tool accepts, in the order its schema publishes them.
const fn accepted_fields(id: CommandId) -> &'static [&'static str] {
    match id {
        CommandId::Packages | CommandId::Health => &[],
        CommandId::Add | CommandId::Remove | CommandId::Outline => &["package"],
        CommandId::Show => &["address"],
        CommandId::Resolve => &["text"],
        CommandId::Search => &["query", "kinds", "packages", "lanes", "limit", "cursor"],
        CommandId::Graph => &["address", "relation", "depth", "limit"],
        CommandId::IndexSearch => &["query", "limit"],
        CommandId::PackageVersions | CommandId::PackageProfile => &["package"],
        CommandId::Source => &["address", "context"],
        CommandId::Related => &["address", "limit"],
        CommandId::Explore => &["query", "ecosystem", "sort", "page", "limit"],
        CommandId::Package | CommandId::Unsubscribe => &["package"],
        CommandId::Dependents => &["package", "page", "limit"],
        CommandId::Owner => &["handle", "ecosystem"],
        CommandId::Subscribe => &["package", "project"],
        CommandId::Subscriptions | CommandId::Projects | CommandId::Tree => &[],
        CommandId::Releases => &["mark_seen"],
        CommandId::ProjectCreate => &["name", "lockfile", "hue"],
        CommandId::ProjectDelete => &["project"],
        CommandId::ProjectAdd | CommandId::ProjectRemove => &["package", "project"],
        CommandId::ProjectSync => &["project", "compile"],
        CommandId::TreeOpen => &["subject", "kind", "parent", "focus", "title"],
        CommandId::TreeClose => &["node", "branch"],
    }
}

fn reject_unknown_fields(id: CommandId, arguments: &Map<String, Value>) -> Result<(), Fault> {
    let accepted = accepted_fields(id);
    for name in arguments.keys() {
        if !accepted.contains(&name.as_str()) {
            return Err(
                Fault::new("unknown-argument", name.as_str(), Affordance::None).detailed(
                    if accepted.is_empty() {
                        "this tool takes no arguments".to_owned()
                    } else {
                        format!("this tool accepts {}", accepted.join(" "))
                    },
                ),
            );
        }
    }
    Ok(())
}

fn missing(field: &'static str) -> Fault {
    Fault::new("argument-missing", field, Affordance::None)
        .detailed("this argument is required and was not supplied")
}

fn text<'arguments>(
    arguments: &'arguments Map<String, Value>,
    field: &'static str,
) -> Result<&'arguments str, Fault> {
    match arguments.get(field) {
        None | Some(Value::Null) => Err(missing(field)),
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(value.trim()),
        Some(Value::String(_)) => Err(Fault::new("argument-empty", field, Affordance::None)
            .detailed("this argument was whitespace, which names nothing")),
        Some(_) => Err(Fault::new("argument-type", field, Affordance::None)
            .detailed("this argument must be a string")),
    }
}

fn strings<'arguments>(
    arguments: &'arguments Map<String, Value>,
    field: &'static str,
) -> Result<Option<Vec<&'arguments str>>, Fault> {
    match arguments.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str().ok_or_else(|| {
                    Fault::new("argument-type", field, Affordance::None)
                        .detailed("every entry in this array must be a string")
                })
            })
            .collect::<Result<Vec<&str>, Fault>>()
            .map(Some),
        Some(_) => Err(Fault::new("argument-type", field, Affordance::None)
            .detailed("this argument must be an array of strings")),
    }
}

fn whole_number(arguments: &Map<String, Value>, field: &'static str) -> Result<Option<u64>, Fault> {
    match arguments.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number.as_u64().map(Some).ok_or_else(|| {
            Fault::new("argument-range", field, Affordance::None)
                .detailed("this argument must be a whole number that is not negative")
        }),
        Some(_) => Err(Fault::new("argument-type", field, Affordance::None)
            .detailed("this argument must be a number")),
    }
}

/// Accepts either spelling of a pinned package: the package URL the compiler admits, or the
/// coordinate every result prints. An agent that copies a coordinate out of `packages` and hands it
/// to `add` is doing the obvious thing, and the obvious thing works.
fn package_url(arguments: &Map<String, Value>) -> Result<PackageUrl, Fault> {
    let spelling = text(arguments, "package")?;
    let canonical = if spelling.starts_with("pkg:") {
        spelling.to_owned()
    } else {
        PackageCoordinate::parse(spelling)
            .map(|coordinate| coordinate.package_url_text())
            .map_err(|cause| {
                Fault::new("package", spelling, Affordance::None).detailed(coordinate_detail(cause))
            })?
    };
    PackageUrl::try_from(canonical).map_err(|rejected| {
        Fault::new("package-url", rejected.text.as_str(), Affordance::None).detailed(format!(
            "the {} part of this package url was rejected",
            interface_library::render::common::package_url_slug(rejected.error)
        ))
    })
}

fn coordinate(
    arguments: &Map<String, Value>,
    field: &'static str,
) -> Result<PackageCoordinate, Fault> {
    let spelling = text(arguments, field)?;
    PackageCoordinate::parse(spelling).map_err(|cause| {
        Fault::new("package", spelling, Affordance::Packages).detailed(coordinate_detail(cause))
    })
}

fn coordinate_detail(cause: interface_identity::CoordinateParseError) -> String {
    interface_library::render::common::address_parse_detail(
        interface_identity::AddressParseError::Coordinate { cause },
    )
}

/// Accepts an address or a bare key, because both are things a result printed and a reader copied.
fn locator(spelling: &str) -> Result<PageLocator, Fault> {
    if let Ok((key, _)) = ContentKey::parse(spelling) {
        return Ok(PageLocator::Key(key));
    }
    Address::parse(spelling)
        .map(PageLocator::Address)
        .map_err(|cause| {
            Fault::new("address", spelling, Affordance::Resolve {
                text: spelling.to_owned(),
            })
            .detailed(interface_library::render::common::address_parse_detail(
                cause,
            ))
        })
}

fn search(arguments: &Map<String, Value>) -> Result<SearchRequest, Fault> {
    let query = QueryText::new(text(arguments, "query")?).map_err(|cause| {
        Fault::new("query", String::new(), Affordance::None).detailed(match cause {
            QueryTextError::Empty => "a query cannot be empty".to_owned(),
            QueryTextError::TooLong { observed, maximum } => {
                format!("{observed} bytes exceeds the {maximum}-byte query budget")
            }
        })
    })?;
    Ok(SearchRequest {
        text: query,
        scope: SearchScope {
            packages: coordinate_list(arguments)?,
            kinds: kind_set(arguments)?,
        },
        lanes: lane_set(arguments)?,
        limit: limit(arguments)?,
        cursor: whole_number(arguments, "cursor")?
            .map(|value| Cursor(u32::try_from(value).unwrap_or(u32::MAX))),
    })
}

fn coordinate_list(
    arguments: &Map<String, Value>,
) -> Result<Option<Box<[PackageCoordinate]>>, Fault> {
    let Some(spellings) = strings(arguments, "packages")? else {
        return Ok(None);
    };
    spellings
        .into_iter()
        .map(|spelling| {
            PackageCoordinate::parse(spelling).map_err(|cause| {
                Fault::new("packages", spelling, Affordance::Packages)
                    .detailed(coordinate_detail(cause))
            })
        })
        .collect::<Result<Vec<PackageCoordinate>, Fault>>()
        .map(|list| Some(list.into_boxed_slice()))
}

fn kind_set(arguments: &Map<String, Value>) -> Result<KindSet, Fault> {
    let Some(spellings) = strings(arguments, "kinds")? else {
        return Ok(KindSet::BROWSABLE);
    };
    let mut kinds = KindSet::EMPTY;
    for spelling in spellings {
        let Some(kind) = KindTag::parse(spelling) else {
            return Err(
                Fault::new("kinds", spelling, Affordance::None).detailed(
                    "read nudox://schema-card for the fifteen accepted declaration-kind tags",
                ),
            );
        };
        kinds = kinds.with(kind);
    }
    if kinds.is_empty() {
        return Ok(KindSet::BROWSABLE);
    }
    Ok(kinds)
}

fn lane_set(arguments: &Map<String, Value>) -> Result<LaneSet, Fault> {
    let Some(spellings) = strings(arguments, "lanes")? else {
        return Ok(LaneSet::ALL);
    };
    let mut lanes = LaneSet::EMPTY;
    for spelling in spellings {
        let Some(lane) = Lane::ALL
            .into_iter()
            .find(|lane| lane.label() == spelling)
        else {
            return Err(Fault::new("lanes", spelling, Affordance::None)
                .detailed("the four lanes are exact names graph semantic"));
        };
        lanes = lanes.with(lane);
    }
    if lanes == LaneSet::EMPTY {
        return Ok(LaneSet::ALL);
    }
    Ok(lanes)
}

fn limit(arguments: &Map<String, Value>) -> Result<ResultLimit, Fault> {
    Ok(whole_number(arguments, "limit")?
        .map_or_else(ResultLimit::default, |value| {
            ResultLimit::clamped(u16::try_from(value).unwrap_or(u16::MAX))
        }))
}

fn index_search(arguments: &Map<String, Value>) -> Result<(ExploreQuery, ExploreLimit), Fault> {
    let query = ExploreQuery::new(text(arguments, "query")?).map_err(|cause| {
        Fault::new("query", String::new(), Affordance::None).detailed(explore_query_detail(cause))
    })?;
    let limit = whole_number(arguments, "limit")?.map_or_else(ExploreLimit::default, |value| {
        ExploreLimit::clamped(usize::try_from(value).unwrap_or(usize::MAX))
    });
    Ok((query, limit))
}

fn explore_query_detail(cause: ExploreQueryError) -> String {
    match cause {
        ExploreQueryError::Empty => "a query cannot be empty".to_owned(),
        ExploreQueryError::TooLong { observed, maximum } => {
            format!("{observed} bytes exceeds the {maximum}-byte index-search query budget")
        }
    }
}

fn package_name(arguments: &Map<String, Value>) -> Result<ExplorePackageName, Fault> {
    let spelling = text(arguments, "package")?;
    ExplorePackageName::new(spelling).map_err(|cause| {
        Fault::new("package", spelling, Affordance::None).detailed(match cause {
            ExplorePackageNameError::Empty => "the package name is empty".to_owned(),
            ExplorePackageNameError::TooLong { observed, maximum } => {
                format!("{observed} bytes exceeds the {maximum}-byte package-name budget")
            }
        })
    })
}

fn graph(arguments: &Map<String, Value>) -> Result<GraphRequest, Fault> {
    let spelling = text(arguments, "address")?;
    let source = match ContentKey::parse(spelling) {
        Ok((key, _)) => GraphSource::Key(key),
        Err(_) => GraphSource::Address(Address::parse(spelling).map_err(|cause| {
            Fault::new("address", spelling, Affordance::Resolve {
                text: spelling.to_owned(),
            })
            .detailed(interface_library::render::common::address_parse_detail(
                cause,
            ))
        })?),
    };
    let (kind, direction) = match arguments.get("relation") {
        None | Some(Value::Null) => (None, interface_documents::Direction::Outgoing),
        Some(_) => {
            let spelling = text(arguments, "relation")?;
            let Some((kind, direction)) = parse_relation(spelling) else {
                return Err(Fault::new("relation", spelling, Affordance::None)
                    .detailed("read nudox://schema-card for every relation spelling"));
            };
            (Some(kind), direction)
        }
    };
    Ok(GraphRequest {
        source,
        kind,
        direction,
        depth: whole_number(arguments, "depth")?.map_or(Depth::ONE, |value| {
            Depth::clamped(u8::try_from(value).unwrap_or(u8::MAX))
        }),
        limit: limit(arguments)?,
    })
}

// ── the rewritten surface's rows ────────────────────────────────────────────────────────────────

fn flag(arguments: &Map<String, Value>, field: &'static str) -> Result<bool, Fault> {
    match arguments.get(field) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(Fault::new("argument-type", field, Affordance::None)
            .detailed("this argument must be true or false")),
    }
}

fn optional_text<'arguments>(
    arguments: &'arguments Map<String, Value>,
    field: &'static str,
) -> Result<Option<&'arguments str>, Fault> {
    match arguments.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(_) => text(arguments, field).map(Some),
    }
}

fn ecosystem(arguments: &Map<String, Value>) -> Result<Option<interface_core::PackageEcosystem>, Fault> {
    let Some(spelling) = optional_text(arguments, "ecosystem")? else {
        return Ok(None);
    };
    parse_ecosystem_tag(spelling).map(Some).ok_or_else(|| {
        Fault::new("ecosystem", spelling, Affordance::None)
            .detailed("the seven ecosystems are cargo npm pypi go maven nuget cpp")
    })
}

fn page(arguments: &Map<String, Value>) -> Result<PageNumber, Fault> {
    Ok(whole_number(arguments, "page")?.map_or(PageNumber::FIRST, |value| {
        PageNumber::clamped(u32::try_from(value).unwrap_or(u32::MAX))
    }))
}

fn explore_limit(arguments: &Map<String, Value>) -> Result<ExploreLimit, Fault> {
    Ok(whole_number(arguments, "limit")?.map_or_else(ExploreLimit::default, |value| {
        ExploreLimit::clamped(usize::try_from(value).unwrap_or(usize::MAX))
    }))
}

fn explore(arguments: &Map<String, Value>) -> Result<ExploreRequest, Fault> {
    let query = match optional_text(arguments, "query")? {
        Some(spelling) => Some(ExploreQuery::new(spelling).map_err(|cause| {
            Fault::new("query", spelling, Affordance::None).detailed(explore_query_detail(cause))
        })?),
        None => None,
    };
    let sort = match optional_text(arguments, "sort")? {
        Some(spelling) => ExploreSort::parse(spelling).ok_or_else(|| {
            Fault::new("sort", spelling, Affordance::None)
                .detailed("the sorts are downloads updated relevance name")
        })?,
        None => ExploreSort::default(),
    };
    Ok(ExploreRequest {
        ecosystem: ecosystem(arguments)?,
        query,
        sort,
        page: page(arguments)?,
        limit: explore_limit(arguments)?,
    })
}

/// `ecosystem:name` with an optional `@version`, as every registry row is spelled.
fn follow_key(
    arguments: &Map<String, Value>,
    field: &'static str,
) -> Result<(FollowKey, Option<PackageVersion>), Fault> {
    let spelling = text(arguments, field)?;
    FollowKey::parse_pinned(spelling).map_err(|cause| {
        Fault::new(field, spelling, Affordance::None).detailed(match cause {
            interface_library::FollowKeyError::MissingEcosystem => {
                "spell the package as ecosystem:name, such as cargo:serde".to_owned()
            }
            interface_library::FollowKeyError::UnknownEcosystem => {
                "the seven ecosystems are cargo npm pypi go maven nuget cpp".to_owned()
            }
            interface_library::FollowKeyError::Name(cause) => coordinate_detail(cause),
        })
    })
}

fn explore_name(key: &FollowKey) -> Result<ExplorePackageName, Fault> {
    ExplorePackageName::new(key.name.as_str()).map_err(|cause| {
        Fault::new("package", key.name.as_str(), Affordance::None).detailed(match cause {
            ExplorePackageNameError::Empty => "the package name is empty".to_owned(),
            ExplorePackageNameError::TooLong { observed, maximum } => {
                format!("{observed} bytes exceeds the {maximum}-byte package-name budget")
            }
        })
    })
}

fn owner_handle(arguments: &Map<String, Value>) -> Result<OwnerHandle, Fault> {
    let spelling = text(arguments, "handle")?;
    OwnerHandle::new(spelling.trim_start_matches('@')).map_err(|cause| {
        Fault::new("handle", spelling, Affordance::None).detailed(match cause {
            interface_library::OwnerHandleError::Empty => "the handle is empty".to_owned(),
            interface_library::OwnerHandleError::TooLong { observed, maximum } => {
                format!("{observed} bytes exceeds the {maximum}-byte handle budget")
            }
            interface_library::OwnerHandleError::Character => "the handle carries whitespace".to_owned(),
        })
    })
}

fn project_name_detail(cause: interface_library::ProjectNameError) -> String {
    match cause {
        interface_library::ProjectNameError::Empty => "the project name is empty".to_owned(),
        interface_library::ProjectNameError::TooLong { observed, maximum } => {
            format!("{observed} bytes exceeds the {maximum}-byte project-name budget")
        }
        interface_library::ProjectNameError::Character => {
            "the project name carries a control character".to_owned()
        }
    }
}

fn project_selector(
    arguments: &Map<String, Value>,
    field: &'static str,
) -> Result<Option<ProjectSelector>, Fault> {
    let Some(spelling) = optional_text(arguments, field)? else {
        return Ok(None);
    };
    ProjectSelector::parse(spelling).map(Some).map_err(|cause| {
        Fault::new(field, spelling, Affordance::None).detailed(project_name_detail(cause))
    })
}

fn project_name(arguments: &Map<String, Value>) -> Result<ProjectName, Fault> {
    let spelling = text(arguments, "name")?;
    ProjectName::new(spelling).map_err(|cause| {
        Fault::new("name", spelling, Affordance::None).detailed(project_name_detail(cause))
    })
}

fn lockfile(arguments: &Map<String, Value>) -> Result<Option<LockfileBinding>, Fault> {
    let Some(spelling) = optional_text(arguments, "lockfile")? else {
        return Ok(None);
    };
    LockfileBinding::new(std::path::PathBuf::from(spelling))
        .map(Some)
        .map_err(|cause| Fault::new("lockfile", spelling, Affordance::None).detailed(cause.detail()))
}

fn hue(arguments: &Map<String, Value>) -> Result<Option<ProjectHue>, Fault> {
    let Some(spelling) = optional_text(arguments, "hue")? else {
        return Ok(None);
    };
    ProjectHue::parse(spelling).map(Some).ok_or_else(|| {
        Fault::new("hue", spelling, Affordance::None)
            .detailed("the hues are caramel teal forest green red olive sand")
    })
}

fn member_change(arguments: &Map<String, Value>) -> Result<MemberChange, Fault> {
    Ok(MemberChange {
        selector: project_selector(arguments, "project")?.ok_or_else(|| missing("project"))?,
        coordinate: coordinate(arguments, "package")?,
    })
}

fn node_id(arguments: &Map<String, Value>, field: &'static str) -> Result<Option<TreeNodeId>, Fault> {
    match arguments.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(_)) => Ok(whole_number(arguments, field)?
            .and_then(|value| TreeNodeId::parse(&value.to_string()))),
        Some(Value::String(spelling)) => TreeNodeId::parse(spelling.trim()).map(Some).ok_or_else(|| {
            Fault::new(field, spelling.as_str(), Affordance::None)
                .detailed("a node identity is a positive whole number from `tree`")
        }),
        Some(_) => Err(Fault::new("argument-type", field, Affordance::None)
            .detailed("this argument must be a node identity")),
    }
}

fn tree_open(arguments: &Map<String, Value>) -> Result<TreeOpenRequest, Fault> {
    let spelling = text(arguments, "subject")?;
    let spelled = match optional_text(arguments, "kind")? {
        Some(kind) => format!("{kind} {spelling}"),
        None => spelling.to_owned(),
    };
    let subject = TreeSubject::infer(&spelled).map_err(|cause| {
        Fault::new("subject", spelling, Affordance::None).detailed(format!(
            "a subject is a coordinate, an address, ecosystem:name, @handle, or search words ({cause:?})"
        ))
    })?;
    let focus = match arguments.get("focus") {
        None | Some(Value::Null) => false,
        Some(_) => flag(arguments, "focus")?,
    };
    Ok(TreeOpenRequest {
        subject,
        parent: node_id(arguments, "parent")?,
        opener: Opener::Mcp {
            client: Text::new(DEFAULT_CLIENT),
        },
        focus,
        title: optional_text(arguments, "title")?.map(Text::new),
    })
}

/// The affordance a decode failure should offer, once the server knows which package it concerns.
#[must_use]
pub fn missing_package_affordance(coordinate: &PackageCoordinate) -> Affordance {
    add_affordance(coordinate)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(text: &str) -> Map<String, Value> {
        match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(object)) => object,
            _ => unreachable!("fixture is a JSON object"),
        }
    }

    #[test]
    fn add_accepts_both_pinned_spellings_and_names_the_field_that_refused() {
        let Ok(Command::Add { url }) = decode("add", &object(r#"{"package":"cargo:serde@1.0.196"}"#))
        else {
            unreachable!("a coordinate is an accepted spelling");
        };
        assert_eq!(url.as_ref(), "pkg:cargo/serde@1.0.196");
        let fault = decode("add", &object(r#"{"packages":"serde"}"#))
            .err()
            .unwrap_or_else(|| unreachable!("a misspelled field is refused"));
        assert_eq!(fault.slug, "unknown-argument");
        assert_eq!(fault.operand, "packages");
        assert_eq!(fault.detail, "this tool accepts package");
    }

    #[test]
    fn search_narrows_kinds_and_lanes_from_their_shared_spellings() {
        let Ok(Command::Search(request)) = decode(
            "search",
            &object(r#"{"query":"deserialize","kinds":["fn"],"lanes":["exact","names"],"limit":9}"#),
        ) else {
            unreachable!("fixture decodes");
        };
        assert_eq!(request.text.as_str(), "deserialize");
        assert!(request.scope.kinds.contains(compiler_ir_vocabulary::EntityKind::Function));
        assert!(!request.scope.kinds.contains(compiler_ir_vocabulary::EntityKind::Trait));
        assert_eq!(request.lanes, LaneSet::LOCAL);
        assert_eq!(request.limit.get(), 9);
    }

    #[test]
    fn graph_takes_one_relation_spelling_that_carries_its_direction() {
        let Ok(Command::Graph(request)) = decode(
            "graph",
            &object(r#"{"address":"cargo:serde@1.0.196::de::Deserializer[trait]","relation":"implemented-by"}"#),
        ) else {
            unreachable!("fixture decodes");
        };
        assert_eq!(request.kind, Some(compiler_ir::LinkKind::Implements));
        assert_eq!(request.direction, interface_documents::Direction::Incoming);
        let fault = decode(
            "graph",
            &object(r#"{"address":"cargo:serde@1.0.196","relation":"invented-by"}"#),
        )
        .err()
        .unwrap_or_else(|| unreachable!("an invented relation is refused"));
        assert_eq!(fault.slug, "relation");
        assert_eq!(fault.operand, "invented-by");
    }
}
