//! Constructed client observations retain their meaning on every fault surface.

use crate::*;
use backend_client::{ClientError, RequestCancellationReason};
use std::io::ErrorKind;

#[test]
fn every_client_error_has_an_explicit_truthful_fault_class() {
    let root = backend_library::view_state_root(&[]);
    let basis = backend_library::ViewRevision::from(root);
    let operation_key =
        backend_library::IndexOperationKey::from_bytes([18; 32]).expect("operation key");
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
            ClientError::IndexOperationDeliveryUnknown {
                operation_key,
                cause: Box::new(ClientError::Protocol(
                    "replacement receipt did not prove this saved request".to_owned(),
                )),
            },
            FaultSlug::Protocol,
            CauseSlug::Unproven,
            Affordance::PollIndexOperation { operation_key },
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
    assert_eq!(cases.len(), 21);
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
    let operation_key =
        backend_library::IndexOperationKey::from_bytes([19; 32]).expect("saved key");
    let delivery = Fault::from_client_error(
        &ClientError::IndexOperationDeliveryUnknown {
            operation_key,
            cause: Box::new(ClientError::Startup(
                backend_runtime::RuntimeError::EndpointUnavailable {
                    endpoint: "/fixture/replacement.sock".into(),
                    source: std::io::Error::from(ErrorKind::ConnectionRefused),
                }
                .into(),
            )),
        },
        Operand::Whole,
    );
    assert_eq!(
        delivery.startup_recovery(),
        None,
        "replacement startup cannot authorize replaying the already-sent Add"
    );
    assert_eq!(
        delivery.affordance(),
        &Affordance::PollIndexOperation { operation_key }
    );
    assert!(
        delivery
            .cause()
            .sentence()
            .contains("/fixture/replacement.sock")
    );
    assert_eq!(delivery.unconfirmed_index_delivery(), Some(operation_key));
    assert_eq!(crate::fault_value(&delivery)["unconfirmed_index_delivery"], serde_json::to_value(operation_key).expect("same saved key"));
    assert!(delivery.cause().sentence().contains("is unconfirmed"));
    assert!(delivery.cause().sentence().contains("may already be queued or accepted"));
    assert!(!delivery.cause().sentence().contains("has no admitted outcome"));
    assert_eq!(delivery.affordance().tool_call().expect("exact saved-key observation")["arguments"]["operation_key"],
        serde_json::to_value(operation_key).expect("original key"));
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
    let expected = [1; 32];
    let mut observed = expected;
    observed[31] = 2;
    let fault = Fault::from_client_error(
        &ClientError::StaleRemoteRoot { expected, observed },
        Operand::Whole,
    );
    assert_eq!(
        fault.operand(),
        &Operand::Text(format!(
            "grant root {} differs from current root {}",
            backend_library::encode_id(&expected),
            backend_library::encode_id(&observed),
        ))
    );
    for rendered in [text::fault(&fault, Theme::plain()), markdown::fault(&fault)] {
        assert!(rendered.contains(&backend_library::encode_id(&expected)));
        assert!(rendered.contains(&backend_library::encode_id(&observed)));
    }
    for detail in [Detail::Summary, Detail::Standard, Detail::Full] {
        let encoded = encode_serializable(
            "fault",
            detail,
            None,
            FaultDto::new(&fault),
            DEFAULT_RESPONSE_BUDGET_BYTES,
        )
        .expect("bounded exact-root fault");
        let value: serde_json::Value = serde_json::from_slice(&encoded.bytes).expect("actual JSON");
        assert_eq!(value["operand"], fault.operand().render());
        let operand = value["operand"]
            .as_str()
            .expect("full rendered root operand");
        assert!(operand.contains(&backend_library::encode_id(&expected)));
        assert!(operand.contains(&backend_library::encode_id(&observed)));
    }
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

#[test]
fn verified_owner_image_mode_refusal_gets_exact_cli_and_mcp_repair_guidance() {
    let path = "/home/alice/Backend with 'quotes'/target/debug/backend-locald";
    let fault = Fault::from_client_error(
        &ClientError::OwnerUpdate {
            found: 38,
            supported: 38,
            detail: format!(
                "the installed default owner image could not be proven: owner image {path} is group-writable (uid 1000, gid 1002, mode 0775); no command was submitted"
            ),
        },
        Operand::Whole,
    );
    let expected = format!("chmod g-w -- {}", crate::shell::quote_argument(path));
    assert_eq!(
        fault.affordance().shell().as_deref(),
        Some(expected.as_str())
    );
    assert_eq!(fault.affordance().tool_call(), None);
    assert!(
        fault
            .cause()
            .sentence()
            .contains("rebuild under umask 0022")
    );
    assert!(!fault.cause().sentence().contains("group ownership"));
    let cli = text::fault(&fault, Theme::plain());
    assert!(cli.contains(&format!("→ {expected}")));
    assert!(!cli.contains("re-run the same command"));

    let encoded = encode_serializable(
        "fault",
        Detail::Full,
        None,
        FaultDto::new(&fault),
        DEFAULT_RESPONSE_BUDGET_BYTES,
    )
    .expect("bounded MCP fault DTO");
    let mcp: serde_json::Value = serde_json::from_slice(&encoded.bytes).expect("MCP fault JSON");
    assert_eq!(mcp["shell"], expected);
    assert!(mcp.get("call").is_none());
    assert!(mcp["detail"].as_str().expect("fault detail").contains("umask 0022"));

    let world_and_group = Fault::from_client_error(
        &ClientError::OwnerUpdate {
            found: 38,
            supported: 38,
            detail: format!(
                "the installed default owner image could not be proven: owner image {path} is world-writable (uid 1000, gid 1002, mode 0777); no command was submitted"
            ),
        },
        Operand::Whole,
    );
    assert_eq!(
        world_and_group.affordance().shell().as_deref(),
        Some(format!("chmod go-w -- {}", crate::shell::quote_argument(path)).as_str())
    );
}

#[test]
fn owner_image_mode_refusal_with_non_displayable_path_does_not_offer_a_blind_retry() {
    let fault = Fault::from_client_error(
        &ClientError::OwnerUpdate {
            found: 38,
            supported: 38,
            detail: "the installed default owner image could not be proven: owner image /tmp/bad\nname/backend-locald is group-writable (uid 1000, gid 1002, mode 0775); no command was submitted".into(),
        },
        Operand::Whole,
    );
    assert_eq!(fault.affordance(), &Affordance::None);
    assert!(
        fault
            .cause()
            .sentence()
            .contains("rebuild under umask 0022")
    );
    assert!(!text::fault(&fault, Theme::plain()).contains("re-run the same command"));
}
