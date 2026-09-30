//! The registry, home, and session surfaces, in the same shape as everything else.
//!
//! Many registry rows answer with a [`SurfaceReply`] — registry packages,
//! subscriptions, project folders,
//! session tree nodes, semantic generations, declaration diffs. Today both
//! surfaces print those as pretty-printed JSON, which is the same failure as
//! the outline: a wire value shown to a person.
//!
//! A [`ProductView`] is the small shape all of them fit: a heading, a bounded
//! list of records, and at most one note. A record is one or two lines — a
//! title, an exact operand a caller can pass back, and a few tags — which is
//! exactly the record shape the search and shelf renderings already use, so a
//! reader learns it once.

use crate::drive::ContinuationCursor;
use crate::fault::Fault;
use backend_library::{
    AcquisitionDecision, AdvisoryPackageDto, DeclarationChange, DeclarationRecord, DependencyFacts,
    DiffRecord, ForgeFact, ForgePackageDetailRecord, ForgePackagePin, ForgePackageRecord,
    IndexSearchCursor, IndexSearchPage, IndexSearchResultCount, PackageDependencyRecord,
    PackageReference, ProjectRecord, RegistryDiscoveryCandidate, RegistryEvidenceFacet,
    RegistryMetadata, RegistryNativeAvailability, RegistryNativeDetails, RegistryNativeMetadata,
    RegistryPackageFactAuthority, RegistryPackageFactFreshness, RegistryPackageRecord,
    RegistryPackageSearchGroup, RegistryReleaseMatchScope, RegistrySearchGroupKind,
    RegistrySearchHit, RegistrySearchRelease, ReleaseRecord, SemanticHistoryPublicationStatus,
    SemanticVersionFreshness, SemanticVersionRecord, SubscriptionRecord, SurfaceReply,
    TreeNodeRecord, TreeOpener, TreeSubject, encode_id,
};
use backend_library::{
    IndexCancelReceipt, IndexCancelStatus, IndexJobObservation, IndexJobOutcome,
    IndexJobProgressKind, IndexJobStage, IndexJobTerminal, IndexStartResult,
};

use crate::identity::KeyTag;
use serde::{Deserialize, Serialize};

/// One product record: a title, an operand to pass back, and its tags.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductRecord {
    title: String,
    operand: Option<String>,
    tags: Box<[String]>,
    native_metadata: Option<RegistryNativeMetadata>,
    forge_details: Option<ForgePackageRecord>,
    forge_package_detail: Option<ForgePackageDetailRecord>,
    discovery_details: Option<RegistryDiscoveryCandidate>,
    package_group: Option<RegistryPackageSearchGroup>,
    history_status: Option<SemanticHistoryPublicationStatus>,
}

impl ProductRecord {
    /// Records one row.
    #[must_use]
    pub fn new(title: impl Into<String>, operand: Option<String>, tags: Vec<String>) -> Self {
        Self {
            title: title.into(),
            operand,
            tags: tags.into_boxed_slice(),
            native_metadata: None,
            forge_details: None,
            forge_package_detail: None,
            discovery_details: None,
            package_group: None,
            history_status: None,
        }
    }

    /// Attaches typed native registry facts to one registry row.
    #[must_use]
    pub fn with_native_metadata(mut self, metadata: RegistryNativeMetadata) -> Self {
        self.native_metadata = Some(metadata);
        self
    }

    /// Attaches the bounded typed forge facts carried by one forge row.
    #[must_use]
    pub fn with_forge_details(mut self, record: ForgePackageRecord) -> Self {
        self.forge_details = Some(record);
        self
    }

    /// Attaches typed forge manifest and source-pin details to one package row.
    #[must_use]
    pub fn with_forge_package_detail(mut self, record: ForgePackageDetailRecord) -> Self {
        self.forge_package_detail = Some(record);
        self
    }

    /// Attaches source-attributed registry discovery evidence to one row.
    #[must_use]
    pub fn with_discovery_details(mut self, candidate: RegistryDiscoveryCandidate) -> Self {
        self.discovery_details = Some(candidate);
        self
    }

    /// Attaches all bounded version-specific matches for one package lineage.
    #[must_use]
    pub fn with_package_group(mut self, group: RegistryPackageSearchGroup) -> Self {
        self.package_group = Some(group);
        self
    }

    /// Attaches the exact owner-reported derived-history state for one
    /// immutable semantic generation.
    #[must_use]
    pub fn with_history_status(mut self, status: SemanticHistoryPublicationStatus) -> Self {
        self.history_status = Some(status);
        self
    }

    /// Returns the readable title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Returns the exact operand a caller passes back, when there is one.
    #[must_use]
    pub fn operand(&self) -> Option<&str> {
        self.operand.as_deref()
    }

    /// Returns the tags shown after the title.
    #[must_use]
    pub fn tags(&self) -> &[String] {
        &self.tags
    }

    /// Returns the typed native registry facts carried by this row.
    #[must_use]
    pub fn native_metadata(&self) -> Option<&RegistryNativeMetadata> {
        self.native_metadata.as_ref()
    }

    /// Returns the exact typed forge facts carried by this row.
    #[must_use]
    pub fn forge_details(&self) -> Option<&ForgePackageRecord> {
        self.forge_details.as_ref()
    }

    /// Returns the typed forge manifest/source-pin detail carried by this row.
    #[must_use]
    pub fn forge_package_detail(&self) -> Option<&ForgePackageDetailRecord> {
        self.forge_package_detail.as_ref()
    }

    /// Returns exact source-only registry facts carried by this row.
    #[must_use]
    pub fn discovery_details(&self) -> Option<&RegistryDiscoveryCandidate> {
        self.discovery_details.as_ref()
    }

    /// Returns the source-scoped version group for this package result.
    #[must_use]
    pub fn package_group(&self) -> Option<&RegistryPackageSearchGroup> {
        self.package_group.as_ref()
    }

    /// Returns the exact owner-reported derived-history state, when present.
    #[must_use]
    pub fn history_status(&self) -> Option<&SemanticHistoryPublicationStatus> {
        self.history_status.as_ref()
    }
}

/// One rendered product answer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductView {
    heading: String,
    records: Box<[ProductRecord]>,
    note: Option<String>,
    fault: Option<Fault>,
    index_search_page: Option<IndexSearchPageInfo>,
    index_job: Option<IndexJobProjection>,
}

/// Exact owner-issued indexing state retained alongside its readable projection.
///
/// Keeping the ticket and observation typed here lets every product adapter
/// return the same resumable identity instead of reducing a job reply to a
/// heading or debug string.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
pub enum IndexJobProjection {
    /// Immediate start acknowledgement or terminal result.
    Started(IndexStartResult),
    /// Terminal owner receipt from the generic surface command.
    Terminal(IndexJobTerminal),
    /// Immediate progress, terminal, or unknown-ticket observation.
    Progress(IndexJobObservation),
    /// Immediate cancellation acknowledgement and exact requested identity.
    Cancellation(IndexCancelReceipt),
}

impl IndexJobProjection {
    /// Returns the exact ticket carried by this job result, when available.
    #[must_use]
    pub fn ticket(&self) -> Option<&backend_library::IndexJobTicket> {
        match self {
            Self::Started(IndexStartResult::Started { ticket, .. }) => Some(ticket),
            Self::Started(IndexStartResult::Terminal(terminal))
            | Self::Terminal(terminal)
            | Self::Progress(IndexJobObservation::Terminal(terminal)) => Some(&terminal.ticket),
            Self::Progress(IndexJobObservation::Pending(page)) => Some(&page.ticket),
            Self::Progress(IndexJobObservation::Unknown { ticket, .. }) => Some(ticket),
            Self::Cancellation(receipt) => Some(&receipt.ticket),
        }
    }

    /// Returns the ticket's canonical JSON object, ready to pass to another
    /// structured surface call.
    #[must_use]
    pub fn ticket_json(&self) -> Option<String> {
        self.ticket().map(|ticket| index_ticket_json(ticket))
    }
}

/// Page identity and continuation returned by index search.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexSearchPageInfo {
    snapshot: [u8; 32],
    evaluated_at_millis: u64,
    result_count: IndexSearchResultCount,
    next_cursor: Option<CursorProjection>,
}

/// Where a projected cursor should be passed on the next call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CursorTarget {
    /// The CLI's `--cursor` option.
    CliOption,
    /// The `cursor` argument of `backend.index_search`.
    McpIndexSearchTool,
    /// The tagged `command.cursor` field accepted by `backend.surface`.
    SurfaceCommand,
}

/// One typed owner cursor together with its adapter-facing token and usage.
///
/// The token is the only value that should be rendered or serialized. The
/// family retains the owner cursor separately so an adapter can wrap it
/// without confusing an opaque product token with a presentation cursor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CursorProjection {
    family: ContinuationCursor,
    token: String,
    target: CursorTarget,
}

impl CursorProjection {
    fn index_search(cursor: IndexSearchCursor) -> Self {
        Self {
            token: cursor.as_str().to_owned(),
            family: ContinuationCursor::IndexSearch(cursor),
            target: CursorTarget::CliOption,
        }
    }

    /// Creates an adapter-facing projection of an owner cursor.
    #[must_use]
    pub fn projected(
        family: ContinuationCursor,
        token: impl Into<String>,
        target: CursorTarget,
    ) -> Self {
        Self {
            family,
            token: token.into(),
            target,
        }
    }

    /// Returns the owner-issued cursor family.
    #[must_use]
    pub const fn family(&self) -> &ContinuationCursor {
        &self.family
    }

    /// Returns the token exposed to this surface's caller.
    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Returns the caller's resume field or option.
    #[must_use]
    pub const fn target(&self) -> CursorTarget {
        self.target
    }
}

impl IndexSearchPageInfo {
    /// Structural index snapshot shared by pages in this cursor chain.
    #[must_use]
    pub const fn snapshot(&self) -> &[u8; 32] {
        &self.snapshot
    }

    /// Time at which mutable freshness overlays were evaluated.
    #[must_use]
    pub const fn evaluated_at_millis(&self) -> u64 {
        self.evaluated_at_millis
    }

    /// Exact count, lower bound, or unknown count state.
    #[must_use]
    pub const fn result_count(&self) -> IndexSearchResultCount {
        self.result_count
    }

    /// Caller-facing continuation for the following result page.
    #[must_use]
    pub fn next_cursor(&self) -> Option<&str> {
        self.next_cursor.as_ref().map(CursorProjection::token)
    }

    /// Typed owner cursor retained behind the caller-facing projection.
    #[must_use]
    pub fn cursor_projection(&self) -> Option<&CursorProjection> {
        self.next_cursor.as_ref()
    }
}

impl ProductView {
    /// Returns the heading naming what was asked.
    #[must_use]
    pub fn heading(&self) -> &str {
        &self.heading
    }

    /// Returns the records in reply order.
    #[must_use]
    pub fn records(&self) -> &[ProductRecord] {
        &self.records
    }

    /// Returns the one-line note, when the reply carried a scalar answer.
    #[must_use]
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// Returns the fault explaining a fact the configured feed does not publish.
    #[must_use]
    pub const fn fault(&self) -> Option<&Fault> {
        self.fault.as_ref()
    }

    /// Returns page metadata when this answer came from index search.
    #[must_use]
    pub fn index_search_page(&self) -> Option<&IndexSearchPageInfo> {
        self.index_search_page.as_ref()
    }

    /// Returns the exact owner indexing state carried by this product reply.
    #[must_use]
    pub const fn index_job(&self) -> Option<&IndexJobProjection> {
        self.index_job.as_ref()
    }

    /// Returns this product answer's typed owner cursor, when it has one.
    #[must_use]
    pub fn cursor_family(&self) -> Option<&ContinuationCursor> {
        self.index_search_page
            .as_ref()?
            .cursor_projection()
            .map(CursorProjection::family)
    }

    /// Replaces the caller-facing cursor while retaining its owner family.
    #[must_use]
    pub fn project_cursor(mut self, token: impl Into<String>, target: CursorTarget) -> Self {
        let token = token.into();
        if let Some(cursor) = self
            .index_search_page
            .as_mut()
            .and_then(|page| page.next_cursor.as_mut())
        {
            *cursor = CursorProjection::projected(cursor.family.clone(), token, target);
        }
        self
    }

    fn with_index_search_page(mut self, page: &IndexSearchPage) -> Self {
        self.index_search_page = Some(IndexSearchPageInfo {
            snapshot: page.snapshot,
            evaluated_at_millis: page.evaluated_at_millis,
            result_count: page.result_count,
            next_cursor: page
                .next_cursor
                .as_ref()
                .cloned()
                .map(CursorProjection::index_search),
        });
        self
    }

    fn with_index_job(mut self, index_job: IndexJobProjection) -> Self {
        self.index_job = Some(index_job);
        self
    }

    /// Records one product answer a surface assembled itself.
    ///
    /// An accepted intent is not a [`SurfaceReply`], but it is the same shape
    /// to a reader, so it uses the same value rather than a parallel one.
    #[must_use]
    pub fn assembled(heading: impl Into<String>, records: Vec<ProductRecord>) -> Self {
        Self {
            heading: heading.into(),
            records: records.into_boxed_slice(),
            note: None,
            fault: None,
            index_search_page: None,
            index_job: None,
        }
    }

    /// Records one product answer that is a single sentence.
    #[must_use]
    pub fn stated(heading: impl Into<String>, note: impl Into<String>) -> Self {
        Self {
            heading: heading.into(),
            records: Box::new([]),
            note: Some(note.into()),
            fault: None,
            index_search_page: None,
            index_job: None,
        }
    }

    fn rows(heading: &str, records: Vec<ProductRecord>) -> Self {
        Self {
            heading: heading.to_owned(),
            records: records.into_boxed_slice(),
            note: None,
            fault: None,
            index_search_page: None,
            index_job: None,
        }
    }

    fn scalar(heading: &str, note: impl Into<String>) -> Self {
        Self {
            heading: heading.to_owned(),
            records: Box::new([]),
            note: Some(note.into()),
            fault: None,
            index_search_page: None,
            index_job: None,
        }
    }

    fn refused(heading: &str, fault: Fault) -> Self {
        Self {
            heading: heading.to_owned(),
            records: Box::new([]),
            note: None,
            fault: Some(fault),
            index_search_page: None,
            index_job: None,
        }
    }
}

/// Lowers one durable product reply into the shared record shape.
#[must_use]
pub fn product_view(reply: &SurfaceReply) -> ProductView {
    registry_view(reply)
        .or_else(|| home_view(reply))
        .unwrap_or_else(|| session_view(reply))
}

/// Registry, library, and semantic-generation answers.
fn registry_view(reply: &SurfaceReply) -> Option<ProductView> {
    Some(match reply {
        SurfaceReply::Read(records) => {
            ProductView::rows("read", records.iter().map(declaration_row).collect())
        }
        SurfaceReply::References { target, references } => ProductView::rows(
            format!("references to {}", target.as_str()).as_str(),
            references.iter().map(reference_row).collect(),
        ),
        SurfaceReply::Diff(records) => {
            ProductView::rows("diff", records.iter().map(diff_row).collect())
        }
        SurfaceReply::Explored(records) => ProductView::rows("explore", registry_rows(records)),
        SurfaceReply::Package(records) => ProductView::rows("package", registry_rows(records)),
        SurfaceReply::PackageDetails { registry, forge } => {
            let mut rows = registry_rows(registry);
            rows.extend(forge.iter().map(forge_package_detail_row));
            ProductView::rows("package", rows)
        }
        SurfaceReply::ForgePackageAdded(record) => {
            ProductView::rows("forge-add", vec![forge_row(record)])
        }
        SurfaceReply::ForgePackageReferenced(record) => {
            ProductView::rows("forge-reference", vec![forge_row(record)])
        }
        SurfaceReply::IndexSearch(records) => {
            ProductView::rows("index-search", registry_rows(records))
        }
        SurfaceReply::IndexSearchWithDiscovery(hits) => ProductView::rows(
            "index-search",
            hits.iter().map(registry_search_hit_row).collect(),
        ),
        SurfaceReply::IndexSearchPage(page) => ProductView::rows(
            "index-search",
            page.hits.iter().map(registry_search_hit_row).collect(),
        )
        .with_index_search_page(page),
        SurfaceReply::PackageVersions(records) => {
            ProductView::rows("package-versions", registry_rows(records))
        }
        SurfaceReply::Advisory(advisory) => advisory_view(advisory),
        SurfaceReply::Dependents(metadata) => metadata_view("dependents", metadata),
        SurfaceReply::Dependencies(facts) => dependency_view("dependencies", facts),
        SurfaceReply::PackageGraphPage(page) => package_graph_page_view(page),
        SurfaceReply::Owner(metadata) => owner_view(metadata),
        SurfaceReply::SemanticVersions(records) => ProductView::rows(
            "semantic-versions",
            records.iter().map(semantic_row).collect(),
        ),
        SurfaceReply::SemanticVersionSelected(record) => {
            ProductView::rows("select-semantic-version", vec![semantic_row(record)])
        }
        SurfaceReply::IndexStarted(result) => {
            index_start_view(result).with_index_job(IndexJobProjection::Started(result.clone()))
        }
        SurfaceReply::IndexTerminal(terminal) => index_terminal_view(terminal)
            .with_index_job(IndexJobProjection::Terminal(terminal.clone())),
        SurfaceReply::IndexProgress(observation) => index_observation_view(observation)
            .with_index_job(IndexJobProjection::Progress(observation.clone())),
        SurfaceReply::IndexCancellation(receipt) => index_cancellation_view(&receipt.status)
            .with_index_job(IndexJobProjection::Cancellation(receipt.clone())),
        _ => return None,
    })
}

fn index_start_view(result: &IndexStartResult) -> ProductView {
    match result {
        IndexStartResult::Started { ticket, stage } => ProductView::rows(
            "index-start",
            vec![ProductRecord::new(
                format!("index job {} started", ticket.id()),
                Some(index_ticket_json(ticket)),
                vec![
                    format!("stage {}", index_stage(*stage)),
                    "poll index_progress".to_owned(),
                ],
            )],
        ),
        IndexStartResult::Terminal(terminal) => index_terminal_view(terminal),
    }
}

fn index_terminal_view(terminal: &IndexJobTerminal) -> ProductView {
    let (state, detail) = match &terminal.outcome {
        IndexJobOutcome::Published => ("published", None),
        IndexJobOutcome::Refused(reason) => ("refused", Some(reason.as_str())),
        IndexJobOutcome::Cancelled => ("cancelled", None),
        IndexJobOutcome::Failed(reason) => ("failed", Some(reason.as_str())),
    };
    let mut tags = vec![format!("outcome {state}")];
    if let Some(detail) = detail {
        tags.push(detail.to_owned());
    }
    ProductView::rows(
        "index-job-terminal",
        vec![ProductRecord::new(
            format!("index job {} is {state}", terminal.ticket.id()),
            Some(index_ticket_json(&terminal.ticket)),
            tags,
        )],
    )
}

fn index_observation_view(observation: &IndexJobObservation) -> ProductView {
    match observation {
        IndexJobObservation::Pending(page) => {
            let mut tags = vec![
                format!("stage {}", index_stage(page.stage)),
                format!("next_sequence {}", page.next_sequence),
            ];
            if page.truncated {
                tags.push("older progress events aged out".to_owned());
            }
            if page.has_more {
                tags.push("more events available".to_owned());
            }
            let mut records = vec![ProductRecord::new(
                format!("index job {} pending", page.ticket.id()),
                Some(index_ticket_json(&page.ticket)),
                tags,
            )];
            records.extend(page.events.iter().map(|event| {
                let mut tags = vec![format!("sequence {}", event.sequence)];
                let title = match &event.kind {
                    IndexJobProgressKind::StageChanged { stage } => {
                        format!("stage changed to {}", index_stage(*stage))
                    }
                    IndexJobProgressKind::ProfileStarted {
                        profile,
                        ordinal,
                        total,
                    } => {
                        tags.push(format!("profile {ordinal}/{total}"));
                        format!("{} profile started", profile.name().unwrap_or("unknown"))
                    }
                    IndexJobProgressKind::ProfileAdmitted {
                        profile,
                        ordinal,
                        total,
                    } => {
                        tags.push(format!("profile {ordinal}/{total}"));
                        format!("{} profile admitted", profile.name().unwrap_or("unknown"))
                    }
                };
                ProductRecord::new(title, None, tags)
            }));
            ProductView::rows("index-progress", records)
        }
        IndexJobObservation::Terminal(terminal) => index_terminal_view(terminal),
        IndexJobObservation::Unknown {
            ticket,
            current_owner_epoch,
        } => {
            let owner_restarted = ticket.owner_epoch() != *current_owner_epoch;
            ProductView::rows(
                "index-progress",
                vec![ProductRecord::new(
                    format!(
                        "index job {} is unknown{}",
                        ticket.id(),
                        if owner_restarted {
                            " after owner restart"
                        } else {
                            ""
                        }
                    ),
                    Some(index_ticket_json(ticket)),
                    vec![if owner_restarted {
                        "ticket owner epoch differs from current owner".to_owned()
                    } else {
                        "ticket is no longer active or retained".to_owned()
                    }],
                )],
            )
        }
    }
}

fn index_cancellation_view(status: &IndexCancelStatus) -> ProductView {
    match status {
        IndexCancelStatus::Requested => ProductView::scalar(
            "index-cancel",
            "cancellation was requested; poll index_progress",
        ),
        IndexCancelStatus::Terminal(terminal) => index_terminal_view(terminal),
        IndexCancelStatus::Unknown => ProductView::scalar(
            "index-cancel",
            "no active or retained terminal job matched the exact ticket",
        ),
    }
}

fn index_stage(stage: IndexJobStage) -> &'static str {
    match stage {
        IndexJobStage::Acquiring => "acquiring",
        IndexJobStage::Staging => "staging",
        IndexJobStage::Scanning => "scanning",
        IndexJobStage::Compiling => "compiling",
        IndexJobStage::Publishing => "publishing",
    }
}

fn index_ticket_json(ticket: &backend_library::IndexJobTicket) -> String {
    serde_json::json!({
        "id": ticket.id(),
        "owner_epoch": ticket.owner_epoch(),
        "package": ticket.package(),
    })
    .to_string()
}

fn discovery_row(candidate: &backend_library::RegistryDiscoveryCandidate) -> ProductRecord {
    let source = candidate
        .source
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let standing = match candidate.standing {
        backend_library::RegistryDiscoveryStanding::Published => "published",
        backend_library::RegistryDiscoveryStanding::Yanked => "yanked",
        backend_library::RegistryDiscoveryStanding::Withdrawn => "withdrawn",
        backend_library::RegistryDiscoveryStanding::RecipeAvailable => "recipe available",
    };
    let completeness = match candidate.completeness {
        backend_library::RegistryDiscoveryCompleteness::CompleteThroughCursor => {
            "complete through cursor"
        }
        backend_library::RegistryDiscoveryCompleteness::Windowed => "windowed coverage",
        backend_library::RegistryDiscoveryCompleteness::Unsupported => "unsupported feed",
        backend_library::RegistryDiscoveryCompleteness::Incomplete => "incomplete coverage",
    };
    let freshness = match candidate.freshness {
        backend_library::RegistryDiscoveryFreshness::Current { .. } => "current observation",
        backend_library::RegistryDiscoveryFreshness::Historical { .. } => "historical observation",
        backend_library::RegistryDiscoveryFreshness::Expired { .. } => "expired observation",
        backend_library::RegistryDiscoveryFreshness::Unavailable {
            historical: true, ..
        } => "refresh unavailable · historical",
        backend_library::RegistryDiscoveryFreshness::Unavailable {
            historical: false, ..
        } => "refresh unavailable",
    };
    let frontier = if candidate.caught_up {
        "source caught up"
    } else {
        "source backfill in progress"
    };
    let mut tags = vec![
        "discovered · unacquired".to_owned(),
        standing.to_owned(),
        completeness.to_owned(),
        frontier.to_owned(),
        freshness.to_owned(),
        format!("source {source}"),
    ];
    match &candidate.metadata.downloads {
        RegistryEvidenceFacet::Known(value) => tags.push(format!("{value} downloads")),
        RegistryEvidenceFacet::Absent => tags.push("downloads not published".to_owned()),
        RegistryEvidenceFacet::Unknown => tags.push("downloads unknown".to_owned()),
    }
    match &candidate.metadata.advisories {
        RegistryEvidenceFacet::Known(advisories) if advisories.is_empty() => {
            tags.push("no advisories reported".to_owned())
        }
        RegistryEvidenceFacet::Known(advisories) => {
            tags.push(format!("{} advisory record(s)", advisories.len()))
        }
        RegistryEvidenceFacet::Absent => tags.push("advisories not published".to_owned()),
        RegistryEvidenceFacet::Unknown => tags.push("advisories unknown".to_owned()),
    }
    ProductRecord::new(
        candidate.coordinate.as_str(),
        Some(candidate.coordinate.as_str().to_owned()),
        tags,
    )
    .with_discovery_details(candidate.clone())
}

fn registry_search_hit_row(hit: &RegistrySearchHit) -> ProductRecord {
    match hit {
        RegistrySearchHit::Acquired(record) | RegistrySearchHit::LocalDeclaration(record) => {
            registry_row(record)
        }
        RegistrySearchHit::Discovered(candidate) => discovery_row(candidate),
        RegistrySearchHit::ForgeDiscovered(candidate) => forge_discovery_row(candidate),
        RegistrySearchHit::ForgeSourcePin(candidate) => forge_package_detail_row(candidate),
        RegistrySearchHit::PackageGroup(group) => registry_search_group_row(group),
    }
}

fn forge_package_detail_row(detail: &ForgePackageDetailRecord) -> ProductRecord {
    let source = detail
        .source_id
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let (title, operand, pin_label) = match &detail.pin {
        ForgePackagePin::PackageVersion { coordinate } => (
            coordinate.as_str().to_owned(),
            Some(coordinate.as_str().to_owned()),
            "manifest package version",
        ),
        ForgePackagePin::PinnedRevision { .. } => (
            match &detail.manifest.name {
                ForgeFact::Recorded(name) => format!("{} · source pin", name.as_str()),
                ForgeFact::Unavailable(_) => {
                    format!("{} · source pin", detail.source.repository_url())
                }
            },
            Some(detail.source.canonical()),
            "source pin · no package version",
        ),
    };
    let yanked = match &detail.registry.yanked {
        backend_library::RegistryEvidenceFacet::Known(true) => "registry yanked",
        backend_library::RegistryEvidenceFacet::Known(false) => "registry not yanked",
        backend_library::RegistryEvidenceFacet::Absent => "registry yank state absent",
        backend_library::RegistryEvidenceFacet::Unknown => "registry yank state unknown",
    };
    let downloads = match &detail.registry.downloads {
        backend_library::RegistryDownloadCount::Exact(value) => {
            format!("{value} registry downloads")
        }
        backend_library::RegistryDownloadCount::Approximate(value) => {
            format!("about {value} registry downloads")
        }
        backend_library::RegistryDownloadCount::Unavailable(_) => {
            "registry downloads unknown".to_owned()
        }
    };
    let advisory = match &detail.registry.advisory.coverage {
        backend_library::AdvisoryCoverage::Complete
            if detail.registry.advisory.advisories.is_empty() =>
        {
            "no registry advisories reported".to_owned()
        }
        backend_library::AdvisoryCoverage::Complete => {
            format!(
                "{} registry advisory record(s)",
                detail.registry.advisory.advisories.len()
            )
        }
        _ => "registry advisories unknown".to_owned(),
    };
    ProductRecord::new(
        title,
        operand,
        vec![
            pin_label.to_owned(),
            detail.manifest.ecosystem.as_str().to_owned(),
            format!("forge source {source}"),
            format!("repository revision: {}", detail.source.canonical()),
            format!("resolved commit: {}", detail.resolved_commit.as_hex()),
            format!("manifest: {}", detail.manifest.path.as_str()),
            yanked.to_owned(),
            downloads,
            advisory,
        ],
    )
    .with_forge_package_detail(detail.clone())
}

fn registry_search_group_row(group: &RegistryPackageSearchGroup) -> ProductRecord {
    let source = group
        .source
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let mut versions = group
        .releases
        .iter()
        .map(|release| match release {
            RegistrySearchRelease::Acquired(record) => record.coordinate.as_str().to_owned(),
            RegistrySearchRelease::Discovered(candidate) => {
                candidate.coordinate.as_str().to_owned()
            }
            RegistrySearchRelease::ForgeDiscovered(candidate) => {
                candidate.coordinate.as_str().to_owned()
            }
        })
        .collect::<Vec<_>>();
    versions.sort();
    let kind = match group.kind {
        backend_library::RegistrySearchGroupKind::Acquired => "acquired",
        backend_library::RegistrySearchGroupKind::Discovered => "discovered · unacquired",
        backend_library::RegistrySearchGroupKind::Forge => "forge source",
    };
    let mut tags = vec![
        kind.to_owned(),
        group.ecosystem.as_str().to_owned(),
        format!("source {source}"),
        match group.release_match_scope {
            RegistryReleaseMatchScope::ReleaseMatches => {
                format!("{} matching version(s)", versions.len())
            }
            RegistryReleaseMatchScope::LineageMetadataOnly => {
                format!("{} representative version(s)", versions.len())
            }
        },
    ];
    if group.release_match_scope == RegistryReleaseMatchScope::LineageMetadataOnly {
        tags.push("package metadata matched across releases".to_owned());
    }
    if group.more_releases {
        tags.push(match group.release_match_scope {
            RegistryReleaseMatchScope::ReleaseMatches => "more matching versions".to_owned(),
            RegistryReleaseMatchScope::LineageMetadataOnly => "more versions in lineage".to_owned(),
        });
    }
    if let RegistrySearchGroupKind::Discovered = group.kind {
        let known_yanks = group
            .releases
            .iter()
            .filter(|release| {
                matches!(release, RegistrySearchRelease::Discovered(candidate)
                if candidate.standing == backend_library::RegistryDiscoveryStanding::Yanked)
            })
            .count();
        if known_yanks > 0 {
            tags.push(format!("{known_yanks} yanked version(s)"));
        }
    }
    ProductRecord::new(group.lineage.as_str(), versions.first().cloned(), tags)
        .with_package_group(group.clone())
}

fn forge_discovery_row(candidate: &backend_library::ForgeDiscoveryCandidate) -> ProductRecord {
    let source = candidate
        .source
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let commit = brief_forge_fact(&candidate.commit, |value| value.as_str().to_owned());
    let license = brief_forge_fact(&candidate.metadata.license, |value| {
        value.as_str().to_owned()
    });
    let readme = match &candidate.metadata.readme {
        backend_library::ForgeFact::Recorded(readme) => {
            format!("recorded ({} bytes)", readme.as_str().len())
        }
        backend_library::ForgeFact::Unavailable(reason) => reason.as_str().to_owned(),
    };
    let dependencies = match &candidate.manifest.dependencies {
        backend_library::DependencyFacts::Known(rows) => {
            format!("{} known", rows.len())
        }
        backend_library::DependencyFacts::Unknown(reason) => {
            format!("unknown: {}", reason.as_str())
        }
        backend_library::DependencyFacts::Unavailable(reason) => {
            format!("unavailable: {}", reason.as_str())
        }
    };
    ProductRecord::new(
        candidate.coordinate.as_str(),
        Some(candidate.coordinate.as_str().to_owned()),
        vec![
            "discovered · forge source".to_owned(),
            format!("forge source {source}"),
            format!(
                "repository revision: {}",
                candidate.forge_coordinate.as_str()
            ),
            format!("resolved commit: {commit}"),
            format!("language: {}", candidate.manifest.ecosystem.as_str()),
            format!("manifest: {}", candidate.manifest.path.as_str()),
            format!("license: {license}"),
            format!("readme: {readme}"),
            format!("dependencies: {dependencies}"),
        ],
    )
}

/// Subscription and project-folder answers.
fn home_view(reply: &SurfaceReply) -> Option<ProductView> {
    Some(match reply {
        SurfaceReply::PackageProfile {
            latest,
            versions,
            candidate_authority,
        } => ProductView::rows(
            "package-profile",
            match latest {
                Some(record) => {
                    let mut row = registry_row(record);
                    row.tags = append(&row.tags, format!("{versions} version(s)"));
                    vec![row]
                }
                None if *versions == 0 => {
                    vec![ProductRecord::new("no version recorded", None, Vec::new())]
                }
                None => vec![ProductRecord::new(
                    "latest release is not confirmed",
                    None,
                    vec![
                        format!("{versions} recorded version(s)"),
                        registry_authority_tag(*candidate_authority),
                    ],
                )],
            },
        ),
        SurfaceReply::Subscribed(record) => {
            ProductView::rows("subscribe", vec![subscription_row(record)])
        }
        SurfaceReply::Unsubscribed(removed) => ProductView::scalar(
            "unsubscribe",
            if *removed {
                "the subscription was removed"
            } else {
                "no subscription existed for that package"
            },
        ),
        SurfaceReply::Subscriptions(records) => ProductView::rows(
            "subscriptions",
            records.iter().map(subscription_row).collect(),
        ),
        SurfaceReply::Releases(records) => {
            ProductView::rows("releases", records.iter().map(release_row).collect())
        }
        SurfaceReply::Projects(records) => {
            ProductView::rows("projects", records.iter().map(project_row).collect())
        }
        SurfaceReply::ProjectCreated(record) => {
            ProductView::rows("project-create", vec![project_row(record)])
        }
        SurfaceReply::ProjectAdded(record) => {
            ProductView::rows("project-add", vec![project_row(record)])
        }
        SurfaceReply::ProjectRemoved(record) => {
            ProductView::rows("project-remove", vec![project_row(record)])
        }
        SurfaceReply::ProjectSynced(record) => {
            ProductView::rows("project-sync", vec![project_row(record)])
        }
        SurfaceReply::ProjectDeleted(id) => ProductView::scalar(
            "project-delete",
            format!("project {} was deleted", id.get()),
        ),
        SurfaceReply::ProjectTree(tree) => tree_view(tree),
        SurfaceReply::AdvisoryRefreshed(states) => ProductView::rows(
            "advisory-refresh",
            states.iter().map(advisory_source_row).collect(),
        ),
        _ => return None,
    })
}

/// Session-tree answers.
fn session_view(reply: &SurfaceReply) -> ProductView {
    match reply {
        SurfaceReply::Tree(records) => {
            ProductView::rows("tree", records.iter().map(tree_row).collect())
        }
        SurfaceReply::TreeOpened(record) => ProductView::rows("tree-open", vec![tree_row(record)]),
        SurfaceReply::TreeClosed(count) => {
            ProductView::scalar("tree-close", format!("{count} node(s) closed"))
        }
        other => ProductView::scalar("surface", format!("{:?}", other.id())),
    }
}

/// A project's tree: the lede, what affects it, each role, then each package
/// that is here twice, in the same words the desktop's Library page uses.
fn tree_view(tree: &backend_library::browse::ProjectTree) -> ProductView {
    let reading = crate::browse::read_tree(tree);
    let mut records = Vec::new();
    let mut tags = Vec::new();
    tags.extend(reading.elsewhere.clone());
    tags.push(reading.health.clone());
    tags.extend(reading.twice_line.clone());
    records.push(ProductRecord::new(
        reading.lede.clone(),
        Some(tree.root.clone()),
        tags,
    ));
    if let Some(note) = &reading.source_note {
        records.push(ProductRecord::new(note.clone(), None, Vec::new()));
    }
    for alert in &reading.alerts {
        let mut tags = vec![alert.id.clone()];
        tags.extend(alert.summary.clone());
        records.push(ProductRecord::new(
            alert.title.clone(),
            Some(alert.why.clone()),
            tags,
        ));
    }
    for role in &reading.roles {
        let mut tags = Vec::new();
        tags.extend(role.serving.clone());
        tags.extend(role.rows.iter().map(|row| match &row.at_rest {
            Some(rest) => format!("{} ({rest})", row.name),
            None => row.name.clone(),
        }));
        tags.extend(role.brings.clone());
        records.push(ProductRecord::new(role.label, None, tags));
    }
    for twice in &reading.twice {
        let copies = twice
            .copies
            .iter()
            .map(|(version, yours)| {
                if *yours {
                    format!("{version} (yours)")
                } else {
                    version.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" · ");
        records.push(ProductRecord::new(
            format!("{} · {copies}", twice.name),
            Some(twice.paths.join("  |  ")),
            vec![twice.verdict.clone()],
        ));
    }
    ProductView::rows(&format!("your tree · {}", reading.name), records)
}

fn advisory_source_row(state: &backend_library::browse::AdvisorySourceState) -> ProductRecord {
    let mut tags = vec![
        format!(
            "{} advisories",
            crate::browse::count(usize::try_from(state.advisories).unwrap_or(usize::MAX))
        ),
        if state.complete {
            "a complete source".to_owned()
        } else {
            "a partial source".to_owned()
        },
    ];
    tags.extend(state.error.clone());
    ProductRecord::new(state.source.clone(), None, tags)
}

fn append(tags: &[String], extra: String) -> Box<[String]> {
    let mut all = tags.to_vec();
    all.push(extra);
    all.into_boxed_slice()
}

fn owner_view(metadata: &RegistryMetadata<Box<[RegistryPackageRecord]>>) -> ProductView {
    match metadata {
        RegistryMetadata::Recorded(records) => {
            ProductView::rows("owner", records.iter().map(owner_row).collect())
        }
        RegistryMetadata::NotRecorded(reason) => metadata_view("owner", metadata),
    }
}

fn owner_row(record: &RegistryPackageRecord) -> ProductRecord {
    if matches!(
        &record.coordinate,
        backend_library::PackageReference::Local(_)
    ) && record.coordinate.as_str().contains("::")
    {
        return ProductRecord::new(
            record.name.as_str(),
            Some(record.coordinate.as_str().to_owned()),
            vec![
                format!("{:?}", record.ecosystem).to_lowercase(),
                "indexed file".to_owned(),
            ],
        );
    }
    registry_row(record)
}

fn metadata_view(
    heading: &str,
    metadata: &RegistryMetadata<Box<[RegistryPackageRecord]>>,
) -> ProductView {
    match metadata {
        RegistryMetadata::Recorded(records) => ProductView::rows(heading, registry_rows(records)),
        RegistryMetadata::NotRecorded(reason) => ProductView::refused(
            heading,
            Fault::new(
                crate::fault::FaultSlug::LaneUnavailable,
                crate::fault::Operand::Text(heading.to_owned()),
                crate::fault::Cause::new(
                    crate::fault::CauseSlug::Unconfigured,
                    format!(
                        "the configured feed does not publish this fact: {}",
                        reason.as_str()
                    ),
                ),
                crate::fault::Affordance::None,
            ),
        ),
    }
}

fn dependency_view(
    heading: &str,
    facts: &DependencyFacts<Box<[PackageDependencyRecord]>>,
) -> ProductView {
    match facts {
        DependencyFacts::Known(records) => {
            ProductView::rows(heading, records.iter().map(dependency_row).collect())
        }
        DependencyFacts::Unknown(reason) | DependencyFacts::Unavailable(reason) => {
            ProductView::refused(
                heading,
                Fault::new(
                    crate::fault::FaultSlug::LaneUnavailable,
                    crate::fault::Operand::Text(heading.to_owned()),
                    crate::fault::Cause::new(
                        crate::fault::CauseSlug::Unconfigured,
                        reason.as_str().to_owned(),
                    ),
                    crate::fault::Affordance::None,
                ),
            )
        }
    }
}

fn package_graph_page_view(page: &backend_library::PackageGraphPage) -> ProductView {
    use backend_library::{PackageGraphDirection, PackageGraphKnowledge, PackageGraphPageTerminal};
    let direction = match page.direction {
        PackageGraphDirection::Dependencies => "dependencies",
        PackageGraphDirection::Dependents => "dependents",
    };
    let mut records = vec![ProductRecord::new(
        format!("{} · {direction}", page.package.as_str()),
        Some(page.package.as_str().to_owned()),
        vec![
            format!("view root: {}", lower_hex(&page.view_root)),
            format!("facts witness: {}", lower_hex(&page.facts_witness)),
            page.catalog_snapshot
                .map(|snapshot| format!("catalog snapshot: {}", lower_hex(&snapshot)))
                .unwrap_or_else(|| "catalog snapshot: unavailable".to_owned()),
        ],
    )];
    match &page.knowledge {
        PackageGraphKnowledge::Known if page.rows.is_empty() => records.push(ProductRecord::new(
            "Known empty dependency graph",
            None,
            Vec::new(),
        )),
        PackageGraphKnowledge::Known => records.push(ProductRecord::new(
            format!("{} graph edges · known", page.rows.len()),
            None,
            Vec::new(),
        )),
        PackageGraphKnowledge::Unknown { reason } => records.push(ProductRecord::new(
            "Graph facts are unknown",
            reason.as_ref().map(|value| value.as_str().to_owned()),
            Vec::new(),
        )),
        PackageGraphKnowledge::Unavailable { reason } => records.push(ProductRecord::new(
            "Graph facts are unavailable",
            Some(reason.as_str().to_owned()),
            Vec::new(),
        )),
        PackageGraphKnowledge::Partial {
            reason,
            unavailable,
        } => records.push(ProductRecord::new(
            if *unavailable {
                "Some graph sources are unavailable · recorded matches shown"
            } else {
                "Some graph sources are unknown · recorded matches shown"
            },
            Some(reason.as_str().to_owned()),
            Vec::new(),
        )),
        PackageGraphKnowledge::Ambiguous { sources } => {
            records.push(ProductRecord::new(
                "Several authorities publish this coordinate",
                None,
                vec!["choose an exact source authority".to_owned()],
            ));
            records.extend(sources.iter().map(|source| {
                ProductRecord::new(
                    format!("Use authority {}", source.authority.selector()),
                    Some(source.authority.selector()),
                    vec![source.coordinate.as_str().to_owned()],
                )
            }));
        }
    }
    records.extend(page.rows.iter().map(|edge| {
        let (title, target) = match page.direction {
            PackageGraphDirection::Dependencies => {
                let resolved = edge
                    .target
                    .resolved
                    .as_ref()
                    .map(|package| format!(" · {}", package.as_str()))
                    .unwrap_or_default();
                (
                    format!(
                        "{} / {}{resolved}",
                        edge.target.ecosystem.as_str(),
                        edge.target.name.as_str()
                    ),
                    Some(edge.target.name.as_str().to_owned()),
                )
            }
            PackageGraphDirection::Dependents => (
                edge.source.as_str().to_owned(),
                Some(edge.source.as_str().to_owned()),
            ),
        };
        let authority = edge.source_authority.selector();
        let mut tags = vec![
            format!("source authority: {authority}"),
            format!("requirement: {}", edge.target.requirement.as_str()),
            format!("scope: {}", format!("{:?}", edge.scope).to_lowercase()),
            format!("evidence: {:?}", edge.evidence.authority),
            format!("frontier: {}", lower_hex(&edge.evidence.frontier)),
            format!("provenance: {}", lower_hex(&edge.evidence.provenance)),
        ];
        if edge.optional {
            tags.push("optional".to_owned());
        }
        if let Some(resolved) = &edge.target.resolved {
            tags.push(format!("resolved: {}", resolved.as_str()));
        }
        ProductRecord::new(title, target, tags)
    }));
    match &page.terminal {
        PackageGraphPageTerminal::More(cursor) => {
            let encoded = serde_json::to_string(cursor).unwrap_or_else(|_| "{}".to_owned());
            records.push(ProductRecord::new(
                "More graph rows · pass this cursor to continue",
                Some(encoded),
                vec!["cursor is bound to this exact root, facts witness and authority".to_owned()],
            ));
        }
        PackageGraphPageTerminal::Cancelled => {
            records.push(ProductRecord::new("Graph read cancelled", None, Vec::new()))
        }
        PackageGraphPageTerminal::Complete => {}
    }
    ProductView::rows("package-graph", records)
}

fn lower_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn dependency_row(record: &PackageDependencyRecord) -> ProductRecord {
    ProductRecord::new(
        format!(
            "{} {}",
            record.target.name.as_str(),
            record.target.requirement.as_str()
        ),
        record
            .target
            .resolved
            .as_ref()
            .map(|package| package.as_str().to_owned()),
        vec![
            format!("{:?}", record.scope).to_lowercase(),
            record.target.ecosystem.as_str().to_owned(),
            format!("authority: {:?}", record.evidence.authority).to_lowercase(),
        ],
    )
}

fn registry_rows(records: &[RegistryPackageRecord]) -> Vec<ProductRecord> {
    records.iter().map(registry_row).collect()
}

fn registry_row(record: &RegistryPackageRecord) -> ProductRecord {
    ProductRecord::new(
        format!("{} {}", record.name.as_str(), record.version.as_str()),
        Some(record.coordinate.as_str().to_owned()),
        vec![
            format!("{:?}", record.ecosystem).to_lowercase(),
            format!("{} byte(s)", record.bytes),
            format!("release: {:?}", record.standing).to_lowercase(),
            registry_authority_tag(record.authority),
            format!("downloads: {:?}", record.downloads).to_lowercase(),
            format!(
                "advisory: {:?} / {:?}",
                record.advisory.coverage, record.advisory.freshness
            )
            .to_lowercase(),
            native_metadata_tag(record),
        ],
    )
    .with_native_metadata(record.native_metadata.clone())
}

fn registry_authority_tag(authority: Option<RegistryPackageFactAuthority>) -> String {
    match authority.map(|authority| authority.release_facts_freshness) {
        Some(RegistryPackageFactFreshness::Current { .. }) => "registry facts: observed".to_owned(),
        Some(RegistryPackageFactFreshness::Historical) => "registry facts: historical".to_owned(),
        None => "registry facts: unverified".to_owned(),
    }
}

fn native_metadata_tag(record: &RegistryPackageRecord) -> String {
    match (
        &record.native_metadata.availability,
        &record.native_metadata.details,
    ) {
        (RegistryNativeAvailability::Recorded, details) => format!(
            "native: {}",
            match details {
                RegistryNativeDetails::Cargo(_) => "cargo",
                RegistryNativeDetails::Npm(_) => "npm",
                RegistryNativeDetails::Pypi(_) => "pypi",
                RegistryNativeDetails::Maven(_) => "maven",
                RegistryNativeDetails::Nuget(_) => "nuget",
                RegistryNativeDetails::Golang(_) => "go",
                RegistryNativeDetails::Cpp(_) => "conan",
                RegistryNativeDetails::Unavailable { .. } => "unavailable",
            }
        ),
        (RegistryNativeAvailability::NotRecorded(_), _) => "native: not-recorded".to_owned(),
    }
}

fn forge_row(record: &ForgePackageRecord) -> ProductRecord {
    let source = match &record.source {
        ForgeFact::Recorded(value) | ForgeFact::Unavailable(value) => value.as_str().to_owned(),
    };
    let commit = brief_forge_fact(&record.commit, |value| value.as_str().to_owned());
    let tree = brief_forge_fact(&record.tree, |value| value.as_str().to_owned());
    let mut tags = vec![
        format!("provider: {}", record.provider.as_str()),
        format!("revision: {}", record.revision.as_str()),
        format!("commit: {commit}"),
        format!("tree: {tree}"),
        format!("source: {source}"),
        format!("manifests: {}", record.manifests.len()),
        format!(
            "description: {}",
            brief_forge_fact(&record.metadata.description, |value| value
                .as_str()
                .to_owned())
        ),
        format!(
            "license: {}",
            brief_forge_fact(&record.metadata.license, |value| value.as_str().to_owned())
        ),
        format!(
            "stars: {}",
            brief_forge_fact(&record.metadata.stars, ToString::to_string)
        ),
        format!(
            "forks: {}",
            brief_forge_fact(&record.metadata.forks, ToString::to_string)
        ),
        format!(
            "topics: {}",
            match &record.metadata.topics {
                ForgeFact::Recorded(topics) => topics.len().to_string(),
                ForgeFact::Unavailable(reason) => reason.as_str().to_owned(),
            }
        ),
        format!(
            "readme: {}",
            match &record.metadata.readme {
                ForgeFact::Recorded(readme) =>
                    format!("recorded ({} bytes)", readme.as_str().len()),
                ForgeFact::Unavailable(reason) => reason.as_str().to_owned(),
            }
        ),
    ];
    for manifest in record.manifests.iter().take(8) {
        tags.push(format!("manifest: {}", manifest.path.as_str()));
    }
    if record.manifests.len() > 8 {
        tags.push(format!("and {} more manifests", record.manifests.len() - 8));
    }
    ProductRecord::new(
        format!("{} / {}", record.owner.as_str(), record.repository.as_str()),
        Some(record.coordinate.as_str().to_owned()),
        tags,
    )
    .with_forge_details(record.clone())
}

fn brief_forge_fact<T>(fact: &ForgeFact<T>, recorded: impl FnOnce(&T) -> String) -> String {
    let value = match fact {
        ForgeFact::Recorded(value) => recorded(value),
        ForgeFact::Unavailable(reason) => reason.as_str().to_owned(),
    };
    if value.chars().count() <= 160 {
        return value;
    }
    let mut brief = value.chars().take(157).collect::<String>();
    brief.push_str("...");
    brief
}

fn advisory_view(advisory: &AdvisoryPackageDto) -> ProductView {
    let decision = match &advisory.decision {
        AcquisitionDecision::Allow => "allow".to_owned(),
        AcquisitionDecision::Warn(reasons) => format!(
            "warn ({})",
            reasons
                .iter()
                .map(|reason| format!("{reason:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        AcquisitionDecision::Deny(reasons) => format!(
            "deny ({})",
            reasons
                .iter()
                .map(|reason| format!("{reason:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    let mut rows = vec![ProductRecord::new(
        "security decision",
        None,
        vec![
            format!("decision: {decision}"),
            format!("coverage: {:?}", advisory.coverage),
            format!("freshness: {:?}", advisory.freshness),
            format!("yanked: {}", advisory.yanked),
            format!("unlisted: {}", advisory.unlisted),
        ],
    )];
    if advisory.advisories.is_empty() {
        rows.push(ProductRecord::new(
            "no matching advisory object",
            None,
            vec!["coverage and freshness remain authoritative".to_owned()],
        ));
    } else {
        rows.extend(advisory.advisories.iter().map(|claim| {
            let sources = claim
                .source_ids
                .iter()
                .map(|source| format!("{:?}:{}", source.source, source.id))
                .collect::<Vec<_>>()
                .join(", ");
            let aliases = if claim.aliases.is_empty() {
                "none".to_owned()
            } else {
                claim.aliases.join(", ")
            };
            let affected = claim
                .affected
                .iter()
                .map(|range| format!("{}:{:?}", range.package.name, range.matcher))
                .collect::<Vec<_>>()
                .join(", ");
            let fixed = if claim.fixed_ranges.is_empty() {
                "none".to_owned()
            } else {
                claim.fixed_ranges.join(", ")
            };
            ProductRecord::new(
                claim.canonical_id.clone(),
                None,
                vec![
                    format!("sources: {sources}"),
                    format!("aliases: {aliases}"),
                    format!("severity: {:?}", claim.severity),
                    format!(
                        "categories: {}",
                        claim
                            .categories
                            .iter()
                            .map(|category| format!("{category:?}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    format!(
                        "affected: {}",
                        if affected.is_empty() {
                            "unspecified"
                        } else {
                            &affected
                        }
                    ),
                    format!("fixed: {fixed}"),
                    format!("statuses: {:?}", claim.statuses),
                ],
            )
        }));
    }
    ProductView::rows("advisory", rows)
}

fn declaration_row(record: &DeclarationRecord) -> ProductRecord {
    let tags = record.stable_id.map_or_else(
        || vec!["not found".to_owned()],
        |id| vec![format!("key {}", KeyTag::from_key(&id))],
    );
    ProductRecord::new(
        record.signature.as_ref().map_or_else(
            || record.label.as_str().to_owned(),
            |signature| signature.as_str().to_owned(),
        ),
        Some(record.label.as_str().to_owned()),
        tags,
    )
}

/// Renders one source-verified use of the queried declaration.
fn reference_row(record: &backend_library::ReferenceRecord) -> ProductRecord {
    let mut tags = vec![
        match record.relation {
            backend_library::SemanticLinkKind::Calls => "calls".to_owned(),
            backend_library::SemanticLinkKind::MethodCall => "method call".to_owned(),
            backend_library::SemanticLinkKind::TypeReference => "type reference".to_owned(),
            backend_library::SemanticLinkKind::Reads => "reads".to_owned(),
            backend_library::SemanticLinkKind::Writes => "writes".to_owned(),
            backend_library::SemanticLinkKind::Imports => "imports".to_owned(),
            backend_library::SemanticLinkKind::Implements => "implements".to_owned(),
            backend_library::SemanticLinkKind::Overrides => "overrides".to_owned(),
            backend_library::SemanticLinkKind::Reexports => "re-exports".to_owned(),
            backend_library::SemanticLinkKind::Inherits => "inherits".to_owned(),
            backend_library::SemanticLinkKind::Documents => "documents".to_owned(),
        },
        match record.evidence.confidence {
            backend_library::SemanticConfidence::Syntactic => "syntactic".to_owned(),
            backend_library::SemanticConfidence::Heuristic => "heuristic".to_owned(),
            backend_library::SemanticConfidence::Indexed => "indexed".to_owned(),
            backend_library::SemanticConfidence::Imported => "imported".to_owned(),
            backend_library::SemanticConfidence::Compiler => "compiler".to_owned(),
        },
    ];
    match record.evidence.source.as_ref() {
        Some(span) => tags.push(format!(
            "{}:{}-{}",
            span.file.as_str(),
            span.start,
            span.end
        )),
        None => tags.push("site not captured".to_owned()),
    }
    ProductRecord::new(
        record.site.as_str().to_owned(),
        Some(record.site.as_str().to_owned()),
        tags,
    )
}

fn diff_row(record: &DiffRecord) -> ProductRecord {
    let change = match record.change {
        DeclarationChange::Added => "added",
        DeclarationChange::Removed => "removed",
        DeclarationChange::Changed => "changed",
        DeclarationChange::Indeterminate => "indeterminate",
    };
    let mut tags = vec![change.to_owned()];
    if !record.links.is_empty() {
        tags.push(format!("{} relation change(s)", record.links.len()));
    }
    ProductRecord::new(
        record.label.as_str().to_owned(),
        Some(record.label.as_str().to_owned()),
        tags,
    )
}

fn semantic_row(record: &SemanticVersionRecord) -> ProductRecord {
    let mut tags = vec![
        format!(
            "generation {}",
            KeyTag::from_key(&record.generation.to_bytes())
        ),
        format!("{} artifact(s)", record.artifacts),
        format!("{} semantic byte(s)", record.semantic_bytes),
    ];
    if let PackageReference::Purl(coordinate) = &record.package {
        tags.push(format!("version {}", coordinate.version()));
    }
    tags.push(if record.complete {
        "complete".to_owned()
    } else {
        "partial".to_owned()
    });
    if record.selected {
        tags.push("selected".to_owned());
    }
    tags.push(match record.freshness {
        SemanticVersionFreshness::Current { .. } => "current source input".to_owned(),
        SemanticVersionFreshness::Historical {
            selected_input,
            latest_input,
        } => format!(
            "historical source input {} · latest {}",
            digest_prefix(&selected_input),
            digest_prefix(&latest_input),
        ),
        SemanticVersionFreshness::Unverified => "freshness unverified".to_owned(),
    });
    tags.push(semantic_history_label(&record.history_status).to_owned());
    ProductRecord::new(
        record.coordinate.as_str().to_owned(),
        Some(encode_id(&record.generation.to_bytes())),
        tags,
    )
    .with_history_status(record.history_status.clone())
}

fn semantic_history_label(status: &SemanticHistoryPublicationStatus) -> &'static str {
    match status {
        SemanticHistoryPublicationStatus::NotSelected => "derived history not selected",
        SemanticHistoryPublicationStatus::NotRequested { .. } => "derived history not requested",
        SemanticHistoryPublicationStatus::Pending { .. } => "derived history pending",
        SemanticHistoryPublicationStatus::Deferred { .. } => {
            "derived history deferred · retry scheduled"
        }
        SemanticHistoryPublicationStatus::Published { .. } => "derived history published",
        SemanticHistoryPublicationStatus::Refused { .. } => "derived history refused",
        SemanticHistoryPublicationStatus::Superseded { .. } => "derived history superseded",
    }
}

/// Renders bounded-cardinality semantic publication status as one readable
/// line while retaining exact identifiers and references in the typed DTO.
pub(crate) fn semantic_history_details(status: &SemanticHistoryPublicationStatus) -> String {
    match status {
        SemanticHistoryPublicationStatus::NotSelected => {
            "Derived history is not selected for this compiler generation.".to_owned()
        }
        SemanticHistoryPublicationStatus::NotRequested { selection_id } => format!(
            "Derived history has not been requested for committed selection {}.",
            full_digest(selection_id),
        ),
        SemanticHistoryPublicationStatus::Pending { selection_id } => format!(
            "Derived history publication is pending for committed selection {}.",
            full_digest(selection_id),
        ),
        SemanticHistoryPublicationStatus::Deferred {
            selection_id,
            reason,
        } => format!(
            "Derived history was deferred and is retryable; the owner will reschedule committed selection {}: {}",
            full_digest(selection_id),
            single_line(reason),
        ),
        SemanticHistoryPublicationStatus::Published {
            selection_id,
            commit,
            reference,
        } => format!(
            "Derived history is published for committed selection {} at commit {} (reference: {}).",
            full_digest(selection_id),
            full_digest(commit),
            single_line(reference),
        ),
        SemanticHistoryPublicationStatus::Refused {
            selection_id,
            reason,
        } => format!(
            "Derived history publication was refused for committed selection {}: {}. The committed semantic generation remains selected.",
            full_digest(selection_id),
            single_line(reason),
        ),
        SemanticHistoryPublicationStatus::Superseded { selection_id } => format!(
            "Derived history publication was superseded for selection {}; the owner will reconcile the current selection.",
            full_digest(selection_id),
        ),
    }
}

fn full_digest(digest: &[u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn single_line(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

fn digest_prefix(digest: &[u8; 32]) -> String {
    digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn subscription_row(record: &SubscriptionRecord) -> ProductRecord {
    let mut tags = Vec::with_capacity(2);
    if let Some(project) = record.project {
        tags.push(format!("project {}", project.get()));
    }
    tags.push(record.seen.as_ref().map_or_else(
        || "nothing seen yet".to_owned(),
        |seen| format!("seen {}", seen.as_str()),
    ));
    ProductRecord::new(
        package_title(&record.package),
        Some(operand(&record.package)),
        tags,
    )
}

fn release_row(record: &ReleaseRecord) -> ProductRecord {
    ProductRecord::new(
        format!(
            "{} {}",
            package_title(&record.package),
            record.version.as_str()
        ),
        Some(operand(&record.package)),
        vec![if record.seen { "seen" } else { "new" }.to_owned()],
    )
}

fn project_row(record: &ProjectRecord) -> ProductRecord {
    let mut tags = vec![
        format!("id {}", record.id.get()),
        format!("{} member(s)", record.members.len()),
    ];
    if let Some(lockfile) = record.lockfile.as_ref() {
        tags.push(format!("lockfile {}", lockfile.as_str()));
    }
    for name in record.member_manifest_names.iter() {
        tags.push(format!("member {}", name.as_str()));
    }
    ProductRecord::new(
        record.name.as_str().to_owned(),
        Some(record.name.as_str().to_owned()),
        tags,
    )
}

fn tree_row(record: &TreeNodeRecord) -> ProductRecord {
    let mut tags = vec![
        format!("node {}", record.id.get()),
        opener_name(&record.opener).to_owned(),
    ];
    if let Some(parent) = record.parent {
        tags.push(format!("under {}", parent.get()));
    }
    if record.active {
        tags.push("active".to_owned());
    }
    if let TreeSubject::Declaration(_) = &record.subject {
        if let Some((name, path)) = record.title.as_str().split_once(" · ") {
            tags.push(format!("name {name}"));
            tags.push(format!("path {path}"));
        }
    }
    ProductRecord::new(
        format!(
            "{} · {}",
            record.title.as_str(),
            subject_text(&record.subject)
        ),
        Some(record.id.get().to_string()),
        tags,
    )
}

const fn opener_name(opener: &TreeOpener) -> &'static str {
    match opener {
        TreeOpener::Desktop => "desktop",
        TreeOpener::Cli => "cli",
        TreeOpener::Mcp(_) => "mcp",
    }
}

fn subject_text(subject: &TreeSubject) -> String {
    match subject {
        TreeSubject::Package(package) => format!("package {}", package.as_str()),
        TreeSubject::Declaration(text) => {
            if let Some((name, path)) = text.as_str().split_once("::").and_then(|(_, rest)| {
                rest.rsplit_once("::").map(|(path, name)| {
                    (
                        name.to_owned(),
                        path.rsplit_once(':')
                            .map_or(path, |(path_without_line, _)| path_without_line)
                            .to_owned(),
                    )
                })
            }) {
                format!("declaration {name} at {path}")
            } else {
                format!("declaration {}", text.as_str())
            }
        }
        TreeSubject::Explore(query) => query.as_ref().map_or_else(
            || "explore".to_owned(),
            |query| format!("explore {}", query.as_str()),
        ),
        TreeSubject::Search(text) => format!("search {}", text.as_str()),
        TreeSubject::Owner(text) => format!("owner {}", text.as_str()),
    }
}

fn package_title(package: &PackageReference) -> String {
    match package {
        PackageReference::Purl(url) => url.lineage_name().to_owned(),
        PackageReference::Local(label) => label.as_str().to_owned(),
    }
}

fn operand(package: &PackageReference) -> String {
    package.as_str().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_library::{
        PackageCoordinate, PackageReference, RegistryDownloadCount, RegistryEcosystem,
        RegistryFactAvailability, RegistryNativeMetadata, RegistryPackageSearchGroup,
        RegistryReleaseMatchScope, RegistryReleaseStanding, RegistrySearchGroupKind,
        SemanticGenerationId, SemanticLanguageProfile, SemanticVersionFreshness,
    };

    #[test]
    fn index_search_cursor_projection_keeps_owner_family_and_projects_both_renderings() {
        let owner_cursor = IndexSearchCursor::new("owner-v4-token").expect("owner cursor");
        let reply = SurfaceReply::IndexSearchPage(IndexSearchPage {
            snapshot: [7; 32],
            evaluated_at_millis: 11,
            hits: Box::new([]),
            next_cursor: Some(owner_cursor),
            result_count: IndexSearchResultCount::AtLeast(1),
        });
        let view = product_view(&reply);
        assert!(crate::markdown::product(&view).contains("pass `--cursor`"));
        assert!(crate::markdown::product(&view).contains("owner-v4-token"));

        let projected = view.project_cursor("mcp1-signed-token", CursorTarget::SurfaceCommand);
        assert_eq!(
            projected
                .index_search_page()
                .and_then(IndexSearchPageInfo::next_cursor),
            Some("mcp1-signed-token")
        );
        assert!(
            crate::markdown::product(&projected)
                .contains("set `command.cursor` in `backend.surface`")
        );
        assert!(crate::markdown::product(&projected).contains("mcp1-signed-token"));
        assert!(!crate::markdown::product(&projected).contains("owner-v4-token"));
        assert!(matches!(
            projected.cursor_family(),
            Some(ContinuationCursor::IndexSearch(cursor))
                if cursor.as_str() == "owner-v4-token"
        ));
        assert_eq!(
            crate::dto::ProductDto::new(&projected)
                .index_search_page
                .and_then(|page| page.next_cursor)
                .as_deref(),
            Some("mcp1-signed-token")
        );
    }

    fn semantic_version(
        history_status: SemanticHistoryPublicationStatus,
        selected: bool,
        complete: bool,
        freshness: SemanticVersionFreshness,
    ) -> SemanticVersionRecord {
        let coordinate = PackageCoordinate::parse("pkg:cargo/history-demo@1.0.0")
            .expect("canonical Rust package coordinate");
        SemanticVersionRecord {
            package: PackageReference::Purl(coordinate.clone()),
            coordinate,
            profile: SemanticLanguageProfile::from_name("rust").expect("Rust profile"),
            generation: SemanticGenerationId::new([0x11; 32]),
            generation_root: [0x22; 32],
            dependency_set: [0x33; 32],
            manifest: [0x44; 32],
            artifacts: 3,
            semantic_bytes: 4096,
            complete,
            selected,
            freshness,
            history_status,
        }
    }

    fn semantic_versions_view(record: SemanticVersionRecord) -> ProductView {
        product_view(&SurfaceReply::SemanticVersions(
            vec![record].into_boxed_slice(),
        ))
    }

    #[test]
    fn every_semantic_history_state_survives_the_typed_product_projection() {
        let cases = [
            (
                SemanticHistoryPublicationStatus::NotSelected,
                false,
                "derived history not selected",
            ),
            (
                SemanticHistoryPublicationStatus::NotRequested {
                    selection_id: [0x10; 32],
                },
                true,
                "derived history not requested",
            ),
            (
                SemanticHistoryPublicationStatus::Pending {
                    selection_id: [0x20; 32],
                },
                true,
                "derived history pending",
            ),
            (
                SemanticHistoryPublicationStatus::Deferred {
                    selection_id: [0x30; 32],
                    reason: "queue pressure".to_owned(),
                },
                true,
                "derived history deferred · retry scheduled",
            ),
            (
                SemanticHistoryPublicationStatus::Published {
                    selection_id: [0x40; 32],
                    commit: [0x41; 32],
                    reference: "selected-native-v3".to_owned(),
                },
                true,
                "derived history published",
            ),
            (
                SemanticHistoryPublicationStatus::Refused {
                    selection_id: [0x50; 32],
                    reason: "store unavailable".to_owned(),
                },
                true,
                "derived history refused",
            ),
            (
                SemanticHistoryPublicationStatus::Superseded {
                    selection_id: [0x60; 32],
                },
                true,
                "derived history superseded",
            ),
        ];
        for (status, selected, label) in cases {
            let view = semantic_versions_view(semantic_version(
                status.clone(),
                selected,
                false,
                SemanticVersionFreshness::Unverified,
            ));
            let row = view.records().first().expect("semantic generation row");
            assert_eq!(row.history_status(), Some(&status));
            assert!(row.tags().iter().any(|tag| tag == label));
            let dto = crate::dto::ProductDto::new(&view);
            assert_eq!(dto.records[0].history_status, Some(status));
        }
    }

    #[test]
    fn deferred_history_is_typed_retryable_and_inside_summary_and_full_budgets() {
        let status = SemanticHistoryPublicationStatus::Deferred {
            selection_id: [0x5a; 32],
            reason: "bounded history worker queue is full".to_owned(),
        };
        let view = semantic_versions_view(semantic_version(
            status.clone(),
            true,
            false,
            SemanticVersionFreshness::Unverified,
        ));
        let row = view.records().first().expect("semantic generation row");
        assert_eq!(row.history_status(), Some(&status));
        assert!(row.tags().iter().any(|tag| tag == "partial"));
        assert!(
            row.tags()
                .iter()
                .any(|tag| tag == "derived history deferred · retry scheduled")
        );
        assert!(!row.tags().iter().any(|tag| tag == "complete"));
        assert!(!row.tags().iter().any(|tag| tag == "current source input"));
        let markdown = crate::markdown::product(&view);
        assert!(markdown.contains("Derived history was deferred and is retryable"));
        assert!(markdown.contains("bounded history worker queue is full"));
        assert!(markdown.contains(&"5a".repeat(32)));
        let terminal = crate::text::product(&view, crate::Theme::plain());
        assert!(terminal.contains("Derived history was deferred and is retryable"));
        assert!(terminal.contains("bounded history worker queue is full"));

        let answer = crate::Answer::Product(Box::new(view.clone()));
        for detail in [crate::Detail::Summary, crate::Detail::Full] {
            let payload =
                crate::encode_answer(&answer, detail, None, crate::DEFAULT_RESPONSE_BUDGET_BYTES)
                    .expect("semantic history fits the typed response budget");
            assert_eq!(payload.budget.bytes, payload.bytes.len());
            let value: serde_json::Value =
                serde_json::from_slice(&payload.bytes).expect("typed answer JSON");
            assert_eq!(value["answer"], "product");
            assert_eq!(value["records"][0]["history_status"]["state"], "deferred");
            assert_eq!(
                value["records"][0]["history_status"]["selection_id"],
                serde_json::to_value([0x5a; 32]).expect("selection id JSON")
            );
            assert_eq!(
                value["records"][0]["history_status"]["reason"],
                "bounded history worker queue is full"
            );
        }
    }

    #[test]
    fn published_history_preserves_the_exact_reference_without_claiming_input_completeness() {
        let reference = "selected-native-v3/branch-00017".to_owned();
        let status = SemanticHistoryPublicationStatus::Published {
            selection_id: [0x6b; 32],
            commit: [0x7c; 32],
            reference: reference.clone(),
        };
        let view = semantic_versions_view(semantic_version(
            status.clone(),
            true,
            false,
            SemanticVersionFreshness::Unverified,
        ));
        let row = view.records().first().expect("semantic generation row");
        assert_eq!(row.history_status(), Some(&status));
        assert!(row.tags().iter().any(|tag| tag == "partial"));
        assert!(row.tags().iter().any(|tag| tag == "freshness unverified"));
        assert!(
            row.tags()
                .iter()
                .any(|tag| tag == "derived history published")
        );
        assert!(!row.tags().iter().any(|tag| tag == "complete"));
        assert!(!row.tags().iter().any(|tag| tag == "current source input"));
        assert!(crate::markdown::product(&view).contains(&reference));

        let dto = crate::dto::ProductDto::new(&view);
        assert_eq!(dto.records[0].history_status, Some(status.clone()));
        let value = serde_json::to_value(&dto).expect("semantic product DTO");
        assert_eq!(value["records"][0]["history_status"]["state"], "published");
        assert_eq!(
            value["records"][0]["history_status"]["reference"],
            reference
        );
        let decoded: crate::dto::ProductDto =
            serde_json::from_value(value).expect("typed status DTO round trip");
        assert_eq!(decoded.records[0].history_status, Some(status));
    }

    #[test]
    fn refused_derived_history_stays_a_product_status_not_a_generation_fault() {
        let status = SemanticHistoryPublicationStatus::Refused {
            selection_id: [0x8d; 32],
            reason: "the sidecar store refused publication".to_owned(),
        };
        let view = semantic_versions_view(semantic_version(
            status.clone(),
            true,
            false,
            SemanticVersionFreshness::Historical {
                selected_input: [0x91; 32],
                latest_input: [0xa2; 32],
            },
        ));
        assert!(view.fault().is_none());
        let row = view
            .records()
            .first()
            .expect("selected compiler generation remains");
        assert_eq!(row.history_status(), Some(&status));
        assert!(row.tags().iter().any(|tag| tag == "partial"));
        assert!(
            row.tags()
                .iter()
                .any(|tag| tag == "historical source input 919191919191 · latest a2a2a2a2a2a2")
        );
        assert!(
            row.tags()
                .iter()
                .any(|tag| tag == "derived history refused")
        );
        let markdown = crate::markdown::product(&view);
        assert!(markdown.contains("sidecar store refused publication"));
        assert!(markdown.contains("The committed semantic generation remains selected"));
        let value = serde_json::to_value(crate::dto::ProductDto::new(&view))
            .expect("product is still returned as product data");
        assert_eq!(value["records"][0]["history_status"]["state"], "refused");
        assert!(value.get("fault").is_none());
    }

    #[test]
    fn index_job_projection_preserves_exact_ticket_observation_through_summary_budget() {
        let ticket = backend_library::IndexJobTicket::new(
            std::num::NonZeroU64::new(9).expect("nonzero ticket"),
            [6; 16],
            PackageReference::parse("pkg:cargo/serde@1.0.228").expect("pinned package"),
        );
        let reply = SurfaceReply::IndexProgress(backend_library::IndexJobObservation::Pending(
            backend_library::IndexProgressPage {
                ticket: ticket.clone(),
                stage: backend_library::IndexJobStage::Compiling,
                events: Box::new([]),
                next_sequence: 5,
                truncated: true,
                has_more: false,
            },
        ));
        let view = product_view(&reply);
        let job = view.index_job().expect("typed job projection");
        assert_eq!(job.ticket(), Some(&ticket));
        assert!(crate::markdown::product(&view).contains("after_sequence 5"));
        assert!(crate::markdown::product(&view).contains(&job.ticket_json().expect("ticket")));

        let dto = crate::dto::ProductDto::new(&view);
        assert_eq!(dto.index_job.as_ref(), Some(job));
        let answer = crate::drive::Answer::Product(Box::new(view));
        let encoded = crate::encode_answer(
            &answer,
            crate::Detail::Summary,
            None,
            crate::DEFAULT_RESPONSE_BUDGET_BYTES,
        )
        .expect("bounded summary retains ticket and observation");
        let value: serde_json::Value =
            serde_json::from_slice(&encoded.bytes).expect("typed summary JSON");
        assert_eq!(value["index_job"]["kind"], "progress");
        assert_eq!(value["index_job"]["value"]["detail"]["ticket"]["id"], 9);
        assert_eq!(value["index_job"]["value"]["detail"]["next_sequence"], 5);
        assert_eq!(value["index_job"]["value"]["detail"]["truncated"], true);
    }

    #[test]
    fn cancellation_projection_keeps_the_receipt_ticket_even_for_requested_state() {
        let ticket = backend_library::IndexJobTicket::new(
            std::num::NonZeroU64::new(11).expect("nonzero ticket"),
            [4; 16],
            PackageReference::parse("/workspace/project").expect("local project"),
        );
        let reply = SurfaceReply::IndexCancellation(backend_library::IndexCancelReceipt {
            ticket: ticket.clone(),
            status: backend_library::IndexCancelStatus::Requested,
        });
        let view = product_view(&reply);
        let job = view.index_job().expect("typed cancellation projection");
        assert_eq!(job.ticket(), Some(&ticket));
        assert!(crate::markdown::product(&view).contains(&job.ticket_json().expect("ticket")));
        let encoded =
            serde_json::to_value(crate::dto::ProductDto::new(&view)).expect("typed product DTO");
        assert_eq!(encoded["index_job"]["kind"], "cancellation");
        assert_eq!(encoded["index_job"]["value"]["ticket"]["id"], 11);
        assert_eq!(
            encoded["index_job"]["value"]["status"]["state"],
            "requested"
        );
    }

    #[test]
    fn lineage_metadata_only_results_are_not_presented_as_release_matches() {
        let coordinate =
            PackageReference::parse("pkg:cargo/split-facet@1.0.0").expect("package coordinate");
        let metadata = RegistryNativeMetadata::unavailable(RegistryEcosystem::Cargo, "test");
        let record = RegistryPackageRecord {
            coordinate,
            ecosystem: RegistryEcosystem::Cargo,
            name: backend_library::ProductText::new("split-facet").expect("name"),
            version: backend_library::ProductText::new("1.0.0").expect("version"),
            bytes: 0,
            standing: RegistryReleaseStanding::Available,
            downloads: RegistryDownloadCount::Unavailable(RegistryFactAvailability::Unsupported),
            facts_version: [0; 32],
            authority: None,
            native_metadata_version: metadata.identity().expect("metadata identity"),
            native_metadata: metadata,
            forge_sources: Box::new([]),
            advisory: backend_library::AdvisoryPackageDto::unknown(),
        };
        let group = RegistryPackageSearchGroup {
            kind: RegistrySearchGroupKind::Acquired,
            source: [0; 32],
            ecosystem: RegistryEcosystem::Cargo,
            lineage: backend_library::ProductText::new("split-facet").expect("lineage"),
            releases: Box::new([RegistrySearchRelease::Acquired(record)]),
            release_match_scope: RegistryReleaseMatchScope::LineageMetadataOnly,
            more_releases: true,
        };

        let row = registry_search_group_row(&group);
        let tags = row.tags();
        assert!(tags.iter().any(|tag| tag == "1 representative version(s)"));
        assert!(
            tags.iter()
                .any(|tag| tag == "package metadata matched across releases")
        );
        assert!(tags.iter().any(|tag| tag == "more versions in lineage"));
        assert!(!tags.iter().any(|tag| tag == "1 matching version(s)"));
        assert!(!tags.iter().any(|tag| tag == "more matching versions"));
    }

    #[test]
    fn forge_source_pin_keeps_commit_out_of_package_coordinate_and_json() {
        let source = backend_library::ForgeCoordinate::new(
            "https://github.com/acme/source-only",
            backend_library::ForgeRevision::Branch(
                backend_library::ForgeRefName::new("main").expect("branch"),
            ),
            None::<String>,
        )
        .expect("source coordinate");
        let commit =
            backend_library::ForgeObjectId::parse("0123456789abcdef0123456789abcdef01234567")
                .expect("commit");
        fn unavailable<T>() -> backend_library::ForgeFact<T> {
            backend_library::ForgeFact::Unavailable(
                backend_library::ProductText::new("not reported by forge authority")
                    .expect("static availability reason"),
            )
        }
        let detail = backend_library::ForgePackageDetailRecord {
            source: source.clone(),
            source_id: source.identity(),
            resolved_commit: commit.clone(),
            resolved_tree: None,
            package_coordinate: None,
            pin: ForgePackagePin::PinnedRevision {
                requested_revision: source.revision().clone(),
                resolved_commit: commit,
            },
            manifest: backend_library::ForgePackageManifestDetail {
                path: backend_library::ProductText::new("Cargo.toml").expect("path"),
                ecosystem: backend_library::RegistryEcosystem::Cargo,
                name: backend_library::ForgeFact::Recorded(
                    backend_library::ProductText::new("source-only-widget").expect("name"),
                ),
                version: unavailable(),
                dependencies: backend_library::DependencyFacts::Known(Box::default()),
            },
            metadata: backend_library::ForgeRepositoryMetadataRecord {
                owner: unavailable(),
                description: unavailable(),
                license: unavailable(),
                readme: unavailable(),
                topics: unavailable(),
                stars: unavailable(),
                forks: unavailable(),
            },
            registry: backend_library::ForgePackageRegistryEvidence {
                yanked: backend_library::RegistryEvidenceFacet::Unknown,
                downloads: RegistryDownloadCount::Unavailable(RegistryFactAvailability::Unknown),
                advisory: AdvisoryPackageDto::unknown(),
            },
        };
        detail.admit().expect("valid source-pin details");

        let view = product_view(&SurfaceReply::IndexSearchWithDiscovery(Box::new([
            RegistrySearchHit::ForgeSourcePin(detail.clone()),
        ])));
        let row = view.records().first().expect("source-pin presentation row");
        assert!(row.title().contains("source pin"));
        let source_text = source.canonical();
        assert_eq!(row.operand(), Some(source_text.as_str()));
        assert!(
            row.tags()
                .iter()
                .any(|tag| tag == "source pin · no package version")
        );
        assert!(
            row.tags()
                .iter()
                .any(|tag| tag == "registry downloads unknown")
        );
        assert_eq!(row.forge_package_detail(), Some(&detail));

        let json = serde_json::to_value(crate::dto::ProductDto::new(&view))
            .expect("serialize source-pin presenter DTO");
        assert_eq!(
            json["records"][0]["forge_package_detail"]["pin"]["state"],
            "pinned-revision"
        );
        assert!(json["records"][0]["forge_package_detail"]["package_coordinate"].is_null());
        assert_eq!(json["records"][0]["operand"], source_text);
    }
}
