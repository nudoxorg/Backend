use super::reply_admission::{CapabilityAdmission, CoverageAdmission, VerifierAdmission};
use super::{
    CursorWire, DTO_VERSION, EmptyWire, FrontierWire, ReplyDto, TextWire, WireCertificate,
    WireSchema, cursor_from_wire, cursor_from_wire_with_capability, cursor_to_wire,
    frontier_from_wire, frontier_to_wire,
};
use crate::canonical::{
    BranchSchema, LogSchema, ObjectSchema, PackageSchema, SymbolSchema, ViewRecipeSchema,
    ViewVersionSchema, encode_id,
};
use crate::{
    Basis, CommandReply, Coverage, CoverageCapability, DeclarationKind, Document, Fragment,
    Freshness, Lane, Outline, OutlineNode, Reason, RevisionReceipt, Row, RowId, RowState,
    SourceLocation, ViewRoot, ViewSnapshot,
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
    Added(DeltaWire),
    Removed(DeltaWire),
    Document(DocumentWire),
    Page(DocumentWire),
    Outline(OutlineWire),
    Names(SnapshotWire),
    Resolved(RowsWire),
    Search(SnapshotWire),
    Graph(SnapshotWire),
    Health(ViewRootWire),
    Revision(RevisionWire),
    Error(ErrorWire),
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
#[serde(tag = "kind", content = "data", deny_unknown_fields)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FreshnessWire {
    Current(EmptyWire),
    Stale(StaleWire),
    Unknown(EmptyWire),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StaleWire {
    observed: String,
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
    source: Option<SourceLocationWire>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceLocationWire {
    path: String,
    start_line: u32,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", deny_unknown_fields)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CoverageWire {
    Complete(EmptyWire),
    Partial(PartialWire),
    Unavailable(UnavailableWire),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PartialWire {
    completed: u16,
    total: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UnavailableWire {
    lane: String,
    reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocumentWire {
    symbol: String,
    basis: String,
    source: Option<BasisWire>,
    fragments: Vec<FragmentWire>,
    signature: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", deny_unknown_fields)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FragmentWire {
    Text(TextWire),
    Code(TextWire),
    Link(LinkWire),
    Break(EmptyWire),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LinkWire {
    label: String,
    target: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OutlineWire {
    package: String,
    basis: String,
    source: Option<BasisWire>,
    root: OutlineNodeWire,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OutlineNodeWire {
    symbol: String,
    children: Vec<OutlineNodeWire>,
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
        CommandReply::Health(root) => ReplyWire::Health(view_root_to_wire(root)),
        CommandReply::Revision(receipt) => ReplyWire::Revision(RevisionWire {
            root: encode_id(receipt.root().as_bytes()),
            source: encode_id(receipt.source().as_bytes()),
            cursor: cursor_to_wire(receipt.cursor()),
        }),
        CommandReply::Error(message) => ReplyWire::Error(ErrorWire {
            message: message.clone(),
        }),
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
        ReplyWire::Added(value) => CommandReply::Added(certificate.intent_value(&value.id)?),
        ReplyWire::Removed(value) => CommandReply::Removed(certificate.intent_value(&value.id)?),
        ReplyWire::Document(value) => {
            CommandReply::Document(document_from_wire(value, certificate)?)
        }
        ReplyWire::Page(value) => CommandReply::Page(document_from_wire(value, certificate)?),
        ReplyWire::Outline(value) => CommandReply::Outline(outline_from_wire(value, certificate)?),
        ReplyWire::Names(value) => CommandReply::Names(snapshot_from_wire_with_admission(
            value,
            certificate,
            admission,
        )?),
        ReplyWire::Resolved(value) => CommandReply::Resolved(
            value
                .rows
                .into_iter()
                .map(|row| row_from_wire(row, certificate))
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
        ReplyWire::Health(value) => CommandReply::Health(view_root_from_wire_with_admission(
            &value,
            certificate,
            admission,
        )?),
        ReplyWire::Revision(value) => {
            let Some(capability) = admission.admit(certificate, &value.source)? else {
                return Err("revision reply requires admitted producer coverage".to_owned());
            };
            let source =
                certificate.version_value::<ObjectSchema>(WireSchema::Object, &value.source)?;
            let root = certificate.producer_root_value::<crate::ViewRelation>(
                WireSchema::ViewRelation,
                &value.root,
                &capability,
            )?;
            let cursor = cursor_from_wire_with_capability(&value.cursor, certificate, &capability)?;
            if cursor.root() != root || cursor.query_offset() != 0 {
                return Err("revision cursor does not match its visible root".to_owned());
            }
            CommandReply::Revision(RevisionReceipt::new(root, cursor, source))
        }
        ReplyWire::Error(value) => CommandReply::Error(value.message),
    })
}

pub(crate) fn snapshot_to_wire(snapshot: &ViewSnapshot) -> SnapshotWire {
    SnapshotWire {
        root: view_root_to_wire(&snapshot.root),
        freshness: freshness_to_wire(snapshot.freshness),
        next: snapshot.next.map(cursor_to_wire),
    }
}

pub(crate) fn snapshot_from_wire(
    value: SnapshotWire,
    certificate: &WireCertificate,
    capability: Option<CoverageCapability>,
) -> Result<ViewSnapshot, String> {
    snapshot_from_wire_with_admission(value, certificate, &CapabilityAdmission(capability))
}

fn snapshot_from_wire_with_admission<A: CoverageAdmission>(
    value: SnapshotWire,
    certificate: &WireCertificate,
    admission: &A,
) -> Result<ViewSnapshot, String> {
    let root = view_root_from_wire_with_admission(&value.root, certificate, admission)?;
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
        root,
        freshness,
        next,
    })
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
    view_root_from_wire_with_admission(value, certificate, &CapabilityAdmission(capability))
}

fn view_root_from_wire_with_admission<A: CoverageAdmission>(
    value: &ViewRootWire,
    certificate: &WireCertificate,
    admission: &A,
) -> Result<ViewRoot, String> {
    let recipe = certificate
        .key_bytes::<ViewRecipeSchema>(WireSchema::ViewRecipe, &value.recipe)
        .map_err(|error| format!("view recipe: {error}"))?;
    let version = certificate
        .version_value::<ViewVersionSchema>(WireSchema::ViewVersion, &value.version)
        .map_err(|error| format!("view version: {error}"))?;
    let claimed_root = certificate
        .root_value::<crate::ViewRelation>(WireSchema::ViewRelation, &value.root)
        .map_err(|error| format!("view result root: {error}"))?;
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
        source: row.source.as_ref().map(|source| SourceLocationWire {
            path: source.path().to_owned(),
            start_line: source.start_line(),
        }),
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
        .map(|fragment| fragment_from_wire(fragment, certificate))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("snapshot row document: {error}"))?
        .into_boxed_slice();
    let id =
        row_id_from_wire(&value.id, certificate).map_err(|error| format!("identity: {error}"))?;
    let basis = match capability {
        Some(capability) => basis_from_wire_with_capability(&value.basis, certificate, capability),
        None => basis_from_wire(&value.basis, certificate),
    }
    .map_err(|error| format!("basis: {error}"))?;
    let package = value
        .package
        .as_deref()
        .map(|id| certificate.key_value::<PackageSchema>(WireSchema::Package, id))
        .transpose()
        .map_err(|error| format!("package: {error}"))?;
    let parent = value
        .parent
        .as_deref()
        .map(|id| certificate.key_value::<SymbolSchema>(WireSchema::Symbol, id))
        .transpose()
        .map_err(|error| format!("parent: {error}"))?;
    Ok(Row {
        id,
        basis,
        state: row_state_from_wire(&value.state),
        label: value.label,
        score: value.score,
        package,
        parent,
        document,
        signature: value.signature,
        kind: value.kind.map(|kind| DeclarationKind::from_name(&kind)),
        source: value
            .source
            .map(|source| SourceLocation::new(source.path, source.start_line))
            .transpose()?,
    })
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
        .map(|fragment| fragment_from_wire(fragment, certificate))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("snapshot row document: {error}"))?
        .into_boxed_slice();
    let id = capability
        .map_or_else(
            || row_id_from_wire(&value.id, certificate),
            |capability| row_id_from_wire_with_capability(&value.id, certificate, capability),
        )
        .map_err(|error| format!("snapshot row identity: {error}"))?;
    let package = value
        .package
        .as_deref()
        .map(|id| match capability {
            Some(capability) => certificate
                .key_value::<PackageSchema>(WireSchema::Package, id)
                .or_else(|_| certificate.producer_key_value(WireSchema::Package, id, capability)),
            None => certificate.key_value::<PackageSchema>(WireSchema::Package, id),
        })
        .transpose()
        .map_err(|error| format!("snapshot row package: {error}"))?;
    let parent = value
        .parent
        .as_deref()
        .map(|id| match capability {
            Some(capability) => certificate
                .key_value::<SymbolSchema>(WireSchema::Symbol, id)
                .or_else(|_| certificate.producer_key_value(WireSchema::Symbol, id, capability)),
            None => certificate.key_value::<SymbolSchema>(WireSchema::Symbol, id),
        })
        .transpose()
        .map_err(|error| format!("snapshot row parent: {error}"))?;
    Ok(Row {
        id,
        basis: expected,
        state: row_state_from_wire(&value.state),
        label: value.label,
        score: value.score,
        package,
        parent,
        document,
        signature: value.signature,
        kind: value.kind.map(|kind| DeclarationKind::from_name(&kind)),
        source: value
            .source
            .map(|source| SourceLocation::new(source.path, source.start_line))
            .transpose()?,
    })
}

fn row_id_from_wire_with_capability(
    value: &RowIdWire,
    certificate: &WireCertificate,
    capability: &CoverageCapability,
) -> Result<RowId, String> {
    match value.kind.as_str() {
        "package" => certificate
            .key_value::<PackageSchema>(WireSchema::Package, &value.id)
            .or_else(|_| certificate.producer_key_value(WireSchema::Package, &value.id, capability))
            .map(RowId::Package),
        "symbol" => certificate
            .key_value::<SymbolSchema>(WireSchema::Symbol, &value.id)
            .or_else(|_| certificate.producer_key_value(WireSchema::Symbol, &value.id, capability))
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
            certificate.key_value::<PackageSchema>(WireSchema::Package, &value.id)?,
        )),
        "symbol" => Ok(RowId::Symbol(
            certificate.key_value::<SymbolSchema>(WireSchema::Symbol, &value.id)?,
        )),
        "object" => Ok(RowId::Object(
            certificate.version_value::<ObjectSchema>(WireSchema::Object, &value.id)?,
        )),
        _ => Err("unknown stable row identity kind".to_owned()),
    }
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

pub(crate) fn freshness_to_wire(freshness: Freshness) -> FreshnessWire {
    match freshness {
        Freshness::Current => FreshnessWire::Current(EmptyWire {}),
        Freshness::Stale { observed } => FreshnessWire::Stale(StaleWire {
            observed: encode_id(observed.as_bytes()),
        }),
        Freshness::Unknown => FreshnessWire::Unknown(EmptyWire {}),
    }
}

pub(crate) fn freshness_from_wire(
    freshness: FreshnessWire,
    certificate: &WireCertificate,
) -> Result<Freshness, String> {
    Ok(match freshness {
        FreshnessWire::Current(_) => Freshness::Current,
        FreshnessWire::Stale(value) => Freshness::Stale {
            observed: certificate
                .root_value::<crate::ViewRelation>(WireSchema::ViewRelation, &value.observed)?,
        },
        FreshnessWire::Unknown(_) => Freshness::Unknown,
    })
}

pub(crate) fn coverage_to_wire(coverage: Coverage) -> CoverageWire {
    match coverage {
        Coverage::Complete => CoverageWire::Complete(EmptyWire {}),
        Coverage::Partial { completed, total } => {
            CoverageWire::Partial(PartialWire { completed, total })
        }
        Coverage::Unavailable { lane, reason } => CoverageWire::Unavailable(UnavailableWire {
            lane: lane_name(lane).to_owned(),
            reason: reason_name(reason).to_owned(),
        }),
    }
}

pub(crate) fn coverage_from_wire(coverage: CoverageWire) -> Result<Coverage, String> {
    Ok(match coverage {
        CoverageWire::Complete(_) => Coverage::Complete,
        CoverageWire::Partial(value) if value.completed <= value.total => Coverage::Partial {
            completed: value.completed,
            total: value.total,
        },
        CoverageWire::Partial(_) => return Err("invalid partial coverage bounds".to_owned()),
        CoverageWire::Unavailable(value) => Coverage::Unavailable {
            lane: match value.lane.as_str() {
                "exact" => Lane::Exact,
                "names" => Lane::Names,
                "graph" => Lane::Graph,
                "semantic" => Lane::Semantic,
                _ => return Err("unknown coverage lane".to_owned()),
            },
            reason: match value.reason.as_str() {
                "no_index" => Reason::NoIndex,
                "unconfigured" => Reason::Unconfigured,
                "offline" => Reason::Offline,
                "cancelled" => Reason::Cancelled,
                "incomplete" => Reason::Incomplete,
                _ => return Err("unknown coverage reason".to_owned()),
            },
        },
    })
}

fn lane_name(lane: Lane) -> &'static str {
    match lane {
        Lane::Exact => "exact",
        Lane::Names => "names",
        Lane::Graph => "graph",
        Lane::Semantic => "semantic",
    }
}

fn reason_name(reason: Reason) -> &'static str {
    match reason {
        Reason::NoIndex => "no_index",
        Reason::Unconfigured => "unconfigured",
        Reason::Offline => "offline",
        Reason::Cancelled => "cancelled",
        Reason::Incomplete => "incomplete",
    }
}

pub(crate) fn document_to_wire(document: &Document) -> DocumentWire {
    DocumentWire {
        symbol: encode_id(document.symbol.as_bytes()),
        basis: encode_id(document.basis().as_bytes()),
        source: document.source_basis().map(basis_to_wire),
        fragments: document.fragments.iter().map(fragment_to_wire).collect(),
        signature: document.signature.clone(),
    }
}

fn fragment_to_wire(fragment: &Fragment) -> FragmentWire {
    match fragment {
        Fragment::Text(text) => FragmentWire::Text(TextWire { text: text.clone() }),
        Fragment::Code(text) => FragmentWire::Code(TextWire { text: text.clone() }),
        Fragment::Link { label, target } => FragmentWire::Link(LinkWire {
            label: label.clone(),
            target: encode_id(target.as_bytes()),
        }),
        Fragment::Break => FragmentWire::Break(EmptyWire {}),
    }
}

fn fragment_from_wire(
    fragment: FragmentWire,
    certificate: &WireCertificate,
) -> Result<Fragment, String> {
    Ok(match fragment {
        FragmentWire::Text(value) => Fragment::Text(value.text),
        FragmentWire::Code(value) => Fragment::Code(value.text),
        FragmentWire::Link(value) => Fragment::Link {
            label: value.label,
            target: certificate.key_value::<SymbolSchema>(WireSchema::Symbol, &value.target)?,
        },
        FragmentWire::Break(_) => Fragment::Break,
    })
}

pub(crate) fn document_from_wire(
    value: DocumentWire,
    certificate: &WireCertificate,
) -> Result<Document, String> {
    let basis =
        certificate.root_value::<crate::ViewRelation>(WireSchema::ViewRelation, &value.basis)?;
    let source = value
        .source
        .as_ref()
        .map(|source| basis_from_wire(source, certificate))
        .transpose()?;
    if source.is_some_and(|source| source.root != basis) {
        return Err("document source basis does not match its root".to_owned());
    }
    let fragments = value
        .fragments
        .into_iter()
        .map(|fragment| fragment_from_wire(fragment, certificate))
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Document {
        symbol: certificate.key_value::<SymbolSchema>(WireSchema::Symbol, &value.symbol)?,
        basis,
        source,
        fragments: fragments.into_boxed_slice(),
        signature: value.signature,
    })
}

pub(crate) fn outline_to_wire(outline: &Outline) -> OutlineWire {
    OutlineWire {
        package: encode_id(outline.package.as_bytes()),
        basis: encode_id(outline.basis().as_bytes()),
        source: outline.source_basis().map(basis_to_wire),
        root: outline_node_to_wire(&outline.root),
    }
}

pub(crate) fn outline_from_wire(
    value: OutlineWire,
    certificate: &WireCertificate,
) -> Result<Outline, String> {
    let basis =
        certificate.root_value::<crate::ViewRelation>(WireSchema::ViewRelation, &value.basis)?;
    let source = value
        .source
        .as_ref()
        .map(|source| basis_from_wire(source, certificate))
        .transpose()?;
    if source.is_some_and(|source| source.root != basis) {
        return Err("outline source basis does not match its root".to_owned());
    }
    Ok(Outline {
        package: certificate.key_value::<PackageSchema>(WireSchema::Package, &value.package)?,
        basis,
        source,
        root: outline_node_from_wire(value.root, certificate)?,
    })
}

pub(crate) fn outline_node_to_wire(node: &OutlineNode) -> OutlineNodeWire {
    OutlineNodeWire {
        symbol: encode_id(node.symbol.as_bytes()),
        children: node.children.iter().map(outline_node_to_wire).collect(),
    }
}

pub(crate) fn outline_node_from_wire(
    value: OutlineNodeWire,
    certificate: &WireCertificate,
) -> Result<OutlineNode, String> {
    Ok(OutlineNode {
        symbol: certificate.key_value::<SymbolSchema>(WireSchema::Symbol, &value.symbol)?,
        children: value
            .children
            .into_iter()
            .map(|child| outline_node_from_wire(child, certificate))
            .collect::<Result<Vec<_>, _>>()?
            .into_boxed_slice(),
    })
}
