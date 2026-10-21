//! Shared strict JSON codecs for process-facing DTO bodies.
//!
//! Framing belongs to each transport because CLI and MCP use different error
//! surfaces. The body grammar and certificate dispatch do not belong there:
//! keeping them here guarantees that both clients parse the same versioned
//! envelope and perform the same producer admission before presentation.

use super::ReplyDto;
use crate::{CommandDto, Cursor};
use backend_version::ProducerObservationVerifier;
use serde::de::DeserializeOwned;

/// Largest admitted command envelope before JSON parsing or owned string allocation.
pub const MAX_COMMAND_BODY: usize = 256 * 1024;
/// Largest admitted reply envelope before JSON parsing or owned string allocation.
///
/// Four MiB matches the local command/reply frame ceiling. Remote-index reply
/// bodies use a slightly smaller ceiling to leave room for their frame
/// envelope; this shared codec remains transport neutral and accepts the full
/// local reply body allowance.
pub const MAX_REPLY_BODY: usize = 4 * 1024 * 1024;

/// Streams a reply into a fallibly allocated output buffer capped at 4 MiB.
/// Catalog reply serialization borrows rows, text and certificates.
///
/// # Errors
/// Distinguishes encoded-byte overflow from allocation, arithmetic and wire serialization faults.
pub fn encode_reply_body_bounded(reply: &ReplyDto) -> Result<Vec<u8>, super::BoundedJsonError> {
    super::bounded_json::bounded_json(reply, MAX_REPLY_BODY)
}

/// Deserializes one reply envelope after enforcing the shared encoded-body cap.
///
/// All byte-oriented reply entry points use this before constructing JSON
/// strings or invoking producer verification.
pub(super) fn parse_reply_body<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    if bytes.len() > MAX_REPLY_BODY {
        return Err(format!("reply body exceeds {MAX_REPLY_BODY} bytes"));
    }
    // Inspect only the bounded envelope version before allocating strict nested
    // payloads. A different protocol can add fields our reader does not know;
    // the useful refusal is the version mismatch, before those shape errors.
    check_live_version(bytes, "reply")?;
    serde_json::from_slice(bytes).map_err(|error| error.to_string())
}

// This header supplies no authority and never replaces the original bytes.
// Duplicate versions remain a parse error; current payloads are subsequently
// decoded through their closed, certificate-aware grammar.
pub(super) fn check_live_version(bytes: &[u8], kind: &str) -> Result<(), String> {
    check_version_header_against(bytes, kind, super::DTO_VERSION)
}

pub(super) fn check_version_header_against(
    bytes: &[u8],
    kind: &str,
    supported: u16,
) -> Result<(), String> {
    #[derive(serde::Deserialize)]
    struct VersionHeader {
        version: u16,
    }
    let header: VersionHeader = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    super::command::ensure_version_against(header.version, kind, supported)
}

pub(super) fn parse_command_body<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    if bytes.len() > MAX_COMMAND_BODY {
        return Err(format!("command body exceeds {MAX_COMMAND_BODY} bytes"));
    }
    check_live_version(bytes, "command")?;
    serde_json::from_slice(bytes).map_err(|error| error.to_string())
}

fn parse_command_body_value(bytes: &[u8]) -> Result<serde_json::Value, String> {
    if bytes.len() > MAX_COMMAND_BODY {
        return Err(format!("command body exceeds {MAX_COMMAND_BODY} bytes"));
    }
    serde_json::from_slice(bytes).map_err(|error| error.to_string())
}

/// Extracts the correlation ID from a syntactically valid command envelope.
///
/// This is deliberately weaker than command admission: listeners use it only
/// to correlate an error that prevented the full DTO from being admitted. It
/// never constructs an identity-bearing [`CommandDto`]. Bodies larger than
/// [`MAX_COMMAND_BODY`] bytes do not reach the JSON parser.
#[must_use]
pub fn command_request_id(bytes: &[u8]) -> Option<u64> {
    let value = parse_command_body_value(bytes).ok()?;
    value.get("request_id")?.as_u64()
}

/// Decodes one strict command DTO body.
///
/// # Errors
///
/// Returns an error for malformed JSON, unknown fields, unsupported versions,
/// or missing canonical claims for identity-bearing commands.
pub fn decode_command_body(bytes: &[u8]) -> Result<CommandDto, String> {
    let request: CommandDto = parse_command_body(bytes)?;
    crate::admit_request(&request).map_err(|error| error.to_string())?;
    Ok(request)
}

/// Decodes a command against the durable owner's admitted cursor.
///
/// This scoped boundary permits graph-query and exact-grammar catalog-page
/// continuations to use digest-only commitments. The catalog owner must
/// recompute the exact predecessor page before accepting that cursor. Every
/// other command retains the canonical standalone decoder.
///
/// # Errors
///
/// Returns an error when the bounded envelope or any owner-bound identity
/// claim fails admission.
pub fn decode_command_body_for_owner(bytes: &[u8], owner: Cursor) -> Result<CommandDto, String> {
    if bytes.len() > MAX_COMMAND_BODY {
        return Err(format!("command body exceeds {MAX_COMMAND_BODY} bytes"));
    }
    let request = CommandDto::decode_for_owner(bytes, owner)?;
    crate::admit_request(&request).map_err(|error| error.to_string())?;
    Ok(request)
}

/// Encodes one command DTO and verifies that its exact typed value survives a
/// strict or explicitly owner-bound round trip.
///
/// # Errors
///
/// Returns an error when serialization fails or the encoded envelope cannot
/// be admitted back to the original command.
pub fn encode_command_body(request: &CommandDto) -> Result<Vec<u8>, String> {
    crate::admit_request(request).map_err(|error| error.to_string())?;
    let bytes = serde_json::to_vec(request).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_COMMAND_BODY {
        return Err(format!("command body exceeds {MAX_COMMAND_BODY} bytes"));
    }
    // Ordinary requests must survive the producer-certificate decoder before
    // an exact comparison. Comparing an envelope only with itself would let a
    // malformed canonical preimage ride along unchecked. Graph-query resumes
    // are the deliberate exception: their commitments can only be reopened by
    // the owner decoder. Catalog continuations retain the full canonical root.
    let owner_bound_resume = matches!(
        &request.command,
        crate::Command::GraphQuery(query) if query.page().continuation().is_some()
    );
    if !owner_bound_resume {
        let _: CommandDto = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    }
    let admitted = CommandDto::decode_against(&bytes, request)?;
    if admitted != *request {
        return Err("request certificate does not admit the caller-owned command".to_owned());
    }
    Ok(bytes)
}

/// Decodes one strict reply DTO body.
///
/// A certificate-bearing envelope always takes the canonical producer path;
/// identity-free error replies may use the ordinary serde path. The caller
/// still runs [`crate::admit_reply`] against its request after this body
/// decoder returns. Envelopes larger than [`MAX_REPLY_BODY`] bytes are rejected
/// before parsing.
///
/// # Errors
///
/// Returns an error for malformed JSON, unknown fields, unsupported versions,
/// invalid producer canonical claims, or envelopes larger than the 4 MiB reply
/// body limit.
pub fn decode_reply_body(bytes: &[u8]) -> Result<ReplyDto, String> {
    decode_reply_body_with(bytes, |bytes| {
        ReplyDto::decode_with_certificate(bytes, None)
    })
}

fn decode_reply_body_with(
    bytes: &[u8],
    decode_certificate: impl FnOnce(&[u8]) -> Result<ReplyDto, String>,
) -> Result<ReplyDto, String> {
    let value: serde_json::Value = parse_reply_body(bytes)?;
    if value
        .get("certificate")
        .is_some_and(|certificate| !certificate.is_null())
    {
        decode_certificate(bytes)
    } else {
        serde_json::from_value(value).map_err(|error| error.to_string())
    }
}

/// Decodes a reply body using an authenticated producer observation verifier.
/// Identity-free error envelopes retain the ordinary bounded serde path;
/// complete view envelopes must carry a certificate whose exact observation
/// is admitted by `verifier` before a typed capability is created.
///
/// # Errors
///
/// Returns an error for malformed JSON, unknown fields, unsupported versions,
/// a producer observation rejected by `verifier`, or envelopes larger than
/// the 4 MiB reply body limit.
pub fn decode_reply_body_with_verifier<V: ProducerObservationVerifier>(
    bytes: &[u8],
    verifier: &V,
) -> Result<ReplyDto, String> {
    decode_reply_body_with(bytes, |bytes| {
        ReplyDto::decode_with_verifier(bytes, verifier)
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::{
        MAX_COMMAND_BODY, MAX_REPLY_BODY, command_request_id, decode_reply_body,
        decode_reply_body_with_verifier,
    };
    use crate::{
        Cursor, ProducerObservationClaims, ProducerObservationVerifier, ReplyDto,
        UntrustedProducerObservation, WireCertificate,
    };
    use std::cell::Cell;

    struct CountingVerifier(Cell<usize>);

    impl CountingVerifier {
        fn new() -> Self {
            Self(Cell::new(0))
        }

        fn calls(&self) -> usize {
            self.0.get()
        }
    }

    impl ProducerObservationVerifier for CountingVerifier {
        type Error = &'static str;

        fn verify(
            &self,
            observation: &UntrustedProducerObservation,
        ) -> Result<ProducerObservationClaims, Self::Error> {
            self.0.set(self.0.get() + 1);
            if observation.producer_identity() != *observation.scope_root().as_bytes()
                || observation.context() != *observation.scope_root().as_bytes()
                || observation.evidence() != observation.scope_root().as_bytes()
            {
                return Err("invalid test producer observation");
            }
            Ok(ProducerObservationClaims::new(
                observation.producer_identity(),
                observation.scope_root(),
                observation.context(),
                *blake3::hash(observation.evidence()).as_bytes(),
            ))
        }
    }

    fn certified_health_reply() -> ReplyDto {
        let source_root = crate::view_state_root(&[]);
        let basis = crate::Basis::new(source_root, crate::object_version(b"source"));
        let root = crate::ViewRoot::empty_checked(
            crate::view_key(b"view"),
            basis,
            crate::Frontier::new(basis.branch, basis.log, basis.schema, source_root, 0),
            crate::wire::tests::capability(basis.object),
        )
        .expect("complete test root");
        let certificate = crate::wire::tests::certificate(&root);
        ReplyDto::health(41, root.clone(), Cursor::for_view_root(&root))
            .with_certificate(certificate)
    }

    fn over_limit_padding(bytes: &[u8], limit: usize) -> Vec<u8> {
        let mut padded = bytes.to_vec();
        padded.resize(limit + 1, b' ');
        padded
    }

    #[test]
    fn dto36_rejects_batched_coverage_before_payload_admission() {
        assert_eq!(crate::DTO_VERSION, 38);
        let mut envelope = serde_json::json!({
            "version": crate::DTO_VERSION,
            "request_id": 41,
            "certificate": null,
            "reply": {"kind": "surface", "data": {
                "result": "package-compiler-outcomes",
                "data": {"state": "coverage", "page": {"rows": [
                    {"path": "src/one.spec.ts", "cause": "preserved"},
                    {"path": "src/two.spec.ts", "cause": "preserved"}
                ], "unknown_batch_field": true}}
            }}
        });
        let current = serde_json::to_vec(&envelope).expect("bounded versioned envelope");
        let old_peer = super::check_version_header_against(&current, "reply", 36)
            .expect_err("DTO36 client refuses the DTO38 coverage page before shape admission");
        assert!(old_peer.contains("reply DTO version 38"));
        assert!(old_peer.contains("this build supports 36"));
        assert!(!old_peer.contains("unknown_batch_field"));
        envelope["version"] = serde_json::json!(36);
        let old_bytes = serde_json::to_vec(&envelope).expect("old cohort envelope");
        let error = super::decode_reply_body(&old_bytes)
            .expect_err("current client refuses a v36 multirow payload before deserialization");
        assert!(error.contains("reply DTO version 36"));
        assert!(error.contains("this build supports 38"));
        assert!(!error.contains("unknown_batch_field"));
    }

    #[test]
    fn outline_order_cutover_refuses_dto33_before_page_shape_or_verification() {
        assert_eq!(crate::DTO_VERSION, 38);
        let current = serde_json::to_vec(&serde_json::json!({
            "request_id": 41,
            "certificate": null,
            "reply": {"kind": "projection_page", "data": {
                "outline_order": {"start": 0, "symbols": [], "unknown_future_field": true}
            }},
            "version": crate::DTO_VERSION
        }))
        .expect("new outline envelope");
        // The maintained v33 byte-entry version preflight, not a replay of an
        // old binary, rejects the new cohort before its strict page grammar.
        let old_peer = super::check_version_header_against(&current, "reply", 33)
            .expect_err("v33 peer refuses the v38 owner");
        assert!(old_peer.contains("reply DTO version 38"));
        assert!(old_peer.contains("this build supports 33"));
        assert!(!old_peer.contains("unknown field"));
        let mut old: serde_json::Value = serde_json::from_slice(&current).expect("envelope");
        old["version"] = serde_json::json!(33);
        old["reply"]["data"]
            .as_object_mut()
            .expect("page")
            .remove("outline_order");
        let bytes = serde_json::to_vec(&old).expect("old owner reply");
        let verifier = CountingVerifier::new();
        for error in [
            decode_reply_body(&bytes).expect_err("v38 client refuses v33 owner"),
            decode_reply_body_with_verifier(&bytes, &verifier)
                .expect_err("refuse before verification"),
            ReplyDto::decode_with_certificate(&bytes, None)
                .expect_err("certificate path version guard"),
        ] {
            assert!(error.contains("reply DTO version 33"));
            assert!(error.contains("this build supports 38"));
            assert!(!error.contains("unknown field"));
        }
        assert_eq!(verifier.calls(), 0);
        assert!(
            decode_reply_body(&current)
                .expect_err("same-version closed grammar")
                .contains("unknown field")
        );
        let journal = crate::JournalViewGrammarV3::from_checked_container(3)
            .expect("unchanged journal container");
        for version in [33, 34, 35, 36] {
            assert!(
                journal.check_envelope_version(version).is_ok(),
                "persisted snapshots/events retain their exact grammar"
            );
        }
    }

    #[test]
    fn source_selection_cutover_refuses_dto34_before_health_shape_or_verification() {
        assert_eq!(crate::DTO_VERSION, 38);
        let current = serde_json::to_vec(&serde_json::json!({
            "request_id": 41,
            "certificate": null,
            "reply": {"kind": "health", "data": {
                "source_selection": {"unknown_future_field": true}
            }},
            "version": crate::DTO_VERSION
        }))
        .expect("new health envelope");
        let old_peer = super::check_version_header_against(&current, "reply", 34)
            .expect_err("v34 peer refuses the v38 owner before health shape");
        assert!(old_peer.contains("reply DTO version 38"));
        assert!(old_peer.contains("this build supports 34"));
        assert!(!old_peer.contains("unknown field"));
        let mut old: serde_json::Value = serde_json::from_slice(&current).expect("envelope");
        old["version"] = serde_json::json!(34);
        old["reply"]["data"]
            .as_object_mut()
            .expect("health")
            .remove("source_selection");
        let bytes = serde_json::to_vec(&old).expect("old owner reply");
        let verifier = CountingVerifier::new();
        for error in [
            decode_reply_body(&bytes).expect_err("v38 client refuses v34 owner"),
            decode_reply_body_with_verifier(&bytes, &verifier)
                .expect_err("refuse before verification"),
            ReplyDto::decode_with_certificate(&bytes, None)
                .expect_err("certificate path version guard"),
        ] {
            assert!(error.contains("reply DTO version 34"));
            assert!(error.contains("this build supports 38"));
            assert!(!error.contains("unknown field"));
        }
        assert_eq!(verifier.calls(), 0);
        assert!(
            decode_reply_body(&current).is_err(),
            "same-version invalid health shape is refused"
        );
    }

    #[test]
    fn profile_refusal_cutover_refuses_dto35_before_closed_event_shape() {
        assert_eq!(crate::DTO_VERSION, 38);
        let current = serde_json::to_vec(&serde_json::json!({
            "version": crate::DTO_VERSION,
            "request_id": 41,
            "certificate": null,
            "reply": {"kind": "surface", "data": {
                "result": "index-progress", "data": {
                    "event": "profile-refused", "unknown_future_field": true
                }
            }}
        }))
        .expect("version preflight adversary");
        // This runs the real byte-entry preflight parameterized by the old
        // supported cohort. It does not pretend to execute an old binary.
        let old_peer = super::check_version_header_against(&current, "reply", 35)
            .expect_err("installed DTO35 CLI/MCP/GUI refuses DTO38 owner before tag parsing");
        assert!(old_peer.contains("reply DTO version 38"));
        assert!(old_peer.contains("this build supports 35"));
        assert!(old_peer.contains("same build"));
        assert!(!old_peer.contains("unknown"));
        for version in [36, 37] {
            let mut other: serde_json::Value = serde_json::from_slice(&current).expect("wire");
            other["version"] = serde_json::json!(version);
            let bytes = serde_json::to_vec(&other).expect("other cohort");
            let verifier = CountingVerifier::new();
            for error in [
                decode_reply_body(&bytes).expect_err("different live cohort"),
                decode_reply_body_with_verifier(&bytes, &verifier).expect_err("before proof"),
                ReplyDto::decode_with_certificate(&bytes, None).expect_err("same byte-entry guard"),
            ] {
                assert!(error.contains(&format!("reply DTO version {version}")));
                assert!(error.contains("this build supports 38"));
                assert!(error.contains("CLI/MCP/GUI"));
                assert!(!error.contains("unknown"));
            }
            assert_eq!(verifier.calls(), 0);
        }
        assert!(
            decode_reply_body(&current).is_err(),
            "current closed grammar stays strict"
        );
        let grammar = crate::JournalViewGrammarV3::from_checked_container(3)
            .expect("unchanged persisted container");
        assert!(grammar.check_envelope_version(35).is_ok());
        assert!(grammar.check_envelope_version(36).is_ok());
        assert!(grammar.check_envelope_version(37).is_ok());
    }

    #[test]
    fn protocol_mismatch_precedes_unknown_strict_python_payload_fields() {
        let verifier = CountingVerifier::new();
        for version in [crate::DTO_VERSION - 1, crate::DTO_VERSION + 1] {
            let bytes = serde_json::to_vec(&serde_json::json!({
                "version": version,
                "request_id": 41,
                "certificate": {"future_producer_contract": true},
                "reply": {"kind": "surface", "data": {
                    "result": "package-profile", "data": {"future_python_metadata": true}
                }}
            }))
            .expect("future protocol payload");
            let error = decode_reply_body(&bytes).expect_err("explicit protocol refusal");
            assert!(error.contains(&format!("reply DTO version {version}")));
            assert!(error.contains(&format!("this build supports {}", crate::DTO_VERSION)));
            assert!(!error.contains("unknown field"));
            let error = decode_reply_body_with_verifier(&bytes, &verifier)
                .expect_err("refuse before producer verification");
            assert!(error.contains("same build"));
        }
        assert_eq!(verifier.calls(), 0);
        let current = serde_json::to_vec(&serde_json::json!({
            "version": crate::DTO_VERSION,
            "request_id": 41,
            "certificate": null,
            "reply": {"kind": "error", "data": {"message": "failure", "future_field": true}}
        }))
        .expect("unknown current field");
        assert!(
            decode_reply_body(&current).is_err(),
            "same-version grammar stays closed"
        );
    }

    #[test]
    fn joined_local_declaration_reply_roundtrips_only_under_current_protocol() {
        let record = crate::LocalDeclarationSearchRecord {
            coordinate: crate::ProductText::new("/project::src/react.tsx:17::react")
                .expect("declaration coordinate"),
            name: crate::ProductText::new("react").expect("declaration name"),
            source: crate::LocalDeclarationSource::Captured {
                path: crate::ProductText::new("src/react.tsx").expect("captured path"),
                line: std::num::NonZeroU32::new(17).expect("captured line"),
            },
        };
        for surface in [
            crate::SurfaceReply::ExploredDeclarations(Box::new([record.clone()])),
            crate::SurfaceReply::IndexSearchWithDiscovery(Box::new([
                crate::RegistrySearchHit::LocalDeclaration(record),
            ])),
        ] {
            surface
                .admit(surface.id())
                .expect("bounded current surface");
            let expected = ReplyDto::new(41, crate::CommandReply::Surface(surface));
            let bytes = serde_json::to_vec(&expected).expect("actual encoded reply");
            assert_eq!(
                decode_reply_body(&bytes).expect("current closed reply"),
                expected
            );
            let mut old: serde_json::Value =
                serde_json::from_slice(&bytes).expect("reply envelope");
            old["version"] = serde_json::json!(23);
            let error = decode_reply_body(&serde_json::to_vec(&old).expect("old envelope"))
                .expect_err("old producer cohort");
            assert!(error.contains("reply DTO version 23"));
        }
    }

    #[test]
    fn old_live_view_subscription_and_page_headers_refuse_before_nested_fields() {
        let reply = certified_health_reply();
        let cursor = reply.health_cursor().expect("certified health cursor");
        let crate::CommandReply::Health(root) = reply.reply else {
            panic!("certified health fixture");
        };
        let bytes = br#"{"version":23,"kind":"future","future_nested_contract":{"python_metadata":true,"partial_terminal":true}}"#;
        for (kind, error) in [
            (
                "view",
                crate::ViewDto::decode_with_certificate(bytes, None).expect_err("old view"),
            ),
            (
                "event",
                crate::EventDto::decode_with_certificate(bytes, None).expect_err("old event"),
            ),
            (
                "compact view event",
                crate::decode_compact_view_event(bytes, cursor, &root)
                    .expect_err("old compact event"),
            ),
            (
                "subscription",
                crate::SubscriptionDto::decode_against_root(bytes, cursor, &root, None)
                    .expect_err("old subscription"),
            ),
            (
                "snapshot page",
                crate::SnapshotPageDto::decode_against(bytes, cursor, &root.descriptor())
                    .expect_err("old snapshot page"),
            ),
        ] {
            assert!(error.contains(&format!("{kind} DTO version 23")));
            assert!(!error.contains("unknown field"));
        }
    }

    #[test]
    fn live_command_version_refusal_precedes_unknown_nested_contracts() {
        let expected = crate::CommandDto::new(41, crate::Command::Health);
        for version in [23, crate::DTO_VERSION + 1] {
            // Version is last: checking only an initial prefix cannot pass.
            let bytes = format!(
                r#"{{"request_id":41,"command":{{"kind":"surface","data":{{"future_partial_contract":true}}}},"certificate":{{"future_python_contract":true}},"version":{version}}}"#
            ).into_bytes();
            for error in [
                crate::decode_command_body(&bytes).expect_err("old command protocol"),
                crate::decode_command_body_for_owner(&bytes, Cursor::new())
                    .expect_err("old owner-scoped command protocol"),
                crate::CommandDto::decode_against(&bytes, &expected)
                    .expect_err("old expected command protocol"),
            ] {
                assert!(error.contains(&format!("command DTO version {version}")));
                assert!(!error.contains("unknown field"));
            }
        }
        let current = format!(
            r#"{{"version":{},"request_id":41,"command":{{"kind":"health","data":{{"future_field":true}}}}}}"#,
            crate::DTO_VERSION
        );
        assert!(crate::decode_command_body(current.as_bytes()).is_err());
        let duplicate = format!(
            r#"{{"version":23,"version":{},"request_id":41,"command":{{"kind":"health"}}}}"#,
            crate::DTO_VERSION
        );
        assert!(
            crate::decode_command_body(duplicate.as_bytes())
                .expect_err("ambiguous version")
                .contains("duplicate field")
        );
    }

    #[test]
    fn reply_decoders_reject_oversized_bodies_before_json_or_verification() {
        let too_large = vec![b'{'; MAX_REPLY_BODY + 1];
        let limit_error = format!("reply body exceeds {MAX_REPLY_BODY} bytes");
        assert_eq!(decode_reply_body(&too_large), Err(limit_error.clone()));

        let verifier = CountingVerifier::new();
        assert_eq!(
            decode_reply_body_with_verifier(&too_large, &verifier),
            Err(limit_error.clone())
        );

        let expected = ReplyDto::error(41, "failure");
        assert_eq!(
            ReplyDto::decode_against(&too_large, &expected),
            Err(limit_error.clone())
        );
        assert_eq!(
            ReplyDto::decode_with_certificate(&too_large, None),
            Err(limit_error.clone())
        );
        assert_eq!(
            ReplyDto::decode_with_verifier(&too_large, &verifier),
            Err(limit_error)
        );
        assert_eq!(verifier.calls(), 0);

        let expected = ReplyDto::error(41, "failure");
        let encoded_expected = serde_json::to_vec(&expected).expect("encode expected reply");
        let padded_error = over_limit_padding(&encoded_expected, MAX_REPLY_BODY);
        assert_eq!(
            decode_reply_body(&padded_error),
            Err(format!("reply body exceeds {MAX_REPLY_BODY} bytes"))
        );
        assert_eq!(
            ReplyDto::decode_against(&padded_error, &expected),
            Err(format!("reply body exceeds {MAX_REPLY_BODY} bytes"))
        );

        let certified_error =
            ReplyDto::error(42, "certified failure").with_certificate(WireCertificate::new());
        let encoded_certified_error =
            serde_json::to_vec(&certified_error).expect("encode certified error");
        let padded_certified_error = over_limit_padding(&encoded_certified_error, MAX_REPLY_BODY);
        assert_eq!(
            ReplyDto::decode_with_certificate(&padded_certified_error, None),
            Err(format!("reply body exceeds {MAX_REPLY_BODY} bytes"))
        );

        let certified = serde_json::to_vec(&certified_health_reply()).expect("encode health");
        let padded_certificate = over_limit_padding(&certified, MAX_REPLY_BODY);
        assert_eq!(
            decode_reply_body_with_verifier(&padded_certificate, &verifier),
            Err(format!("reply body exceeds {MAX_REPLY_BODY} bytes"))
        );
        assert_eq!(
            ReplyDto::decode_with_verifier(&padded_certificate, &verifier),
            Err(format!("reply body exceeds {MAX_REPLY_BODY} bytes"))
        );
        assert_eq!(verifier.calls(), 0);
    }

    #[test]
    fn reply_codec_keeps_certificate_and_identity_free_error_paths() {
        let large_error_text = "x".repeat(MAX_COMMAND_BODY + 1);
        let failure = ReplyDto::error(41, large_error_text);
        let encoded_failure = serde_json::to_vec(&failure).expect("encode error reply");
        assert!(encoded_failure.len() > MAX_COMMAND_BODY);
        assert!(encoded_failure.len() < MAX_REPLY_BODY);
        assert_eq!(decode_reply_body(&encoded_failure), Ok(failure.clone()));

        let verifier = CountingVerifier::new();
        assert_eq!(
            decode_reply_body_with_verifier(&encoded_failure, &verifier),
            Ok(failure.clone())
        );
        assert_eq!(verifier.calls(), 0);

        let certified = certified_health_reply();
        let encoded_certified = serde_json::to_vec(&certified).expect("encode health");
        let decoded = decode_reply_body_with_verifier(&encoded_certified, &verifier)
            .expect("verified certificate path");
        assert_eq!(decoded.request_id, certified.request_id);
        let direct_decoded = ReplyDto::decode_with_verifier(&encoded_certified, &verifier)
            .expect("verified direct certificate path");
        assert_eq!(direct_decoded.request_id, certified.request_id);
        assert!(verifier.calls() > 0);
        assert!(decode_reply_body(&encoded_certified).is_err());

        let expected = ReplyDto::error(42, "expected");
        let encoded_expected = serde_json::to_vec(&expected).expect("encode expected reply");
        assert_eq!(
            ReplyDto::decode_against(&encoded_expected, &expected),
            Ok(expected)
        );

        let certified_error =
            ReplyDto::error(43, "certified failure").with_certificate(WireCertificate::new());
        let encoded_certified_error =
            serde_json::to_vec(&certified_error).expect("encode certified error");
        assert_eq!(
            ReplyDto::decode_with_certificate(&encoded_certified_error, None),
            Ok(certified_error)
        );
    }

    #[test]
    fn command_request_id_refuses_invalid_and_oversized_envelopes() {
        assert_eq!(command_request_id(br#"{"request_id":41}"#), Some(41));
        assert_eq!(command_request_id(br#"{"request_id":"41"}"#), None);
        assert_eq!(command_request_id(b"{"), None);

        let valid_but_oversized = over_limit_padding(br#"{"request_id":41}"#, MAX_COMMAND_BODY);
        assert_eq!(command_request_id(&valid_but_oversized), None);
    }
}
