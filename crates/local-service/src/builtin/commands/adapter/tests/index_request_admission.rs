//! Real owner dispatch and retained operation lineage for overlapping starts.
//! The closed fixture deliberately has no Python compiler; its actual source
//! capture/refusal must survive replacement work and an owner reopen.
use super::*;
use crate::builtin::BuiltinModelError;
use backend_library::{
    CommandReply, IndexOperationKey, IndexOperationObservation, IndexOperationState,
    IndexOperationStatus, IndexStartResult, PackageReference, SurfaceCommand, SurfaceReply,
};

fn dispatch(
    fixture: &mut AdapterFixture,
    id: u64,
    command: SurfaceCommand,
) -> Result<Executed, BuiltinModelError> {
    let request = backend_library::CommandDto::new(id, Command::Surface(command));
    let body = serde_json::to_vec(&request).expect("production surface request");
    let (adapter, daemon) = fixture.parts();
    let result = adapter.execute_or_defer(daemon, &body, id)?;
    if let Executed::Reply(bytes) = &result {
        let reply = backend_library::decode_reply_body(bytes).expect("strict client decode");
        backend_library::admit_reply(&request, &reply).expect("exact request/reply binding");
    }
    Ok(result)
}

fn surface(result: Executed) -> SurfaceReply {
    let Executed::Reply(bytes) = result else {
        panic!("immediate owner reply required");
    };
    let reply = backend_library::decode_reply_body(&bytes).expect("strict surface decode");
    let CommandReply::Surface(reply) = reply.reply else {
        panic!("surface response required: {:?}", reply.reply);
    };
    reply
}

fn status(fixture: &mut AdapterFixture, key: IndexOperationKey) -> IndexOperationStatus {
    let reply = surface(
        dispatch(
            fixture,
            510,
            SurfaceCommand::IndexOperationStatus { operation_key: key },
        )
        .expect("actual durable status read"),
    );
    let SurfaceReply::IndexOperationStatus(IndexOperationObservation::Known(status)) = reply else {
        panic!("exact retained status: {reply:?}");
    };
    status
}

fn drain(fixture: &mut AdapterFixture) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let (adapter, daemon) = fixture.parts();
        adapter.poll_deferred(daemon);
        if adapter.indexing.is_none() && adapter.waiting.is_empty() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "original owner work did not settle"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn prepare_project(fixture: &AdapterFixture) -> PackageReference {
    fs::write(
        Path::new(&fixture.label).join("pyproject.toml"),
        "[project]\nname=\"n15_refusal\"\nversion=\"1.0.0\"\n",
    )
    .expect("real project manifest");
    fs::write(
        Path::new(&fixture.label).join("source.py"),
        "def retained_failure(value: int) -> int:\n    return value + 1\n",
    )
    .expect("real captured source");
    PackageReference::parse(fixture.label.clone()).expect("actual project address")
}

#[test]
fn active_unkeyed_start_joins_exact_request_without_second_scan_or_ticket() {
    let mut fixture = AdapterFixture::new();
    let package = prepare_project(&fixture);
    let operation = IndexOperationKey::from_bytes([0xa1; 32]).expect("durable operation");
    let initial = surface(
        dispatch(
            &mut fixture,
            501,
            SurfaceCommand::IndexOperationStart {
                cargo_target: None,
                operation_key: operation,
                package: package.clone(),
                execution_intent: CompileExecutionIntent::Interactive,
            },
        )
        .expect("actual keyed admission"),
    );
    let SurfaceReply::IndexOperationStarted(IndexOperationObservation::Known(initial)) = initial
    else {
        panic!("keyed acceptance");
    };
    let IndexOperationState::Active { ticket, stage } = initial.state else {
        panic!("scan remains owner-active before polling");
    };
    let next_ticket = fixture.parts().0.next_index_ticket;
    for id in [502, 503] {
        let joined = surface(
            dispatch(
                &mut fixture,
                id,
                SurfaceCommand::IndexStart {
                    cargo_target: None,
                    package: package.clone(),
                    execution_intent: CompileExecutionIntent::Interactive,
                },
            )
            .expect("immediate joined start"),
        );
        assert_eq!(
            joined,
            SurfaceReply::IndexStarted(IndexStartResult::Started {
                ticket: ticket.clone(),
                stage,
            })
        );
        let (adapter, _) = fixture.parts();
        assert_eq!(adapter.next_index_ticket, next_ticket);
        assert!(
            adapter.waiting.is_empty(),
            "joining is not a queued replacement"
        );
        let job = adapter.indexing.as_ref().expect("original worker retained");
        assert_eq!(job.operation_key, Some(operation));
        assert_eq!(job.owner_ticket, ticket);
    }
    drain(&mut fixture);
    let terminal = status(&mut fixture, operation);
    assert!(
        matches!(terminal.state, IndexOperationState::Failed { .. }),
        "{terminal:?}"
    );
    assert_eq!(fixture.parts().0.next_index_ticket, next_ticket);
    assert_eq!(fixture.parts().0.index_terminals.len(), 1);
}

#[test]
fn closed_failed_receipt_survives_unkeyed_replacement_cold_reopen_and_remove() {
    let mut fixture = AdapterFixture::new();
    let package = prepare_project(&fixture);
    let operation = IndexOperationKey::from_bytes([0xa2; 32]).expect("durable operation");
    dispatch(
        &mut fixture,
        504,
        SurfaceCommand::IndexOperationStart {
            cargo_target: None,
            operation_key: operation,
            package: package.clone(),
            execution_intent: CompileExecutionIntent::Interactive,
        },
    )
    .expect("original keyed start");
    drain(&mut fixture);
    let original = status(&mut fixture, operation);
    assert!(
        matches!(original.state, IndexOperationState::Failed { .. }),
        "{original:?}"
    );
    let captured = original
        .source_capture
        .as_ref()
        .expect("actual retained source capture");
    assert_eq!(captured.operation_key(), operation);
    assert!(captured.profiles().iter().all(|profile| matches!(
        profile.state,
        backend_library::IndexOperationSemanticProfileState::Unavailable { .. }
            | backend_library::IndexOperationSemanticProfileState::Failed { .. }
    )));
    // A workspace root identifies source/semantic content, not the caller's
    // request. Commit identity already distinguishes an unkeyed attempt with
    // unchanged inputs; use a genuinely new source frontier for this retained
    // receipt/replacement control so both source roots must differ.
    fs::write(
        Path::new(&fixture.label).join("source.py"),
        "def retained_failure(value: int) -> int:\n    return value + 2\n",
    )
    .expect("replace actual captured source bytes");
    let replacement = surface(
        dispatch(
            &mut fixture,
            505,
            SurfaceCommand::IndexStart {
                cargo_target: None,
                package: package.clone(),
                execution_intent: CompileExecutionIntent::Interactive,
            },
        )
        .expect("distinct unkeyed replacement after terminal"),
    );
    assert!(matches!(
        replacement,
        SurfaceReply::IndexStarted(IndexStartResult::Started { .. })
    ));
    drain(&mut fixture);
    {
        let (_, daemon) = fixture.parts();
        let selected = crate::builtin::commands::index::source_capture_summary_for_root(
            daemon, &package, None, None,
        )
        .expect("actual replacement capture")
        .expect("unkeyed marker is selected");
        assert_ne!(selected.commit_identity, *captured.commit_identity());
        assert_ne!(selected.workspace_root, *captured.workspace_root());
        assert_eq!(
            selected.profiles.len(),
            captured.profiles().len(),
            "the new capture retains every original profile"
        );
        for (profile, original) in selected.profiles.iter().zip(captured.profiles()) {
            assert_eq!(
                profile.profile, original.profile,
                "exact capture profile identity"
            );
            assert_ne!(
                original.source_version, profile.source_version,
                "replacement source version must reflect actual changed bytes"
            );
        }
    }
    assert_eq!(status(&mut fixture, operation), original);
    fixture = fixture.reopen();
    assert_eq!(
        status(&mut fixture, operation),
        original,
        "cold receipt owns original evidence"
    );
    let package_key = fixture.package;
    let label = fixture.label.clone();
    let (adapter, daemon) = fixture.parts();
    let Executed::Reply(bytes) = adapter
        .execute_or_defer(daemon, &remove_body(506, package_key, &label), 506)
        .expect("ordinary remove")
    else {
        panic!("settled project remove is immediate");
    };
    let removed = backend_library::decode_reply_body(&bytes).expect("typed remove receipt");
    assert!(matches!(removed.reply, CommandReply::Removed(_)));
    assert!(!project_is_admitted(daemon, package_key));
    assert_eq!(
        status(&mut fixture, operation),
        original,
        "removal is not receipt erasure"
    );
    let fresh = IndexOperationKey::from_bytes([0xa3; 32]).expect("independent fresh retry");
    dispatch(
        &mut fixture,
        507,
        SurfaceCommand::IndexOperationStart {
            cargo_target: None,
            operation_key: fresh,
            package,
            execution_intent: CompileExecutionIntent::Interactive,
        },
    )
    .expect("new retry admitted after honest removal");
    drain(&mut fixture);
    assert!(matches!(
        status(&mut fixture, fresh).state,
        IndexOperationState::Failed { .. }
    ));
    assert_eq!(status(&mut fixture, operation), original);
}

#[test]
fn overlap_join_does_not_conflate_other_project_intent_or_durable_key() {
    let mut fixture = AdapterFixture::new();
    let package = prepare_project(&fixture);
    let operation = IndexOperationKey::from_bytes([0xa4; 32]).expect("key");
    dispatch(
        &mut fixture,
        508,
        SurfaceCommand::IndexOperationStart {
            cargo_target: None,
            operation_key: operation,
            package: package.clone(),
            execution_intent: CompileExecutionIntent::Interactive,
        },
    )
    .expect("live exact request");
    let unrelated = Path::new(&fixture.label)
        .parent()
        .expect("workspace")
        .join("unrelated");
    fs::create_dir(&unrelated).expect("other real project");
    let other =
        PackageReference::parse(unrelated.to_str().expect("UTF-8 path")).expect("other package");
    let other_key = IndexOperationKey::from_bytes([0xa5; 32]).expect("different key");
    for (id, command) in [
        (
            511,
            SurfaceCommand::IndexStart {
                cargo_target: None,
                package: other,
                execution_intent: CompileExecutionIntent::Interactive,
            },
        ),
        (
            512,
            SurfaceCommand::IndexStart {
                cargo_target: None,
                package: package.clone(),
                execution_intent: CompileExecutionIntent::Background,
            },
        ),
        (
            513,
            SurfaceCommand::IndexOperationStart {
                cargo_target: None,
                operation_key: other_key,
                package,
                execution_intent: CompileExecutionIntent::Interactive,
            },
        ),
    ] {
        assert!(matches!(
            dispatch(&mut fixture, id, command),
            Ok(Executed::Deferred)
        ));
    }
    let (adapter, daemon) = fixture.parts();
    assert_eq!(
        adapter.waiting.len(),
        3,
        "distinct mutations retain FIFO admission"
    );
    assert_eq!(
        adapter
            .indexing
            .as_ref()
            .expect("original job")
            .operation_key,
        Some(operation)
    );
    assert!(
        adapter
            .index_operations
            .entry(other_key)
            .expect("durable probe")
            .is_none()
    );
    let read = serde_json::to_vec(&backend_library::CommandDto::new(514, Command::Health))
        .expect("owner read");
    assert!(
        matches!(
            adapter.execute_or_defer(daemon, &read, 514),
            Ok(Executed::Reply(_))
        ),
        "no global read serialization"
    );
    drain(&mut fixture);
    assert!(matches!(
        status(&mut fixture, operation).state,
        IndexOperationState::Failed { .. }
    ));
    assert!(matches!(
        status(&mut fixture, other_key).state,
        IndexOperationState::Failed { .. }
    ));
}

#[test]
fn queued_remove_is_an_ordering_barrier_for_later_matching_start() {
    let mut fixture = AdapterFixture::new();
    let package = prepare_project(&fixture);
    let operation = IndexOperationKey::from_bytes([0xa6; 32]).expect("key");
    dispatch(
        &mut fixture,
        520,
        SurfaceCommand::IndexOperationStart {
            cargo_target: None,
            operation_key: operation,
            package: package.clone(),
            execution_intent: CompileExecutionIntent::Interactive,
        },
    )
    .expect("original active request");
    let package_key = fixture.package;
    let label = fixture.label.clone();
    let remove = remove_body(521, package_key, &label);
    let (adapter, daemon) = fixture.parts();
    let original_ticket = adapter
        .indexing
        .as_ref()
        .expect("original job")
        .owner_ticket
        .clone();
    let next = adapter.next_index_ticket;
    assert!(matches!(
        adapter.execute_or_defer(daemon, &remove, 521),
        Ok(Executed::Deferred)
    ));
    assert!(
        matches!(
            dispatch(
                &mut fixture,
                522,
                SurfaceCommand::IndexStart {
                    cargo_target: None,
                    package,
                    execution_intent: CompileExecutionIntent::Interactive,
                }
            ),
            Ok(Executed::Deferred)
        ),
        "a later start cannot bypass the queued remove"
    );
    assert_eq!(fixture.parts().0.waiting.len(), 2);
    assert_eq!(fixture.parts().0.next_index_ticket, next);
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut responses = Vec::new();
    loop {
        let (adapter, daemon) = fixture.parts();
        responses.extend(adapter.poll_deferred(daemon));
        if adapter.indexing.is_none() && adapter.waiting.is_empty() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "ordered remove/start did not settle"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        responses
            .iter()
            .map(|(ticket, _)| *ticket)
            .collect::<Vec<_>>(),
        [521, 522]
    );
    let removed =
        backend_library::decode_reply_body(responses[0].1.as_ref().expect("remove reply"))
            .expect("strict remove decode");
    assert_eq!(removed.request_id, 521);
    assert!(matches!(removed.reply, CommandReply::Removed(_)));
    let restarted =
        backend_library::decode_reply_body(responses[1].1.as_ref().expect("start reply"))
            .expect("strict start decode");
    assert_eq!(restarted.request_id, 522);
    let CommandReply::Surface(SurfaceReply::IndexStarted(IndexStartResult::Started {
        ticket, ..
    })) = restarted.reply
    else {
        panic!("post-remove independent start");
    };
    assert_ne!(ticket, original_ticket);
    assert_eq!(fixture.parts().0.next_index_ticket, next + 1);
    assert!(matches!(
        status(&mut fixture, operation).state,
        IndexOperationState::Failed { .. }
    ));
}
