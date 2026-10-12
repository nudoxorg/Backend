//! Constructed client observations retain their meaning on every fault surface.

use crate::*;
use backend_client::{ClientError, RequestCancellationReason};
use std::io::ErrorKind;

#[test]
fn every_client_error_has_an_explicit_truthful_fault_class() {
    let root = backend_library::view_state_root(&[]);
    let basis = backend_library::ViewRevision::from(root);
    let cases = [
        (
            ClientError::Io("fixture endpoint".into()),
            FaultSlug::Endpoint,
            CauseSlug::Unreachable,
            Affordance::Retry,
        ),
        (
            ClientError::Startup(
                backend_runtime::RuntimeError::MissingExecutable("/fixture/backend-locald".into())
                    .into(),
            ),
            FaultSlug::Endpoint,
            CauseSlug::Unreachable,
            Affordance::None,
        ),
        (
            ClientError::Disconnected(ErrorKind::ConnectionReset),
            FaultSlug::Endpoint,
            CauseSlug::Unreachable,
            Affordance::Retry,
        ),
        (
            ClientError::QueryPreparationDisconnected {
                basis,
                kind: ErrorKind::TimedOut,
            },
            FaultSlug::Endpoint,
            CauseSlug::Unreachable,
            Affordance::Retry,
        ),
        (
            ClientError::MutationOutcomeUnknown(ErrorKind::BrokenPipe),
            FaultSlug::Endpoint,
            CauseSlug::Unreachable,
            Affordance::None,
        ),
        (
            ClientError::RemoteDeadlineExceeded,
            FaultSlug::Endpoint,
            CauseSlug::Unreachable,
            Affordance::Retry,
        ),
        (
            ClientError::RequestCancelled(RequestCancellationReason::Explicit),
            FaultSlug::Endpoint,
            CauseSlug::Unreachable,
            Affordance::None,
        ),
        (
            ClientError::Protocol("deadline cursor revoked words are not a classification".into()),
            FaultSlug::Protocol,
            CauseSlug::Unproven,
            Affordance::None,
        ),
        (
            ClientError::Transport(backend_replication::ReplicationError::MessageTooLarge),
            FaultSlug::Transport,
            CauseSlug::Oversized,
            Affordance::None,
        ),
        (
            ClientError::CommandFailed(backend_library::CommandFailure::NotFound),
            FaultSlug::NotFound,
            CauseSlug::Absent,
            Affordance::None,
        ),
        (
            ClientError::IncoherentView,
            FaultSlug::IncoherentView,
            CauseSlug::Unproven,
            Affordance::WaitForReadiness,
        ),
        (
            ClientError::BasisMismatch {
                expected: root,
                observed: root,
            },
            FaultSlug::WrongBasis,
            CauseSlug::Moved,
            Affordance::Retry,
        ),
        (
            ClientError::FreshnessMismatch,
            FaultSlug::Freshness,
            CauseSlug::Unproven,
            Affordance::Retry,
        ),
        (
            ClientError::RequestMismatch {
                expected: 17,
                observed: 29,
            },
            FaultSlug::RequestMismatch,
            CauseSlug::Unproven,
            Affordance::Retry,
        ),
        (
            ClientError::CursorMismatch,
            FaultSlug::CursorMismatch,
            CauseSlug::Moved,
            Affordance::Retry,
        ),
        (
            ClientError::StaleCursor,
            FaultSlug::CursorMismatch,
            CauseSlug::Moved,
            Affordance::Retry,
        ),
        (
            ClientError::StaleSelection,
            FaultSlug::WrongBasis,
            CauseSlug::Moved,
            Affordance::Retry,
        ),
        (
            ClientError::StaleRemoteRoot {
                expected: [1; 32],
                observed: [2; 32],
            },
            FaultSlug::WrongBasis,
            CauseSlug::Moved,
            Affordance::None,
        ),
        (
            ClientError::StaleRemoteCapability,
            FaultSlug::Endpoint,
            CauseSlug::Refused,
            Affordance::None,
        ),
        (
            ClientError::RemoteCapabilityRevoked,
            FaultSlug::Endpoint,
            CauseSlug::Refused,
            Affordance::None,
        ),
    ];
    assert_eq!(cases.len(), 20);
    for (error, slug, cause, affordance) in cases {
        let fault = Fault::from_client_error(&error, Operand::Text("original operand".into()));
        assert_eq!(fault.slug(), slug, "{error:?}");
        assert_eq!(fault.cause().slug(), cause, "{error:?}");
        assert_eq!(fault.affordance(), &affordance, "{error:?}");
        let human = text::fault(&fault, Theme::plain());
        let markdown = markdown::fault(&fault);
        assert!(human.contains(fault.cause().sentence()), "{error:?}");
        assert!(markdown.contains(fault.cause().sentence()), "{error:?}");
        for detail in [Detail::Summary, Detail::Standard, Detail::Full] {
            let encoded = encode_serializable(
                "fault",
                detail,
                None,
                FaultDto::new(&fault),
                DEFAULT_RESPONSE_BUDGET_BYTES,
            )
            .expect("bounded actual fault DTO");
            assert!(encoded.bytes.len() <= DEFAULT_RESPONSE_BUDGET_BYTES);
            let value: serde_json::Value =
                serde_json::from_slice(&encoded.bytes).expect("actual JSON");
            assert_eq!(value["slug"], slug.as_str());
            assert_eq!(value["cause"], cause.as_str());
            assert_eq!(value["detail"], fault.cause().sentence());
            if matches!(affordance, Affordance::None) {
                assert!(value.get("shell").is_none());
                assert!(value.get("call").is_none());
            }
        }
        if !matches!(
            error,
            ClientError::CursorMismatch | ClientError::StaleCursor
        ) {
            assert_ne!(fault.slug(), FaultSlug::CursorMismatch, "{error:?}");
        }
    }
}

#[test]
fn deadline_retry_is_a_new_bounded_read_and_never_cursor_repair() {
    let operand = Operand::Text("selected remote read".into());
    let fault = Fault::from_client_error(&ClientError::RemoteDeadlineExceeded, operand.clone());
    assert_eq!(fault.operand(), &operand);
    assert!(fault.cause().sentence().contains("bounded client deadline"));
    assert!(fault.cause().sentence().contains("new bounded request"));
    assert!(
        fault
            .cause()
            .sentence()
            .contains("check endpoint availability")
    );
    for rendered in [text::fault(&fault, Theme::plain()), markdown::fault(&fault)] {
        assert!(!rendered.contains("cursor"));
        assert!(!rendered.contains("continuation"));
        assert!(rendered.contains("new bounded request"));
    }
    assert!(text::fault(&fault, Theme::plain()).contains("re-run the same command"));
    assert_eq!(fault.affordance().tool_call(), None);
}

#[test]
fn cancellation_unknown_mutation_and_startup_repair_never_suggest_blind_retry() {
    for reason in [
        RequestCancellationReason::Explicit,
        RequestCancellationReason::ClientDisconnected,
        RequestCancellationReason::Shutdown,
    ] {
        let fault =
            Fault::from_client_error(&ClientError::RequestCancelled(reason), Operand::Whole);
        assert_eq!(fault.affordance(), &Affordance::None);
        assert_eq!(fault.cause().sentence(), reason.to_string());
    }
    let unknown = Fault::from_client_error(
        &ClientError::MutationOutcomeUnknown(ErrorKind::TimedOut),
        Operand::Whole,
    );
    assert_eq!(unknown.affordance(), &Affordance::None);
    assert!(unknown.cause().sentence().contains("outcome is unknown"));
    for error in [
        backend_runtime::RuntimeError::MissingExecutable("/fixture/backend-locald".into()),
        backend_runtime::RuntimeError::InvalidPath("fixture deterministic configuration refusal"),
        backend_runtime::RuntimeError::EndpointUnavailable {
            endpoint: "/fixture/socket".into(),
            source: std::io::Error::from(ErrorKind::ConnectionRefused),
        },
    ] {
        let startup: backend_client::StartupFailure = error.into();
        let recovery = startup.recovery();
        let diagnostic = startup.to_string();
        let fault = Fault::from_client_error(&ClientError::Startup(startup), Operand::Whole);
        assert_eq!(fault.startup_recovery(), Some(recovery));
        assert!(fault.cause().sentence().contains(&diagnostic));
        assert_eq!(fault.affordance(), &Affordance::startup(recovery));
    }
}

#[test]
fn remote_authorization_refusals_preserve_basis_and_require_new_authority() {
    let fault = Fault::from_client_error(
        &ClientError::StaleRemoteRoot {
            expected: [1; 32],
            observed: [2; 32],
        },
        Operand::Whole,
    );
    assert_eq!(
        fault.operand(),
        &Operand::Basis {
            expected: KeyTag::from_key(&[1; 32]),
            observed: KeyTag::from_key(&[2; 32])
        }
    );
    for error in [
        ClientError::StaleRemoteCapability,
        ClientError::RemoteCapabilityRevoked,
    ] {
        let fault = Fault::from_client_error(&error, Operand::Whole);
        assert_eq!(fault.affordance(), &Affordance::None);
        assert!(fault.cause().sentence().contains("grant"));
        assert!(fault.cause().sentence().contains("obtain"));
    }
}
