use super::reply_admission::{CapabilityAdmission, CoverageAdmission, VerifierAdmission};
use super::reply_content::{
    DocumentWire, FragmentWire, OutlineWire, SourceAvailabilityWire, SourceExcerptWire,
    document_from_wire_with_admission, document_to_wire, fragment_from_wire_with_capability,
    fragment_to_wire, outline_from_wire_with_admission, outline_to_wire,
    source_availability_from_wire, source_availability_to_wire, source_excerpt_from_wire,
    source_excerpt_not_captured, source_excerpt_to_wire,
};
use super::reply_failure::{
    CommandFailureWire, command_failure_from_wire, command_failure_to_wire,
};
use super::reply_graph_query::{
    GraphQueryPageWire, page_from_wire as graph_query_page_from_wire,
    page_to_wire as graph_query_page_to_wire,
};
use super::reply_page::{
    PageTerminalWire, ProjectionPageWire, page_from_wire as projection_page_from_wire,
};
use super::{
    CoverageWire, CursorWire, DTO_VERSION, EmptyWire, FreshnessWire, FrontierWire, HealthWire,
    ReplyDto, WireCertificate, WireSchema, coverage_from_wire, coverage_to_wire, cursor_from_wire,
    cursor_from_wire_with_capability, cursor_to_wire, freshness_from_wire, freshness_to_wire,
    frontier_from_wire, frontier_to_wire, inventory_from_wire, inventory_to_wire,
    progress_from_wire, progress_to_wire,
};
use crate::canonical::{
    BranchSchema, LogSchema, ObjectSchema, PackageSchema, SymbolSchema, ViewRecipeSchema,
    ViewVersionSchema, encode_id,
};
use crate::{
    Basis, CommandReply, CoverageCapability, DeclarationKind, GraphRelation, HealthReport,
    PageTerminal, RevisionReceipt, Row, RowId, RowIdentityPreimage, RowState, SemanticLinkKind,
    ViewRoot, ViewSnapshot,
};
use backend_version::ProducerObservationVerifier;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReplyEnvelope {
    pub(crate) version: u16,
    pub(crate) request_id: u64,
    pub(crate) reply: ReplyWire,
    pub(crate) certificate: Option<WireCertificate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) health_cursor: Option<CursorWire>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", deny_unknown_fields)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReplyWire {
    Packages(SnapshotWire),
    ProjectionPage(ProjectionPageWire),
    Added(DeltaWire),
    Removed(DeltaWire),
    Document(DocumentWire),
    Page(DocumentWire),
    Outline(OutlineWire),
    Names(SnapshotWire),
    Resolved(RowsWire),
    Search(SnapshotWire),
    Graph(SnapshotWire),
    GraphQueryPage(GraphQueryPageWire),
    Surface(crate::SurfaceReply),
    Health(ViewRootWire),
    Readiness(HealthWire),
    Revision(RevisionWire),
    Error(ErrorWire),
    Failed(CommandFailureWire),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RevisionWire {
    root: String,
    source: String,
    cursor: CursorWire,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeltaWire {
    pub(crate) id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ErrorWire {
    message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RowsWire {
    rows: Vec<RowWire>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SnapshotWire {
    pub(crate) root: ViewRootWire,
    pub(crate) freshness: FreshnessWire,
    pub(crate) next: Option<CursorWire>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) graph_relations: Option<GraphRelationsWire>,
}

/// Versioned typed-edge sidecar for graph snapshots. Keeping this outside the
/// committed view root means older row/view certificates remain valid while a
/// receiver can reject an edge vocabulary it does not understand explicitly.
pub(crate) const GRAPH_RELATIONS_SCHEMA: u16 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GraphRelationsWire {
    pub(crate) schema: u16,
    pub(crate) relations: Vec<GraphRelationWire>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GraphRelationWire {
    pub(crate) from: RowIdWire,
    pub(crate) to: RowIdWire,
    pub(crate) relation: SemanticLinkKind,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ViewRootWire {
    recipe: String,
    version: String,
    root: String,
    basis: BasisWire,
    frontier: FrontierWire,
    rows: Vec<RowWire>,
    coverage: Vec<CoverageWire>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BasisWire {
    root: String,
    object: String,
    branch: String,
    log: String,
    schema: u16,
}

pub(crate) fn basis_object(value: &BasisWire) -> &str {
    &value.object
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RowWire {
    id: RowIdWire,
    basis: BasisWire,
    state: RowStateWire,
    label: String,
    score: Option<u32>,
    package: Option<String>,
    parent: Option<String>,
    document: Vec<FragmentWire>,
    signature: Option<String>,
    kind: Option<String>,
    source: SourceAvailabilityWire,
    #[serde(default = "source_excerpt_not_captured")]
    excerpt: SourceExcerptWire,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RowIdWire {
    pub(crate) kind: String,
    pub(crate) id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", deny_unknown_fields)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RowStateWire {
    Ready(EmptyWire),
    Loading(EmptyWire),
    Failed(EmptyWire),
}

impl Serialize for ReplyDto {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if matches!(&self.reply, CommandReply::Health(_)) {
            if self.certificate().is_none() || self.health_cursor().is_none() {
                return Err(serde::ser::Error::custom(
                    "health reply requires an exact owner cursor and producer certificate",
                ));
            }
            if self
                .health_cursor()
                .is_some_and(|cursor| cursor.query_offset() != 0)
            {
                return Err(serde::ser::Error::custom(
                    "health reply cursor carries a query offset",
                ));
            }
        } else if self.health_cursor().is_some() {
            return Err(serde::ser::Error::custom(
                "health cursor attached to a non-health reply",
            ));
        }
        let reply = reply_to_wire(&self.reply);
        ReplyEnvelope {
            version: DTO_VERSION,
            request_id: self.request_id,
            reply,
            certificate: self.certificate().cloned(),
            health_cursor: matches!(&self.reply, CommandReply::Health(_))
                .then_some(self.health_cursor())
                .flatten()
                .map(cursor_to_wire),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ReplyDto {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let envelope = ReplyEnvelope::deserialize(deserializer)?;
        if envelope.version != DTO_VERSION {
            return Err(serde::de::Error::custom("unsupported reply DTO version"));
        }
        let empty_certificate = WireCertificate::new();
        let certificate = envelope.certificate.as_ref().unwrap_or(&empty_certificate);
        let reply =
            reply_from_wire(envelope.reply, certificate, None).map_err(serde::de::Error::custom)?;
        let health_cursor = envelope
            .health_cursor
            .as_ref()
            .map(|cursor| cursor_from_wire(cursor, certificate))
            .transpose()
            .map_err(serde::de::Error::custom)?;
        if health_cursor.is_some() && !matches!(&reply, CommandReply::Health(_)) {
            return Err(serde::de::Error::custom(
                "health cursor attached to a non-health reply",
            ));
        }
        if matches!(&reply, CommandReply::Health(_)) && health_cursor.is_none() {
            return Err(serde::de::Error::custom(
                "health reply omitted its exact owner cursor",
            ));
        }
        if let Some(cursor) = health_cursor
            && envelope.certificate.is_some()
        {
            certificate
                .cursor_claim(cursor)
                .map_err(serde::de::Error::custom)?;
        }
        if let CommandReply::Health(root) = &reply {
            let cursor = health_cursor.ok_or_else(|| {
                serde::de::Error::custom("health reply omitted its exact owner cursor")
            })?;
            crate::CompleteViewProjection::admit(root.clone(), cursor).map_err(|error| {
                serde::de::Error::custom(format!(
                    "health reply is not a complete projection: {error:?}"
                ))
            })?;
        }
        let mut dto = Self::new(envelope.request_id, reply);
        if let Some(certificate) = envelope.certificate {
            dto = dto.with_certificate(certificate);
        }
        if let Some(cursor) = health_cursor {
            dto = dto.with_health_cursor(cursor);
        }
        Ok(dto)
    }
}

pub(crate) fn reply_to_wire(reply: &CommandReply) -> ReplyWire {
    match reply {
        CommandReply::Packages(snapshot) => ReplyWire::Packages(snapshot_to_wire(snapshot)),
        CommandReply::ProjectionPage(page) => ReplyWire::ProjectionPage(ProjectionPageWire {
            snapshot: snapshot_to_wire(&page.snapshot),
            terminal: match page.terminal {
                PageTerminal::Complete => PageTerminalWire::Complete(EmptyWire {}),
                PageTerminal::More(continuation) => {
                    PageTerminalWire::More(cursor_to_wire(continuation.cursor()))
                }
                PageTerminal::Cancelled => PageTerminalWire::Cancelled(EmptyWire {}),
            },
        }),
        CommandReply::Added(id) => ReplyWire::Added(DeltaWire {
            id: encode_id(id.as_bytes()),
        }),
        CommandReply::Removed(id) => ReplyWire::Removed(DeltaWire {
            id: encode_id(id.as_bytes()),
        }),
        CommandReply::Document(document) => ReplyWire::Document(document_to_wire(document)),
        CommandReply::Page(document) => ReplyWire::Page(document_to_wire(document)),
        CommandReply::Outline(outline) => ReplyWire::Outline(outline_to_wire(outline)),
        CommandReply::Names(snapshot) => ReplyWire::Names(snapshot_to_wire(snapshot)),
        CommandReply::Resolved(rows) => ReplyWire::Resolved(RowsWire {
            rows: rows.iter().map(row_to_wire).collect(),
        }),
        CommandReply::Search(snapshot) => ReplyWire::Search(snapshot_to_wire(snapshot)),
        CommandReply::Graph(snapshot) => ReplyWire::Graph(snapshot_to_wire(snapshot)),
        CommandReply::GraphQueryPage(page) => {
            ReplyWire::GraphQueryPage(graph_query_page_to_wire(page))
        }
        CommandReply::Surface(reply) => ReplyWire::Surface(reply.clone()),
        CommandReply::Health(root) => ReplyWire::Health(view_root_to_wire(root)),
        CommandReply::Readiness(report) => {
            let revision = report.revision();
            ReplyWire::Readiness(HealthWire {
                root: encode_id(revision.root().as_bytes()),
                source: encode_id(revision.source().as_bytes()),
                cursor: cursor_to_wire(revision.cursor()),
                basis: basis_to_wire(report.basis()),
                coverage: report
                    .coverage()
                    .iter()
                    .copied()
                    .map(coverage_to_wire)
                    .collect(),
                row_count: report.row_count(),
                capabilities: inventory_to_wire(report.capabilities()),
                progress: progress_to_wire(report.progress()),
            })
        }
        CommandReply::Revision(receipt) => ReplyWire::Revision(RevisionWire {
            root: encode_id(receipt.root().as_bytes()),
            source: encode_id(receipt.source().as_bytes()),
            cursor: cursor_to_wire(receipt.cursor()),
        }),
        CommandReply::Error(message) => ReplyWire::Error(ErrorWire {
            message: message.clone(),
        }),
        CommandReply::Failed(failure) => ReplyWire::Failed(command_failure_to_wire(failure)),
    }
}

pub(crate) fn reply_from_wire(
    reply: ReplyWire,
    certificate: &WireCertificate,
    capability: Option<CoverageCapability>,
) -> Result<CommandReply, String> {
    reply_from_wire_with_admission(reply, certificate, &CapabilityAdmission(capability))
}

pub(crate) fn reply_from_wire_with_verifier<V: ProducerObservationVerifier>(
    reply: ReplyWire,
    certificate: &WireCertificate,
    verifier: &V,
) -> Result<CommandReply, String> {
    reply_from_wire_with_admission(reply, certificate, &VerifierAdmission(verifier))
}

fn reply_from_wire_with_admission<A: CoverageAdmission>(
    reply: ReplyWire,
    certificate: &WireCertificate,
    admission: &A,
) -> Result<CommandReply, String> {
    Ok(match reply {
        ReplyWire::Packages(value) => CommandReply::Packages(snapshot_from_wire_with_admission(
            value,
            certificate,
            admission,
        )?),
        ReplyWire::ProjectionPage(value) => {
            CommandReply::ProjectionPage(projection_page_from_wire(value, certificate, admission)?)
        }
        ReplyWire::Added(value) => CommandReply::Added(certificate.intent_value(&value.id)?),
        ReplyWire::Removed(value) => CommandReply::Removed(certificate.intent_value(&value.id)?),
        ReplyWire::Document(value) => CommandReply::Document(document_from_wire_with_admission(
            value,
            certificate,
            admission,
        )?),
        ReplyWire::Page(value) => CommandReply::Page(document_from_wire_with_admission(
            value,
            certificate,
            admission,
        )?),
        ReplyWire::Outline(value) => CommandReply::Outline(outline_from_wire_with_admission(
            value,
            certificate,
            admission,
        )?),
        ReplyWire::Names(value) => CommandReply::Names(snapshot_from_wire_with_admission(
            value,
            certificate,
            admission,
        )?),
        ReplyWire::Resolved(value) => CommandReply::Resolved(
            value
                .rows
                .into_iter()
                .map(|row| row_from_wire_with_admission(row, certificate, admission))
                .collect::<Result<Vec<_>, _>>()?
                .into_boxed_slice(),
        ),
        ReplyWire::Search(value) => CommandReply::Search(snapshot_from_wire_with_admission(
            value,
            certificate,
            admission,
        )?),
        ReplyWire::Graph(value) => CommandReply::Graph(snapshot_from_wire_with_admission(
            value,
            certificate,
            admission,
        )?),
        ReplyWire::GraphQueryPage(value) => {
            CommandReply::GraphQueryPage(graph_query_page_from_wire(value, certificate, admission)?)
        }
        ReplyWire::Surface(value) => CommandReply::Surface(value),
        ReplyWire::Health(value) => CommandReply::Health(view_root_from_wire_with_admission(
            &value,
            certificate,
            admission,
            true,
        )?),
        ReplyWire::Readiness(value) => {
            CommandReply::Readiness(readiness_from_wire(value, certificate, admission)?)
        }
        ReplyWire::Revision(value) => {
            CommandReply::Revision(revision_from_wire(&value, certificate, admission)?)
        }
        ReplyWire::Error(value) => CommandReply::Error(value.message),
        ReplyWire::Failed(value) => CommandReply::Failed(command_failure_from_wire(value)?),
    })
}

fn readiness_from_wire<A: CoverageAdmission>(
    value: HealthWire,
    certificate: &WireCertificate,
    admission: &A,
) -> Result<HealthReport, String> {
    let Some(capability) = admission.admit(certificate, &value.source)? else {
        return Err("readiness reply requires admitted producer coverage".to_owned());
    };
    let source = certificate.version_value::<ObjectSchema>(WireSchema::Object, &value.source)?;
    let root = certificate
        .producer_root_value::<crate::ViewRelation>(
            WireSchema::ViewRelation,
            &value.root,
            &capability,
        )
        .or_else(|_| {
            certificate.root_value::<crate::ViewRelation>(WireSchema::ViewRelation, &value.root)
        })?;
    let basis = basis_from_wire_with_capability(&value.basis, certificate, &capability)?;
    let cursor = cursor_from_wire_with_capability(&value.cursor, certificate, &capability)?;
    if cursor.root() != root || cursor.query_offset() != 0 || source != basis.object {
        return Err("readiness identities do not describe one owner state".to_owned());
    }
    let coverage = value
        .coverage
        .into_iter()
        .map(coverage_from_wire)
        .collect::<Result<Vec<_>, _>>()?;
    let progress = progress_from_wire(value.progress)?;
    Ok(HealthReport::from_admitted_parts(
        RevisionReceipt::new(root, cursor, source),
        basis,
        coverage.into_boxed_slice(),
        value.row_count,
        inventory_from_wire(value.capabilities)?,
    )
    .with_progress(progress))
}

fn revision_from_wire<A: CoverageAdmission>(
    value: &RevisionWire,
    certificate: &WireCertificate,
    admission: &A,
) -> Result<RevisionReceipt, String> {
    let Some(capability) = admission.admit(certificate, &value.source)? else {
        return Err("revision reply requires admitted producer coverage".to_owned());
    };
    let source = certificate.version_value::<ObjectSchema>(WireSchema::Object, &value.source)?;
    let root = certificate.producer_root_value::<crate::ViewRelation>(
        WireSchema::ViewRelation,
        &value.root,
        &capability,
    )?;
    let cursor = cursor_from_wire_with_capability(&value.cursor, certificate, &capability)?;
    if cursor.root() != root || cursor.query_offset() != 0 {
        return Err("revision cursor does not match its visible root".to_owned());
    }
    Ok(RevisionReceipt::new(root, cursor, source))
}

pub(crate) fn snapshot_to_wire(snapshot: &ViewSnapshot) -> SnapshotWire {
    SnapshotWire {
        root: view_root_to_wire(&snapshot.root),
        freshness: freshness_to_wire(snapshot.freshness),
        next: snapshot.next.map(cursor_to_wire),
        graph_relations: snapshot.graph_relations.as_deref().map(|relations| {
            GraphRelationsWire {
                schema: GRAPH_RELATIONS_SCHEMA,
                relations: relations
                    .iter()
                    .map(|relation| GraphRelationWire {
                        from: row_id_to_wire(relation.from),
                        to: row_id_to_wire(relation.to),
                        relation: relation.relation,
                    })
                    .collect(),
            }
        }),
    }
}

pub(crate) fn snapshot_from_wire(
    value: SnapshotWire,
    certificate: &WireCertificate,
    capability: Option<CoverageCapability>,
) -> Result<ViewSnapshot, String> {
    snapshot_from_wire_with_admission(value, certificate, &CapabilityAdmission(capability))
}

pub(crate) fn snapshot_from_wire_with_admission<A: CoverageAdmission>(
    value: SnapshotWire,
    certificate: &WireCertificate,
    admission: &A,
) -> Result<ViewSnapshot, String> {
    let root = view_root_from_wire_with_admission(&value.root, certificate, admission, false)?;
    let freshness = freshness_from_wire(value.freshness, certificate)?;
    let next = value
        .next
        .as_ref()
        .map(|cursor| cursor_from_wire(cursor, certificate))
        .transpose()?;
    match next {
        Some(cursor)
            if cursor.recipe() != root.recipe
                || cursor.version() != root.version
                || cursor.root() != root.root
                || cursor.branch() != root.frontier.branch
                || cursor.log() != root.frontier.log
                || cursor.schema() != root.frontier.schema =>
        {
            return Err("snapshot continuation cursor does not match its view root".to_owned());
        }
        _ => {}
    }
    Ok(ViewSnapshot {
        graph_relations: graph_relations_from_wire(value.graph_relations, &root, certificate)?,
        root,
        freshness,
        next,
    })
}

fn graph_relations_from_wire(
    value: Option<GraphRelationsWire>,
    root: &ViewRoot,
    certificate: &WireCertificate,
) -> Result<Option<Box<[GraphRelation]>>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.schema != GRAPH_RELATIONS_SCHEMA {
        return Err("unsupported graph relation sidecar schema".to_owned());
    }
    let mut relations = Vec::with_capacity(value.relations.len());
    for relation in value.relations {
        let from = row_id_from_wire(&relation.from, certificate)?;
        let to = row_id_from_wire(&relation.to, certificate)?;
        if root.row(from).is_none() || root.row(to).is_none() {
            return Err("graph relation sidecar names an absent row".to_owned());
        }
        relations.push(GraphRelation::new(from, to, relation.relation));
    }
    relations.sort_unstable();
    if relations.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err("graph relation sidecar contains a duplicate edge".to_owned());
    }
    Ok(Some(relations.into_boxed_slice()))
}

pub(crate) fn view_root_to_wire(root: &ViewRoot) -> ViewRootWire {
    ViewRootWire {
        recipe: encode_id(root.recipe.as_bytes()),
        version: encode_id(root.version.as_bytes()),
        root: encode_id(root.root.as_bytes()),
        basis: basis_to_wire(root.basis),
        frontier: frontier_to_wire(root.frontier),
        rows: root.rows().iter().map(row_to_wire).collect(),
        coverage: root
            .coverage
            .iter()
            .copied()
            .map(coverage_to_wire)
            .collect(),
    }
}

pub(crate) fn view_root_from_wire(
    value: &ViewRootWire,
    certificate: &WireCertificate,
    capability: Option<CoverageCapability>,
) -> Result<ViewRoot, String> {
    view_root_from_wire_with_admission(value, certificate, &CapabilityAdmission(capability), false)
}

fn view_root_from_wire_with_admission<A: CoverageAdmission>(
    value: &ViewRootWire,
    certificate: &WireCertificate,
    admission: &A,
    allow_producer_root: bool,
) -> Result<ViewRoot, String> {
    let recipe = certificate
        .key_bytes::<ViewRecipeSchema>(WireSchema::ViewRecipe, &value.recipe)
        .map_err(|error| format!("view recipe: {error}"))?;
    let version = certificate
        .version_value::<ViewVersionSchema>(WireSchema::ViewVersion, &value.version)
        .map_err(|error| format!("view version: {error}"))?;
    let coverage = value
        .coverage
        .iter()
        .cloned()
        .map(coverage_from_wire)
        .collect::<Result<Vec<_>, _>>()?;
    let capability = if coverage.iter().any(|value| value.is_complete()) {
        Some(
            admission
                .admit(certificate, &value.basis.object)
                .map_err(|error| format!("view coverage: {error}"))?
                .ok_or_else(|| {
                    "complete view requires an externally admitted coverage capability".to_owned()
                })?,
        )
    } else {
        // A verifier is allowed to supply a capability only for complete
        // coverage. Avoid asking it to admit a claim for an incomplete view.
        None
    };
    let claimed_root = match capability.as_ref() {
        Some(capability) if allow_producer_root => certificate
            .root_value::<crate::ViewRelation>(WireSchema::ViewRelation, &value.root)
            .or_else(|_| {
                certificate.producer_root_value::<crate::ViewRelation>(
                    WireSchema::ViewRelation,
                    &value.root,
                    capability,
                )
            })
            .map_err(|error| format!("view result root: {error}"))?,
        _ => certificate
            .root_value::<crate::ViewRelation>(WireSchema::ViewRelation, &value.root)
            .map_err(|error| format!("view result root: {error}"))?,
    };
    let basis = match capability.as_ref() {
        Some(capability) => basis_from_wire_with_capability(&value.basis, certificate, capability)
            .map_err(|error| format!("view basis: {error}"))?,
        None => basis_from_wire(&value.basis, certificate)
            .map_err(|error| format!("view basis: {error}"))?,
    };
    let frontier = match capability.as_ref() {
        Some(capability) => {
            super::frontier_from_wire_with_capability(&value.frontier, certificate, capability)
                .map_err(|error| format!("view frontier: {error}"))?
        }
        None => frontier_from_wire(&value.frontier, certificate)
            .map_err(|error| format!("view frontier: {error}"))?,
    };
    let rows = value
        .rows
        .iter()
        .cloned()
        .map(|row| row_from_wire_with_capability(row, certificate, capability.as_ref()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("view row: {error}"))?;
    let root = if let Some(capability) = capability {
        ViewRoot::new_checked(recipe, basis, frontier, rows, coverage, capability)
            .map_err(|error| format!("invalid certified complete view root: {error:?}"))?
    } else {
        ViewRoot::new_incomplete(recipe, basis, frontier, rows, coverage)
            .map_err(|error| format!("invalid certified view root: {error:?}"))?
    };
    if root.root() != claimed_root {
        return Err("view root certificate does not match canonical rows".to_owned());
    }
    if root.version() != version {
        return Err("view version certificate does not match canonical view".to_owned());
    }
    Ok(root)
}

pub(crate) fn basis_to_wire(basis: Basis) -> BasisWire {
    BasisWire {
        root: encode_id(basis.root.as_bytes()),
        object: encode_id(basis.object.as_bytes()),
        branch: encode_id(basis.branch.as_bytes()),
        log: encode_id(basis.log.as_bytes()),
        schema: basis.schema,
    }
}

pub(crate) fn basis_from_wire(
    value: &BasisWire,
    certificate: &WireCertificate,
) -> Result<Basis, String> {
    Ok(Basis::with_context(
        certificate.root_value::<crate::ViewRelation>(WireSchema::ViewRelation, &value.root)?,
        certificate.version_value::<ObjectSchema>(WireSchema::Object, &value.object)?,
        certificate.key_value::<BranchSchema>(WireSchema::Branch, &value.branch)?,
        certificate.key_value::<LogSchema>(WireSchema::Log, &value.log)?,
        value.schema,
    ))
}

pub(crate) fn basis_from_wire_with_capability(
    value: &BasisWire,
    certificate: &WireCertificate,
    capability: &CoverageCapability,
) -> Result<Basis, String> {
    let root = certificate
        .producer_root_value::<crate::ViewRelation>(
            WireSchema::ViewRelation,
            &value.root,
            capability,
        )
        .or_else(|_| {
            certificate.root_value::<crate::ViewRelation>(WireSchema::ViewRelation, &value.root)
        })?;
    Ok(Basis::with_context(
        root,
        certificate.version_value::<ObjectSchema>(WireSchema::Object, &value.object)?,
        certificate.key_value::<BranchSchema>(WireSchema::Branch, &value.branch)?,
        certificate.key_value::<LogSchema>(WireSchema::Log, &value.log)?,
        value.schema,
    ))
}

pub(crate) fn row_to_wire(row: &Row) -> RowWire {
    RowWire {
        id: row_id_to_wire(row.id),
        basis: basis_to_wire(row.basis),
        state: row_state_to_wire(row.state),
        label: row.label.clone(),
        score: row.score,
        package: row.package.map(|value| encode_id(value.as_bytes())),
        parent: row.parent.map(|value| encode_id(value.as_bytes())),
        document: row.document.iter().map(fragment_to_wire).collect(),
        signature: row.signature.clone(),
        kind: row.kind.map(|kind| kind.name().to_owned()),
        source: source_availability_to_wire(&row.source),
        excerpt: source_excerpt_to_wire(&row.excerpt),
    }
}

pub(crate) fn row_from_wire(value: RowWire, certificate: &WireCertificate) -> Result<Row, String> {
    row_from_wire_with_capability(value, certificate, None)
}

fn row_from_wire_with_capability(
    value: RowWire,
    certificate: &WireCertificate,
    capability: Option<&CoverageCapability>,
) -> Result<Row, String> {
    let document = value
        .document
        .into_iter()
        .map(|fragment| fragment_from_wire_with_capability(fragment, certificate, capability))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("snapshot row document: {error}"))?
        .into_boxed_slice();
    let id = capability
        .map_or_else(
            || row_id_from_wire(&value.id, certificate),
            |capability| row_id_from_wire_with_capability(&value.id, certificate, capability),
        )
        .map_err(|error| format!("identity: {error}"))?;
    let identity_preimage = row_identity_preimage_from_wire(&value.id, certificate)?;
    let basis = match capability {
        Some(capability) => basis_from_wire_with_capability(&value.basis, certificate, capability),
        None => basis_from_wire(&value.basis, certificate),
    }
    .map_err(|error| format!("basis: {error}"))?;
    let package = value
        .package
        .as_deref()
        .map(|id| match capability {
            Some(capability) => certificate.row_identity_or_key_or_producer::<PackageSchema>(
                WireSchema::Package,
                id,
                capability,
            ),
            None => certificate.row_identity_or_key_value::<PackageSchema>(WireSchema::Package, id),
        })
        .transpose()
        .map_err(|error| format!("package: {error}"))?;
    let parent = value
        .parent
        .as_deref()
        .map(|id| match capability {
            Some(capability) => certificate.row_identity_or_key_or_producer::<SymbolSchema>(
                WireSchema::Symbol,
                id,
                capability,
            ),
            None => certificate.row_identity_or_key_value::<SymbolSchema>(WireSchema::Symbol, id),
        })
        .transpose()
        .map_err(|error| format!("parent: {error}"))?;
    Ok(Row {
        id,
        identity_preimage,
        basis,
        state: row_state_from_wire(&value.state),
        label: value.label,
        score: value.score,
        package,
        parent,
        document,
        signature: value.signature,
        kind: value.kind.map(|kind| DeclarationKind::from_name(&kind)),
        source: source_availability_from_wire(value.source)?,
        excerpt: source_excerpt_from_wire(value.excerpt)?,
    })
}

fn row_from_wire_with_admission<A: CoverageAdmission>(
    value: RowWire,
    certificate: &WireCertificate,
    admission: &A,
) -> Result<Row, String> {
    let capability = admission.admit(certificate, basis_object(&value.basis))?;
    row_from_wire_with_capability(value, certificate, capability.as_ref())
}

/// Decodes a row while pinning its source basis to a caller-owned checked
/// basis.  Compact journal events use this path so a row delta does not need
/// to repeat a canonical source relation root or its full source certificate.
/// Stable row/package/parent/link identities still require their individual
/// producer claims.
pub(crate) fn row_from_wire_against(
    value: RowWire,
    expected: Basis,
    certificate: &WireCertificate,
) -> Result<Row, String> {
    row_from_wire_against_admission(value, expected, certificate, None)
}

pub(crate) fn row_from_wire_against_with_capability(
    value: RowWire,
    expected: Basis,
    certificate: &WireCertificate,
    capability: &CoverageCapability,
) -> Result<Row, String> {
    row_from_wire_against_admission(value, expected, certificate, Some(capability))
}

fn row_from_wire_against_admission(
    value: RowWire,
    expected: Basis,
    certificate: &WireCertificate,
    capability: Option<&CoverageCapability>,
) -> Result<Row, String> {
    if value.basis != basis_to_wire(expected) {
        return Err("compact row basis does not match its checked source".to_owned());
    }
    let document = value
        .document
        .into_iter()
        .map(|fragment| fragment_from_wire_with_capability(fragment, certificate, capability))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("snapshot row document: {error}"))?
        .into_boxed_slice();
    let id = capability
        .map_or_else(
            || row_id_from_wire(&value.id, certificate),
            |capability| row_id_from_wire_with_capability(&value.id, certificate, capability),
        )
        .map_err(|error| format!("snapshot row identity: {error}"))?;
    let identity_preimage = row_identity_preimage_from_wire(&value.id, certificate)?;
    let package = value
        .package
        .as_deref()
        .map(|id| match capability {
            Some(capability) => certificate.row_identity_or_key_or_producer::<PackageSchema>(
                WireSchema::Package,
                id,
                capability,
            ),
            None => certificate.row_identity_or_key_value::<PackageSchema>(WireSchema::Package, id),
        })
        .transpose()
        .map_err(|error| format!("snapshot row package: {error}"))?;
    let parent = value
        .parent
        .as_deref()
        .map(|id| match capability {
            Some(capability) => certificate.row_identity_or_key_or_producer::<SymbolSchema>(
                WireSchema::Symbol,
                id,
                capability,
            ),
            None => certificate.row_identity_or_key_value::<SymbolSchema>(WireSchema::Symbol, id),
        })
        .transpose()
        .map_err(|error| format!("snapshot row parent: {error}"))?;
    Ok(Row {
        id,
        identity_preimage,
        basis: expected,
        state: row_state_from_wire(&value.state),
        label: value.label,
        score: value.score,
        package,
        parent,
        document,
        signature: value.signature,
        kind: value.kind.map(|kind| DeclarationKind::from_name(&kind)),
        source: source_availability_from_wire(value.source)?,
        excerpt: source_excerpt_from_wire(value.excerpt)?,
    })
}

fn row_id_from_wire_with_capability(
    value: &RowIdWire,
    certificate: &WireCertificate,
    capability: &CoverageCapability,
) -> Result<RowId, String> {
    match value.kind.as_str() {
        "package" => certificate
            .row_identity_or_key_or_producer::<PackageSchema>(
                WireSchema::Package,
                &value.id,
                capability,
            )
            .map(RowId::Package),
        "symbol" => certificate
            .row_identity_or_key_or_producer::<SymbolSchema>(
                WireSchema::Symbol,
                &value.id,
                capability,
            )
            .map(RowId::Symbol),
        "object" => certificate
            .version_value::<ObjectSchema>(WireSchema::Object, &value.id)
            .map(RowId::Object),
        _ => Err("unknown stable row identity kind".to_owned()),
    }
}

pub(crate) fn row_id_to_wire(id: RowId) -> RowIdWire {
    match id {
        RowId::Package(value) => RowIdWire {
            kind: "package".to_owned(),
            id: encode_id(value.as_bytes()),
        },
        RowId::Symbol(value) => RowIdWire {
            kind: "symbol".to_owned(),
            id: encode_id(value.as_bytes()),
        },
        RowId::Object(value) => RowIdWire {
            kind: "object".to_owned(),
            id: encode_id(value.as_bytes()),
        },
    }
}

pub(crate) fn row_id_from_wire(
    value: &RowIdWire,
    certificate: &WireCertificate,
) -> Result<RowId, String> {
    match value.kind.as_str() {
        "package" => Ok(RowId::Package(
            certificate
                .row_identity_or_key_value::<PackageSchema>(WireSchema::Package, &value.id)?,
        )),
        "symbol" => Ok(RowId::Symbol(
            certificate.row_identity_or_key_value::<SymbolSchema>(WireSchema::Symbol, &value.id)?,
        )),
        "object" => Ok(RowId::Object(
            certificate.version_value::<ObjectSchema>(WireSchema::Object, &value.id)?,
        )),
        _ => Err("unknown stable row identity kind".to_owned()),
    }
}

fn row_identity_preimage_from_wire(
    value: &RowIdWire,
    certificate: &WireCertificate,
) -> Result<Option<RowIdentityPreimage>, String> {
    let schema = match value.kind.as_str() {
        "package" => WireSchema::Package,
        "symbol" => WireSchema::Symbol,
        "object" => return Ok(None),
        _ => return Err("unknown stable row identity kind".to_owned()),
    };
    certificate
        .row_identity_preimage(schema, &value.id)
        .and_then(|preimage| {
            preimage
                .map(|value| RowIdentityPreimage::try_new(value).map_err(|error| error.to_string()))
                .transpose()
        })
}

pub(crate) fn row_state_to_wire(state: RowState) -> RowStateWire {
    match state {
        RowState::Ready => RowStateWire::Ready(EmptyWire {}),
        RowState::Loading => RowStateWire::Loading(EmptyWire {}),
        RowState::Failed => RowStateWire::Failed(EmptyWire {}),
    }
}

pub(crate) fn row_state_from_wire(state: &RowStateWire) -> RowState {
    match state {
        RowStateWire::Ready(_) => RowState::Ready,
        RowStateWire::Loading(_) => RowState::Loading,
        RowStateWire::Failed(_) => RowState::Failed,
    }
}
