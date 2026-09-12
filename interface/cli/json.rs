//! Defines json behavior for `interface-cli`, whose purpose is to project the one shared local library onto a command line.
//! This module owns the json invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! One [`Reply`] as one JSON object, built from typed transfer records rather than assembled from strings.
//!
//! The envelope is always `{"command", "ok", …}` with exactly one payload key, or `"error"` when
//! the reply carried a typed failure. The `error.kind` slug is the same word the terminal prints,
//! so a script that branches on it and a person reading the terminal are looking at one vocabulary.
//! A usage error has no command yet, so it carries `usage_error` and omits `command`.

use interface_documents::{
    Block, Census, Inline, Outline, Page, Prose, Signature, Symbol, Target, TokenKind,
};
use interface_identity::{KindTag, PackageCoordinate};
use interface_library::{
    AddOutcome, Capability, CapabilityState, Health, PackageCard, RemoveOutcome, Reply, Resolution,
    Shelf, ShelfEntry, ShelfStatus,
    render::{
        common::{
            Affordances, capability_slug, confidence_label, degradation_slug, unavailability_slug,
        },
        text::{TerminalAffordances, TextOptions, fault_of},
    },
};
use interface_search::{Coverage, GraphTerminal, LaneReport, SearchTerminal, Truncation};
use serde::Serialize;

use crate::args::{UsageError, UsageKind};

/// The one envelope every JSON reply shares.
#[derive(Default, Serialize)]
struct Envelope {
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<&'static str>,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<FaultDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage_error: Option<UsageDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    packages: Option<ShelfDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    add: Option<AddDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remove: Option<RemoveDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    page: Option<PageDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    outline: Option<OutlineDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolve: Option<ResolveDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    search: Option<SearchDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    graph: Option<GraphDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    health: Option<HealthDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    index_search: Option<IndexSearchDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    package_versions: Option<PackageVersionsDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    package_profile: Option<PackageProfileDto>,
}

#[derive(Serialize)]
struct FaultDto {
    kind: &'static str,
    operand: String,
    detail: String,
    affordance: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    package: Option<String>,
}

#[derive(Serialize)]
struct UsageDto {
    kind: &'static str,
    token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<&'static str>,
    detail: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    accepted: Vec<String>,
}

#[derive(Serialize)]
struct SymbolDto {
    address: String,
    key: String,
    key_abbreviation: String,
    name: String,
    kind: &'static str,
    visibility: &'static str,
}

#[derive(Serialize)]
struct TokenDto {
    kind: &'static str,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<String>,
}

#[derive(Serialize)]
struct CensusDto {
    declarations: u32,
    public: u32,
    documented: u32,
    kinds: Vec<KindCountDto>,
}

#[derive(Serialize)]
struct KindCountDto {
    kind: &'static str,
    count: u32,
}

#[derive(Serialize)]
struct ShelfDto {
    epoch: u64,
    entries: Vec<EntryDto>,
}

#[derive(Serialize)]
struct EntryDto {
    coordinate: String,
    status: &'static str,
    requested_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cause: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    census: Option<CensusDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    published_at: Option<u64>,
}

#[derive(Serialize)]
struct AddDto {
    coordinate: String,
    census: CensusDto,
    published_at: u64,
}

#[derive(Serialize)]
struct RemoveDto {
    package: String,
    outcome: &'static str,
}

#[derive(Serialize)]
struct PageDto {
    #[serde(flatten)]
    symbol: SymbolDto,
    language: &'static str,
    signature: String,
    signature_tokens: Vec<TokenDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    documentation: Vec<String>,
    members: Vec<MemberGroupDto>,
    relations: Vec<RelationGroupDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<SourceDto>,
}

#[derive(Serialize)]
struct MemberGroupDto {
    kind: &'static str,
    rows: Vec<MemberDto>,
}

#[derive(Serialize)]
struct MemberDto {
    #[serde(flatten)]
    symbol: SymbolDto,
    signature: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
}

#[derive(Serialize)]
struct RelationGroupDto {
    relation: &'static str,
    direction: &'static str,
    rows: Vec<RelationDto>,
}

#[derive(Serialize)]
struct RelationDto {
    target: String,
    resolution: &'static str,
    confidence: &'static str,
}

#[derive(Serialize)]
struct SourceDto {
    file: String,
    start: u32,
    end: u32,
}

#[derive(Serialize)]
struct OutlineDto {
    package: String,
    census: CensusDto,
    nodes: Vec<OutlineNodeDto>,
}

#[derive(Serialize)]
struct OutlineNodeDto {
    depth: usize,
    #[serde(flatten)]
    symbol: SymbolDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
}

#[derive(Serialize)]
struct ResolveDto {
    resolution: &'static str,
    candidates: Vec<SymbolDto>,
}

#[derive(Serialize)]
struct SearchDto {
    query: String,
    hits: Vec<HitDto>,
    lanes: Vec<LaneDto>,
    truncation: TruncationDto,
}

#[derive(Serialize)]
struct HitDto {
    #[serde(flatten)]
    symbol: SymbolDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    signature: Option<String>,
    signature_tokens: Vec<TokenDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    lane: &'static str,
    score: u32,
}

#[derive(Serialize)]
struct LaneDto {
    lane: &'static str,
    coverage: CoverageDto,
    hits: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    elapsed_micros: Option<u64>,
}

#[derive(Serialize)]
struct CoverageDto {
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    searched: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total: Option<u32>,
}

#[derive(Serialize)]
struct TruncationDto {
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<u32>,
}

#[derive(Serialize)]
struct GraphDto {
    source: SymbolDto,
    edges: Vec<EdgeDto>,
    coverage: CoverageDto,
}

#[derive(Serialize)]
struct EdgeDto {
    from: String,
    relation: &'static str,
    direction: &'static str,
    target: String,
    resolution: &'static str,
    confidence: &'static str,
    hop: u8,
}

#[derive(Serialize)]
struct HealthDto {
    capabilities: Vec<CapabilityDto>,
}

#[derive(Serialize)]
struct CapabilityDto {
    capability: &'static str,
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Serialize)]
struct IndexSearchDto {
    hits: Vec<IndexHitDto>,
    coverage: ExploreCoverageDto,
}

#[derive(Serialize)]
struct IndexHitDto {
    document: String,
    matched: String,
    score: u32,
}

#[derive(Serialize)]
struct ExploreCoverageDto {
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    searched: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total: Option<usize>,
}

#[derive(Serialize)]
struct PackageVersionsDto {
    versions: Vec<PackageVersionDto>,
}

#[derive(Serialize)]
struct PackageProfileDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    latest: Option<PackageVersionDto>,
    versions: Vec<PackageVersionDto>,
}

#[derive(Serialize)]
struct PackageVersionDto {
    version: String,
    checksum: String,
    active: bool,
    cycle: u64,
}

/// Renders one reply as one JSON object.
///
/// # Errors
///
/// Returns the exact serialization failure; nothing here can produce one from valid UTF-8, but the
/// caller decides what an I/O-class failure costs rather than this module panicking.
pub(crate) fn reply_json(
    command: &'static str,
    reply: &Reply,
    ok: bool,
    options: &TextOptions<'_>,
) -> Result<String, serde_json::Error> {
    let mut envelope = Envelope {
        command: Some(command),
        ok,
        error: fault_dto(reply, options),
        ..Envelope::default()
    };
    match reply {
        Reply::Packages(Ok(shelf)) => envelope.packages = Some(shelf_dto(shelf)),
        Reply::Added(AddOutcome::Ready { card }) => envelope.add = Some(add_dto(card)),
        Reply::Removed(RemoveOutcome::Removed) => {
            envelope.remove = Some(RemoveDto {
                package: options.subject.unwrap_or_default().to_owned(),
                outcome: "removed",
            });
        }
        Reply::Page(Ok(page)) => envelope.page = Some(page_dto(page)),
        Reply::Outline(Ok(outline)) => envelope.outline = Some(outline_dto(outline)),
        Reply::Resolved(Ok(resolution)) => envelope.resolve = Some(resolve_dto(resolution)),
        Reply::Searched(terminal) => envelope.search = Some(search_dto(terminal)),
        Reply::Graphed(Ok(terminal)) => envelope.graph = Some(graph_dto(terminal)),
        Reply::Health(health) => envelope.health = Some(health_dto(health)),
        Reply::IndexSearched(Ok(page)) => envelope.index_search = Some(index_search_dto(page)),
        Reply::Versions(Ok(rows)) => {
            envelope.package_versions = Some(package_versions_dto(rows));
        }
        Reply::Profiled(Ok(profile)) => {
            envelope.package_profile = Some(package_profile_dto(profile));
        }
        _ => {}
    }
    serde_json::to_string(&envelope)
}

/// Renders one usage error as one JSON object.
///
/// # Errors
///
/// Returns the exact serialization failure.
pub(crate) fn usage_json(error: &UsageError) -> Result<String, serde_json::Error> {
    let accepted = match &error.kind {
        UsageKind::BadValue { accepted } => accepted.clone(),
        UsageKind::UnknownCommand { nearest } => {
            nearest.iter().map(|name| (*name).to_owned()).collect()
        }
        _ => Vec::new(),
    };
    let envelope = Envelope {
        ok: false,
        usage_error: Some(UsageDto {
            kind: error.kind.slug(),
            token: error.token.clone(),
            command: error.command,
            detail: crate::help::usage_detail(error),
            accepted,
        }),
        ..Envelope::default()
    };
    serde_json::to_string(&envelope)
}

fn fault_dto(reply: &Reply, options: &TextOptions<'_>) -> Option<FaultDto> {
    let fault = fault_of(reply, options)?;
    let affordances = TerminalAffordances::new(options.attachment);
    Some(FaultDto {
        kind: fault.slug,
        package: PackageCoordinate::parse(&fault.operand)
            .ok()
            .map(|coordinate| coordinate.to_string()),
        operand: fault.operand,
        detail: fault.detail,
        affordance: affordances
            .spell(&fault.affordance)
            .unwrap_or_else(|| "no action available".to_owned()),
    })
}

fn symbol_dto(symbol: &Symbol) -> SymbolDto {
    SymbolDto {
        address: symbol.address.to_string(),
        key: symbol.key().to_string(),
        key_abbreviation: symbol.key().abbreviation().to_string(),
        name: symbol.name.to_string(),
        kind: KindTag::of(symbol.kind).as_str(),
        visibility: interface_library::render::common::visibility_label(symbol.visibility)
            .unwrap_or("unknown"),
    }
}

fn token_dtos(signature: &Signature) -> Vec<TokenDto> {
    signature
        .tokens()
        .iter()
        .map(|token| TokenDto {
            kind: token_kind_slug(token.kind),
            text: token.text.to_string(),
            target: token.target.as_ref().map(target_text),
        })
        .collect()
}

const fn token_kind_slug(kind: TokenKind) -> &'static str {
    match kind {
        TokenKind::Keyword => "keyword",
        TokenKind::Name => "name",
        TokenKind::Type => "type",
        TokenKind::Lifetime => "lifetime",
        TokenKind::Punctuation => "punctuation",
        TokenKind::Literal => "literal",
        TokenKind::Binding => "binding",
        TokenKind::Text => "text",
    }
}

fn target_text(target: &Target) -> String {
    match target {
        Target::Local(symbol) => symbol.address.to_string(),
        Target::External(external) => external.path.to_string(),
        Target::Unresolved(text) => text.to_string(),
    }
}

const fn target_resolution(target: &Target) -> &'static str {
    match target {
        Target::Local(_) => "local",
        Target::External(_) => "external",
        Target::Unresolved(_) => "unresolved",
    }
}

fn census_dto(census: &Census) -> CensusDto {
    CensusDto {
        declarations: census.entities.0,
        public: census.public.0,
        documented: census.documented.0,
        kinds: census
            .kinds()
            .map(|row| KindCountDto {
                kind: KindTag::of(row.kind).as_str(),
                count: row.count.0,
            })
            .collect(),
    }
}

fn shelf_dto(shelf: &Shelf) -> ShelfDto {
    ShelfDto {
        epoch: shelf.epoch.0,
        entries: shelf.entries.iter().map(entry_dto).collect(),
    }
}

fn entry_dto(entry: &ShelfEntry) -> EntryDto {
    let mut dto = EntryDto {
        coordinate: entry.coordinate.to_string(),
        status: status_slug(&entry.status),
        requested_at: entry.requested_at.0,
        phase: None,
        cause: None,
        detail: None,
        census: None,
        published_at: None,
    };
    match &entry.status {
        ShelfStatus::Requested => {}
        ShelfStatus::Compiling { phase } => {
            dto.phase = Some(interface_library::CompilePhaseProgress::of(*phase).label());
        }
        ShelfStatus::Ready { card } => {
            dto.census = Some(census_dto(&card.census));
            dto.published_at = Some(card.published_at.0);
        }
        ShelfStatus::Failed { cause } => {
            dto.cause = Some(interface_library::render::common::shelf_failure_slug(cause));
            if let interface_library::ShelfFailure::Compiler { summary } = cause {
                dto.detail = Some(summary.to_string());
            }
        }
    }
    dto
}

const fn status_slug(status: &ShelfStatus) -> &'static str {
    match status {
        ShelfStatus::Requested => "requested",
        ShelfStatus::Compiling { .. } => "compiling",
        ShelfStatus::Ready { .. } => "ready",
        ShelfStatus::Failed { .. } => "failed",
    }
}

fn add_dto(card: &PackageCard) -> AddDto {
    AddDto {
        coordinate: card.coordinate.to_string(),
        census: census_dto(&card.census),
        published_at: card.published_at.0,
    }
}

fn page_dto(page: &Page) -> PageDto {
    PageDto {
        symbol: symbol_dto(&page.symbol),
        language: interface_library::render::common::fence_tag(page.language),
        signature: page.signature.plain(),
        signature_tokens: token_dtos(&page.signature),
        summary: page.prose.summary().map(|text| text.to_string()),
        documentation: prose_blocks(&page.prose),
        members: page
            .members
            .iter()
            .map(|group| MemberGroupDto {
                kind: KindTag::of(group.kind).as_str(),
                rows: group
                    .rows
                    .iter()
                    .map(|row| MemberDto {
                        symbol: symbol_dto(&row.symbol),
                        signature: row.signature.plain(),
                        summary: row.summary.as_ref().map(ToString::to_string),
                    })
                    .collect(),
            })
            .collect(),
        relations: page
            .relations
            .iter()
            .map(|group| RelationGroupDto {
                relation: interface_search::relation_label(group.role.kind, group.role.direction)
                    .as_str(),
                direction: direction_slug(group.role.direction),
                rows: group
                    .rows
                    .iter()
                    .map(|row| RelationDto {
                        target: target_text(&row.target),
                        resolution: target_resolution(&row.target),
                        confidence: confidence_label(row.confidence),
                    })
                    .collect(),
            })
            .collect(),
        source: page.source.as_ref().map(|location| SourceDto {
            file: location.file.to_string(),
            start: location.span.start.0,
            end: location.span.end.0,
        }),
    }
}

const fn direction_slug(direction: interface_documents::Direction) -> &'static str {
    match direction {
        interface_documents::Direction::Outgoing => "outgoing",
        interface_documents::Direction::Incoming => "incoming",
    }
}

fn prose_blocks(prose: &Prose) -> Vec<String> {
    prose
        .blocks()
        .iter()
        .map(|block| match block {
            Block::Paragraph(inlines) => inlines
                .iter()
                .map(|inline| match inline {
                    Inline::Text(text) | Inline::Code(text) => text.as_str().to_owned(),
                    Inline::Link { label, .. } => label.as_str().to_owned(),
                    Inline::Break => " ".to_owned(),
                })
                .collect(),
            Block::Code(text) => text.as_str().to_owned(),
        })
        .collect()
}

fn outline_dto(outline: &Outline) -> OutlineDto {
    OutlineDto {
        package: outline.package.to_string(),
        census: census_dto(&outline.census),
        nodes: outline
            .walk()
            .map(|(node, depth)| OutlineNodeDto {
                depth,
                symbol: symbol_dto(&node.symbol),
                summary: node.summary.as_ref().map(ToString::to_string),
            })
            .collect(),
    }
}

fn resolve_dto(resolution: &Resolution) -> ResolveDto {
    match resolution {
        Resolution::Exact(symbol) => ResolveDto {
            resolution: "exact",
            candidates: vec![symbol_dto(symbol)],
        },
        Resolution::Ambiguous(candidates) => ResolveDto {
            resolution: "ambiguous",
            candidates: candidates.iter().map(symbol_dto).collect(),
        },
        Resolution::Unknown { .. } => ResolveDto {
            resolution: "unknown",
            candidates: Vec::new(),
        },
    }
}

fn search_dto(terminal: &SearchTerminal) -> SearchDto {
    SearchDto {
        query: terminal.request.text.as_str().to_owned(),
        hits: terminal
            .hits
            .iter()
            .map(|hit| HitDto {
                symbol: symbol_dto(&hit.symbol),
                signature: hit.signature.as_ref().map(Signature::plain),
                signature_tokens: hit.signature.as_ref().map(token_dtos).unwrap_or_default(),
                summary: hit.summary.as_ref().map(ToString::to_string),
                lane: hit.lane.label(),
                score: hit.score.0,
            })
            .collect(),
        lanes: terminal.lanes.iter().map(lane_dto).collect(),
        truncation: match terminal.truncation {
            Truncation::Complete => TruncationDto {
                state: "complete",
                next_cursor: None,
            },
            Truncation::Truncated { next } => TruncationDto {
                state: "truncated",
                next_cursor: Some(next.0),
            },
        },
    }
}

fn lane_dto(report: &LaneReport) -> LaneDto {
    LaneDto {
        lane: report.lane.label(),
        coverage: coverage_dto(report.coverage),
        hits: report.hits.0,
        elapsed_micros: report.elapsed.map(|elapsed| elapsed.0),
    }
}

fn coverage_dto(coverage: Coverage) -> CoverageDto {
    match coverage {
        Coverage::Complete => CoverageDto {
            state: "complete",
            reason: None,
            searched: None,
            total: None,
        },
        Coverage::Partial { searched, total } => CoverageDto {
            state: "partial",
            reason: None,
            searched: Some(searched.0),
            total: Some(total.0),
        },
        Coverage::Degraded { reason } => CoverageDto {
            state: "degraded",
            reason: Some(degradation_slug(reason)),
            searched: None,
            total: None,
        },
        Coverage::Unavailable { reason } => CoverageDto {
            state: "unavailable",
            reason: Some(unavailability_slug(reason)),
            searched: None,
            total: None,
        },
    }
}

fn graph_dto(terminal: &GraphTerminal) -> GraphDto {
    GraphDto {
        source: symbol_dto(&terminal.source),
        edges: terminal
            .edges
            .iter()
            .map(|edge| {
                let direction = if edge.from.key() == terminal.source.key() {
                    interface_documents::Direction::Outgoing
                } else {
                    interface_documents::Direction::Incoming
                };
                EdgeDto {
                    from: edge.from.address.to_string(),
                    relation: interface_search::relation_label(edge.kind, direction).as_str(),
                    direction: direction_slug(direction),
                    target: target_text(&edge.to),
                    resolution: target_resolution(&edge.to),
                    confidence: confidence_label(edge.confidence),
                    hop: edge.hop.get(),
                }
            })
            .collect(),
        coverage: coverage_dto(terminal.coverage),
    }
}

fn health_dto(health: &Health) -> HealthDto {
    HealthDto {
        capabilities: health
            .rows()
            .iter()
            .map(|(capability, state)| CapabilityDto {
                capability: Capability::label(*capability),
                state: capability_slug(state),
                detail: match state {
                    CapabilityState::Unreachable { detail } => Some(detail.to_string()),
                    CapabilityState::Ready
                    | CapabilityState::Unconfigured
                    | CapabilityState::Detached => None,
                },
            })
            .collect(),
    }
}

fn index_search_dto(page: &interface_library::IndexSearchPage) -> IndexSearchDto {
    IndexSearchDto {
        hits: page
            .hits
            .iter()
            .map(|hit| IndexHitDto {
                document: hit.document.as_str().to_owned(),
                matched: hit.matched.as_ref().to_owned(),                score: hit.score.0,
            })
            .collect(),
        coverage: explore_coverage_dto(page.coverage),
    }
}

fn explore_coverage_dto(
    coverage: interface_library::ExploreCoverage,
) -> ExploreCoverageDto {
    match coverage {
        interface_library::ExploreCoverage::Complete => ExploreCoverageDto {
            state: "complete",
            reason: None,
            searched: None,
            total: None,
        },
        interface_library::ExploreCoverage::Partial { searched, total } => ExploreCoverageDto {
            state: "partial",
            reason: None,
            searched: Some(searched),
            total: Some(total),
        },
        interface_library::ExploreCoverage::Unavailable { reason } => ExploreCoverageDto {
            state: "unavailable",
            reason: Some(match reason {
                interface_library::ExploreUnavailable::EmptyIndex => "empty-index",
                interface_library::ExploreUnavailable::StoreFault { slug } => slug,
                interface_library::ExploreUnavailable::CatalogAbsent => "catalog-absent",
            }),
            searched: None,
            total: None,
        },
    }
}

fn package_versions_dto(rows: &interface_library::PackageVersionRows) -> PackageVersionsDto {
    PackageVersionsDto {
        versions: rows.rows.iter().map(package_version_dto).collect(),
    }
}

fn package_profile_dto(
    profile: &interface_library::PackageProfile,
) -> PackageProfileDto {
    PackageProfileDto {
        latest: profile.latest.as_ref().map(package_version_dto),
        versions: profile
            .versions
            .rows
            .iter()
            .map(package_version_dto)
            .collect(),
    }
}

fn package_version_dto(row: &interface_library::PackageVersionRow) -> PackageVersionDto {
    PackageVersionDto {
        version: row.version.as_str().to_owned(),
        checksum: interface_library::checksum_hex(row.checksum),
        active: row.active.is_active(),
        cycle: row.cycle.get(),
    }
}
