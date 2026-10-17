//! Real captured42 bytes support transport controls only, not native publication/latency claims.
use super::*;
use backend_library::{
    PackageReference, SemanticSourceReference, SemanticVersionDescriptor, SemanticVersionRecord,
    SemanticVersionsPage,
};
use serde_json::{Value, json};
fn captured() -> SemanticVersionRecord {
    let packet: Value = serde_json::from_str(include_str!(
        "../../../../crates/present/fixtures/semantic-versions-semver-public.json"
    ))
    .expect("unchanged real42 capture");
    serde_json::from_value(packet["surface"]["data"][0].clone()).expect("complete typed record")
}
pub(super) fn page(limit: u16, offset: u64, total: u64, root: u8) -> SemanticVersionsPage {
    let original = captured();
    let mut entries = Vec::new();
    for ordinal in offset..total.min(offset + u64::from(limit)) {
        let mut record = original.clone();
        if ordinal != 0 {
            // Strict native key order after the real capture's first member.
            let mut generation = original.generation.to_bytes();
            let mut carry = ordinal;
            for byte in generation.iter_mut().rev() {
                carry += u64::from(*byte);
                *byte = carry as u8;
                carry >>= 8;
            }
            assert_eq!(carry, 0, "synthetic key increment cannot wrap");
            record.generation = backend_library::SemanticGenerationId::new(generation);
            record.selected = false;
            record.selected_source_frontier = None;
            record.history_status = backend_library::SemanticHistoryPublicationStatus::NotSelected;
        }
        if let Some(frontier) = &mut record.selected_source_frontier {
            frontier.source_relation_root = [root; 32];
        }
        entries.push(SemanticVersionDescriptor::Available {
            reference: SemanticSourceReference::capture(&record, [root; 32], [34; 32], &[7; 32])
                .expect("sealed synthetic page observation"),
        });
    }
    let end = offset + entries.len() as u64;
    let more = end < total;
    let next_cursor = if more {
        let metadata = entries.last().expect("nonempty page").metadata();
        Some(
            backend_library::SemanticVersionsCursor::issue(
                backend_library::SemanticVersionsCursorClaims {
                    package: original.package.clone(),
                    source_relation_root: [root; 32],
                    semantic_relation_root: [34; 32],
                    coordinate: metadata.coordinate.clone(),
                    profile: metadata.profile,
                    generation: metadata.generation,
                    limit,
                    total,
                    offset: end,
                },
                &[7; 32],
            )
            .expect("synthetic native transport continuation"),
        )
    } else {
        None
    };
    let page = SemanticVersionsPage {
        package: original.package.clone(),
        source_relation_root: [root; 32],
        semantic_relation_root: [34; 32],
        total,
        offset,
        entries: entries.into_boxed_slice(),
        next_cursor,
        more,
        complete: !more,
    };
    page.admit().expect("self-consistent native transport page");
    page
}

struct Versions {
    calls: Vec<SurfaceCommand>,
    moved: bool,
}
impl Engine for Versions {
    fn revision(&mut self) -> Result<ViewStateRoot, ClientError> {
        unreachable!("native descriptor page carries its roots")
    }
    fn health(&mut self) -> Result<HealthReport, ClientError> {
        unreachable!("no health read")
    }
    fn probe(&mut self, _: Probe<'_>) -> Result<ReplyDto, ClientError> {
        unreachable!("only typed surface reads")
    }
    fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
        let SurfaceCommand::SemanticVersions { limit, cursor, .. } = &command else {
            panic!("semantic versions only")
        };
        let offset = cursor
            .as_ref()
            .map_or(0, |cursor| cursor.claims().unwrap().offset);
        let root = if self.moved && !self.calls.is_empty() {
            9
        } else {
            8
        };
        let reply = SurfaceReply::SemanticVersionsPage(page(*limit, offset, 10, root));
        self.calls.push(command);
        Ok(reply)
    }
}
fn encoded(answer: &Answer, budget: usize) -> Result<(), Fault> {
    backend_present::encode_answer(answer, backend_present::Detail::Full, None, budget)
        .map(|_| ())
        .map_err(backend_present::oversized_fault)
}
fn selected_page(answer: &Answer) -> &SemanticVersionsPage {
    let Answer::Product(view) = answer else {
        panic!("product")
    };
    let Some(backend_present::ProductSemanticData::VersionsPage(page)) = view.semantic_data()
    else {
        panic!("exact native page")
    };
    page
}

#[test]
fn cli_versions_default_and_actual_serialized_budget_keep_complete_pages_and_effective_credit() {
    let package = captured().package;
    let credit = backend_library::DEFAULT_SEMANTIC_VERSIONS_LIMIT;
    let words = vec!["semantic-versions".to_owned(), package.as_str().to_owned()];
    let invocation = crate::invoke::parse(&words, None).expect("CLI shared default");
    let command = backend_present::lower(&invocation, "/fixture").unwrap();
    assert!(
        matches!(command,Request::Surface(command) if matches!(*command,SurfaceCommand::SemanticVersions{limit,cursor:None,..} if limit==credit))
    );
    let surface: SurfaceCommand =
        serde_json::from_value(json!({"operation":"semantic-versions","package":package})).unwrap();
    assert!(
        matches!(surface,SurfaceCommand::SemanticVersions{limit,cursor:None,..} if limit==credit)
    );
    // A stricter caller budget is derived from the complete real serialization,
    // not a fabricated oversized fault or a claimed production performance.
    let full = Answer::Product(Box::new(backend_present::product_view(
        &SurfaceReply::SemanticVersionsPage(page(credit, 0, 10, 8)),
    )));
    let bytes = backend_present::encode_answer(
        &full,
        backend_present::Detail::Full,
        None,
        backend_present::MAX_RESPONSE_BUDGET_BYTES,
    )
    .unwrap()
    .bytes
    .len();
    for budget in [bytes / 2, backend_present::DEFAULT_RESPONSE_BUDGET_BYTES] {
        let mut engine = Versions {
            calls: vec![],
            moved: false,
        };
        let answer = versions_with_output_admission(
            &mut engine,
            &package,
            credit,
            None,
            || Ok(()),
            |answer| encoded(answer, budget),
        )
        .expect("complete admitted page");
        let actual = selected_page(&answer);
        let next = actual.next_cursor.as_ref().unwrap();
        let claims = next.claims().unwrap();
        assert_eq!(actual, &page(claims.limit, 0, 10, 8));
        assert_eq!(claims.offset, actual.entries.len() as u64);
        if budget == bytes / 2 {
            assert!(engine.calls.len() > 1);
            assert!(claims.limit < credit);
        }
        encoded(&answer, budget).unwrap();
        let Answer::Product(view) = &answer else {
            unreachable!()
        };
        let shell = view
            .semantic_versions_cli_continuation(backend_present::Detail::Full)
            .unwrap();
        assert!(shell.contains(&format!("--limit {}", claims.limit)));
        assert!(shell.contains(next.as_str()));
        for text in [
            backend_present::markdown::answer_with_detail(
                &answer,
                backend_present::Detail::Full,
                None,
                backend_present::ContinuationTarget::CliOption,
            ),
            backend_present::text::answer_with_detail(
                &answer,
                backend_present::Theme::plain(),
                backend_present::Detail::Full,
                None,
                backend_present::ContinuationTarget::CliOption,
            ),
        ] {
            assert!(text.contains(&shell));
        }
        let continued = versions_with_output_admission(
            &mut engine,
            &package,
            claims.limit,
            Some(next),
            || Ok(()),
            |answer| encoded(answer, backend_present::DEFAULT_RESPONSE_BUDGET_BYTES),
        )
        .expect("same native credit");
        assert_eq!(selected_page(&continued).offset, claims.offset);
        assert!(
            matches!(engine.calls.last(),Some(SurfaceCommand::SemanticVersions{limit,cursor:Some(cursor),..}) if *limit==claims.limit && cursor==next)
        );
    }
}

#[test]
fn cli_versions_resizing_refuses_changed_basis_fixed_cursor_and_expired_original_caller() {
    let package = captured().package;
    let credit = backend_library::DEFAULT_SEMANTIC_VERSIONS_LIMIT;
    let full = Answer::Product(Box::new(backend_present::product_view(
        &SurfaceReply::SemanticVersionsPage(page(credit, 0, 10, 8)),
    )));
    let bytes = backend_present::encode_answer(
        &full,
        backend_present::Detail::Full,
        None,
        backend_present::MAX_RESPONSE_BUDGET_BYTES,
    )
    .unwrap()
    .bytes
    .len();
    let mut moved = Versions {
        calls: vec![],
        moved: true,
    };
    assert!(
        versions_with_output_admission(
            &mut moved,
            &package,
            credit,
            None,
            || Ok(()),
            |answer| encoded(answer, bytes / 2)
        )
        .is_err()
    );
    assert_eq!(
        moved.calls.len(),
        2,
        "changed roots never lend their rows to an earlier page"
    );
    let cursor = page(credit, 0, 10, 8).next_cursor.unwrap();
    let mut fixed = Versions {
        calls: vec![],
        moved: false,
    };
    assert!(
        versions_with_output_admission(
            &mut fixed,
            &package,
            credit,
            Some(&cursor),
            || Ok(()),
            |answer| encoded(answer, 1)
        )
        .is_err()
    );
    assert_eq!(
        fixed.calls.len(),
        1,
        "a cursor's fixed credit cannot resize"
    );
    let lifetime = backend_client::RequestContext::new();
    lifetime
        .read_deadline(std::time::Duration::ZERO)
        .unwrap_err();
    let mut stopped = Versions {
        calls: vec![],
        moved: false,
    };
    assert!(
        versions_with_output_admission(
            &mut stopped,
            &package,
            credit,
            None,
            || lifetime
                .check()
                .map_err(|error| Fault::from_client_error(&error, backend_present::Operand::Whole)),
            |_| Ok(())
        )
        .is_err()
    );
    assert!(
        stopped.calls.is_empty(),
        "stopped caller cannot receive a fresh owner lifetime"
    );
}
