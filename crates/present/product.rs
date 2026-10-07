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
use crate::fault::{Fault, Operand};
use backend_library::{
    AcquisitionDecision, AdvisoryPackageDto, DeclarationChange, DeclarationRecord, DependencyFacts,
    DiffRecord, ForgeFact, ForgePackageDetailRecord, ForgePackagePin, ForgePackageRecord,
    IndexSearchCursor, IndexSearchPage, IndexSearchResultCount, LocalDeclarationSearchRecord,
    LocalDeclarationSource, PackageDependencyRecord, PackageReference, ProjectRecord,
    RegistryDiscoveryCandidate, RegistryEvidenceFacet, RegistryMetadata,
    RegistryNativeAvailability, RegistryNativeDetails, RegistryNativeMetadata,
    RegistryPackageFactAuthority, RegistryPackageFactFreshness, RegistryPackageRecord,
    RegistryPackageSearchGroup, RegistryReleaseMatchScope, RegistrySearchGroupKind,
    RegistrySearchHit, RegistrySearchRelease, ReleaseRecord, SelectedProjectSourceFrontier,
    SemanticHistoryPublicationStatus, SemanticLanguageProfile, SemanticVersionFreshness,
    SemanticVersionRecord, SubscriptionRecord, SurfaceReply, TreeNodeRecord, TreeOpener,
    TreeSubject, encode_id,
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
    source_metadata: Option<backend_library::PythonProjectMetadata>,
    forge_details: Option<ForgePackageRecord>,
    forge_package_detail: Option<ForgePackageDetailRecord>,
    discovery_details: Option<RegistryDiscoveryCandidate>,
    package_group: Option<RegistryPackageSearchGroup>,
    local_declaration: Option<LocalDeclarationSearchRecord>,
    history_status: Option<ProductSemanticHistoryStatus>,
    compiler_profile: Option<SemanticLanguageProfile>,
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
            source_metadata: None,
            forge_details: None,
            forge_package_detail: None,
            discovery_details: None,
            package_group: None,
            local_declaration: None,
            history_status: None,
            compiler_profile: None,
        }
    }

    /// Attaches typed native registry facts to one registry row.
    #[must_use]
    pub fn with_native_metadata(mut self, metadata: RegistryNativeMetadata) -> Self {
        self.native_metadata = Some(metadata);
        self
    }

    /// Attaches bounded source declarations separately from native registry authority.
    #[must_use]
    pub fn with_source_metadata(
        mut self,
        metadata: backend_library::PythonProjectMetadata,
    ) -> Self {
        let mut tags = self.tags.into_vec();
        tags.extend(python_metadata_tags(&metadata));
        self.tags = tags.into_boxed_slice();
        self.source_metadata = Some(metadata);
        self
    }

    fn with_optional_source_metadata(
        self,
        metadata: Option<&backend_library::PythonProjectMetadata>,
    ) -> Self {
        match metadata {
            Some(metadata) => self.with_source_metadata(metadata.clone()),
            None => self,
        }
    }

    /// Returns the packaging declarations and their source evidence.
    #[must_use]
    pub fn source_metadata(&self) -> Option<&backend_library::PythonProjectMetadata> {
        self.source_metadata.as_ref()
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

    /// Attaches exact local source provenance, independently of registry metadata.
    #[must_use]
    pub fn with_local_declaration(mut self, record: LocalDeclarationSearchRecord) -> Self {
        self.local_declaration = Some(record);
        self
    }

    /// Attaches all bounded version-specific matches for one package lineage.
    #[must_use]
    pub fn with_package_group(mut self, group: RegistryPackageSearchGroup) -> Self {
        self.package_group = Some(group);
        self
    }

    /// Attaches a concise typed projection of the owner-reported history state.
    /// The complete publication proof remains in the exact semantic data facet.
    #[must_use]
    pub fn with_history_status(mut self, status: &SemanticHistoryPublicationStatus) -> Self {
        self.history_status = Some(ProductSemanticHistoryStatus::from(status));
        self
    }

    /// Attaches the exact closed compiler profile to one semantic generation row.
    #[must_use]
    pub fn with_compiler_profile(mut self, profile: SemanticLanguageProfile) -> Self {
        self.compiler_profile = Some(profile);
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

    /// Returns the selected local declaration and its exact source location.
    #[must_use]
    pub fn local_declaration(&self) -> Option<&LocalDeclarationSearchRecord> {
        self.local_declaration.as_ref()
    }

    /// Returns the source-scoped version group for this package result.
    #[must_use]
    pub fn package_group(&self) -> Option<&RegistryPackageSearchGroup> {
        self.package_group.as_ref()
    }

    /// Returns the concise derived-history state shown beside this row.
    #[must_use]
    pub fn history_status(&self) -> Option<&ProductSemanticHistoryStatus> {
        self.history_status.as_ref()
    }

    /// Returns the exact compiler profile carried by a semantic generation row.
    #[must_use]
    pub const fn compiler_profile(&self) -> Option<SemanticLanguageProfile> {
        self.compiler_profile
    }
}

/// Readable publication state copied from the immutable semantic version reply.
///
/// A row needs status, retry/refusal detail, and the published reference. The
/// complete selected catalog and per-image proofs belong to
/// [`ProductSemanticData::Versions`], where callers copy the exact source
/// operand. Keeping that catalog out of this projection prevents duplicating
/// it in the same answer. This value never re-reads mutable owner state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProductSemanticHistoryStatus {
    /// This version is not the committed selection.
    NotSelected,
    /// The selection has not been reconciled with derived history.
    NotRequested {
        /// Exact committed selection identity.
        selection_id: [u8; 32],
    },
    /// A worker is producing derived history for the selection.
    Pending {
        /// Exact committed selection identity.
        selection_id: [u8; 32],
    },
    /// The selection is waiting for a bounded worker slot.
    Deferred {
        /// Exact committed selection identity.
        selection_id: [u8; 32],
        /// Owner-reported retry detail.
        reason: String,
    },
    /// Complete package history has been published at the exact reference.
    Published {
        /// Exact committed selection identity.
        selection_id: [u8; 32],
        /// Commit containing the derived history.
        commit: [u8; 32],
        /// Reference containing the published commit.
        reference: String,
        /// Concise proof status for the readable row.
        proof: ProductSemanticHistoryProofSummary,
    },
    /// Derived history production or admission was refused.
    Refused {
        /// Exact committed selection identity.
        selection_id: [u8; 32],
        /// Owner-reported refusal detail.
        reason: String,
    },
    /// The selected marker advanced while the worker ran.
    Superseded {
        /// Exact superseded selection identity.
        selection_id: [u8; 32],
    },
}

/// Publication facts needed by a readable row, without its per-image catalog.
///
/// Older product DTOs carried the complete proof here. Deserialization accepts
/// those additional fields; the exact proof in new DTOs lives in `semantic_data`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProductSemanticHistoryProofSummary {
    /// Exact public branch tip verified by the publication proof.
    pub reference_tip: [u8; 32],
    /// Persisted input replay authority reported by the proof.
    pub input_replay_status: backend_library::SemanticHistoryInputReplayStatus,
}

impl From<&SemanticHistoryPublicationStatus> for ProductSemanticHistoryStatus {
    fn from(status: &SemanticHistoryPublicationStatus) -> Self {
        match status {
            SemanticHistoryPublicationStatus::NotSelected => Self::NotSelected,
            SemanticHistoryPublicationStatus::NotRequested { selection_id } => Self::NotRequested {
                selection_id: *selection_id,
            },
            SemanticHistoryPublicationStatus::Pending { selection_id } => Self::Pending {
                selection_id: *selection_id,
            },
            SemanticHistoryPublicationStatus::Deferred {
                selection_id,
                reason,
            } => Self::Deferred {
                selection_id: *selection_id,
                reason: reason.clone(),
            },
            SemanticHistoryPublicationStatus::Published {
                selection_id,
                commit,
                reference,
                proof,
            } => Self::Published {
                selection_id: *selection_id,
                commit: *commit,
                reference: reference.clone(),
                proof: ProductSemanticHistoryProofSummary {
                    reference_tip: proof.reference_tip,
                    input_replay_status: proof.input_replay_status,
                },
            },
            SemanticHistoryPublicationStatus::Refused {
                selection_id,
                reason,
            } => Self::Refused {
                selection_id: *selection_id,
                reason: reason.clone(),
            },
            SemanticHistoryPublicationStatus::Superseded { selection_id } => Self::Superseded {
                selection_id: *selection_id,
            },
        }
    }
}

/// Exact semantic operand/result facet, separate from readable display rows.
/// Values are copied from the typed reply, never reconstructed from row labels.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum ProductSemanticData {
    /// Exact compiler-version records usable as the shape read source operand.
    Versions(Box<[SemanticVersionRecord]>),
    /// Existing shape wire egress after certificate-bearing direct admission.
    Shapes(backend_library::SemanticShapeExport),
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
    index_operation: Option<backend_library::IndexOperationObservation>,
    selected_source_frontier: Option<SelectedProjectSourceFrontier>,
    package_source_membership_page: Option<backend_library::PackageSourceMembershipPageResultV1>,
    semantic_data: Option<ProductSemanticData>,
    package_discovery: Option<PackageDiscoveryProjection>,
}

/// Exact metadata lookup evidence retained by CLI and MCP presentation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PackageDiscoveryProjection {
    /// Exact immutable registry package requested by the caller.
    pub package: backend_library::PackageCoordinate,
    /// Source-only positive, negative, or unavailable evidence.
    pub observation: backend_library::RegistryPackageDiscoveryObservation,
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

    /// Returns exact package metadata evidence, separate from acquired records.
    #[must_use]
    pub fn package_discovery(&self) -> Option<&PackageDiscoveryProjection> {
        self.package_discovery.as_ref()
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

    /// Returns the exact durable index-operation observation carried by this
    /// product reply, when present.
    #[must_use]
    pub const fn index_operation(&self) -> Option<&backend_library::IndexOperationObservation> {
        self.index_operation.as_ref()
    }

    /// Complete bounded compiler shape egress, never a reconstructed source projection.
    #[must_use]
    pub fn semantic_data(&self) -> Option<&ProductSemanticData> {
        self.semantic_data.as_ref()
    }

    /// Returns the exact selected Project membership captured with this semantic query.
    #[must_use]
    pub fn selected_source_frontier(&self) -> Option<&SelectedProjectSourceFrontier> {
        self.selected_source_frontier.as_ref()
    }

    /// Returns the exact typed page needed to resume selected source membership.
    #[must_use]
    pub fn package_source_membership_page(
        &self,
    ) -> Option<&backend_library::PackageSourceMembershipPageResultV1> {
        self.package_source_membership_page.as_ref()
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

    fn with_index_operation(
        mut self,
        index_operation: backend_library::IndexOperationObservation,
    ) -> Self {
        self.index_operation = Some(index_operation);
        self
    }

    fn with_selected_source_frontier(
        mut self,
        frontier: Option<SelectedProjectSourceFrontier>,
    ) -> Self {
        self.selected_source_frontier = frontier;
        self
    }

    fn with_package_source_membership_page(
        mut self,
        page: backend_library::PackageSourceMembershipPageResultV1,
    ) -> Self {
        self.package_source_membership_page = Some(page);
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
            index_operation: None,
            selected_source_frontier: None,
            package_source_membership_page: None,
            semantic_data: None,
            package_discovery: None,
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
            index_operation: None,
            selected_source_frontier: None,
            package_source_membership_page: None,
            semantic_data: None,
            package_discovery: None,
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
            index_operation: None,
            selected_source_frontier: None,
            package_source_membership_page: None,
            semantic_data: None,
            package_discovery: None,
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
            index_operation: None,
            selected_source_frontier: None,
            package_source_membership_page: None,
            semantic_data: None,
            package_discovery: None,
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
            index_operation: None,
            selected_source_frontier: None,
            package_source_membership_page: None,
            semantic_data: None,
            package_discovery: None,
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
        SurfaceReply::ExploredDeclarations(records) => ProductView::rows(
            "explore",
            records.iter().map(local_declaration_search_row).collect(),
        ),
        SurfaceReply::Package(records) => ProductView::rows("package", registry_rows(records)),
        SurfaceReply::PackageDiscovery {
            package,
            observation,
        } => {
            use backend_library::RegistryPackageDiscoveryObservation;
            let mut view = match observation {
                RegistryPackageDiscoveryObservation::Observed { candidates } => ProductView::rows(
                    "package",
                    candidates
                        .iter()
                        .cloned()
                        .map(RegistrySearchHit::Discovered)
                        .map(|hit| registry_search_hit_row(&hit))
                        .collect(),
                ),
                RegistryPackageDiscoveryObservation::Missing { .. } => ProductView::scalar(
                    "package",
                    format!(
                        "{} is absent from the observed registry package metadata",
                        package.as_str()
                    ),
                ),
                RegistryPackageDiscoveryObservation::Unavailable { reason, .. } => {
                    ProductView::scalar(
                        "package",
                        format!(
                            "registry package metadata is unavailable: {}",
                            reason.as_str()
                        ),
                    )
                }
            };
            view.package_discovery = Some(PackageDiscoveryProjection {
                package: package.clone(),
                observation: observation.clone(),
            });
            view
        }
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
        SurfaceReply::SemanticVersions(records) => {
            let mut view = ProductView::rows(
                "semantic-versions",
                records.iter().map(semantic_row).collect(),
            )
            .with_selected_source_frontier(
                records
                    .iter()
                    .find_map(|record| record.selected_source_frontier.clone()),
            );
            view.semantic_data = Some(ProductSemanticData::Versions(records.clone()));
            view
        }
        SurfaceReply::SemanticVersionSelected(record) => {
            let mut view = ProductView::rows("select-semantic-version", vec![semantic_row(record)]);
            view.semantic_data = Some(ProductSemanticData::Versions(Box::new([record.clone()])));
            view
        }
        SurfaceReply::SemanticShapes(export) => {
            let mut view = ProductView::stated(
                "semantic-shapes",
                "Compiler-owned shapes with exact selected-source provenance.",
            );
            view.semantic_data = Some(ProductSemanticData::Shapes(export.clone()));
            view
        }
        SurfaceReply::PackageSourceMembershipPage(page) => {
            let view = match page {
                backend_library::PackageSourceMembershipPageResultV1::Page {
                    package,
                    file_count,
                    files,
                    next,
                    exclusions,
                    ..
                } => {
                    let mut rows = Vec::with_capacity(files.len().saturating_add(1));
                    rows.push(ProductRecord::new(
                        format!(
                            "{} · {} selected source file(s)",
                            package.as_str(),
                            files.len()
                        ),
                        None,
                        vec![
                            format!("Project frontier: {file_count} file(s) total"),
                            format!("exclusions: {exclusions:?}"),
                            format!(
                                "continuation: {}",
                                if next.is_some() {
                                    "available"
                                } else {
                                    "complete"
                                }
                            ),
                        ],
                    ));
                    rows.extend(files.iter().map(|file| {
                        ProductRecord::new(
                            file.path.clone(),
                            Some(file.path.clone()),
                            vec![
                                format!("{:?}", file.language),
                                format!(
                                    "InputContentSchema v1: {}",
                                    encode_id(&file.content_version)
                                ),
                                file.source_identity.map_or_else(
                                    || "SourceFactDomain: not recorded".to_owned(),
                                    |identity| {
                                        format!("SourceFactDomain: {}", encode_id(&identity))
                                    },
                                ),
                            ],
                        )
                    }));
                    ProductView::rows("package-source-membership", rows)
                }
                backend_library::PackageSourceMembershipPageResultV1::Stale { package, .. } => {
                    ProductView::scalar(
                        "package-source-membership",
                        format!(
                            "The selected Project source root for {} changed. Start a fresh first page.",
                            package.as_str()
                        ),
                    )
                }
                backend_library::PackageSourceMembershipPageResultV1::Unavailable {
                    package,
                    reason,
                } => ProductView::scalar(
                    "package-source-membership",
                    format!(
                        "The selected source membership for {} is unavailable: {reason:?}.",
                        package.as_str()
                    ),
                ),
            };
            view.with_package_source_membership_page(page.clone())
        }
        SurfaceReply::IndexStarted(result) => {
            index_start_view(result).with_index_job(IndexJobProjection::Started(result.clone()))
        }
        SurfaceReply::IndexOperationStarted(observation)
        | SurfaceReply::IndexOperationStatus(observation) => {
            index_operation_view(observation).with_index_operation(observation.clone())
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

fn index_operation_view(observation: &backend_library::IndexOperationObservation) -> ProductView {
    use backend_library::IndexOperationObservation as O;
    match observation {
        O::OutsideReceiptWindow {
            operation_key,
            request_digest,
        } => ProductView::rows(
            "index-operation",
            vec![ProductRecord::new(
                "index operation receipt is outside the evidence window".to_owned(),
                Some(operation_key.to_hex()),
                vec![
                    "this key was consumed and cannot be accepted again".to_owned(),
                    "publication outcome is not established by this observation".to_owned(),
                    format!("request digest {}", full_digest(request_digest)),
                ],
            )],
        ),
        O::Unknown { operation_key } => ProductView::rows(
            "index-operation",
            vec![ProductRecord::new(
                "index operation key is unknown".to_owned(),
                Some(operation_key.to_hex()),
                vec!["unknown does not mean published".to_owned()],
            )],
        ),
        O::Known(status) => {
            let state = match &status.state {
                backend_library::IndexOperationState::Accepted => "accepted".to_owned(),
                backend_library::IndexOperationState::Active { stage, .. } => {
                    format!("active at {}", index_stage(*stage))
                }
                backend_library::IndexOperationState::Published(receipt) => {
                    let _published_root = receipt.view_root();
                    "published".to_owned()
                }
                backend_library::IndexOperationState::PartiallyPublished {
                    refused_profiles,
                    ..
                } => {
                    format!(
                        "partially published; {} language profiles unavailable; inspect package-profile for toolchain guidance",
                        refused_profiles.len()
                    )
                }
                backend_library::IndexOperationState::Failed {
                    detail,
                    compiler_failure,
                    ..
                } => {
                    let sentence = compiler_failure.as_ref().map_or_else(
                        || detail.as_str().to_owned(),
                        |failure| {
                            Fault::compiler_refusal(
                                failure,
                                Operand::Text(status.operation_key.to_hex()),
                            )
                            .cause()
                            .sentence()
                            .to_owned()
                        },
                    );
                    format!("failed: {sentence}")
                }
                backend_library::IndexOperationState::Unresolved { detail, .. } => {
                    format!("unresolved: {}", detail.as_str())
                }
            };
            let mut facts = vec![format!("package {}", status.package.as_str())];
            if let Some(capture) = &status.source_capture {
                facts.push(format!(
                    "source capture root {} at sequence {}",
                    lower_hex(capture.workspace_root()),
                    capture.workspace_sequence()
                ));
                for profile in capture.profiles() {
                    let semantic = match profile.state {
                        backend_library::IndexOperationSemanticProfileState::Pending { prior } => {
                            match prior {
                                Some(prior) => format!(
                                    "pending; prior generation {} is retained as stale",
                                    lower_hex(&prior.generation)
                                ),
                                None => "pending; no prior generation is selected".to_owned(),
                            }
                        }
                        backend_library::IndexOperationSemanticProfileState::Unavailable {
                            reason,
                        } => format!("unavailable: {reason:?}"),
                        backend_library::IndexOperationSemanticProfileState::Failed {
                            prior,
                            reason,
                        } => format!(
                            "failed: {reason:?}; prior generation {} is retained as stale",
                            lower_hex(&prior.generation)
                        ),
                        backend_library::IndexOperationSemanticProfileState::Published {
                            generation,
                            coverage,
                        } => format!(
                            "published generation {} with {coverage:?} coverage",
                            lower_hex(&generation)
                        ),
                    };
                    facts.push(format!(
                        "{} source version {} ({} files, observation {}): {semantic}",
                        profile.profile.name().unwrap_or("unknown profile"),
                        lower_hex(&profile.source_version),
                        profile.source_count,
                        profile.observation_sequence,
                    ));
                }
            }
            ProductView::rows(
                "index-operation",
                vec![ProductRecord::new(
                    format!("index operation {state}"),
                    Some(status.operation_key.to_hex()),
                    facts,
                )],
            )
        }
    }
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
        IndexJobOutcome::RefusedWithCompilerFailure { .. } => ("refused", None),
        IndexJobOutcome::Cancelled => ("cancelled", None),
        IndexJobOutcome::Failed(reason) => ("failed", Some(reason.as_str())),
    };
    let mut tags = vec![format!("outcome {state}")];
    if let Some(detail) = detail {
        tags.push(detail.to_owned());
    }
    if let IndexJobOutcome::RefusedWithCompilerFailure { failure, .. } = &terminal.outcome {
        let fault =
            Fault::compiler_refusal(failure, Operand::Text(index_ticket_json(&terminal.ticket)));
        tags.push(fault.cause().sentence().to_owned());
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

fn local_declaration_search_row(record: &LocalDeclarationSearchRecord) -> ProductRecord {
    let source = match &record.source {
        LocalDeclarationSource::Captured { path, line } => format!("{}:{line}", path.as_str()),
        LocalDeclarationSource::NotCaptured => "source not captured".to_owned(),
    };
    ProductRecord::new(
        record.name.as_str(),
        Some(record.coordinate.as_str().to_owned()),
        vec!["local declaration".to_owned(), source],
    )
    .with_local_declaration(record.clone())
}

fn registry_search_hit_row(hit: &RegistrySearchHit) -> ProductRecord {
    match hit {
        RegistrySearchHit::Acquired(record) => registry_row(record),
        RegistrySearchHit::LocalDeclaration(record) => local_declaration_search_row(record),
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
                ForgeFact::Recorded(name) => format!("{} · source pin", single_line(name.as_str())),
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
            format!("manifest: {}", single_line(detail.manifest.path.as_str())),
            yanked.to_owned(),
            downloads,
            advisory,
        ],
    )
    .with_forge_package_detail(detail.clone())
    .with_optional_source_metadata(detail.manifest.python_metadata.as_ref())
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
            format!(
                "manifest: {}",
                single_line(candidate.manifest.path.as_str())
            ),
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
            source_metadata,
        } => ProductView::rows(
            "package-profile",
            match latest {
                Some(record) => {
                    let mut row = registry_row(record);
                    row.tags = append(&row.tags, format!("{versions} version(s)"));
                    if let Some(metadata) = source_metadata {
                        row = row.with_source_metadata(metadata.clone());
                    }
                    vec![row]
                }
                None if *versions == 0 => {
                    let mut row = ProductRecord::new("no version recorded", None, Vec::new());
                    if let Some(metadata) = source_metadata {
                        row = row.with_source_metadata(metadata.clone());
                    }
                    vec![row]
                }
                None => vec![
                    ProductRecord::new(
                        "latest release is not confirmed",
                        None,
                        vec![
                            format!("{versions} recorded version(s)"),
                            registry_authority_tag(*candidate_authority),
                        ],
                    )
                    .with_optional_source_metadata(source_metadata.as_ref()),
                ],
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
        SurfaceReply::CargoPackageSourceFile(result) => match result {
            backend_library::CargoPackageSourceFileResultV1::Read {
                path,
                contents,
                semantic: backend_library::CargoPackageSourceSemanticStatusV1::NotIndexed,
                ..
            } => ProductView::scalar(
                "cargo-source-file",
                format!(
                    "{} · source-only, not indexed\n\n{}",
                    path.as_str(),
                    fenced_source_text(contents)
                ),
            ),
            backend_library::CargoPackageSourceFileResultV1::Stale { .. } => ProductView::scalar(
                "cargo-source-file",
                "The Cargo source receipt is stale. Reload the package tree before opening this file.",
            ),
            backend_library::CargoPackageSourceFileResultV1::Unavailable { reason, .. } => {
                ProductView::scalar(
                    "cargo-source-file",
                    format!("The owner could not read this source file: {reason:?}."),
                )
            }
        },
        SurfaceReply::CargoPackageSourceInventory(result) => match result {
            backend_library::CargoPackageSourceInventoryResultV1::Listed(inventory) => {
                let coverage = match inventory.coverage {
                    backend_library::CargoPackageSourceInventoryCoverageV1::Complete => {
                        "complete for supported regular source/document files; internal directories and links are excluded".to_owned()
                    }
                    backend_library::CargoPackageSourceInventoryCoverageV1::Truncated { limit } => {
                        format!("truncated at {limit} paths; additional paths may exist")
                    }
                    backend_library::CargoPackageSourceInventoryCoverageV1::Partial { reason } => {
                        format!("partial inventory ({reason:?}); additional paths may exist")
                    }
                };
                let mut records = Vec::with_capacity(inventory.paths.len().saturating_add(1));
                records.push(ProductRecord::new(
                    format!(
                        "{} · {} observed path(s)",
                        inventory.package.as_str(),
                        inventory.paths.len()
                    ),
                    None,
                    vec![coverage],
                ));
                records.extend(inventory.paths.iter().map(|path| {
                    ProductRecord::new(
                        path.as_str(),
                        Some(path.as_str().to_owned()),
                        vec!["source-only address · revalidate before reading".to_owned()],
                    )
                }));
                ProductView::assembled("cargo-source-inventory", records)
            }
            backend_library::CargoPackageSourceInventoryResultV1::Stale { .. } => {
                ProductView::scalar(
                    "cargo-source-inventory",
                    "The Cargo source receipt is stale. Reload the package tree before listing files.",
                )
            }
            backend_library::CargoPackageSourceInventoryResultV1::Unavailable {
                reason, ..
            } => ProductView::scalar(
                "cargo-source-inventory",
                format!("The owner could not list this source: {reason:?}."),
            ),
        },
        SurfaceReply::CargoPackageReadme(result) => match result {
            backend_library::CargoPackageReadmeResultV1::Read { readme, .. } => {
                let origin = backend_library::CargoPackageReadmeOriginV1::from_result(result)
                    .and_then(|origin| serde_json::to_string(&origin).ok())
                    .unwrap_or_else(|| "unavailable".to_owned());
                ProductView::scalar(
                    "cargo-package-readme",
                    format!(
                        "{:?}/{} · {:?}\nOrigin JSON: {}\n\n{}",
                        readme.root_scope,
                        readme.path.as_str(),
                        readme.selection,
                        origin,
                        readme.contents
                    ),
                )
            }
            backend_library::CargoPackageReadmeResultV1::Absent { reason, .. } => {
                ProductView::scalar(
                    "cargo-package-readme",
                    format!("This exact Cargo package release has no README ({reason:?})."),
                )
            }
            backend_library::CargoPackageReadmeResultV1::Stale { .. } => ProductView::scalar(
                "cargo-package-readme",
                "The Cargo source receipt is stale. Reload the package tree before opening its README.",
            ),
            backend_library::CargoPackageReadmeResultV1::Unavailable { reason, .. } => {
                ProductView::scalar(
                    "cargo-package-readme",
                    format!("The owner could not read this package README: {reason:?}."),
                )
            }
        },
        SurfaceReply::CargoPackageReadmeLink(result) => match result {
            backend_library::CargoPackageReadmeLinkResultV1::Anchor { fragment, .. } => {
                ProductView::scalar(
                    "cargo-package-readme-link",
                    format!("README anchor #{fragment}"),
                )
            }
            backend_library::CargoPackageReadmeLinkResultV1::Read {
                root_scope,
                path,
                fragment,
                contents,
                ..
            } => ProductView::scalar(
                "cargo-package-readme-link",
                format!(
                    "{root_scope:?}/{}{}\n\n{}",
                    path.as_str(),
                    fragment
                        .as_deref()
                        .map_or_else(String::new, |fragment| format!("#{fragment}")),
                    contents
                ),
            ),
            backend_library::CargoPackageReadmeLinkResultV1::Stale { .. } => ProductView::scalar(
                "cargo-package-readme-link",
                "The README link origin is stale. Reload the package README before following its links.",
            ),
            backend_library::CargoPackageReadmeLinkResultV1::Unavailable { reason, .. } => {
                ProductView::scalar(
                    "cargo-package-readme-link",
                    format!("The owner could not follow this README link: {reason:?}."),
                )
            }
        },
        other => ProductView::scalar("surface", format!("{:?}", other.id())),
    }
}

fn fenced_source_text(contents: &str) -> String {
    let longest_run = |needle: char| {
        contents
            .chars()
            .fold((0_usize, 0_usize), |(longest, current), character| {
                if character == needle {
                    let current = current.saturating_add(1);
                    (longest.max(current), current)
                } else {
                    (longest, 0)
                }
            })
            .0
    };
    let backticks = longest_run('`');
    let tildes = longest_run('~');
    let (marker, length) = if backticks <= tildes {
        ('`', backticks.saturating_add(1).max(3))
    } else {
        ('~', tildes.saturating_add(1).max(3))
    };
    let fence = std::iter::repeat_n(marker, length).collect::<String>();
    format!("{fence}text\n{contents}\n{fence}")
}

/// A project's tree: the lede, what affects it, each role, then each package
/// that is here twice, in the same words the desktop's Library page uses.
fn tree_view(tree: &backend_library::browse::ProjectTree) -> ProductView {
    let reading = crate::browse::read_tree(tree);
    let mut records = Vec::new();
    let mut tags = Vec::new();
    tags.extend(reading.locked_inactive_note.clone());
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
        RegistryMetadata::Partial { value, reason } => {
            let mut view = ProductView::rows("owner", value.iter().map(owner_row).collect());
            view.note = Some(format!("Partial coverage: {}", reason.as_str()));
            view
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
        RegistryMetadata::Partial { value, reason } => {
            let mut view = ProductView::rows(heading, registry_rows(value));
            view.note = Some(format!("Partial coverage: {}", reason.as_str()));
            view
        }
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
                        single_line(edge.target.name.as_str())
                    ),
                    Some(single_line(edge.target.name.as_str())),
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
            format!(
                "requirement: {}",
                single_line(edge.target.requirement.as_str())
            ),
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
            single_line(record.target.name.as_str()),
            single_line(record.target.requirement.as_str())
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

fn python_metadata_tags(metadata: &backend_library::PythonProjectMetadata) -> Vec<String> {
    use backend_library::{DependencyScope, PythonMetadataFact};
    let mut tags = vec![format!(
        "Python source metadata: {}",
        single_line(&metadata.manifest_path)
    )];
    for (label, fact) in [
        ("declared package", &metadata.name),
        ("description", &metadata.description),
        ("homepage", &metadata.homepage),
        ("docs", &metadata.documentation),
        ("repository", &metadata.repository),
        ("requires Python", &metadata.requires_python),
    ] {
        match fact {
            PythonMetadataFact::Recorded { value, .. } => {
                tags.push(format!("{label}: {}", single_line(value)))
            }
            PythonMetadataFact::Dynamic { reason, .. }
            | PythonMetadataFact::Partial { reason, .. } => {
                tags.push(format!("{label}: unavailable ({})", single_line(reason)))
            }
            PythonMetadataFact::Omitted { .. } => {}
        }
    }
    match &metadata.version {
        PythonMetadataFact::Dynamic { reason, .. } | PythonMetadataFact::Partial { reason, .. } => {
            tags.push(format!(
                "package version: unavailable ({})",
                single_line(reason)
            ))
        }
        PythonMetadataFact::Omitted { .. } => tags.push("package version: not declared".to_owned()),
        PythonMetadataFact::Recorded { value, .. } => {
            tags.push(format!("declared package version: {}", single_line(value)))
        }
    }
    match &metadata.dependencies {
        PythonMetadataFact::Recorded { value, .. } | PythonMetadataFact::Partial { value, .. } => {
            tags.push(format!(
                "declared dependencies: {} runtime, {} optional, {} build/test",
                value
                    .iter()
                    .filter(|dep| dep.scope == DependencyScope::Runtime)
                    .count(),
                value
                    .iter()
                    .filter(|dep| dep.scope == DependencyScope::Optional)
                    .count(),
                value
                    .iter()
                    .filter(|dep| matches!(
                        dep.scope,
                        DependencyScope::Build | DependencyScope::Development
                    ))
                    .count()
            ));
            if let PythonMetadataFact::Partial { reason, .. } = &metadata.dependencies {
                tags.push(format!(
                    "dependency completeness: unavailable ({})",
                    single_line(reason)
                ));
            }
        }
        PythonMetadataFact::Dynamic { reason, .. } => tags.push(format!(
            "declared dependencies: unavailable ({})",
            single_line(reason)
        )),
        PythonMetadataFact::Omitted { .. } => {
            tags.push("declared dependencies: not recorded".to_owned())
        }
    }
    tags
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
        tags.push(format!("manifest: {}", single_line(manifest.path.as_str())));
        if let Some(metadata) = &manifest.python_metadata {
            tags.extend(python_metadata_tags(metadata));
        }
    }
    if record.manifests.len() > 8 {
        tags.push(format!("and {} more manifests", record.manifests.len() - 8));
    }
    ProductRecord::new(
        single_line(&format!(
            "{} / {}",
            record.owner.as_str(),
            record.repository.as_str()
        )),
        Some(record.coordinate.as_str().to_owned()),
        tags.into_iter().map(|tag| single_line(&tag)).collect(),
    )
    .with_forge_details(record.clone())
}

fn brief_forge_fact<T>(fact: &ForgeFact<T>, recorded: impl FnOnce(&T) -> String) -> String {
    let value = single_line(&match fact {
        ForgeFact::Recorded(value) => recorded(value),
        ForgeFact::Unavailable(reason) => reason.as_str().to_owned(),
    });
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
            "{} [bytes{}..{})",
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
        format!("{} manifest byte(s)", record.semantic_bytes),
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
    .with_history_status(&record.history_status)
    .with_compiler_profile(record.profile)
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

/// Renders the typed status projection; the exact proof stays in the facet.
pub(crate) fn semantic_history_details(status: &ProductSemanticHistoryStatus) -> String {
    match status {
        ProductSemanticHistoryStatus::NotSelected => {
            "Derived history is not selected for this compiler generation.".to_owned()
        }
        ProductSemanticHistoryStatus::NotRequested { selection_id } => format!(
            "Derived history has not been requested for committed selection {}.",
            full_digest(selection_id),
        ),
        ProductSemanticHistoryStatus::Pending { selection_id } => format!(
            "Derived history publication is pending for committed selection {}.",
            full_digest(selection_id),
        ),
        ProductSemanticHistoryStatus::Deferred {
            selection_id,
            reason,
        } => format!(
            "Derived history was deferred and is retryable; the owner will reschedule committed selection {}: {}",
            full_digest(selection_id),
            single_line(reason),
        ),
        ProductSemanticHistoryStatus::Published {
            selection_id,
            commit,
            reference,
            proof,
        } => format!(
            "Derived history is published for committed selection {} at commit {} (reference: {}, verified tip: {}). Compiler input replay remains unproven.",
            full_digest(selection_id),
            full_digest(commit),
            single_line(reference),
            full_digest(&proof.reference_tip),
        ),
        ProductSemanticHistoryStatus::Refused {
            selection_id,
            reason,
        } => format!(
            "Derived history publication was refused for committed selection {}: {}. The committed semantic generation remains selected.",
            full_digest(selection_id),
            single_line(reason),
        ),
        ProductSemanticHistoryStatus::Superseded { selection_id } => format!(
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
            if character.is_control()
                || matches!(character, '\u{2028}' | '\u{2029}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            {
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
    fn python_profile_source_metadata_survives_readable_and_typed_projection() {
        use backend_library::{PythonMetadataEvidence, PythonMetadataFact, PythonProjectMetadata};
        let evidence = PythonMetadataEvidence {
            path: "pyproject.toml".to_owned(),
            digest: [7; 32],
            bytes: 128,
        };
        let omitted = PythonMetadataFact::Omitted {
            evidence: vec![evidence.clone()],
        };
        let metadata = PythonProjectMetadata {
            manifest_path: "pyproject.toml".to_owned(),
            evidence: vec![evidence.clone()],
            name: PythonMetadataFact::Recorded {
                value: "dynamic-app".to_owned(),
                evidence: vec![evidence.clone()],
            },
            version: PythonMetadataFact::Dynamic {
                reason: "hatch version\n\u{1b}[31mrequires unsupported interpretation".to_owned(),
                evidence: vec![evidence.clone()],
            },
            description: PythonMetadataFact::Recorded {
                value: "An HTTP client\nforged row\u{1b}[31m\r\t".to_owned(),
                evidence: vec![evidence.clone()],
            },
            documentation: PythonMetadataFact::Recorded {
                value: "https://example.test/docs".to_owned(),
                evidence: vec![evidence.clone()],
            },
            homepage: omitted.clone(),
            repository: omitted.clone(),
            requires_python: omitted,
            dependencies: PythonMetadataFact::Dynamic {
                reason: "dependency hook is dynamic".to_owned(),
                evidence: vec![evidence],
            },
        };
        let reply = SurfaceReply::PackageProfile {
            latest: None,
            versions: 1,
            candidate_authority: None,
            source_metadata: Some(metadata.clone()),
        };
        reply
            .admit(backend_library::CommandId::PackageProfile)
            .expect("bounded source declaration");
        let view = product_view(&reply);
        assert_eq!(view.records()[0].source_metadata(), Some(&metadata));
        for tag in view.records()[0].tags() {
            assert!(
                !tag.chars().any(char::is_control),
                "unsafe display tag: {tag:?}"
            );
        }
        let plain = crate::text::product(&view, crate::Theme::plain());
        assert!(!plain.contains('\u{1b}'));
        assert!(!plain.contains("client\nforged"));
        let text = crate::markdown::product(&view);
        assert!(text.contains("An HTTP client"));
        assert!(text.contains("https://example.test/docs"));
        assert!(text.contains("package version: unavailable"));
        assert!(text.contains("declared dependencies: unavailable"));
        assert!(!text.contains("0.0.0"));
        assert!(!text.contains('\u{1b}'));
        assert!(!text.contains("client\nforged"));
        let value =
            serde_json::to_value(crate::dto::ProductDto::new(&view)).expect("typed product JSON");
        assert_eq!(
            value["records"][0]["source_metadata"]["version"]["state"],
            "dynamic"
        );
        assert_eq!(
            value["records"][0]["source_metadata"]["evidence"][0]["path"],
            "pyproject.toml"
        );
        assert_eq!(
            value["records"][0]["source_metadata"]["evidence"][0]["digest"][0],
            7
        );
        assert!(value["records"][0].get("native_metadata").is_none());
        assert_eq!(
            value["records"][0]["source_metadata"]["description"]["value"],
            "An HTTP client\nforged row\u{1b}[31m\r\t"
        );
        assert!(
            value["records"][0]["tags"]
                .as_array()
                .expect("display tags")
                .iter()
                .all(|tag| !tag.as_str().expect("tag").chars().any(char::is_control))
        );
    }

    #[test]
    fn dependency_declaration_controls_cannot_inject_readable_rows() {
        use backend_library::{
            DependencyAuthority, DependencyEvidence, DependencyScope, PACKAGE_GRAPH_PAGE_SCHEMA,
            PackageDependencyTarget, PackageGraphDirection, PackageGraphKnowledge,
            PackageGraphPage, PackageGraphPageTerminal, PackageGraphSourceAuthority,
            PackageGraphSourceKey,
        };
        let requirement =
            "requests>=2; platform_machine == 'literal\nforged\u{1b}[31m\u{2028}\u{202e}'";
        let source = PackageReference::parse("pkg:pypi/sample@1").expect("source");
        let authority = PackageGraphSourceAuthority::Local([7; 32]);
        let record = PackageDependencyRecord::new_with_source_authority(
            source.clone(),
            authority,
            PackageDependencyTarget::new(RegistryEcosystem::Pypi, "requests", requirement, None)
                .expect("bounded source spelling"),
            DependencyScope::Runtime,
            false,
            DependencyEvidence {
                authority: DependencyAuthority::LocalManifest,
                frontier: [7; 32],
                provenance: [8; 32],
            },
        );
        let page = PackageGraphPage {
            schema: PACKAGE_GRAPH_PAGE_SCHEMA,
            view_root: [9; 32],
            facts_witness: [10; 32],
            catalog_snapshot: None,
            package: source.clone(),
            direction: PackageGraphDirection::Dependencies,
            source: Some(PackageGraphSourceKey::new(source, authority)),
            knowledge: PackageGraphKnowledge::Known,
            rows: Box::new([record.clone()]),
            terminal: PackageGraphPageTerminal::Complete,
        };
        page.admit().expect("bounded typed graph page");
        for reply in [
            SurfaceReply::Dependencies(DependencyFacts::Known(Box::new([record]))),
            SurfaceReply::PackageGraphPage(page),
        ] {
            let view = product_view(&reply);
            for row in view.records() {
                assert!(!row.title().chars().any(char::is_control));
                assert!(
                    row.tags()
                        .iter()
                        .all(|tag| !tag.chars().any(char::is_control))
                );
            }
            for text in [
                crate::text::product(&view, crate::Theme::plain()),
                crate::markdown::product(&view),
            ] {
                assert!(!text.contains("literal\nforged"));
                assert!(!text.contains(['\u{1b}', '\u{2028}', '\u{202e}']));
                assert!(text.contains("requests"));
            }
            let original = serde_json::to_value(&reply).expect("source facts");
            assert!(original.to_string().contains("literal\\nforged"));
        }
    }

    #[test]
    fn local_declaration_search_preserves_source_provenance_in_cli_and_mcp() {
        let coordinate = backend_library::ProductText::new("/project::src/react.tsx:17::react")
            .expect("coordinate");
        let record = LocalDeclarationSearchRecord {
            coordinate: coordinate.clone(),
            name: backend_library::ProductText::new("react").expect("name"),
            source: LocalDeclarationSource::Captured {
                path: backend_library::ProductText::new("src/react.tsx").expect("path"),
                line: std::num::NonZeroU32::new(17).expect("line"),
            },
        };
        let reply = SurfaceReply::IndexSearchWithDiscovery(Box::new([
            RegistrySearchHit::LocalDeclaration(record.clone()),
        ]));
        reply.admit(reply.id()).expect("local search reply");
        let view = product_view(&reply);
        assert_eq!(view.records()[0].operand(), Some(coordinate.as_str()));
        for rendered in [
            crate::markdown::product(&view),
            crate::text::product(&view, crate::Theme::plain()),
        ] {
            assert!(rendered.contains("local declaration"));
            assert!(rendered.contains("src/react.tsx:17"));
            for fiction in [
                "cargo",
                "byte(s)",
                "release:",
                "downloads:",
                "registry facts:",
            ] {
                assert!(
                    !rendered.contains(fiction),
                    "local declaration cannot claim {fiction}"
                );
            }
        }
        let dto = crate::dto::ProductDto::new(&view);
        assert_eq!(dto.records[0].local_declaration.as_ref(), Some(&record));
        assert!(dto.records[0].native_metadata.is_none());
        assert!(dto.records[0].package_group.is_none());
        let explored = SurfaceReply::ExploredDeclarations(Box::new([record.clone()]));
        explored
            .admit(explored.id())
            .expect("typed local exploration");
        assert_eq!(
            crate::dto::ProductDto::new(&product_view(&explored)).records[0]
                .local_declaration
                .as_ref(),
            Some(&record)
        );
        let mut mismatched = record.clone();
        mismatched.source = LocalDeclarationSource::Captured {
            path: backend_library::ProductText::new("src/other.tsx").expect("path"),
            line: std::num::NonZeroU32::new(17).expect("line"),
        };
        let mismatched = SurfaceReply::ExploredDeclarations(Box::new([mismatched]));
        assert!(mismatched.admit(mismatched.id()).is_err());
        let mut wrong_line = record.clone();
        wrong_line.source = LocalDeclarationSource::Captured {
            path: backend_library::ProductText::new("src/react.tsx").expect("path"),
            line: std::num::NonZeroU32::new(18).expect("line"),
        };
        let wrong_line = SurfaceReply::ExploredDeclarations(Box::new([wrong_line]));
        assert!(wrong_line.admit(wrong_line.id()).is_err());
        let mut uncaptured = record.clone();
        uncaptured.source = LocalDeclarationSource::NotCaptured;
        let uncaptured = local_declaration_search_row(&uncaptured);
        assert!(
            uncaptured
                .tags()
                .iter()
                .any(|tag| tag == "source not captured")
        );
        assert!(
            !uncaptured
                .tags()
                .iter()
                .any(|tag| tag.contains("src/react.tsx"))
        );
        let mut invalid = record;
        invalid.coordinate =
            backend_library::ProductText::new("pkg:npm/react@19.2.0").expect("purl");
        let invalid = SurfaceReply::IndexSearchWithDiscovery(Box::new([
            RegistrySearchHit::LocalDeclaration(invalid),
        ]));
        assert!(invalid.admit(invalid.id()).is_err());
    }

    #[test]
    fn aged_operation_receipt_keeps_consumed_identity_without_claiming_publication() {
        let operation_key = backend_library::IndexOperationKey::from_bytes([0x31; 32])
            .expect("nonzero caller-owned key");
        let observation = backend_library::IndexOperationObservation::OutsideReceiptWindow {
            operation_key,
            request_digest: [0x72; 32],
        };
        for reply in [
            SurfaceReply::IndexOperationStarted(observation.clone()),
            SurfaceReply::IndexOperationStatus(observation.clone()),
        ] {
            let view = product_view(&reply);
            assert!(
                view.index_job().is_none(),
                "a tombstone cannot create a live job or terminal receipt"
            );
            assert_eq!(
                view.records()[0].operand(),
                Some(operation_key.to_hex().as_str())
            );
            for rendered in [
                crate::markdown::product(&view),
                crate::text::product(&view, crate::Theme::plain()),
            ] {
                assert!(rendered.contains("outside the evidence window"));
                assert!(rendered.contains("cannot be accepted again"));
                assert!(rendered.contains("publication outcome is not established"));
                assert!(rendered.contains(&full_digest(&[0x72; 32])));
                assert!(!rendered.contains("is unknown"));
            }
            let dto = crate::dto::ProductDto::new(&view);
            assert_eq!(dto.records.len(), 1);
        }
    }

    #[test]
    fn partial_registry_metadata_keeps_coverage_in_cli_and_mcp_without_hiding_rows() {
        let metadata = RegistryNativeMetadata::unavailable(RegistryEcosystem::Cargo, "test");
        let record = RegistryPackageRecord {
            coordinate: PackageReference::parse("pkg:cargo/proven@1.0.0").expect("coordinate"),
            ecosystem: RegistryEcosystem::Cargo,
            name: backend_library::ProductText::from_static("proven"),
            version: backend_library::ProductText::from_static("1.0.0"),
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
        let reply = SurfaceReply::Dependents(RegistryMetadata::Partial {
            value: Box::new([record]),
            reason: backend_library::ProductText::from_static(
                "some source authorities were unresolved",
            ),
        });
        let projected = product_view(&reply);
        assert_eq!(projected.records().len(), 1);
        assert!(
            projected
                .note()
                .is_some_and(|note| note.contains("Partial coverage"))
        );
        assert!(crate::dto::ProductDto::new(&projected).note.is_some());

        let markdown = crate::markdown::product(&projected);
        let terminal = crate::text::product(&projected, crate::Theme::plain());
        assert!(
            markdown.contains("Partial coverage") && markdown.contains("pkg:cargo/proven@1.0.0")
        );
        assert!(
            terminal.contains("Partial coverage") && terminal.contains("pkg:cargo/proven@1.0.0")
        );
        let dto = crate::dto::ProductDto::new(&projected);
        assert_eq!(dto.records.len(), 1);
        assert!(dto.note.is_some());
    }

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

    #[test]
    fn package_source_membership_dto_preserves_typed_cursor_and_reads_older_payloads() {
        let package = PackageReference::parse("demo-package").expect("local package");
        let cursor = backend_library::PackageSourceMembershipCursorV1 {
            schema: backend_library::PACKAGE_SOURCE_MEMBERSHIP_SCHEMA,
            package: package.clone(),
            project_key: [0x11; 32],
            source_relation_root: [0x22; 32],
            source_version: [0x33; 32],
            membership_page: 0,
            membership_offset: 0,
            ordinal: 0,
            last_file_key: [0x44; 32],
        };
        let page = backend_library::PackageSourceMembershipPageResultV1::Page {
            package,
            project_key: [0x11; 32],
            source_relation_root: [0x22; 32],
            source_version: [0x33; 32],
            file_count: 2,
            start_offset: 0,
            scope: backend_library::PackageSourceMembershipScopeV1::IndexedProjectMembership,
            exclusions: backend_library::PackageSourceMembershipExclusionsV1::NotCaptured,
            files: vec![backend_library::PackageSourceMembershipFileV1 {
                file_key: [0x44; 32],
                path: "src/index.ts".to_owned(),
                language: backend_library::PackageSourceMembershipLanguageV1::TypeScript,
                content_version: [0x55; 32],
                source_identity: Some([0x66; 32]),
            }]
            .into_boxed_slice(),
            next: Some(cursor),
        };
        let view = product_view(&SurfaceReply::PackageSourceMembershipPage(page.clone()));
        let dto = crate::dto::ProductDto::new(&view);
        let encoded = serde_json::to_value(&dto).expect("product DTO JSON");
        assert_eq!(
            encoded["package_source_membership_page"]["next"]["last_file_key"][0],
            0x44
        );
        assert_eq!(
            encoded["package_source_membership_page"]["files"][0]["path"],
            "src/index.ts"
        );
        assert_eq!(
            encoded["package_source_membership_page"]["files"][0]["language"],
            "typescript"
        );
        assert_eq!(
            serde_json::from_value::<crate::dto::ProductDto>(encoded.clone())
                .expect("current DTO round trip")
                .package_source_membership_page,
            Some(page)
        );

        let mut older_payload = encoded;
        older_payload
            .as_object_mut()
            .expect("object DTO")
            .remove("package_source_membership_page");
        assert!(
            serde_json::from_value::<crate::dto::ProductDto>(older_payload)
                .expect("previous DTO without source page")
                .package_source_membership_page
                .is_none()
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
            selected_source_frontier: None,
        }
    }

    fn semantic_versions_view(record: SemanticVersionRecord) -> ProductView {
        product_view(&SurfaceReply::SemanticVersions(
            vec![record].into_boxed_slice(),
        ))
    }

    #[test]
    fn semantic_generation_projection_preserves_profile_and_decodes_older_dto() {
        let profile = SemanticLanguageProfile::from_name("rust").expect("Rust profile");
        let view = semantic_versions_view(semantic_version(
            SemanticHistoryPublicationStatus::NotSelected,
            false,
            true,
            SemanticVersionFreshness::Current {
                input_digest: [0x71; 32],
            },
        ));
        let row = view.records().first().expect("semantic generation row");
        assert_eq!(row.compiler_profile(), Some(profile));

        let dto = crate::dto::ProductDto::new(&view);
        assert_eq!(dto.records[0].compiler_profile, Some(profile));
        let mut old_value = serde_json::to_value(&dto).expect("serialized product DTO");
        assert_eq!(
            old_value["records"][0]["compiler_profile"],
            serde_json::json!(profile.to_bytes()),
            "the typed projection preserves the existing two-byte wire shape"
        );
        old_value["records"][0]
            .as_object_mut()
            .expect("record object")
            .remove("compiler_profile");
        let decoded: crate::dto::ProductDto =
            serde_json::from_value(old_value).expect("older DTO without compiler profile");
        assert_eq!(decoded.records[0].compiler_profile, None);
    }

    #[test]
    fn selected_source_frontier_survives_product_and_older_dto_projection() {
        let mut record = semantic_version(
            SemanticHistoryPublicationStatus::NotSelected,
            true,
            true,
            SemanticVersionFreshness::Current {
                input_digest: [0x81; 32],
            },
        );
        let package =
            PackageReference::parse("/workspace/large-project").expect("local project package");
        record.package = package.clone();
        let frontier = SelectedProjectSourceFrontier {
            package,
            source_relation_root: [0x82; 32],
            source_version: [0x83; 32],
            file_count: 2_916,
        };
        record.selected_source_frontier = Some(frontier.clone());

        let view = semantic_versions_view(record);
        assert_eq!(view.selected_source_frontier(), Some(&frontier));
        let dto = crate::dto::ProductDto::new(&view);
        assert_eq!(dto.selected_source_frontier, Some(frontier.clone()));

        let mut old_value = serde_json::to_value(&dto).expect("selected frontier DTO");
        assert_eq!(old_value["selected_source_frontier"]["file_count"], 2_916);
        old_value
            .as_object_mut()
            .expect("product object")
            .remove("selected_source_frontier");
        let older: crate::dto::ProductDto =
            serde_json::from_value(old_value).expect("older product row decodes");
        assert_eq!(older.selected_source_frontier, None);
    }

    #[test]
    fn durable_index_operation_projection_preserves_all_states_and_published_receipt() {
        use backend_library::{
            CompileExecutionIntent, IndexOperationFailureReason, IndexOperationKey,
            IndexOperationObservation, IndexOperationPublicationReceipt, IndexOperationState,
            IndexOperationStatus, IndexOperationUnresolvedReason, ProductText,
        };

        let package = PackageReference::parse("/workspace/project").expect("local package");
        let key = IndexOperationKey::from_bytes([0x31; 32]).expect("operation key");
        let status = |state| {
            IndexOperationStatus::new(
                key,
                package.clone(),
                CompileExecutionIntent::Interactive,
                state,
            )
        };
        let receipt_root = backend_library::view_state_root(&[]);
        let basis = backend_library::Basis::new(
            receipt_root,
            backend_library::object_version(b"index-operation-projection-test"),
        );
        let frontier = backend_library::Frontier::new(
            backend_library::branch_key("main"),
            backend_library::log_key("library"),
            backend_library::CURSOR_SCHEMA,
            receipt_root,
            0,
        );
        let view = backend_library::ViewRoot::new_incomplete(
            backend_library::view_key(b"index-operation-projection-test-view"),
            basis,
            frontier,
            Vec::new(),
            Vec::new(),
        )
        .expect("incomplete projection view");
        let receipt = IndexOperationPublicationReceipt::from_published_view(
            Some([0x41; 32]),
            [0x42; 32],
            [0x43; 32],
            9,
            &view,
            backend_library::Cursor::for_view_root(&view),
        )
        .expect("checked published receipt");
        let compiler_failure = backend_library::PackageCompilerFailure::from_package_terminal(
            "classes/comparator.d.ts",
            &backend_library::interface::CompilerTerminal::Toolchain {
                source: backend_library::interface::SourceAuthority {
                    identity: backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(b"declaration"),
                    byte_len: 11,
                },
                language: backend_semantic::vocabulary::Language::TypeScript,
                stage: backend_semantic::vocabulary::Stage::LowerIr,
                selected: backend_semantic::vocabulary::NativeTool::TypeScriptCompiler,
                configured: None,
            },
        ).expect("valid setup terminal").expect("closed compiler refusal");
        let observations = [
            IndexOperationObservation::Unknown { operation_key: key },
            IndexOperationObservation::OutsideReceiptWindow {
                operation_key: key,
                request_digest: [0x51; 32],
            },
            IndexOperationObservation::Known(status(IndexOperationState::Accepted)),
            IndexOperationObservation::Known(status(IndexOperationState::Active {
                ticket: backend_library::IndexJobTicket::new(
                    std::num::NonZeroU64::new(7).expect("nonzero job ticket"),
                    [0x61; 16],
                    package.clone(),
                ),
                stage: backend_library::IndexJobStage::Compiling,
            })),
            IndexOperationObservation::Known(status(IndexOperationState::Published(receipt))),
            IndexOperationObservation::Known(status(IndexOperationState::Failed {
                reason: IndexOperationFailureReason::WorkerFailed,
                detail: ProductText::new("bounded worker detail").expect("failure detail"),
                compiler_failure: None,
            })),
            IndexOperationObservation::Known(status(IndexOperationState::Failed {
                reason: IndexOperationFailureReason::Refused,
                detail: ProductText::from_static("compiler rejected declaration"),
                compiler_failure: Some(compiler_failure.clone()),
            })),
            IndexOperationObservation::Known(status(IndexOperationState::Unresolved {
                reason: IndexOperationUnresolvedReason::RestartedDuringPublication,
                detail: ProductText::new("bounded unresolved detail").expect("unresolved detail"),
            })),
        ];

        for observation in observations {
            let expected = serde_json::to_value(&observation).expect("exact observation encoding");
            for reply in [
                SurfaceReply::IndexOperationStarted(observation.clone()),
                SurfaceReply::IndexOperationStatus(observation.clone()),
            ] {
                let view = product_view(&reply);
                assert_eq!(view.index_operation(), Some(&observation));
                if let IndexOperationObservation::Known(status) = &observation
                    && let IndexOperationState::Failed {
                        compiler_failure: Some(_),
                        ..
                    } = &status.state
                {
                    let human = &view.records()[0].title;
                    assert!(human.contains(
                        "classes/comparator.d.ts: setup/toolchain_configuration_mismatch"
                    ));
                    assert!(human.contains("Set NUDOX_TSC"));
                    assert!(
                        !human.contains("source_identity"),
                        "digest JSON belongs only in the typed DTO"
                    );
                }
                let dto = crate::dto::ProductDto::new(&view);
                assert_eq!(dto.index_operation.as_ref(), Some(&observation));
                let encoded = serde_json::to_value(&dto).expect("full product DTO");
                assert_eq!(encoded["index_operation"], expected);
                let decoded: crate::dto::ProductDto =
                    serde_json::from_value(encoded.clone()).expect("typed observation roundtrip");
                assert_eq!(decoded.index_operation.as_ref(), Some(&observation));

                let mut older = encoded.clone();
                older
                    .as_object_mut()
                    .expect("product object")
                    .remove("index_operation");
                let decoded_older: crate::dto::ProductDto =
                    serde_json::from_value(older).expect("older DTO without operation field");
                assert_eq!(decoded_older.index_operation, None);

                let answer = crate::drive::Answer::Product(Box::new(view));
                for detail in [crate::Detail::Summary, crate::Detail::Full] {
                    let payload = crate::encode_answer(
                        &answer,
                        detail,
                        None,
                        crate::DEFAULT_RESPONSE_BUDGET_BYTES,
                    )
                    .expect("bounded index-operation projection");
                    let value: serde_json::Value =
                        serde_json::from_slice(&payload.bytes).expect("typed answer JSON");
                    assert_eq!(value["index_operation"], expected);
                }
            }
        }
    }

    #[test]
    fn durable_index_operation_view_exposes_structural_capture_and_pending_semantics() {
        use backend_library::{
            CompileExecutionIntent, IndexOperationKey, IndexOperationObservation,
            IndexOperationSemanticProfileState, IndexOperationSourceCaptureReceipt,
            IndexOperationSourceProfile, IndexOperationState, IndexOperationStatus,
            SemanticLanguageProfile,
        };

        let key = IndexOperationKey::from_bytes([0x29; 32]).expect("operation key");
        let package = PackageReference::parse("/workspace/project").expect("local package");
        let receipt = IndexOperationSourceCaptureReceipt::from_checked_parts(
            key,
            [0x31; 32],
            [0x32; 32],
            11,
            vec![IndexOperationSourceProfile {
                profile: SemanticLanguageProfile::from_name("rust").expect("Rust profile"),
                source_version: [0x33; 32],
                input_digest: [0x34; 32],
                observation_sequence: 12,
                source_count: 7,
                state: IndexOperationSemanticProfileState::Pending { prior: None },
            }]
            .into_boxed_slice(),
        )
        .expect("checked source receipt");
        let observation = IndexOperationObservation::Known(
            IndexOperationStatus::new(
                key,
                package,
                CompileExecutionIntent::Interactive,
                IndexOperationState::Accepted,
            )
            .with_source_capture(Some(receipt.clone())),
        );
        let view = product_view(&SurfaceReply::IndexOperationStatus(observation.clone()));
        assert_eq!(view.index_operation(), Some(&observation));
        let tags = view.records()[0].tags();
        assert!(
            tags.iter()
                .any(|tag| tag.contains(&lower_hex(receipt.workspace_root())))
        );
        assert!(
            tags.iter()
                .any(|tag| tag.contains("pending; no prior generation"))
        );
        assert!(tags.iter().any(|tag| tag.contains("source version")));
    }

    fn published_history_proof(
        commit: [u8; 32],
    ) -> backend_library::SemanticHistoryPublicationProof {
        let mut proof = backend_library::SemanticHistoryPublicationProof {
            selection: backend_library::SemanticHistorySelectionStamp {
                namespace: [0x12; 16],
                profile: SemanticLanguageProfile::from_name("rust").expect("Rust profile"),
                source_coordinate: [0x13; 32],
                selection_revision: 17,
                selected_root: [0x14; 32],
                closure_id: [0x15; 32],
                catalog_root: [0x16; 32],
            },
            target_package: "test-package".to_owned(),
            target_coordinate: "test-coordinate".to_owned(),
            package_identity: [0; 32],
            images: vec![backend_library::SemanticHistoryImagePublicationProof {
                image: backend_library::SemanticHistoryImageIdentity {
                    artifact_ordinal: 0,
                    semantic_generation: [0x17; 32],
                    manifest_root: [0x18; 32],
                    image_identity: [0x19; 32],
                },
                history_commit: commit,
                parent_commits: Box::new([]),
            }]
            .into_boxed_slice(),
            reference_tip: commit,
            reachable_commit: commit,
            input_replay_status: backend_library::SemanticHistoryInputReplayStatus::Unproven,
        };
        proof.package_identity = proof.recompute_package_identity();
        proof
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
                    proof: published_history_proof([0x41; 32]),
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
            let record = semantic_version(
                status.clone(),
                selected,
                false,
                SemanticVersionFreshness::Unverified,
            );
            let view = semantic_versions_view(record.clone());
            let row = view.records().first().expect("semantic generation row");
            assert_eq!(
                row.history_status(),
                Some(&ProductSemanticHistoryStatus::from(&status))
            );
            assert!(row.tags().iter().any(|tag| tag == label));
            let dto = crate::dto::ProductDto::new(&view);
            assert_eq!(
                dto.records[0].history_status,
                Some(ProductSemanticHistoryStatus::from(&status))
            );
            assert_eq!(
                dto.semantic_data,
                Some(ProductSemanticData::Versions(Box::new([record])))
            );
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
        assert_eq!(
            row.history_status(),
            Some(&ProductSemanticHistoryStatus::from(&status))
        );
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
            proof: published_history_proof([0x7c; 32]),
        };
        let view = semantic_versions_view(semantic_version(
            status.clone(),
            true,
            false,
            SemanticVersionFreshness::Unverified,
        ));
        let row = view.records().first().expect("semantic generation row");
        assert_eq!(
            row.history_status(),
            Some(&ProductSemanticHistoryStatus::from(&status))
        );
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
        let details = semantic_history_details(&ProductSemanticHistoryStatus::from(&status));
        assert!(details.contains(&format!("verified tip: {}", "7c".repeat(32))));
        assert!(details.contains("Compiler input replay remains unproven"));

        let dto = crate::dto::ProductDto::new(&view);
        assert_eq!(
            dto.records[0].history_status,
            Some(ProductSemanticHistoryStatus::from(&status))
        );
        let value = serde_json::to_value(&dto).expect("semantic product DTO");
        assert_eq!(value["records"][0]["history_status"]["state"], "published");
        assert_eq!(
            value["records"][0]["history_status"]["reference"],
            reference
        );
        let decoded: crate::dto::ProductDto =
            serde_json::from_value(value.clone()).expect("typed status DTO round trip");
        assert_eq!(
            decoded.records[0].history_status,
            Some(ProductSemanticHistoryStatus::from(&status))
        );
        assert!(
            value["records"][0]["history_status"]["proof"]
                .get("images")
                .is_none()
        );
        assert_eq!(
            value["semantic_data"]["value"][0]["history_status"],
            serde_json::to_value(&status).expect("complete immutable history proof")
        );

        // A previous row DTO contains the full proof. It still decodes to the
        // readable projection, with all exact operand fields in the facet.
        let mut older = value;
        older["records"][0]["history_status"] =
            serde_json::to_value(&status).expect("previous complete row status");
        let decoded: crate::dto::ProductDto =
            serde_json::from_value(older).expect("previous full proof row DTO");
        assert_eq!(
            decoded.records[0].history_status,
            Some(ProductSemanticHistoryStatus::from(&status))
        );
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
        assert_eq!(
            row.history_status(),
            Some(&ProductSemanticHistoryStatus::from(&status))
        );
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
                python_metadata: None,
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
        let mut unsafe_path = detail.clone();
        unsafe_path.manifest.path =
            backend_library::ProductText::new("nested\n\u{1b}[31m/setup.py".to_owned())
                .expect("bounded raw source path");
        unsafe_path.manifest.name = ForgeFact::Recorded(
            backend_library::ProductText::new("unsafe\n\u{1b}[31m/name").expect("declared name"),
        );
        unsafe_path.metadata.description = ForgeFact::Recorded(
            backend_library::ProductText::new("description\n\u{1b}[31m injected")
                .expect("description"),
        );
        unsafe_path.admit().expect("relative source path");
        let unsafe_view = product_view(&SurfaceReply::IndexSearchWithDiscovery(Box::new([
            RegistrySearchHit::ForgeSourcePin(unsafe_path.clone()),
        ])));
        assert!(
            unsafe_view.records()[0]
                .tags()
                .iter()
                .all(|tag| !tag.chars().any(char::is_control))
        );
        assert!(
            !unsafe_view.records()[0]
                .title()
                .chars()
                .any(char::is_control)
        );
        assert!(!crate::text::product(&unsafe_view, crate::Theme::plain()).contains('\u{1b}'));
        assert!(!crate::markdown::product(&unsafe_view).contains("nested\n"));
        let acquired = ForgePackageRecord {
            coordinate: backend_library::ProductText::new(source.canonical()).expect("coordinate"),
            provider: backend_library::ProductText::new("github").expect("provider"),
            owner: backend_library::ProductText::new("owner").expect("owner"),
            repository: backend_library::ProductText::new("repository").expect("repository"),
            revision: backend_library::ProductText::new("main").expect("revision"),
            subdir: None,
            commit: unavailable(),
            tree: unavailable(),
            metadata: unsafe_path.metadata.clone(),
            manifests: Box::new([backend_library::ForgeManifestRecord {
                path: unsafe_path.manifest.path.clone(),
                ecosystem: backend_library::ProductText::new("pypi").expect("ecosystem"),
                name: None,
                version: None,
                dependencies: backend_library::DependencyFacts::Unknown(
                    backend_library::ProductText::new("not parsed").expect("reason"),
                ),
                python_metadata: None,
            }]),
            source: unavailable(),
        };
        let acquired_view = ProductView::rows("forge", vec![forge_row(&acquired)]);
        assert!(
            acquired_view.records()[0]
                .tags()
                .iter()
                .all(|tag| !tag.chars().any(char::is_control))
        );
        assert!(!crate::text::product(&acquired_view, crate::Theme::plain()).contains('\u{1b}'));
        assert!(!crate::markdown::product(&acquired_view).contains("nested\n"));
        let unsafe_json =
            serde_json::to_value(crate::dto::ProductDto::new(&unsafe_view)).expect("typed source");
        assert_eq!(
            unsafe_json["records"][0]["forge_package_detail"]["manifest"]["path"],
            unsafe_path.manifest.path.as_str()
        );

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

    #[test]
    fn package_metadata_closed_states_survive_shared_json_presentation() {
        use backend_library::{PackageCoordinate, RegistryPackageDiscoveryObservation};
        let package = PackageCoordinate::parse("pkg:pypi/requests@0.0.0").expect("package");
        for observation in [
            RegistryPackageDiscoveryObservation::Missing {
                source: [1; 32],
                proof: [2; 32],
                observed_at_millis: 100,
            },
            RegistryPackageDiscoveryObservation::Unavailable {
                source: Some([1; 32]),
                reason: backend_library::ProductText::new("HTTP 503".to_owned()).expect("reason"),
            },
        ] {
            let reply = SurfaceReply::PackageDiscovery {
                package: package.clone(),
                observation: observation.clone(),
            };
            assert_eq!(reply.id(), backend_library::CommandId::Package);
            reply
                .admit(backend_library::CommandId::Package)
                .expect("admitted metadata reply");
            let view = product_view(&reply);
            let dto = crate::dto::ProductDto::new(&view);
            let encoded = serde_json::to_value(&dto).expect("shared JSON");
            assert_eq!(encoded["package_discovery"]["package"], package.as_str());
            let decoded: crate::dto::ProductDto =
                serde_json::from_value(encoded).expect("closed typed presentation roundtrip");
            assert_eq!(
                decoded
                    .package_discovery
                    .expect("metadata evidence")
                    .observation,
                observation
            );
            assert!(
                view.records().is_empty(),
                "negative evidence is never an acquired row"
            );
        }
    }
}
