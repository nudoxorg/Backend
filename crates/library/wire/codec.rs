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
    #[derive(serde::Deserialize)]
    struct VersionHeader {
        version: u16,
    }
    let header: VersionHeader = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    super::ensure_version(header.version, kind)
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
/// This scoped boundary permits graph-query continuations to use their
/// constant-size root commitment. Every other command retains the canonical
/// standalone decoder.
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
/// strict round trip.
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
    // are the one deliberate exception: their root claim is an owner-bound
    // commitment and can only be reopened by `decode_command_body_for_owner`.
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
                crate::SnapshotPageDto::decode(bytes, cursor, None, None)
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
