//! Production owner publication, lease planning and admitted wire replay.

use super::*;
use crate::protocol::{EngineRequest, FrameLimits, ResponseFrame, decode_response};
use crate::service::OwnerService;
use backend_engine::{
    Cursor, LocalSubscriptionId, LocalSubscriptionOperation, LocalSubscriptionRequest,
    LocalSubscriptionResponse, SnapshotHydrator, SnapshotPageClaim, SubscriptionDto,
};
use backend_library::CursorRead;

fn owner(directory: &Path) -> EmptyOwner {
    let mut owner = open_empty_owner(directory).expect("actual product owner");
    let daemon = owner.daemon_mut();
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let (initial, cursor) = initial_view_for_workspace(&snapshot).expect("admitted baseline");
    let admission = BuiltinViewAdmission {
        workspace_root: snapshot.root(),
        source_root: initial.basis().root,
    };
    daemon
        .engine_mut()
        .daemon_mut()
        .publish_view(initial, cursor, &admission, None)
        .expect("publish source-bound baseline");
    owner
}

fn publish(owner: &mut EmptyOwner, changes: usize) -> Vec<backend_engine::CommittedViewDelta> {
    let daemon = owner.daemon_mut();
    let current = daemon.engine().daemon().library().view().clone();
    let mut rows = current.row_refs().cloned().collect::<Vec<_>>();
    let first = rows.len();
    rows.extend((first..first + changes).map(|index| {
        let label = format!("transport::row{index:06}");
        Row::new(
            backend_engine::RowId::Symbol(backend_engine::symbol_key(&label)),
            current.basis(),
            label,
        )
    }));
    let target = ViewRoot::new_checked(
        current.recipe(),
        current.basis(),
        current.frontier(),
        rows,
        current.coverage().to_vec(),
        current.capability().expect("admitted capability"),
    )
    .expect("checked target");
    commit_published_target(daemon, current, target).expect("production publication planner")
}

fn call(
    owner: &mut EmptyOwner,
    id: u64,
    operation: LocalSubscriptionOperation,
) -> LocalSubscriptionResponse {
    let limits = FrameLimits::default();
    let bytes = owner
        .engine_prepared(
            id,
            EngineRequest::Subscription(LocalSubscriptionRequest {
                request_id: id,
                operation,
            }),
            limits,
        )
        .expect("actual owner prepared response");
    match decode_response(&bytes, limits).expect("strict outer wire admission") {
        ResponseFrame::Engine {
            request_id,
            status: EngineStatus::Subscription(response),
        } => {
            assert_eq!(request_id, id);
            response
        }
        other => panic!("expected subscription response: {other:?}"),
    }
}

fn open(owner: &mut EmptyOwner, previous: Cursor, id: u64) -> LocalSubscriptionResponse {
    call(
        owner,
        id,
        LocalSubscriptionOperation::Open {
            cursor: if previous == Cursor::new() {
                Box::new([])
            } else {
                previous.encode_control()
            },
            credit: backend_engine::MAX_SNAPSHOT_PAGE_ROWS,
            lease_ms: backend_client::lease_contract::PUBLICATION_LEASE.get(),
        },
    )
}

fn admit_reset(
    owner: &mut EmptyOwner,
    previous: Cursor,
    first: LocalSubscriptionResponse,
    id: u64,
) -> (ViewRoot, Cursor, LocalSubscriptionId, usize) {
    admit_reset_racing(owner, previous, first, id, |_| {})
}

fn admit_reset_racing(
    owner: &mut EmptyOwner,
    previous: Cursor,
    first: LocalSubscriptionResponse,
    mut id: u64,
    race: impl FnOnce(&mut EmptyOwner),
) -> (ViewRoot, Cursor, LocalSubscriptionId, usize) {
    let snapshot = owner.daemon().engine().daemon().owner().snapshot();
    let verifier = WorkspaceViewProducerAdmission::from_snapshot(
        &snapshot,
        backend_engine::object_version(VIEW_SOURCE_VALUE),
    );
    let LocalSubscriptionResponse::SnapshotPage {
        lease,
        payload,
        mut next,
        ..
    } = first
    else {
        panic!("expected authenticated reset");
    };
    let claim = SnapshotPageClaim::decode_with_verifier(&payload, previous, None, &verifier)
        .expect("producer-certified first page");
    let encoded: serde_json::Value = serde_json::from_slice(&payload).expect("snapshot JSON");
    let mut unsupported = encoded.clone();
    unsupported["version"] = serde_json::json!(0);
    assert!(
        SnapshotPageClaim::decode_with_verifier(
            &serde_json::to_vec(&unsupported).expect("unsupported page"),
            previous,
            None,
            &verifier,
        )
        .is_err()
    );
    let mut forged = encoded;
    let coverage = forged["certificate"]["claims"]
        .as_array_mut()
        .expect("claims")
        .iter_mut()
        .find(|claim| claim["kind"] == "coverage")
        .expect("authority claim");
    coverage["data"]["context"] = serde_json::json!("00".repeat(32));
    assert!(
        SnapshotPageClaim::decode_with_verifier(
            &serde_json::to_vec(&forged).expect("forged authority page"),
            previous,
            None,
            &verifier,
        )
        .is_err()
    );
    assert!(claim.rows().len() <= backend_engine::MAX_SNAPSHOT_PAGE_ROWS);
    let mut hydrate = SnapshotHydrator::start(previous, claim).expect("deferred admission");
    let mut pages = 1;
    if next.is_some() {
        assert!(
            hydrate.clone().finish().is_err(),
            "a partial root cannot be admitted"
        );
    }
    race(owner);
    while let Some(token) = next {
        id += 1;
        let LocalSubscriptionResponse::SnapshotPage {
            payload,
            next: following,
            ..
        } = call(
            owner,
            id,
            LocalSubscriptionOperation::Page {
                lease,
                page: token,
                credit: backend_engine::MAX_SNAPSHOT_PAGE_ROWS,
            },
        )
        else {
            panic!("expected bounded continuation page");
        };
        let claim = SnapshotPageClaim::decode_with_verifier(&payload, previous, None, &verifier)
            .expect("producer-certified continuation page");
        assert!(claim.rows().len() <= backend_engine::MAX_SNAPSHOT_PAGE_ROWS);
        hydrate
            .push_page(claim)
            .expect("exact descriptor and anchor");
        pages += 1;
        next = following;
    }
    let CursorRead::Reset { cursor, root, .. } =
        hydrate.finish().expect("complete authenticated root")
    else {
        panic!("expected checked replacement");
    };
    call(
        owner,
        id + 1,
        LocalSubscriptionOperation::Ack {
            lease,
            cursor: cursor.encode_control(),
        },
    );
    (*root, cursor, lease, pages)
}

#[test]
fn publication_transport_capability_only_rebind_uses_authenticated_reset() {
    let temp = tempfile::tempdir().expect("private workspace");
    let mut owner = owner(temp.path());
    publish(&mut owner, 478);
    let base = owner.daemon().engine().daemon().library().view().clone();
    let previous = owner.daemon().engine().daemon().library().cursor();
    let deltas = publish_capability_change(&mut owner);
    assert_eq!(deltas.len(), 1);
    assert_eq!(deltas[0].changed_row_count(), 0);
    assert!(!deltas[0].is_compact_replayable());
    assert!(projection::certificate_for_compact_event(&deltas[0]).is_err());
    let target = owner.daemon().engine().daemon().library().view().clone();
    let expected = owner.daemon().engine().daemon().library().cursor();
    // Reconstruct the prior compact producer representation from this actual
    // owner-committed event. It fails at the consumer's unchanged-capability
    // empty-patch admission, reproducing the causal Unbounded refusal.
    let legacy_certificate = WireCertificate::new().with_claim(WireClaim::Delta {
        schema: backend_engine::WireSchema::ViewRelation,
        id: backend_engine::encode_id(deltas[0].id().as_bytes()),
        base: backend_engine::encode_id(deltas[0].base_root().as_bytes()),
        target: backend_engine::encode_id(deltas[0].target_root().as_bytes()),
        changes: deltas[0].canonical_changes().to_vec().into_boxed_slice(),
    });
    let mut legacy = serde_json::to_value(
        backend_engine::EventDto::new(
            expected,
            backend_engine::CursorEvent::View {
                delta: Box::new(deltas[0].clone()),
            },
        )
        .with_certificate(legacy_certificate),
    )
    .expect("previous producer representation");
    let data = legacy["event"]["data"].as_object_mut().expect("view event");
    data.remove("base");
    data.remove("target");
    data.remove("coverage_scope");
    let rejected = backend_engine::decode_compact_view_event(
        &serde_json::to_vec(&legacy).expect("previous compact bytes"),
        previous,
        &base,
    )
    .expect_err("the previous representation cannot inherit the new authority");
    assert!(rejected.contains("Unbounded"), "causal refusal: {rejected}");
    assert!(
        backend_engine::encode_compact_view_event(expected, &deltas[0], WireCertificate::new())
            .is_err()
    );
    assert_eq!(
        base.row_refs().collect::<Vec<_>>(),
        target.row_refs().collect::<Vec<_>>()
    );
    assert_ne!(base.capability(), target.capability());
    let response = open(&mut owner, previous, 100);
    let (root, cursor, lease, pages) = admit_reset(&mut owner, previous, response, 100);
    assert_eq!(root, target);
    assert_eq!(cursor, expected);
    assert_eq!(pages, 2);
    call(
        &mut owner,
        200,
        LocalSubscriptionOperation::Cancel { lease },
    );
    assert!(
        rebind_published_view(owner.daemon_mut())
            .expect("unchanged capability")
            .is_empty()
    );
    assert_eq!(
        owner.daemon().engine().daemon().library().cursor(),
        expected
    );
}

#[test]
fn publication_transport_reset_remains_pinned_across_racing_large_publication() {
    let temp = tempfile::tempdir().expect("private workspace");
    let mut owner = owner(temp.path());
    publish(&mut owner, 1);
    let previous = owner.daemon().engine().daemon().library().cursor();
    publish(&mut owner, 478);
    let target = owner.daemon().engine().daemon().library().view().clone();
    let expected = owner.daemon().engine().daemon().library().cursor();
    let response = open(&mut owner, previous, 100);
    let (root, cursor, lease, pages) =
        admit_reset_racing(&mut owner, previous, response, 100, |owner| {
            publish(owner, 4097);
            publish(owner, 1);
        });
    assert_eq!(
        root, target,
        "hydration stays on the root selected by its lease"
    );
    assert_eq!(cursor, expected);
    assert_eq!(pages, 2);
    let latest = owner.daemon().engine().daemon().library().view().clone();
    let latest_cursor = owner.daemon().engine().daemon().library().cursor();
    let response = call(
        &mut owner,
        200,
        LocalSubscriptionOperation::Resume {
            lease,
            cursor: cursor.encode_control(),
            credit: 1,
            lease_ms: backend_client::lease_contract::PUBLICATION_LEASE.get(),
        },
    );
    let (root, cursor, lease, pages) = admit_reset(&mut owner, cursor, response, 200);
    assert_eq!(root, latest);
    assert_eq!(cursor, latest_cursor);
    assert!(pages > 16);
    call(
        &mut owner,
        5000,
        LocalSubscriptionOperation::Cancel { lease },
    );
}

#[test]
fn publication_transport_bounds_match_real_owner_and_wire() {
    for changes in [0, 1, 256, 257, 478, 4097] {
        let temp = tempfile::tempdir().expect("private workspace");
        let mut owner = owner(temp.path());
        // A populated base exercises hot patch eligibility, including 256.
        publish(&mut owner, 1);
        let base = owner.daemon().engine().daemon().library().view().clone();
        let previous = owner.daemon().engine().daemon().library().cursor();
        let deltas = publish(&mut owner, changes);
        let target = owner.daemon().engine().daemon().library().view().clone();
        let expected = owner.daemon().engine().daemon().library().cursor();
        assert_eq!(deltas.len(), usize::from(changes != 0));
        assert_eq!(
            expected.sequence(),
            previous.sequence() + u64::from(changes != 0)
        );
        let response = open(&mut owner, previous, 100);
        match response {
            LocalSubscriptionResponse::Opened { cursor, lease, .. } => {
                assert_eq!(changes, 0);
                assert_eq!(cursor, expected.encode_control());
                call(
                    &mut owner,
                    101,
                    LocalSubscriptionOperation::Cancel { lease },
                );
            }
            LocalSubscriptionResponse::Batch { payload, lease, .. } => {
                assert!(changes == 1 || changes == 256);
                assert!(deltas[0].is_compact_replayable());
                let CursorRead::Events { cursor, events } = SubscriptionDto::decode_against_root(
                    &payload,
                    previous,
                    &base,
                    base.capability(),
                )
                .expect("real compact payload admitted against retained base") else {
                    panic!("expected events");
                };
                assert_eq!(cursor, expected);
                assert_eq!(events.len(), 1);
                let backend_engine::CursorEvent::View { delta } = &events[0] else {
                    panic!("view event");
                };
                assert_eq!(
                    delta.clone().apply_to(&base).expect("checked replay"),
                    target
                );
                let mut corrupted: serde_json::Value =
                    serde_json::from_slice(&payload).expect("event JSON");
                corrupted["events"][0]["event"]["data"]["target_root"] = serde_json::json!("00");
                assert!(
                    SubscriptionDto::decode_against_root(
                        &serde_json::to_vec(&corrupted).expect("corrupt payload"),
                        previous,
                        &base,
                        base.capability()
                    )
                    .is_err()
                );
                call(
                    &mut owner,
                    101,
                    LocalSubscriptionOperation::Ack {
                        lease,
                        cursor: cursor.encode_control(),
                    },
                );
                call(
                    &mut owner,
                    102,
                    LocalSubscriptionOperation::Cancel { lease },
                );
            }
            response @ LocalSubscriptionResponse::SnapshotPage { .. } => {
                assert!(changes > 256);
                assert!(!deltas[0].is_compact_replayable());
                let (root, cursor, lease, pages) = admit_reset(&mut owner, previous, response, 100);
                assert_eq!(root, target);
                assert_eq!(cursor, expected);
                assert!(pages >= 2);
                call(
                    &mut owner,
                    200,
                    LocalSubscriptionOperation::Cancel { lease },
                );
            }
            other => panic!("unexpected publication response: {other:?}"),
        }
        // Cold bootstrap takes the same bounded authenticated route for every size.
        let response = open(&mut owner, Cursor::new(), 1000);
        let (root, cursor, lease, _) = admit_reset(&mut owner, Cursor::new(), response, 1000);
        assert_eq!(root, target);
        assert_eq!(cursor, expected);
        call(
            &mut owner,
            2000,
            LocalSubscriptionOperation::Cancel { lease },
        );
    }
}

#[test]
fn publication_transport_credit_prefix_cannot_hide_later_reset() {
    for later_changes in [0, 1, 478] {
        let temp = tempfile::tempdir().expect("private workspace");
        let mut owner = owner(temp.path());
        publish(&mut owner, 1);
        let previous = owner.daemon().engine().daemon().library().cursor();
        let compact = publish(&mut owner, 1);
        assert!(compact.iter().all(|delta| delta.is_compact_replayable()));
        if later_changes == 0 {
            let deltas = publish_capability_change(&mut owner);
            assert!(deltas.iter().all(|delta| !delta.is_compact_replayable()));
        } else {
            publish(&mut owner, later_changes);
        }
        let target = owner.daemon().engine().daemon().library().view().clone();
        let target_cursor = owner.daemon().engine().daemon().library().cursor();
        let response = call(
            &mut owner,
            100,
            LocalSubscriptionOperation::Open {
                cursor: previous.encode_control(),
                credit: 1,
                lease_ms: backend_client::lease_contract::PUBLICATION_LEASE.get(),
            },
        );
        let (root, cursor, lease, _) = admit_reset(&mut owner, previous, response, 100);
        assert_eq!(root, target);
        assert_eq!(cursor, target_cursor);
        call(
            &mut owner,
            5000,
            LocalSubscriptionOperation::Cancel { lease },
        );
    }
}

fn publish_capability_change(owner: &mut EmptyOwner) -> Vec<backend_engine::CommittedViewDelta> {
    use backend_engine::builtin::{
        ProductSemanticCaptureOutcome, ProductSemanticPublicationKey, SemanticSourceCapture,
    };
    use backend_semantic::vocabulary::{LanguageProfile, PackageUrl, TypeScriptSource};
    let label = "pkg:npm/transport-capability-only@1.0.0";
    let key = ProductSemanticPublicationKey::new(
        backend_engine::PackageReference::parse(label.to_owned()).expect("package reference"),
        PackageUrl::parse(label.to_owned()).expect("package coordinate"),
        LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
    )
    .expect("capture key");
    let intent = BuiltinIntent::index_with_capture(
        backend_engine::PackageKey::from_value(label),
        label,
        Vec::new(),
        Vec::new(),
        vec![BuiltinCaptureChange {
            key,
            expected: None,
            capture: SemanticSourceCapture::new(None, [0xC1; 32], [0xC2; 32], 1, 1)
                .expect("capture"),
            outcome: ProductSemanticCaptureOutcome::Pending { prior: None },
            compiler_failure: None,
        }],
    )
    .expect("capture-only intent");
    commands::commit_builtin_intent(owner.daemon_mut(), 10, &intent)
        .expect("actual owner capture commit");
    rebind_published_view(owner.daemon_mut()).expect("production capability rebind")
}

#[test]
fn publication_transport_cancel_partial_reset_then_reopen() {
    let temp = tempfile::tempdir().expect("private workspace");
    let mut owner = owner(temp.path());
    publish(&mut owner, 1);
    let previous = owner.daemon().engine().daemon().library().cursor();
    publish(&mut owner, 478);
    let target = owner.daemon().engine().daemon().library().view().clone();
    let first = open(&mut owner, previous, 100);
    let LocalSubscriptionResponse::SnapshotPage {
        lease,
        next: Some(page),
        ..
    } = first
    else {
        panic!("partial reset");
    };
    call(
        &mut owner,
        101,
        LocalSubscriptionOperation::Cancel { lease },
    );
    let refused = owner
        .engine_prepared(
            102,
            EngineRequest::Subscription(LocalSubscriptionRequest {
                request_id: 102,
                operation: LocalSubscriptionOperation::Page {
                    lease,
                    page,
                    credit: backend_engine::MAX_SNAPSHOT_PAGE_ROWS,
                },
            }),
            FrameLimits::default(),
        )
        .expect_err("cancelled root is released");
    assert_eq!(
        refused,
        crate::ProtocolError::InvalidControl("unknown subscription lease")
    );
    let reopened = open(&mut owner, previous, 200);
    let (root, _, lease, pages) = admit_reset(&mut owner, previous, reopened, 200);
    assert_eq!(root, target);
    assert_eq!(pages, 2);
    call(
        &mut owner,
        5000,
        LocalSubscriptionOperation::Cancel { lease },
    );
}

#[test]
fn prepared_product_view_private_patch_and_reset_match_actual_publication() {
    for count in [1, backend_engine::MAX_VIEW_PATCH_ROWS + 1] {
        let directory = tempfile::tempdir().expect("actual private owner directory");
        let mut owner = owner(directory.path());
        publish(&mut owner, 1);
        let daemon = owner.daemon_mut();
        let prior_cursor = daemon.engine().daemon().library().cursor();
        let current = daemon.engine().daemon().library().view().clone();
        let mut rows = current.row_refs().cloned().collect::<Vec<_>>();
        rows.extend((0..count).map(|index| {
            let label = format!("private::prepared{index:06}");
            Row::new(
                backend_engine::RowId::Symbol(backend_engine::symbol_key(&label)),
                current.basis(),
                label,
            )
        }));
        let target = ViewRoot::new_checked(
            current.recipe(),
            current.basis(),
            current.frontier(),
            rows,
            current.coverage().to_vec(),
            current.capability().expect("actual admitted capability"),
        )
        .expect("checked private target");
        let mut preparation = BuiltinViewPreparation {
            snapshot: daemon.engine().daemon().owner().snapshot(),
            current: current.clone(),
            event: None,
        };
        let private = prepare_published_target(&mut preparation, current.clone(), target.clone())
            .expect("same production planner without borrowing daemon");
        assert_eq!(daemon.engine().daemon().library().view(), &current);
        assert_eq!(daemon.engine().daemon().library().cursor(), prior_cursor);
        assert_eq!(private.len(), 1);
        assert_eq!(
            private[0].is_compact_replayable(),
            count <= backend_engine::MAX_VIEW_PATCH_ROWS
        );
        let actual = commit_published_target(daemon, current, target)
            .expect("ordinary production publication");
        assert_eq!(
            daemon.engine().daemon().library().view(),
            &preparation.current
        );
        assert_eq!(actual[0].id(), private[0].id());
        assert_eq!(actual[0].base_root(), private[0].base_root());
        assert_eq!(actual[0].target_root(), private[0].target_root());
        assert!(
            matches!(preparation.event, Some(backend_engine::CursorEvent::View { delta }) if delta.id() == actual[0].id())
        );
    }
}

#[test]
fn prepared_product_view_noop_keeps_current_root_and_emits_no_event() {
    let directory = tempfile::tempdir().expect("actual private owner directory");
    let owner = owner(directory.path());
    let daemon = owner.daemon();
    let current = daemon.engine().daemon().library().view().clone();
    let mut preparation = BuiltinViewPreparation {
        snapshot: daemon.engine().daemon().owner().snapshot(),
        current: current.clone(),
        event: None,
    };
    let deltas = prepare_published_target(&mut preparation, current.clone(), current.clone())
        .expect("exact retained view is a no-op");
    assert!(deltas.is_empty());
    assert!(preparation.event.is_none());
    assert_eq!(preparation.current, current);
}

#[test]
fn prepared_product_view_stale_snapshot_refuses_new_owner_root_without_relabeling() {
    let directory = tempfile::tempdir().expect("actual private owner directory");
    let mut owner = owner(directory.path());
    publish(&mut owner, 1);
    let daemon = owner.daemon_mut();
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let old_root = snapshot.root();
    let current = daemon.engine().daemon().library().view().clone();
    let old_cursor = daemon.engine().daemon().library().cursor();
    let mut rows = current.row_refs().cloned().collect::<Vec<_>>();
    let label = "private::stale_prepared";
    rows.push(Row::new(
        backend_engine::RowId::Symbol(backend_engine::symbol_key(label)),
        current.basis(),
        label,
    ));
    let target = ViewRoot::new_checked(
        current.recipe(),
        current.basis(),
        current.frontier(),
        rows,
        current.coverage().to_vec(),
        current.capability().expect("old admitted capability"),
    )
    .expect("old checked target");
    let mut preparation = BuiltinViewPreparation {
        snapshot: snapshot.clone(),
        current: current.clone(),
        event: None,
    };
    let deltas = prepare_published_target(&mut preparation, current.clone(), target)
        .expect("old private transition");
    let prepared = PreparedBuiltinView {
        workspace_root: old_root,
        view: preparation.current,
        event: preparation.event,
        outcome: view_publish::PublicationOutcome {
            deltas,
            roots: view_publish::PublishedRoots {
                source: view_publish::source_root(&snapshot).expect("old source root"),
                semantic: view_publish::semantic_root(&snapshot).expect("old semantic root"),
                activated: Default::default(),
            },
            path: view_publish::PublicationPath::Reused,
        },
    };
    let no_op = PreparedBuiltinView {
        workspace_root: old_root,
        view: current.clone(),
        event: None,
        outcome: view_publish::PublicationOutcome {
            deltas: Vec::new(),
            roots: view_publish::PublishedRoots {
                source: view_publish::source_root(&snapshot).expect("old source root"),
                semantic: view_publish::semantic_root(&snapshot).expect("old semantic root"),
                activated: Default::default(),
            },
            path: view_publish::PublicationPath::Reused,
        },
    };
    let label = "pkg:cargo/later-owner-head@1.0.0";
    let intent = BuiltinIntent::add(backend_engine::package_key(label), label.to_owned())
        .expect("later actual intent");
    commands::commit_builtin_intent(daemon, 981, &intent).expect("later actual workspace commit");
    assert_ne!(daemon.engine().daemon().owner().snapshot().root(), old_root);
    assert!(
        install_prepared_builtin_view(daemon, prepared).is_err(),
        "old source capability cannot be relabeled as the current root"
    );
    assert!(
        install_prepared_builtin_view(daemon, no_op).is_err(),
        "even a no-op preparation must retain its original workspace authority"
    );
    assert_eq!(daemon.engine().daemon().library().view(), &current);
    assert_eq!(daemon.engine().daemon().library().cursor(), old_cursor);
}
