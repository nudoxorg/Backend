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
use interface_library::{
    Command, CommandId, PageLocator,
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
    }
}

/// The fields each tool accepts, in the order its schema publishes them.
const fn accepted_fields(id: CommandId) -> &'static [&'static str] {
    match id {
        CommandId::Packages | CommandId::Health => &[],
        CommandId::Add | CommandId::Remove | CommandId::Outline => &["package"],
        CommandId::Show => &["address"],
        CommandId::Resolve => &["text"],
        CommandId::Search => &["query", "kinds", "packages", "lanes", "limit", "cursor"],
        CommandId::Graph => &["address", "relation", "depth", "limit"],
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
