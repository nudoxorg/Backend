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
use interface_identity::{KindTag, PackageCoordinate, ecosystem_tag};
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
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<SourceTextDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    related: Option<RelatedDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    explore: Option<ExploreDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    package: Option<PackageDetailDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dependents: Option<DependentsDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner: Option<OwnerPageDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subscribe: Option<FollowOutcomeDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unsubscribe: Option<FollowOutcomeDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subscriptions: Option<SubscriptionsDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    releases: Option<ReleasesDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    projects: Option<ProjectsDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<ProjectDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    project_deleted: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    project_sync: Option<SyncDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tree: Option<TreeDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tree_open: Option<TreeOutcomeDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tree_close: Option<TreeOutcomeDto>,
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
        Reply::Source(Ok(source)) => envelope.source = Some(source_text_dto(source)),
        Reply::Related(Ok(related)) => envelope.related = Some(related_dto(related)),
        Reply::Explored(Ok(page)) => envelope.explore = Some(explore_dto(page)),
        Reply::Detailed(Ok(detail)) => envelope.package = Some(detail_dto(detail)),
        Reply::Dependents(Ok(page)) => envelope.dependents = Some(dependents_dto(page)),
        Reply::Owned(Ok(page)) => envelope.owner = Some(owner_page_dto(page)),
        Reply::Subscribed(Ok(outcome)) => envelope.subscribe = Some(follow_outcome_dto(outcome)),
        Reply::Unsubscribed(Ok(outcome)) => {
            envelope.unsubscribe = Some(follow_outcome_dto(outcome));
        }
        Reply::Subscriptions(Ok(subscriptions)) => {
            envelope.subscriptions = Some(SubscriptionsDto {
                epoch: subscriptions.epoch.0,
                unseen: subscriptions.unseen.0,
                rows: subscriptions.rows.iter().map(subscription_dto).collect(),
            });
        }
        Reply::Releases(Ok(releases)) => envelope.releases = Some(releases_dto(releases)),
        Reply::Projects(Ok(projects)) => {
            envelope.projects = Some(ProjectsDto {
                epoch: projects.epoch.0,
                rows: projects.rows.iter().map(project_dto).collect(),
            });
        }
        Reply::ProjectCreated(Ok(project))
        | Reply::ProjectAdded(Ok(project))
        | Reply::ProjectRemoved(Ok(project)) => envelope.project = Some(project_dto(project)),
        Reply::ProjectDeleted(Ok(id)) => envelope.project_deleted = Some(id.get()),
        Reply::ProjectSynced(Ok(report)) => envelope.project_sync = Some(sync_dto(report)),
        Reply::Tree(Ok(tree)) => envelope.tree = Some(tree_dto(tree)),
        Reply::TreeOpened(Ok(outcome)) => envelope.tree_open = Some(tree_outcome_dto(outcome)),
        Reply::TreeClosed(Ok(outcome)) => envelope.tree_close = Some(tree_outcome_dto(outcome)),
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

// ── the rewritten surface's rows ────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct ProvenanceDto {
    origin: &'static str,
    fetched_at: u64,
    stale: bool,
}

#[derive(Serialize)]
struct CardDto {
    ecosystem: &'static str,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_stable: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    downloads_total: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    downloads_recent: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    license: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    keywords: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repository: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    on_shelf: Option<String>,
}

#[derive(Serialize)]
struct ExploreDto {
    page: u32,
    sort: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    ecosystem: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<String>,
    has_more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    total: Option<u32>,
    provenance: ProvenanceDto,
    cards: Vec<CardDto>,
}

#[derive(Serialize)]
struct VersionDto {
    version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    published_at: Option<u64>,
    yanked: bool,
    prerelease: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    downloads: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    toolchain: Option<String>,
}

#[derive(Serialize)]
struct OwnerDto {
    ecosystem: &'static str,
    handle: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    display: Option<String>,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
}

#[derive(Serialize)]
struct DependencyDto {
    name: String,
    requirement: String,
    kind: &'static str,
    optional: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    features: Vec<String>,
}

#[derive(Serialize)]
struct DependentsSummaryDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    count: Option<u32>,
    has_more: bool,
    sample: Vec<CardDto>,
}

#[derive(Serialize)]
struct LinksDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    repository: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    homepage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    documentation: Option<String>,
}

#[derive(Serialize)]
struct InstallDto {
    command: String,
    manifest: String,
}

#[derive(Serialize)]
struct PackageDetailDto {
    card: CardDto,
    selected: String,
    install: InstallDto,
    versions: Vec<VersionDto>,
    owners: Vec<OwnerDto>,
    dependencies: Vec<DependencyDto>,
    dependents: DependentsSummaryDto,
    links: LinksDto,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    categories: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    readme: Option<String>,
    provenance: ProvenanceDto,
}

#[derive(Serialize)]
struct DependentsDto {
    ecosystem: &'static str,
    name: String,
    page: u32,
    has_more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    total: Option<u32>,
    provenance: ProvenanceDto,
    cards: Vec<CardDto>,
}

#[derive(Serialize)]
struct OwnerPageDto {
    owner: OwnerDto,
    cards: Vec<CardDto>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    faults: Vec<EcosystemFaultDto>,
    provenance: ProvenanceDto,
}

#[derive(Serialize)]
struct EcosystemFaultDto {
    ecosystem: &'static str,
    kind: &'static str,
    detail: String,
}

#[derive(Serialize)]
struct SubscriptionDto {
    package: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    seen: Option<String>,
    followed_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    checked_at: Option<u64>,
    unseen: u32,
}

#[derive(Serialize)]
struct SubscriptionsDto {
    epoch: u64,
    unseen: u32,
    rows: Vec<SubscriptionDto>,
}

#[derive(Serialize)]
struct FollowOutcomeDto {
    outcome: &'static str,
    package: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    subscription: Option<SubscriptionDto>,
}

#[derive(Serialize)]
struct ReleaseDto {
    package: String,
    version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    published_at: Option<u64>,
    prerelease: bool,
    seen: bool,
}

#[derive(Serialize)]
struct ReleaseFaultDto {
    package: String,
    kind: &'static str,
    detail: String,
}

#[derive(Serialize)]
struct ReleasesDto {
    checked_at: u64,
    provenance: ProvenanceDto,
    rows: Vec<ReleaseDto>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    faults: Vec<ReleaseFaultDto>,
}

#[derive(Serialize)]
struct LockfileDto {
    path: String,
    kind: &'static str,
}

#[derive(Serialize)]
struct ProjectDto {
    id: u32,
    name: String,
    hue: &'static str,
    members: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lockfile: Option<LockfileDto>,
    created_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    synced_at: Option<u64>,
}

#[derive(Serialize)]
struct ProjectsDto {
    epoch: u64,
    rows: Vec<ProjectDto>,
}

#[derive(Serialize)]
struct RepinDto {
    from: String,
    to: String,
}

#[derive(Serialize)]
struct SyncDto {
    project: ProjectDto,
    added: Vec<String>,
    removed: Vec<String>,
    repinned: Vec<RepinDto>,
    unchanged: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unreadable: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    compiles: Vec<String>,
}

#[derive(Serialize)]
struct TreeNodeDto {
    id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent: Option<u64>,
    kind: &'static str,
    subject: String,
    title: String,
    opener: String,
    opened_at: u64,
    focused_at: u64,
    collapsed: bool,
}

#[derive(Serialize)]
struct TreeDto {
    epoch: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    active: Option<u64>,
    nodes: Vec<TreeNodeDto>,
}

#[derive(Serialize)]
struct TreeOutcomeDto {
    outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    node: Option<TreeNodeDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    removed: Option<u32>,
}

#[derive(Serialize)]
struct SpanDto {
    start: u32,
    end: u32,
}

#[derive(Serialize)]
struct SourceTextDto {
    symbol: SymbolDto,
    language: &'static str,
    file: String,
    span: SpanDto,
    first_line: u32,
    text: String,
    highlight: SpanDto,
    truncated: bool,
}

#[derive(Serialize)]
struct RelatedRowDto {
    symbol: SymbolDto,
    relation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
}

#[derive(Serialize)]
struct RelatedDto {
    symbol: SymbolDto,
    graph: String,
    semantic: String,
    rows: Vec<RelatedRowDto>,
}

fn provenance_dto(provenance: interface_library::Provenance) -> ProvenanceDto {
    ProvenanceDto {
        origin: match provenance.origin {
            interface_library::Origin::Live => "live",
            interface_library::Origin::Cached => "cached",
        },
        fetched_at: provenance.fetched_at.0,
        stale: provenance.stale,
    }
}

fn card_dto(card: &interface_library::RegistryCard) -> CardDto {
    CardDto {
        ecosystem: ecosystem_tag(card.ecosystem).as_str(),
        name: card.name.as_str().to_owned(),
        latest: card.latest.as_ref().map(|version| version.as_str().to_owned()),
        latest_stable: card
            .latest_stable
            .as_ref()
            .map(|version| version.as_str().to_owned()),
        description: card.description.as_ref().map(ToString::to_string),
        downloads_total: card
            .downloads
            .and_then(|downloads| downloads.total)
            .map(|count| count.0),
        downloads_recent: card
            .downloads
            .and_then(|downloads| downloads.recent)
            .map(|count| count.0),
        updated_at: card.updated_at.map(|at| at.0),
        license: card.license.as_ref().map(ToString::to_string),
        keywords: card.keywords.iter().map(ToString::to_string).collect(),
        repository: card.repository.as_ref().map(ToString::to_string),
        on_shelf: card.on_shelf.as_ref().map(ToString::to_string),
    }
}

fn explore_dto(page: &interface_library::ExplorePage) -> ExploreDto {
    ExploreDto {
        page: page.request.page.get(),
        sort: page.request.sort.name(),
        ecosystem: page
            .request
            .ecosystem
            .map(|ecosystem| ecosystem_tag(ecosystem).as_str()),
        query: page.request.query.as_ref().map(|query| query.as_str().to_owned()),
        has_more: page.has_more,
        total: page.total.map(|count| count.0),
        provenance: provenance_dto(page.provenance),
        cards: page.cards.iter().map(card_dto).collect(),
    }
}

fn owner_dto(owner: &interface_library::Owner) -> OwnerDto {
    OwnerDto {
        ecosystem: ecosystem_tag(owner.ecosystem).as_str(),
        handle: owner.handle.as_str().to_owned(),
        display: owner.display.as_ref().map(ToString::to_string),
        kind: match owner.kind {
            interface_library::OwnerKind::User => "user",
            interface_library::OwnerKind::Team => "team",
        },
        url: owner.url.as_ref().map(ToString::to_string),
    }
}

fn detail_dto(detail: &interface_library::PackageDetail) -> PackageDetailDto {
    PackageDetailDto {
        card: card_dto(&detail.card),
        selected: detail.selected.as_str().to_owned(),
        install: InstallDto {
            command: detail.install.command.to_string(),
            manifest: detail.install.manifest.to_string(),
        },
        versions: detail
            .versions
            .iter()
            .map(|version| VersionDto {
                version: version.version.as_str().to_owned(),
                published_at: version.published_at.map(|at| at.0),
                yanked: version.yanked,
                prerelease: version.prerelease,
                downloads: version.downloads.map(|count| count.0),
                toolchain: version.toolchain.as_ref().map(ToString::to_string),
            })
            .collect(),
        owners: detail.owners.iter().map(owner_dto).collect(),
        dependencies: detail
            .dependencies
            .iter()
            .map(|dependency| DependencyDto {
                name: dependency.name.as_str().to_owned(),
                requirement: dependency.requirement.to_string(),
                kind: dependency.kind.label(),
                optional: dependency.optional,
                target: dependency.target.as_ref().map(ToString::to_string),
                features: dependency.features.iter().map(ToString::to_string).collect(),
            })
            .collect(),
        dependents: DependentsSummaryDto {
            count: detail.dependents.count.map(|count| count.0),
            has_more: detail.dependents.has_more,
            sample: detail.dependents.sample.iter().map(card_dto).collect(),
        },
        links: LinksDto {
            repository: detail.links.repository.as_ref().map(ToString::to_string),
            homepage: detail.links.homepage.as_ref().map(ToString::to_string),
            documentation: detail.links.documentation.as_ref().map(ToString::to_string),
        },
        categories: detail.categories.iter().map(ToString::to_string).collect(),
        readme: detail
            .readme
            .as_ref()
            .map(|readme| readme.markdown.to_string()),
        provenance: provenance_dto(detail.provenance),
    }
}

fn dependents_dto(page: &interface_library::DependentsPage) -> DependentsDto {
    DependentsDto {
        ecosystem: ecosystem_tag(page.ecosystem).as_str(),
        name: page.name.as_str().to_owned(),
        page: page.page.get(),
        has_more: page.has_more,
        total: page.total.map(|count| count.0),
        provenance: provenance_dto(page.provenance),
        cards: page.cards.iter().map(card_dto).collect(),
    }
}

fn owner_page_dto(page: &interface_library::OwnerPage) -> OwnerPageDto {
    OwnerPageDto {
        owner: owner_dto(&page.owner),
        cards: page.cards.iter().map(card_dto).collect(),
        faults: page
            .faults
            .iter()
            .map(|(ecosystem, error)| EcosystemFaultDto {
                ecosystem: ecosystem_tag(*ecosystem).as_str(),
                kind: error.slug(),
                detail: error.detail(),
            })
            .collect(),
        provenance: provenance_dto(page.provenance),
    }
}

fn subscription_dto(row: &interface_library::Subscription) -> SubscriptionDto {
    SubscriptionDto {
        package: row.key.to_string(),
        seen: row.seen.as_ref().map(|version| version.as_str().to_owned()),
        followed_at: row.followed_at.0,
        project: row.project.map(|project| project.get()),
        checked_at: row.checked_at.map(|at| at.0),
        unseen: row.unseen.0,
    }
}

fn follow_outcome_dto(outcome: &interface_library::FollowOutcome) -> FollowOutcomeDto {
    use interface_library::FollowOutcome;
    match outcome {
        FollowOutcome::Followed { subscription } => FollowOutcomeDto {
            outcome: "followed",
            package: subscription.key.to_string(),
            subscription: Some(subscription_dto(subscription)),
        },
        FollowOutcome::AlreadyFollowing { subscription } => FollowOutcomeDto {
            outcome: "already-following",
            package: subscription.key.to_string(),
            subscription: Some(subscription_dto(subscription)),
        },
        FollowOutcome::Unfollowed { key } => FollowOutcomeDto {
            outcome: "unfollowed",
            package: key.to_string(),
            subscription: None,
        },
        FollowOutcome::NotFollowing { key } => FollowOutcomeDto {
            outcome: "not-following",
            package: key.to_string(),
            subscription: None,
        },
    }
}

fn releases_dto(releases: &interface_library::Releases) -> ReleasesDto {
    ReleasesDto {
        checked_at: releases.checked_at.0,
        provenance: provenance_dto(releases.provenance),
        rows: releases
            .rows
            .iter()
            .map(|row| ReleaseDto {
                package: row.key.to_string(),
                version: row.version.as_str().to_owned(),
                published_at: row.published_at.map(|at| at.0),
                prerelease: row.prerelease,
                seen: row.seen,
            })
            .collect(),
        faults: releases
            .faults
            .iter()
            .map(|fault| ReleaseFaultDto {
                package: fault.key.to_string(),
                kind: fault.error.slug(),
                detail: fault.error.detail(),
            })
            .collect(),
    }
}

fn project_dto(project: &interface_library::Project) -> ProjectDto {
    ProjectDto {
        id: project.id.get(),
        name: project.name.as_str().to_owned(),
        hue: project.hue.name(),
        members: project.members.iter().map(ToString::to_string).collect(),
        lockfile: project.binding.as_ref().map(|binding| LockfileDto {
            path: binding.path.display().to_string(),
            kind: binding.kind.file_name(),
        }),
        created_at: project.created_at.0,
        synced_at: project.synced_at.map(|at| at.0),
    }
}

fn sync_dto(report: &interface_library::SyncReport) -> SyncDto {
    SyncDto {
        project: project_dto(&report.project),
        added: report.added.iter().map(ToString::to_string).collect(),
        removed: report.removed.iter().map(ToString::to_string).collect(),
        repinned: report
            .repinned
            .iter()
            .map(|repin| RepinDto {
                from: repin.from.to_string(),
                to: repin.to.to_string(),
            })
            .collect(),
        unchanged: report.unchanged.0,
        unreadable: report.unreadable.iter().map(ToString::to_string).collect(),
        compiles: report.compiles.iter().map(ToString::to_string).collect(),
    }
}

fn tree_node_dto(node: &interface_library::TreeNode) -> TreeNodeDto {
    TreeNodeDto {
        id: node.id.get(),
        parent: node.parent.map(|parent| parent.get()),
        kind: node.subject.kind(),
        subject: node.subject.to_string(),
        title: node.title.to_string(),
        opener: node.opener.to_string(),
        opened_at: node.opened_at.0,
        focused_at: node.focused_at.0,
        collapsed: node.collapsed,
    }
}

fn tree_dto(tree: &interface_library::SessionTree) -> TreeDto {
    TreeDto {
        epoch: tree.epoch.0,
        active: tree.active.map(|active| active.get()),
        nodes: tree.nodes.iter().map(tree_node_dto).collect(),
    }
}

fn tree_outcome_dto(outcome: &interface_library::TreeOutcome) -> TreeOutcomeDto {
    match outcome {
        interface_library::TreeOutcome::Opened { node, reused } => TreeOutcomeDto {
            outcome: if *reused { "focused" } else { "opened" },
            node: Some(tree_node_dto(node)),
            removed: None,
        },
        interface_library::TreeOutcome::Closed { removed } => TreeOutcomeDto {
            outcome: "closed",
            node: None,
            removed: Some(removed.0),
        },
    }
}

fn source_text_dto(source: &interface_library::SourceText) -> SourceTextDto {
    SourceTextDto {
        symbol: symbol_dto(&source.symbol),
        language: interface_library::render::common::fence_tag(source.language),
        file: source.file.to_string(),
        span: SpanDto {
            start: source.span.start.0,
            end: source.span.end.0,
        },
        first_line: source.first_line.0,
        text: source.text.to_string(),
        highlight: SpanDto {
            start: source.highlight.start.0,
            end: source.highlight.end.0,
        },
        truncated: source.truncated,
    }
}

fn related_dto(related: &interface_library::Related) -> RelatedDto {
    use interface_library::Relation;
    RelatedDto {
        symbol: symbol_dto(&related.symbol),
        graph: interface_library::render::common::coverage_word(related.graph),
        semantic: interface_library::render::common::coverage_word(related.semantic),
        rows: related
            .rows
            .iter()
            .map(|row| RelatedRowDto {
                symbol: symbol_dto(&row.symbol),
                relation: match row.relation {
                    Relation::Linked { kind, direction } => {
                        interface_search::relation_label(kind, direction)
                            .as_str()
                            .to_owned()
                    }
                    Relation::Sibling => "sibling".to_owned(),
                    Relation::Semantic { score } => format!("semantic:{}", score.0),
                    Relation::Lexical { score } => format!("lexical:{}", score.0),
                },
                summary: row.summary.as_ref().map(ToString::to_string),
            })
            .collect(),
    }
}
