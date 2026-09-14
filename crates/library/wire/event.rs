use super::{
    BasisWire, CoverageWire, CursorWire, DTO_VERSION, DeltaWire, EventDto, FrontierWire, RowIdWire,
    RowWire, SnapshotWire, ViewDto, ViewRootWire, WireCertificate, WireSchema, basis_from_wire,
    basis_to_wire, coverage_from_wire, coverage_to_wire, cursor_from_wire, cursor_to_wire,
    ensure_version, frontier_from_wire, frontier_to_wire, required_certificate, row_from_wire,
    row_from_wire_against, row_id_from_wire, row_id_to_wire, row_to_wire, snapshot_from_wire,
    snapshot_to_wire, view_root_from_wire, view_root_to_wire,
};
use crate::canonical::{ViewRecipeSchema, ViewVersionSchema, encode_id};
use crate::{
    Basis, CommittedViewDelta, CoverageCapability, Cursor, CursorEvent, RowChange, ViewDelta,
    ViewRoot,
};
use serde::{Deserialize, Serialize};

impl Serialize for ViewDto {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        #[serde(deny_unknown_fields)]
        struct Envelope<'a> {
            version: u16,
            request_id: u64,
            snapshot: SnapshotWire,
            certificate: Option<WireCertificate>,
            #[serde(skip)]
            marker: core::marker::PhantomData<&'a ()>,
        }
        let snapshot = snapshot_to_wire(&self.snapshot);
        Envelope {
            version: DTO_VERSION,
            request_id: self.request_id,
            snapshot,
            certificate: self.certificate().cloned(),
            marker: core::marker::PhantomData,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ViewDto {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = ViewEnvelopeWire::deserialize(deserializer)?;
        if value.version != DTO_VERSION {
            return Err(serde::de::Error::custom("unsupported view DTO version"));
        }
        let empty_certificate = WireCertificate::new();
        let certificate = value.certificate.as_ref().unwrap_or(&empty_certificate);
        let snapshot = snapshot_from_wire(value.snapshot, certificate, None)
            .map_err(serde::de::Error::custom)?;
        let mut dto = Self::new(value.request_id, snapshot);
        if let Some(certificate) = value.certificate {
            dto = dto.with_certificate(certificate);
        }
        Ok(dto)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ViewEnvelopeWire {
    pub(crate) version: u16,
    pub(crate) request_id: u64,
    pub(crate) snapshot: SnapshotWire,
    pub(crate) certificate: Option<WireCertificate>,
}

impl Serialize for EventDto {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let event = match &self.event {
            CursorEvent::Intent { id } => EventWire::Intent(DeltaWire {
                id: encode_id(id.as_bytes()),
            }),
            CursorEvent::View { delta } => EventWire::View(committed_delta_to_wire(delta)),
        };
        EventEnvelopeWire {
            version: DTO_VERSION,
            cursor: cursor_to_wire(self.cursor),
            event,
            certificate: self.certificate().cloned(),
        }
        .serialize(serializer)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EventEnvelopeWire {
    pub(crate) version: u16,
    pub(crate) cursor: CursorWire,
    pub(crate) event: EventWire,
    pub(crate) certificate: Option<WireCertificate>,
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", deny_unknown_fields)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EventWire {
    Intent(DeltaWire),
    View(EventViewWire),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EventViewWire {
    id: String,
    base_recipe: String,
    target_recipe: String,
    base_version: String,
    target_version: String,
    base_root: String,
    target_root: String,
    source: BasisWire,
    coverage_scope: String,
    frontier: FrontierWire,
    coverage: Vec<CoverageWire>,
    delta: ViewDeltaWire,
    base: ViewRootWire,
    target: ViewRootWire,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", deny_unknown_fields)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ViewDeltaWire {
    Reset(ViewDeltaResetWire),
    Upsert(ViewDeltaUpsertWire),
    Remove(ViewDeltaRemoveWire),
    Patch(ViewDeltaPatchWire),
    Coverage(ViewDeltaCoverageWire),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ViewDeltaResetWire {
    root: ViewRootWire,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ViewDeltaUpsertWire {
    row: RowWire,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ViewDeltaRemoveWire {
    id: RowIdWire,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ViewDeltaPatchWire {
    changes: Vec<RowChangeWire>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", deny_unknown_fields)]
#[serde(rename_all = "snake_case")]
enum RowChangeWire {
    Upsert(Box<ViewDeltaUpsertWire>),
    Remove(ViewDeltaRemoveWire),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ViewDeltaCoverageWire {
    coverage: CoverageWire,
}

pub(crate) fn committed_delta_to_wire(delta: &CommittedViewDelta) -> EventViewWire {
    let base = delta.base_for_wire();
    let target = delta.target_for_wire();
    EventViewWire {
        id: encode_id(delta.id.as_bytes()),
        base_recipe: encode_id(delta.base_recipe.as_bytes()),
        target_recipe: encode_id(delta.target_recipe.as_bytes()),
        base_version: encode_id(delta.base_version.as_bytes()),
        target_version: encode_id(delta.target_version.as_bytes()),
        base_root: encode_id(delta.base_root.as_bytes()),
        target_root: encode_id(delta.target_root.as_bytes()),
        source: basis_to_wire(delta.source),
        coverage_scope: encode_id(delta.coverage_scope().as_bytes()),
        frontier: frontier_to_wire(delta.frontier),
        coverage: delta
            .coverage
            .iter()
            .copied()
            .map(coverage_to_wire)
            .collect(),
        delta: view_delta_to_wire(&delta.delta),
        base: view_root_to_wire(base),
        target: view_root_to_wire(target),
    }
}

pub(crate) fn committed_delta_from_wire(
    value: &EventViewWire,
    certificate: &WireCertificate,
    capability: Option<CoverageCapability>,
) -> Result<CommittedViewDelta, String> {
    let base = view_root_from_wire(&value.base, certificate, capability.clone())?;
    let capability = capability.or_else(|| base.capability()).ok_or_else(|| {
        "certified view transition requires producer source coverage capability".to_owned()
    })?;
    let target = view_root_from_wire(&value.target, certificate, Some(capability.clone()))?;
    let base_recipe =
        certificate.key_bytes::<ViewRecipeSchema>(WireSchema::ViewRecipe, &value.base_recipe)?;
    let target_recipe =
        certificate.key_bytes::<ViewRecipeSchema>(WireSchema::ViewRecipe, &value.target_recipe)?;
    let base_version = certificate
        .version_value::<ViewVersionSchema>(WireSchema::ViewVersion, &value.base_version)?;
    let target_version = certificate
        .version_value::<ViewVersionSchema>(WireSchema::ViewVersion, &value.target_version)?;
    let base_root = certificate
        .root_value::<crate::ViewRelation>(WireSchema::ViewRelation, &value.base_root)?;
    let target_root = certificate
        .root_value::<crate::ViewRelation>(WireSchema::ViewRelation, &value.target_root)?;
    let source = basis_from_wire(&value.source, certificate)?;
    let frontier = frontier_from_wire(&value.frontier, certificate)?;
    let coverage = value
        .coverage
        .iter()
        .cloned()
        .map(coverage_from_wire)
        .collect::<Result<Vec<_>, _>>()?;
    let delta = view_delta_from_wire(&value.delta, certificate, Some(capability.clone()))?;
    let prepared = base
        .prepare(delta, capability)
        .map_err(|error| format!("invalid certified view transition: {error:?}"))?;
    let (_, committed) = base
        .clone()
        .commit(prepared)
        .map_err(|error| format!("invalid certified committed transition: {error:?}"))?;
    let certified_id = certificate.delta_value::<crate::ViewRelation>(
        WireSchema::ViewRelation,
        &value.id,
        base_root,
        target_root,
    )?;
    if committed.id() != certified_id
        || committed.base_recipe() != base_recipe
        || committed.target_recipe() != target_recipe
        || committed.base_version() != base_version
        || committed.target_version() != target_version
        || committed.base_root() != base_root
        || committed.target_root() != target_root
        || committed.source() != source
        || committed.frontier() != frontier
        || committed.coverage() != coverage.as_slice()
        || committed.base_for_wire() != &base
        || committed.target_for_wire() != &target
    {
        return Err("certified view transition claims do not match checked content".to_owned());
    }
    Ok(committed)
}

pub(crate) fn view_delta_from_wire(
    delta: &ViewDeltaWire,
    certificate: &WireCertificate,
    capability: Option<CoverageCapability>,
) -> Result<ViewDelta, String> {
    Ok(match delta {
        ViewDeltaWire::Reset(value) => ViewDelta::Reset {
            root: Box::new(view_root_from_wire(&value.root, certificate, capability)?),
        },
        ViewDeltaWire::Upsert(value) => ViewDelta::Upsert {
            row: row_from_wire(value.row.clone(), certificate)?,
        },
        ViewDeltaWire::Remove(value) => ViewDelta::Remove {
            id: row_id_from_wire(&value.id, certificate)?,
        },
        ViewDeltaWire::Patch(value) => ViewDelta::Patch {
            changes: value
                .changes
                .iter()
                .cloned()
                .map(|change| row_change_from_wire(change, certificate, None))
                .collect::<Result<Vec<_>, _>>()?
                .into(),
        },
        ViewDeltaWire::Coverage(value) => ViewDelta::Coverage {
            coverage: coverage_from_wire(value.coverage.clone())?,
        },
    })
}

pub(crate) fn view_delta_to_wire(delta: &ViewDelta) -> ViewDeltaWire {
    match delta {
        ViewDelta::Reset { root } => ViewDeltaWire::Reset(ViewDeltaResetWire {
            root: view_root_to_wire(root),
        }),
        ViewDelta::Upsert { row } => ViewDeltaWire::Upsert(ViewDeltaUpsertWire {
            row: row_to_wire(row),
        }),
        ViewDelta::Remove { id } => ViewDeltaWire::Remove(ViewDeltaRemoveWire {
            id: row_id_to_wire(*id),
        }),
        ViewDelta::Patch { changes } => ViewDeltaWire::Patch(ViewDeltaPatchWire {
            changes: changes.iter().map(row_change_to_wire).collect(),
        }),
        ViewDelta::Coverage { coverage } => ViewDeltaWire::Coverage(ViewDeltaCoverageWire {
            coverage: coverage_to_wire(*coverage),
        }),
    }
}

/// Encodes one compact, producer-certified view event for durable journals.
///
/// The ordinary [`EventDto`] deliberately carries complete base and target
/// snapshots so a standalone client can admit an event without prior state.
/// A journal already owns the checked base root, so retaining those snapshots
/// would duplicate the entire relation for every one-row update.  This codec
/// persists only the transition metadata, the exact delta, and the bounded
/// certificate claims needed for that delta.
/// # Errors
///
/// Returns an error when the encoded identity or checked state is invalid.
pub fn encode_compact_view_event(
    cursor: Cursor,
    delta: &CommittedViewDelta,
    certificate: WireCertificate,
) -> Result<Vec<u8>, String> {
    // A reset replaces the entire visible relation. Its canonical rows are
    // transferred through bounded snapshot pages, so allowing one here would
    // reintroduce an O(view) journal record.
    if matches!(delta.delta(), ViewDelta::Reset { .. }) {
        return Err("compact view events cannot carry reset transitions".to_owned());
    }
    if cursor.query_offset() != 0
        || cursor.recipe() != delta.target_recipe()
        || cursor.version() != delta.target_version()
        || cursor.root() != delta.target_root()
        || cursor.branch() != delta.frontier().branch
        || cursor.log() != delta.frontier().log
        || cursor.schema() != delta.frontier().schema
    {
        return Err("compact event cursor does not match its transition".to_owned());
    }
    serde_json::to_vec(&CompactEventEnvelopeWire {
        version: DTO_VERSION,
        cursor: cursor_to_wire(cursor),
        event: CompactEventWire::View(CompactViewWire {
            id: encode_id(delta.id().as_bytes()),
            base_recipe: encode_id(delta.base_recipe().as_bytes()),
            target_recipe: encode_id(delta.target_recipe().as_bytes()),
            base_version: encode_id(delta.base_version().as_bytes()),
            target_version: encode_id(delta.target_version().as_bytes()),
            base_root: encode_id(delta.base_root().as_bytes()),
            target_root: encode_id(delta.target_root().as_bytes()),
            source: basis_to_wire(delta.source()),
            frontier: frontier_to_wire(delta.frontier()),
            coverage: delta
                .coverage()
                .iter()
                .copied()
                .map(coverage_to_wire)
                .collect(),
            delta: view_delta_to_wire(delta.delta()),
        }),
        certificate: Some(certificate),
    })
    .map_err(|error| error.to_string())
}

/// Decodes and checks one compact journal event against its retained base.
///
/// The returned transition is freshly committed against `base`; a digest-only
/// target claim is never promoted into a root.  The caller can therefore
/// append the returned event to its in-memory cursor history without keeping
/// any serialized full-view payload in the journal frame.
/// # Errors
///
/// Returns an error when the encoded identity or checked state is invalid.
pub fn decode_compact_view_event(
    bytes: &[u8],
    previous: Cursor,
    base: &ViewRoot,
) -> Result<(Cursor, CommittedViewDelta), String> {
    let envelope: CompactEventEnvelopeWire =
        serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    ensure_version(envelope.version, "compact view event")?;
    let CompactEventWire::View(value) = envelope.event;
    let certificate = required_certificate(envelope.certificate.as_ref())?;
    if previous.query_offset() != 0
        || previous.recipe() != base.recipe()
        || previous.version() != base.version()
        || previous.root() != base.root()
        || previous.branch() != base.frontier().branch
        || previous.log() != base.frontier().log
        || previous.schema() != base.frontier().schema
    {
        return Err("compact event base cursor does not match its retained root".to_owned());
    }
    let capability = base
        .capability()
        .ok_or_else(|| "compact event base lacks complete coverage capability".to_owned())?;
    let delta =
        view_delta_from_wire_against(&value.delta, certificate, capability.clone(), base.basis())?;
    let prepared = base
        .prepare(delta, capability)
        .map_err(|error| format!("compact event transition: {error:?}"))?;
    let (_, committed) = base
        .clone()
        .commit(prepared)
        .map_err(|error| format!("compact event commit: {error:?}"))?;

    if value.id != encode_id(committed.id().as_bytes())
        || value.base_recipe != encode_id(committed.base_recipe().as_bytes())
        || value.target_recipe != encode_id(committed.target_recipe().as_bytes())
        || value.base_version != encode_id(committed.base_version().as_bytes())
        || value.target_version != encode_id(committed.target_version().as_bytes())
        || value.base_root != encode_id(committed.base_root().as_bytes())
        || value.target_root != encode_id(committed.target_root().as_bytes())
        || value.source != basis_to_wire(committed.source())
        || value.frontier != frontier_to_wire(committed.frontier())
    {
        return Err("compact event transition metadata does not match checked content".to_owned());
    }
    let coverage = value
        .coverage
        .into_iter()
        .map(coverage_from_wire)
        .collect::<Result<Vec<_>, _>>()?;
    if coverage.as_slice() != committed.coverage() {
        return Err("compact event coverage does not match checked content".to_owned());
    }
    let certified_id = certificate.delta_value::<crate::ViewRelation>(
        WireSchema::ViewRelation,
        &value.id,
        committed.base_root(),
        committed.target_root(),
    )?;
    if certified_id != committed.id() {
        return Err("compact event certificate transition does not match content".to_owned());
    }
    let event = CursorEvent::View {
        delta: Box::new(committed.clone()),
    };
    let expected = previous
        .advance_event(&event)
        .map_err(|_| "compact event cursor transition is invalid".to_owned())?;
    if !cursor_wire_matches(&envelope.cursor, expected) {
        return Err("compact event cursor does not chain from its predecessor".to_owned());
    }
    Ok((expected, committed))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompactEventEnvelopeWire {
    version: u16,
    cursor: CursorWire,
    event: CompactEventWire,
    certificate: Option<WireCertificate>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", deny_unknown_fields)]
#[serde(rename_all = "snake_case")]
enum CompactEventWire {
    View(CompactViewWire),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompactViewWire {
    id: String,
    base_recipe: String,
    target_recipe: String,
    base_version: String,
    target_version: String,
    base_root: String,
    target_root: String,
    source: BasisWire,
    frontier: FrontierWire,
    coverage: Vec<CoverageWire>,
    delta: ViewDeltaWire,
}

fn cursor_wire_matches(value: &CursorWire, expected: Cursor) -> bool {
    value.recipe() == encode_id(expected.recipe().as_bytes())
        && value.version() == encode_id(expected.version().as_bytes())
        && value.branch() == encode_id(expected.branch().as_bytes())
        && value.log() == encode_id(expected.log().as_bytes())
        && value.schema() == expected.schema()
        && value.root() == encode_id(expected.root().as_bytes())
        && value.sequence() == expected.sequence()
        && value.query_offset() == 0
}

fn view_delta_from_wire_against(
    delta: &ViewDeltaWire,
    certificate: &WireCertificate,
    capability: CoverageCapability,
    expected_basis: Basis,
) -> Result<ViewDelta, String> {
    Ok(match delta {
        ViewDeltaWire::Reset(value) => ViewDelta::Reset {
            root: Box::new(view_root_from_wire(
                &value.root,
                certificate,
                Some(capability),
            )?),
        },
        ViewDeltaWire::Upsert(value) => ViewDelta::Upsert {
            row: row_from_wire_against(value.row.clone(), expected_basis, certificate)?,
        },
        ViewDeltaWire::Remove(value) => ViewDelta::Remove {
            id: row_id_from_wire(&value.id, certificate)?,
        },
        ViewDeltaWire::Patch(value) => ViewDelta::Patch {
            changes: value
                .changes
                .iter()
                .cloned()
                .map(|change| row_change_from_wire(change, certificate, Some(expected_basis)))
                .collect::<Result<Vec<_>, _>>()?
                .into(),
        },
        ViewDeltaWire::Coverage(value) => ViewDelta::Coverage {
            coverage: coverage_from_wire(value.coverage.clone())?,
        },
    })
}

fn row_change_to_wire(change: &RowChange) -> RowChangeWire {
    match change {
        RowChange::Upsert(row) => RowChangeWire::Upsert(Box::new(ViewDeltaUpsertWire {
            row: row_to_wire(row),
        })),
        RowChange::Remove(id) => RowChangeWire::Remove(ViewDeltaRemoveWire {
            id: row_id_to_wire(*id),
        }),
    }
}

fn row_change_from_wire(
    change: RowChangeWire,
    certificate: &WireCertificate,
    expected_basis: Option<Basis>,
) -> Result<RowChange, String> {
    match change {
        RowChangeWire::Upsert(value) => {
            let ViewDeltaUpsertWire { row } = *value;
            match expected_basis {
                Some(basis) => row_from_wire_against(row, basis, certificate),
                None => row_from_wire(row, certificate),
            }
            .map(|row| RowChange::Upsert(Box::new(row)))
        }
        RowChangeWire::Remove(value) => {
            row_id_from_wire(&value.id, certificate).map(RowChange::Remove)
        }
    }
}

fn decode_event_parts(
    cursor: &CursorWire,
    event: EventWire,
    certificate: &WireCertificate,
    capability: Option<CoverageCapability>,
) -> Result<(Cursor, CursorEvent), String> {
    let cursor = cursor_from_wire(cursor, certificate)?;
    let event = match event {
        EventWire::Intent(value) => CursorEvent::Intent {
            id: certificate.intent_value(&value.id)?,
        },
        EventWire::View(value) => CursorEvent::View {
            delta: Box::new(committed_delta_from_wire(&value, certificate, capability)?),
        },
    };
    match &event {
        CursorEvent::View { delta }
            if cursor.recipe() != delta.target_recipe()
                || cursor.version() != delta.target_version()
                || cursor.root() != delta.target_root()
                || cursor.branch() != delta.frontier().branch
                || cursor.log() != delta.frontier().log
                || cursor.schema() != delta.frontier().schema =>
        {
            return Err("event cursor does not match its committed view target".to_owned());
        }
        _ => {}
    }
    Ok((cursor, event))
}

pub(crate) fn decode_event_with_certificate(
    bytes: &[u8],
    capability: Option<CoverageCapability>,
) -> Result<EventDto, String> {
    let value: EventEnvelopeWire =
        serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    ensure_version(value.version, "event")?;
    let certificate = required_certificate(value.certificate.as_ref())?;
    let (cursor, event) = decode_event_parts(&value.cursor, value.event, certificate, capability)?;
    let mut dto = EventDto::new(cursor, event);
    if let Some(certificate) = value.certificate {
        dto = dto.with_certificate(certificate);
    }
    Ok(dto)
}

impl<'de> Deserialize<'de> for EventDto {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = EventEnvelopeWire::deserialize(deserializer)?;
        if value.version != DTO_VERSION {
            return Err(serde::de::Error::custom("unsupported event DTO version"));
        }
        let empty_certificate = WireCertificate::new();
        let certificate = value.certificate.as_ref().unwrap_or(&empty_certificate);
        let event = decode_event_parts(&value.cursor, value.event, certificate, None)
            .map_err(serde::de::Error::custom)?;
        let mut dto = Self::new(event.0, event.1);
        if let Some(certificate) = value.certificate {
            dto = dto.with_certificate(certificate);
        }
        Ok(dto)
    }
}
