//! Bounded subscription event codec and state machine.
//!
//! Snapshot descriptors and page hydration live in `subscription_snapshot`;
//! this facade keeps the event stream API small and stable.

use super::{
    CursorWire, DTO_VERSION, EventDto, ViewDto, WireCertificate, cursor_to_wire, ensure_version,
};
use crate::{
    Cursor, CursorEvent, CursorRead, CursorResetReason, Freshness, Frontier,
    MAX_SUBSCRIPTION_EVENTS,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Typed payload returned by one bounded local subscription request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SubscriptionDto {
    /// A contiguous suffix and the exact cursor after its final event.
    Events {
        /// Cursor after the returned suffix.
        cursor: Cursor,
        /// Producer-certified events, each carrying its after-event cursor.
        events: Box<[EventDto]>,
    },
    /// A complete replacement root after a gap or stream mismatch.
    Reset {
        /// Cursor associated with the replacement root.
        cursor: Cursor,
        /// Producer-certified complete view envelope.
        root: Box<ViewDto>,
        /// Why the prior cursor could not be resumed.
        reason: CursorResetReason,
    },
}

impl SubscriptionDto {
    /// Builds and validates an event suffix.
    ///
    /// Each event cursor must advance exactly once from `previous`, and the
    /// final event cursor must equal `cursor`.  The event bytes can still be
    /// certified later by the wire serializer; this constructor is useful at
    /// a trusted in-process producer boundary.
    ///
    /// # Errors
    ///
    /// Returns an error when the suffix is not contiguous or its final cursor
    /// differs from the supplied cursor.
    pub fn try_events(
        previous: Cursor,
        cursor: Cursor,
        events: impl Into<Box<[EventDto]>>,
    ) -> Result<Self, String> {
        let events = events.into();
        let mut expected = previous;
        for event in &events {
            admit_event_cursor(expected, event.cursor, &event.event)?;
            expected = event.cursor;
        }
        if expected != cursor {
            return Err("subscription event suffix does not end at its cursor".to_owned());
        }
        Ok(Self::Events { cursor, events })
    }

    /// Builds and validates a complete reset payload.
    ///
    /// # Errors
    ///
    /// Returns an error when the root is not a complete producer-admitted
    /// projection or the cursor does not identify that root.
    pub fn try_reset(
        previous: Cursor,
        cursor: Cursor,
        root: ViewDto,
        reason: CursorResetReason,
    ) -> Result<Self, String> {
        if !matches!(root.snapshot.freshness, Freshness::Current) {
            return Err("subscription reset root is not current".to_owned());
        }
        if cursor.sequence() < previous.sequence() {
            return Err("subscription reset cursor moved backwards".to_owned());
        }
        if !same_stream(previous, cursor) {
            return Err("subscription reset changed its stream identity".to_owned());
        }
        let projection =
            crate::CompleteViewProjection::admit(root.snapshot.root.clone(), cursor)
                .map_err(|error| format!("invalid complete subscription reset: {error:?}"))?;
        if projection.root().recipe() != root.snapshot.root.recipe()
            || projection.root().version() != root.snapshot.root.version()
        {
            return Err("subscription reset root changed during admission".to_owned());
        }
        Ok(Self::Reset {
            cursor,
            root: Box::new(root),
            reason,
        })
    }

    /// Converts this typed payload into the cursor read consumed by a
    /// frontend reducer.
    ///
    /// # Errors
    ///
    /// Returns an error if the payload's event cursors, reset root, or source
    /// stream do not match `previous`.
    pub fn into_read(self, previous: Cursor) -> Result<CursorRead, String> {
        match self {
            Self::Events { cursor, events } => {
                let mut expected = previous;
                let mut typed = Vec::with_capacity(events.len());
                for event in events {
                    admit_event_cursor(expected, event.cursor, &event.event)?;
                    expected = event.cursor;
                    typed.push(event.event);
                }
                if expected != cursor {
                    return Err("subscription cursor does not match its event suffix".to_owned());
                }
                Ok(CursorRead::Events {
                    cursor,
                    events: typed.into_boxed_slice(),
                })
            }
            Self::Reset {
                cursor,
                root,
                reason,
            } => {
                if cursor.sequence() <= previous.sequence() {
                    return Err(
                        "subscription reset cursor is not newer than the prior cursor".to_owned(),
                    );
                }
                Self::try_reset(previous, cursor, (*root).clone(), reason)?;
                Ok(CursorRead::Reset {
                    cursor,
                    root: Box::new(root.snapshot.root),
                    reason,
                })
            }
        }
    }

    /// Decodes one strict subscription payload and admits producer
    /// certificates before constructing a [`CursorRead`].
    ///
    /// Empty event suffixes are allowed without a certificate because they
    /// carry no identity-bearing event.  Any non-empty event or reset must be
    /// independently admitted from canonical producer preimages.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed JSON, unknown fields, wrong schema,
    /// missing/forged certificates, cursor gaps, or incomplete reset roots.
    pub fn decode(
        bytes: &[u8],
        previous: Cursor,
        capability: Option<crate::CoverageCapability>,
    ) -> Result<CursorRead, String> {
        decode_subscription(bytes, previous, capability, None)
    }

    /// Decodes a subscription payload using the caller's currently admitted
    /// root for compact stateful view events.  Compact events carry only the
    /// checked transition and certificate claims; the root is deliberately
    /// supplied out of band by the reducer that owns it.
    /// # Errors
    ///
    /// Returns an error when the encoded identity or checked state is invalid.
    pub fn decode_against_root(
        bytes: &[u8],
        previous: Cursor,
        base: &crate::ViewRoot,
        capability: Option<crate::CoverageCapability>,
    ) -> Result<CursorRead, String> {
        decode_subscription(bytes, previous, capability, Some(base))
    }
}

/// Encodes a stateful subscription suffix with compact view events.
///
/// The ordinary event DTO remains available for standalone consumers that do
/// not have a retained root.  A subscription reducer does have that root, so
/// locald uses this form to keep each one-row update bounded by the delta and
/// its certificate rather than serializing the complete base and target.
/// # Errors
///
/// Returns an error when the encoded identity or checked state is invalid.
pub fn encode_compact_subscription(
    previous: Cursor,
    cursor: Cursor,
    events: &[EventDto],
) -> Result<Vec<u8>, String> {
    SubscriptionDto::try_events(previous, cursor, events.to_vec())?;
    let mut encoded = Vec::with_capacity(events.len());
    for event in events {
        let CursorEvent::View { delta } = &event.event else {
            return Err("compact subscriptions cannot carry intent events".to_owned());
        };
        let certificate = event
            .certificate()
            .cloned()
            .ok_or_else(|| "compact subscription event has no certificate".to_owned())?;
        let bytes = super::event::encode_compact_view_event(event.cursor, delta, certificate)?;
        encoded.push(serde_json::from_slice::<Value>(&bytes).map_err(|error| error.to_string())?);
    }
    serde_json::to_vec(&serde_json::json!({
        "version": DTO_VERSION,
        "kind": "events",
        "cursor": cursor_to_wire(cursor),
        "events": encoded,
    }))
    .map_err(|error| error.to_string())
}

fn decode_subscription(
    bytes: &[u8],
    previous: Cursor,
    capability: Option<crate::CoverageCapability>,
    base: Option<&crate::ViewRoot>,
) -> Result<CursorRead, String> {
    let value: Value = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    let kind = value
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing subscription kind".to_owned())?;
    match kind {
        "events" => decode_events(value, previous, capability.as_ref(), base),
        "reset" => decode_reset(value, previous, capability),
        _ => Err("unknown subscription reply kind".to_owned()),
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EventsWire {
    version: u16,
    kind: String,
    cursor: CursorWire,
    events: Vec<Value>,
    #[serde(default)]
    certificate: Option<WireCertificate>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResetWire {
    version: u16,
    kind: String,
    cursor: CursorWire,
    root: Value,
    reason: String,
    #[serde(default)]
    certificate: Option<WireCertificate>,
}

#[derive(Serialize)]
struct EventsOut<'a> {
    version: u16,
    kind: &'static str,
    cursor: CursorWire,
    events: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    certificate: Option<&'a WireCertificate>,
}

#[derive(Serialize)]
struct ResetOut<'a> {
    version: u16,
    kind: &'static str,
    cursor: CursorWire,
    root: Value,
    reason: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    certificate: Option<&'a WireCertificate>,
}

impl Serialize for SubscriptionDto {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Events { cursor, events } => {
                let events = events
                    .iter()
                    .map(serde_json::to_value)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(serde::ser::Error::custom)?;
                EventsOut {
                    version: DTO_VERSION,
                    kind: "events",
                    cursor: cursor_to_wire(*cursor),
                    events,
                    certificate: None,
                }
                .serialize(serializer)
            }
            Self::Reset {
                cursor,
                root,
                reason,
            } => ResetOut {
                version: DTO_VERSION,
                kind: "reset",
                cursor: cursor_to_wire(*cursor),
                root: serde_json::to_value(root).map_err(serde::ser::Error::custom)?,
                reason: reset_reason_name(*reason),
                certificate: None,
            }
            .serialize(serializer),
        }
    }
}

fn decode_events(
    value: Value,
    previous: Cursor,
    capability: Option<&crate::CoverageCapability>,
    base: Option<&crate::ViewRoot>,
) -> Result<CursorRead, String> {
    let wire: EventsWire = serde_json::from_value(value).map_err(|error| error.to_string())?;
    ensure_version(wire.version, "subscription")?;
    if wire.kind != "events" {
        return Err("subscription event kind mismatch".to_owned());
    }
    if wire.events.len() > MAX_SUBSCRIPTION_EVENTS {
        return Err("subscription event batch exceeds its bound".to_owned());
    }
    if wire.events.is_empty() {
        if wire.certificate.is_some() {
            return Err("empty subscription event batch cannot carry a certificate".to_owned());
        }
        let cursor = cursor_from_wire_against(&wire.cursor, previous)?;
        return SubscriptionDto::try_events(previous, cursor, Vec::<EventDto>::new())?
            .into_read(previous);
    }
    let outer_certificate = wire
        .certificate
        .as_ref()
        .map(|certificate| serde_json::to_value(certificate).map_err(|error| error.to_string()));
    let outer_certificate = outer_certificate.transpose()?;
    let mut events = Vec::with_capacity(wire.events.len());
    let mut expected = previous;
    let mut compact_base = base.cloned();
    for value in wire.events {
        let compact = value
            .get("event")
            .and_then(Value::as_object)
            .and_then(|event| event.get("kind"))
            .and_then(Value::as_str)
            .is_some_and(|kind| kind == "view")
            && value
                .get("event")
                .and_then(Value::as_object)
                .and_then(|event| event.get("data"))
                .and_then(Value::as_object)
                .is_some_and(|event| event.get("base").is_none());
        if compact {
            let Some(current_base) = compact_base.as_ref() else {
                return Err("compact subscription requires the admitted view root".to_owned());
            };
            let value = attach_certificate(value, outer_certificate.as_ref())?;
            let bytes = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
            let (cursor, delta) = crate::decode_compact_view_event(&bytes, expected, current_base)?;
            compact_base = Some(delta.target_view().clone());
            admit_event_cursor(
                expected,
                cursor,
                &CursorEvent::View {
                    delta: Box::new(delta.clone()),
                },
            )?;
            expected = cursor;
            events.push(EventDto::new(
                cursor,
                CursorEvent::View {
                    delta: Box::new(delta),
                },
            ));
            continue;
        }
        let value = attach_certificate(value, outer_certificate.as_ref())?;
        let bytes = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
        let event = EventDto::decode_with_certificate(&bytes, capability.cloned())?;
        admit_event_cursor(expected, event.cursor, &event.event)?;
        if let CursorEvent::View { delta } = &event.event {
            compact_base = Some(delta.target_view().clone());
        }
        expected = event.cursor;
        events.push(event);
    }
    let cursor = cursor_from_wire_against(&wire.cursor, expected)?;
    SubscriptionDto::try_events(previous, cursor, events)?.into_read(previous)
}

fn decode_reset(
    value: Value,
    previous: Cursor,
    capability: Option<crate::CoverageCapability>,
) -> Result<CursorRead, String> {
    let wire: ResetWire = serde_json::from_value(value).map_err(|error| error.to_string())?;
    ensure_version(wire.version, "subscription")?;
    if wire.kind != "reset" {
        return Err("subscription reset kind mismatch".to_owned());
    }
    let outer_certificate = wire
        .certificate
        .as_ref()
        .map(serde_json::to_value)
        .transpose()
        .map_err(|error| error.to_string())?;
    let root = attach_certificate(wire.root, outer_certificate.as_ref())?;
    let bytes = serde_json::to_vec(&root).map_err(|error| error.to_string())?;
    let view = ViewDto::decode_with_certificate(&bytes, capability)?;
    let reason = reset_reason(&wire.reason)?;
    let expected = Cursor::for_view(
        view.snapshot.root.recipe(),
        view.snapshot.root.version(),
        Frontier::new(
            view.snapshot.root.frontier().branch,
            view.snapshot.root.frontier().log,
            view.snapshot.root.frontier().schema,
            view.snapshot.root.root(),
            wire.cursor.sequence(),
        ),
    );
    let cursor = cursor_from_wire_against(&wire.cursor, expected)?;
    SubscriptionDto::try_reset(previous, cursor, view, reason)?.into_read(previous)
}

pub(super) fn cursor_from_wire_against(
    value: &CursorWire,
    expected: Cursor,
) -> Result<Cursor, String> {
    if value.recipe() != crate::encode_id(expected.recipe().as_bytes())
        || value.version() != crate::encode_id(expected.version().as_bytes())
        || value.branch() != crate::encode_id(expected.branch().as_bytes())
        || value.log() != crate::encode_id(expected.log().as_bytes())
        || value.schema() != expected.schema()
        || value.root() != crate::encode_id(expected.root().as_bytes())
        || value.sequence() != expected.sequence()
        || value.query_offset() != expected.query_offset()
    {
        return Err("subscription cursor does not match the admitted cursor".to_owned());
    }
    Ok(expected)
}

fn attach_certificate(mut value: Value, certificate: Option<&Value>) -> Result<Value, String> {
    let Some(certificate) = certificate else {
        return Ok(value);
    };
    let object = value
        .as_object_mut()
        .ok_or_else(|| "certified subscription item is not an object".to_owned())?;
    if object.get("certificate").is_none_or(Value::is_null) {
        object.insert("certificate".to_owned(), certificate.clone());
    }
    Ok(value)
}

pub(super) fn same_stream(left: Cursor, right: Cursor) -> bool {
    left.recipe() == right.recipe()
        && left.branch() == right.branch()
        && left.log() == right.log()
        && left.schema() == right.schema()
}

fn admit_event_cursor(
    previous: Cursor,
    observed: Cursor,
    event: &CursorEvent,
) -> Result<(), String> {
    let expected = previous
        .advance_event(event)
        .map_err(|error| format!("subscription event cursor cannot advance: {error:?}"))?;
    if observed != expected {
        return Err("subscription event cursor is not contiguous".to_owned());
    }
    Ok(())
}

pub(super) fn reset_reason(value: &str) -> Result<CursorResetReason, String> {
    match value {
        "gap" => Ok(CursorResetReason::Gap),
        "branch_discarded" => Ok(CursorResetReason::BranchDiscarded),
        "pruned" => Ok(CursorResetReason::Pruned),
        "schema_mismatch" => Ok(CursorResetReason::SchemaMismatch),
        "root_mismatch" => Ok(CursorResetReason::RootMismatch),
        _ => Err("invalid subscription reset reason".to_owned()),
    }
}

pub(super) fn reset_reason_name(reason: CursorResetReason) -> &'static str {
    match reason {
        CursorResetReason::Gap => "gap",
        CursorResetReason::BranchDiscarded => "branch_discarded",
        CursorResetReason::Pruned => "pruned",
        CursorResetReason::SchemaMismatch => "schema_mismatch",
        CursorResetReason::RootMismatch => "root_mismatch",
    }
}
