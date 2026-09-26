//! Snapshot and compact-event envelopes for the product view journal.
//!
//! Framing, checksums, and the durable file stay with the journal. This
//! module encodes the certified view, decodes a recovered payload, and
//! admits that payload onto the selected workspace generation.

use super::super::projection;
use super::Scoped;
use super::VERSION;
use backend_engine::{
    CoverageCapability, Cursor, CursorEvent, Freshness, MAX_SUBSCRIPTION_EVENTS, ViewDto, ViewRoot,
    ViewSnapshot, WorkspaceRoot,
};

/// Encodes one snapshot or compact-event payload bound to a workspace root.
pub(super) fn encode_envelope(
    workspace_root: WorkspaceRoot,
    cursor: Cursor,
    view: &ViewRoot,
    event: Option<&CursorEvent>,
) -> Result<Vec<u8>, String> {
    let view_value = event
        .is_none()
        .then(|| encode_view(view))
        .transpose()?
        .map(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).map_err(json_error))
        .transpose()?;
    let descriptor = backend_engine::encode_view_root_descriptor(&view.descriptor())?;
    let event = event
        .map(|event| {
            let delta = match event {
                CursorEvent::View { delta } => delta,
                CursorEvent::Intent { .. } => {
                    return Err("opaque intent event lacks a durable preimage".to_owned());
                }
            };
            let certificate = projection::certificate_for_compact_event(delta)
                .map_err(|error| format!("event certificate: {error}"))?;
            backend_engine::encode_compact_view_event(cursor, delta, certificate)
        })
        .transpose()?
        .map(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).map_err(json_error))
        .transpose()?;
    serde_json::to_vec(&serde_json::json!({
        "version": VERSION,
        "workspace_root": workspace_root.to_bytes(),
        // The workspace root alone does not identify a view generation: the
        // capability that certified it also binds the commit, so removing the
        // last project returns the root to an earlier value under a newer
        // commit. Recording the capability lets recovery recognize the older
        // frame as a stale cache entry instead of decoding it and failing.
        // An absent field is an older journal written before this was
        // recorded; recovery treats that as unprovable, not as corrupt.
        "capability": view.capability().as_ref().map(capability_fingerprint),
        "cursor": cursor.encode_control(),
        "descriptor": descriptor,
        "view": view_value,
        "event": event,
    }))
    .map_err(json_error)
}

/// Identifies the capability a journal frame was certified against.
///
/// The four inputs are exactly the four values
/// `WireCertificate::admit_coverage_capability` compares against the live
/// capability (`crates/library/wire/claims.rs:452-458`), so a frame whose
/// fingerprint matches is a frame whose certificate can still be admitted,
/// and a frame whose fingerprint differs is one that cannot.
pub(super) fn capability_fingerprint(capability: &CoverageCapability) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.view-journal.capability.v1\0");
    hasher.update(capability.scope_root().as_bytes());
    hasher.update(&capability.producer_identity());
    hasher.update(&capability.context());
    hasher.update(&capability.evidence_digest());
    *hasher.finalize().as_bytes()
}

/// One decoded journal payload before snapshot or event admission.
pub(super) struct DecodedEnvelope {
    pub(super) workspace_root: [u8; 32],
    capability: Option<[u8; 32]>,
    cursor: Vec<u8>,
    descriptor: Vec<u8>,
    view: Option<Vec<u8>>,
    event: Option<Vec<u8>>,
}

/// Decodes one journal payload and checks its envelope version.
pub(super) fn decode_envelope(payload: &[u8]) -> Result<DecodedEnvelope, String> {
    let value: serde_json::Value = serde_json::from_slice(payload).map_err(json_error)?;
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "view journal envelope has no version".to_owned())?;
    if version != u64::from(VERSION) {
        return Err("view journal envelope version mismatch".to_owned());
    }
    let workspace_root = fixed_bytes(value.get("workspace_root"), "workspace root")?;
    let capability = value
        .get("capability")
        .filter(|capability| !capability.is_null())
        .map(|capability| fixed_bytes(Some(capability), "capability fingerprint"))
        .transpose()?;
    let cursor = value
        .get("cursor")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "view journal envelope has no cursor".to_owned())?;
    let cursor = cursor
        .iter()
        .map(|byte| {
            byte.as_u64()
                .and_then(|value| u8::try_from(value).ok())
                .ok_or_else(|| "view journal cursor byte is invalid".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let descriptor = value
        .get("descriptor")
        .ok_or_else(|| "view journal envelope has no descriptor".to_owned())?;
    let descriptor = descriptor_bytes(descriptor)?;
    let view = value
        .get("view")
        .filter(|view| !view.is_null())
        .map(serde_json::to_vec)
        .transpose()
        .map_err(json_error)?;
    let event = value
        .get("event")
        .filter(|event| !event.is_null())
        .map(serde_json::to_vec)
        .transpose()
        .map_err(json_error)?;
    Ok(DecodedEnvelope {
        workspace_root,
        capability,
        cursor,
        descriptor,
        view,
        event,
    })
}

fn decode_cursor(bytes: &[u8], root: &ViewRoot) -> Result<Cursor, String> {
    Cursor::decode_control_for_root(bytes, root)
}

fn fixed_bytes(value: Option<&serde_json::Value>, label: &str) -> Result<[u8; 32], String> {
    let values = value
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("view journal has no {label}"))?;
    if values.len() != 32 {
        return Err(format!("view journal {label} has wrong length"));
    }
    let mut bytes = [0; 32];
    for (slot, value) in bytes.iter_mut().zip(values) {
        *slot = value
            .as_u64()
            .and_then(|value| u8::try_from(value).ok())
            .ok_or_else(|| format!("view journal {label} byte is invalid"))?;
    }
    Ok(bytes)
}

fn descriptor_bytes(value: &serde_json::Value) -> Result<Vec<u8>, String> {
    value
        .as_array()
        .ok_or_else(|| "view journal descriptor is not a byte array".to_owned())?
        .iter()
        .map(|byte| {
            byte.as_u64()
                .and_then(|value| u8::try_from(value).ok())
                .ok_or_else(|| "view journal descriptor byte is invalid".to_owned())
        })
        .collect()
}

fn ensure_descriptor(view: &ViewRoot, descriptor: &[u8]) -> Result<(), String> {
    let expected = backend_engine::encode_view_root_descriptor(&view.descriptor())?;
    if expected.as_slice() == descriptor {
        Ok(())
    } else {
        Err("view journal root descriptor does not match its snapshot".to_owned())
    }
}

fn encode_view(view: &ViewRoot) -> Result<Vec<u8>, String> {
    let certificate = projection::certificate_for_view(view, None)
        .map_err(|error| format!("view certificate: {error}"))?;
    let dto = ViewDto::new(
        0,
        ViewSnapshot {
            root: view.clone(),
            freshness: Freshness::Current,
            next: None,
            graph_relations: None,
            rich_graph: None,
        },
    )
    .with_certificate(certificate);
    serde_json::to_vec(&dto).map_err(json_error)
}

/// Admits one snapshot frame for the selected workspace root.
///
/// The workspace root repeats: removing the last project returns it to an
/// earlier value under a newer commit, and the capability binds the commit.
/// A frame certified against a different capability is therefore a stale cache
/// generation that a later frame supersedes, not a reason to refuse the
/// journal — decoding it here is what made the service refuse to start after
/// a removal.
pub(super) fn admit_snapshot(
    envelope: &DecodedEnvelope,
    capability: &CoverageCapability,
    live: [u8; 32],
) -> Result<Scoped, String> {
    if envelope.capability != Some(live) {
        return Ok(Scoped::Stale);
    }
    let view = decode_view(
        envelope
            .view
            .as_deref()
            .ok_or_else(|| "view journal snapshot has no view".to_owned())?,
        Some(capability.clone()),
    )?;
    let root = view.snapshot.root;
    ensure_descriptor(&root, &envelope.descriptor)?;
    let cursor = decode_cursor(&envelope.cursor, &root)?;
    Ok(Scoped::Accepted {
        root,
        cursor,
        workspace_root: envelope.workspace_root,
    })
}

/// Chains one event frame onto the snapshot it was written against.
///
/// An event belongs to the snapshot it chains from, so a skipped snapshot
/// skips its events with it. Only an event that never had a snapshot at all is
/// malformed.
pub(super) fn apply_event(
    state: &mut Scoped,
    envelope: DecodedEnvelope,
    events: &mut Vec<CursorEvent>,
) -> Result<(), String> {
    if matches!(*state, Scoped::Stale) {
        return Ok(());
    }
    let Scoped::Accepted {
        root: current_root,
        cursor: current_cursor,
        workspace_root,
    } = state
    else {
        return Err("view journal event precedes its snapshot".to_owned());
    };
    if *workspace_root != envelope.workspace_root {
        return Err("view journal workspace head changed within a suffix".to_owned());
    }
    let event_bytes = envelope
        .event
        .ok_or_else(|| "view journal event has no event".to_owned())?;
    let (target_cursor, committed) =
        backend_engine::decode_compact_view_event(&event_bytes, *current_cursor, current_root)
            .map_err(|error| format!("view journal compact event: {error}"))?;
    let target_root = committed
        .clone()
        .apply_to(current_root)
        .map_err(|error| format!("view journal delta: {error:?}"))?;
    let envelope_cursor = decode_cursor(&envelope.cursor, &target_root)?;
    if envelope_cursor != target_cursor
        || envelope.descriptor
            != backend_engine::encode_view_root_descriptor(&target_root.descriptor())
                .map_err(|error| format!("view journal descriptor: {error}"))?
    {
        return Err("view journal event does not chain to target".to_owned());
    }
    *current_root = target_root;
    *current_cursor = target_cursor;
    events.push(CursorEvent::View {
        delta: Box::new(committed),
    });
    if events.len() > MAX_SUBSCRIPTION_EVENTS {
        // The durable journal may outlive a subscriber. Keep only the suffix
        // the protocol can serve in one bounded owner lease; older cursors
        // deterministically receive ResetWithRoot after reopen.
        events.remove(0);
    }
    Ok(())
}

fn decode_view(bytes: &[u8], capability: Option<CoverageCapability>) -> Result<ViewDto, String> {
    ViewDto::decode_with_certificate(bytes, capability)
}

fn json_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}
