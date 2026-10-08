//! Controls cross the shared parsing, projection and actionable next-command seams.
#![allow(clippy::expect_used)]

use crate::*;
use backend_library::{
    DependencyFacts, IndexJobObservation, IndexJobStage, IndexJobTicket, IndexProgressPage,
    PackageReference, ProductText, SurfaceCommand, SurfaceReply,
};

fn ticket() -> IndexJobTicket {
    IndexJobTicket::new(
        std::num::NonZeroU64::new(7).expect("counter"),
        [3; 16],
        PackageReference::parse("/workspace/project").expect("package"),
    )
}

#[test]
fn copied_ticket_objects_and_strings_share_strict_admission_and_guidance() {
    let ticket = ticket();
    let object = serde_json::to_value(&ticket).expect("ticket object");
    let string = serde_json::Value::String(serde_json::to_string(&ticket).expect("copied ticket"));
    assert_eq!(decode_index_job_ticket(&object).expect("object"), ticket);
    assert_eq!(decode_index_job_ticket(&string).expect("string"), ticket);
    let epoch = [3; 16];
    for invalid in [
        serde_json::Value::Null,
        serde_json::json!(7),
        serde_json::json!("7"),
        serde_json::json!({"id": 7, "owner_epoch": [3], "package": {"kind": "local", "value": "/workspace/project"}}),
        serde_json::json!({"id": 0, "owner_epoch": epoch, "package": {"kind": "local", "value": "/workspace/project"}}),
        serde_json::Value::String(" ".repeat(backend_library::MAX_COMMAND_TEXT + 1)),
    ] {
        let fault = decode_index_job_ticket(&invalid).expect_err("invalid ticket");
        assert_eq!(fault.slug(), FaultSlug::Usage);
        assert_eq!(fault.operand().render(), "ticket");
        assert!(
            fault
                .cause()
                .sentence()
                .contains("id, owner_epoch and package unchanged")
        );
    }
    let mut edited = object;
    edited["unexpected"] = serde_json::json!(true);
    assert!(
        decode_index_job_ticket(&edited).is_err(),
        "unknown fields never become a ticket"
    );
}

#[test]
fn accepted_pending_work_offers_an_exact_executable_poll_without_becoming_a_fault() {
    let ticket = ticket();
    let view = product_view(&SurfaceReply::IndexProgress(IndexJobObservation::Pending(
        IndexProgressPage {
            ticket: ticket.clone(),
            stage: IndexJobStage::Compiling,
            events: Box::new([]),
            next_sequence: 19,
            truncated: false,
            has_more: false,
        },
    )));
    let answer = Answer::Product(Box::new(view.clone()));
    assert!(answer.fault().is_none());
    let next = view
        .index_job()
        .expect("job")
        .poll_affordance()
        .expect("next action");
    assert_eq!(
        next.tool_call().expect("tool"),
        serde_json::json!({
            "name": "backend.index_progress", "arguments": {"ticket": ticket, "after_sequence": 19}
        })
    );
    let shell = next.shell().expect("shell");
    assert!(shell.contains("backend index_progress '"));
    assert!(shell.ends_with("--after-sequence 19"));
    #[cfg(unix)]
    {
        let script = format!(r#"backend() {{ printf '%s\000' "$@"; }}; {shell}"#);
        let parsed = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .output()
            .expect("actual executable affordance argv");
        assert!(parsed.status.success());
        let argv = parsed
            .stdout
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| std::str::from_utf8(s).expect("UTF-8 argv"))
            .collect::<Vec<_>>();
        assert_eq!(argv.len(), 4);
        assert_eq!(argv[0], "index_progress");
        let actual: IndexJobTicket =
            serde_json::from_str(argv[1]).expect("strict exact ticket JSON");
        assert_eq!(actual, ticket);
        assert_eq!(&argv[2..], &["--after-sequence", "19"]);
    }

    let markdown = markdown::product(&view);
    assert!(markdown.contains("Operation in flight"));
    assert!(markdown.contains("\"after_sequence\":19"));
}

#[test]
fn dependency_failure_preserves_requested_package_and_does_not_invent_an_sdk_cause() {
    let package = PackageReference::parse("pkg:npm/react@19.1.0").expect("package");
    for facts in [
        DependencyFacts::Unknown(ProductText::from_static("package is not recorded")),
        DependencyFacts::Unavailable(ProductText::from_static("dependency evidence is absent")),
    ] {
        let view = product_view_for_command(
            &SurfaceCommand::Dependencies {
                package: package.clone(),
            },
            &SurfaceReply::Dependencies(facts),
        );
        let answer = Answer::Product(Box::new(view));
        let fault = answer.fault().expect("admitted answer carries refusal");
        assert_eq!(fault.slug(), FaultSlug::LaneUnavailable);
        assert_eq!(fault.cause().slug(), CauseSlug::NotCaptured);
        assert_eq!(fault.operand().render(), package.as_str());
        assert_eq!(
            fault.affordance().tool_call().expect("inspect package"),
            serde_json::json!({
                "name": "backend.package", "arguments": {"package": package.as_str()}
            })
        );
        assert!(markdown::answer(&answer).contains(package.as_str()));
    }
}

#[test]
fn missing_materialization_does_not_promise_an_active_index_job() {
    let absent = Fault::lane(
        backend_library::Lane::Names,
        backend_library::Reason::NoIndex,
    );
    assert_eq!(absent.cause().slug(), CauseSlug::Absent);
    assert_eq!(
        absent.affordance().tool_call().expect("inspect shelf")["name"],
        "backend.packages"
    );
    let incomplete = Fault::lane(
        backend_library::Lane::Names,
        backend_library::Reason::Incomplete,
    );
    assert_eq!(incomplete.cause().slug(), CauseSlug::Indexing);
    assert_eq!(
        incomplete.affordance().tool_call().expect("inspect status")["name"],
        "backend.status"
    );
}

#[test]
fn available_health_distinguishes_empty_publication_from_native_semantic_readiness() {
    use backend_library::{
        Basis, Cursor, Frontier, HealthReport, Library, Row, RowId, ViewRoot, package_key, view_key,
    };
    let empty = Library::new();
    let basis = empty.view().basis();
    let root = ViewRoot::new_incomplete(
        view_key(b"status-publication-control"),
        Basis::new(basis.root, basis.object),
        Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
        vec![Row::new(
            RowId::Package(package_key("/workspace/project")),
            basis,
            "/workspace/project",
        )],
        empty.view().coverage().to_vec(),
    )
    .expect("structural package row claims no compiler coverage");
    let populated =
        Library::from_view(root.clone(), Cursor::for_view_root(&root)).expect("checked view");
    for (library, expected, rows) in [
        (&empty, VisiblePublicationState::Empty, 0),
        (&populated, VisiblePublicationState::Populated, 1),
    ] {
        let report = HealthReport::from_root(library.view(), library.cursor());
        assert_eq!(
            Status::from_report(&report, None).availability(),
            "unknown",
            "a report constructor alone never proves owner availability"
        );
        let status = Status::from_health_reply(&report, None);
        assert_eq!(status.availability(), "available");
        assert_eq!(status.publication_state(), expected);
        assert_eq!(status.rows().get(), rows);
        assert_eq!(status.capabilities().oracles().ready().get(), 0);
        let rendered = markdown::status(&status);
        assert!(rendered.contains(&format!(
            "owner available · publication {} · lanes {}",
            expected.name(),
            status.readiness()
        )));
        for detail in [Detail::Summary, Detail::Standard, Detail::Full] {
            let encoded = encode_answer(
                &Answer::Status(Box::new(status.clone())),
                detail,
                None,
                DEFAULT_RESPONSE_BUDGET_BYTES,
            )
            .expect("bounded status");
            let value: serde_json::Value =
                serde_json::from_slice(&encoded.bytes).expect("status JSON");
            assert_eq!(value["availability"], "available");
            assert_eq!(value["publication"], expected.name());
            assert_eq!(value["readiness"], status.readiness());
            assert_eq!(value["rows"], rows);
            assert_eq!(value["capabilities"]["oracles_ready"], 0);
        }
    }
}

#[test]
fn historical_status_dtos_default_missing_facets_to_unknown_not_empty_or_available() {
    let library = backend_library::Library::new();
    let report = backend_library::HealthReport::from_root(library.view(), library.cursor());
    let mut value = serde_json::to_value(StatusDto::new(&Status::from_report(&report, None)))
        .expect("status DTO");
    let object = value.as_object_mut().expect("DTO fields");
    object.remove("availability");
    object.remove("publication");
    let historical: StatusDto = serde_json::from_value(value).expect("historical projection");
    assert_eq!(historical.availability, "unknown");
    assert_eq!(historical.publication, "unknown");
}
