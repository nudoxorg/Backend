//! Real captured42 bytes support transport controls only, not native publication/latency claims.
use super::*;
use backend_library::{
    SemanticSourceReference, SemanticVersionDescriptor, SemanticVersionRecord, SemanticVersionsPage,
};
use serde_json::{Value, json};
fn captured() -> Result<SemanticVersionRecord, String> {
    let packet: Value = fixture(serde_json::from_str(include_str!(
        "../../../../crates/present/fixtures/semantic-versions-semver-public.json"
    )))?;
    fixture(serde_json::from_value(packet["surface"]["data"][0].clone()))
}
pub(super) fn page(
    limit: u16,
    offset: u64,
    total: u64,
    root: u8,
) -> Result<SemanticVersionsPage, String> {
    let original = captured()?;
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
            reference: fixture(SemanticSourceReference::capture(
                &record, [root; 32], [34; 32], &[7; 32],
            ))?,
        });
    }
    let end = offset + entries.len() as u64;
    let more = end < total;
    let next_cursor = if more {
        let metadata = entries
            .last()
            .ok_or_else(|| "continuation requires a nonempty page".to_owned())?
            .metadata();
        Some(fixture(backend_library::SemanticVersionsCursor::issue(
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
        ))?)
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
    fixture(page.admit())?;
    Ok(page)
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
        let offset = match cursor {
            Some(cursor) => {
                cursor
                    .claims()
                    .map_err(|error| ClientError::Protocol(format!("{error:?}")))?
                    .offset
            }
            None => 0,
        };
        let root = if self.moved && !self.calls.is_empty() {
            9
        } else {
            8
        };
        let reply = SurfaceReply::SemanticVersionsPage(
            page(*limit, offset, 10, root).map_err(ClientError::Protocol)?,
        );
        self.calls.push(command);
        Ok(reply)
    }
}
fn encoded(answer: &Answer, budget: usize) -> Result<(), Fault> {
    backend_present::encode_answer(answer, backend_present::Detail::Full, None, budget)
        .map(|_| ())
        .map_err(backend_present::oversized_fault)
}
fn fixture<T, E: std::fmt::Debug>(result: Result<T, E>) -> Result<T, String> {
    result.map_err(|error| format!("{error:?}"))
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
fn cli_versions_default_and_actual_serialized_budget_keep_complete_pages_and_effective_credit()
-> Result<(), String> {
    let package = captured()?.package;
    let credit = backend_library::DEFAULT_SEMANTIC_VERSIONS_LIMIT;
    let words = vec!["semantic-versions".to_owned(), package.as_str().to_owned()];
    let invocation = fixture(crate::invoke::parse(&words, None))?;
    let command = fixture(backend_present::lower(&invocation, "/fixture"))?;
    assert!(
        matches!(command,Request::Surface(command) if matches!(*command,SurfaceCommand::SemanticVersions{limit,cursor:None,..} if limit==credit))
    );
    let surface: SurfaceCommand = fixture(serde_json::from_value(
        json!({"operation":"semantic-versions","package":package}),
    ))?;
    assert!(
        matches!(surface,SurfaceCommand::SemanticVersions{limit,cursor:None,..} if limit==credit)
    );
    // A stricter caller budget is derived from the complete real serialization,
    // not a fabricated oversized fault or a claimed production performance.
    let full = Answer::Product(Box::new(backend_present::product_view(
        &SurfaceReply::SemanticVersionsPage(page(credit, 0, 10, 8)?),
    )));
    let bytes = fixture(backend_present::encode_answer(
        &full,
        backend_present::Detail::Full,
        None,
        backend_present::MAX_RESPONSE_BUDGET_BYTES,
    ))?
    .bytes
    .len();
    for budget in [bytes / 2, backend_present::DEFAULT_RESPONSE_BUDGET_BYTES] {
        let mut engine = Versions {
            calls: vec![],
            moved: false,
        };
        let answer = fixture(versions_with_output_admission(
            &mut engine,
            &package,
            credit,
            None,
            || Ok(()),
            |answer| encoded(answer, budget),
        ))?;
        let actual = selected_page(&answer);
        let next = actual
            .next_cursor
            .as_ref()
            .ok_or("admitted first page must have a continuation")?;
        let claims = fixture(next.claims())?;
        assert_eq!(actual, &page(claims.limit, 0, 10, 8)?);
        assert_eq!(claims.offset, actual.entries.len() as u64);
        if budget == bytes / 2 {
            assert!(engine.calls.len() > 1);
            assert!(claims.limit < credit);
        }
        fixture(encoded(&answer, budget))?;
        let Answer::Product(view) = &answer else {
            unreachable!()
        };
        let shell = view
            .semantic_versions_cli_continuation(backend_present::Detail::Full)
            .ok_or("admitted cursor must produce a copyable CLI continuation")?;
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
        let continued = fixture(versions_with_output_admission(
            &mut engine,
            &package,
            claims.limit,
            Some(next),
            || Ok(()),
            |answer| encoded(answer, backend_present::DEFAULT_RESPONSE_BUDGET_BYTES),
        ))?;
        assert_eq!(selected_page(&continued).offset, claims.offset);
        assert!(
            matches!(engine.calls.last(),Some(SurfaceCommand::SemanticVersions{limit,cursor:Some(cursor),..}) if *limit==claims.limit && cursor==next)
        );
    }
    Ok(())
}

#[test]
fn cli_versions_resizing_refuses_changed_basis_fixed_cursor_and_expired_original_caller()
-> Result<(), String> {
    let package = captured()?.package;
    let credit = backend_library::DEFAULT_SEMANTIC_VERSIONS_LIMIT;
    let full = Answer::Product(Box::new(backend_present::product_view(
        &SurfaceReply::SemanticVersionsPage(page(credit, 0, 10, 8)?),
    )));
    let bytes = fixture(backend_present::encode_answer(
        &full,
        backend_present::Detail::Full,
        None,
        backend_present::MAX_RESPONSE_BUDGET_BYTES,
    ))?
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
    let cursor = page(credit, 0, 10, 8)?
        .next_cursor
        .ok_or("partial fixture page must have a cursor")?;
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
    assert!(lifetime.read_deadline(std::time::Duration::ZERO).is_err());
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
    Ok(())
}

#[test]
fn cli_versions_summary_keeps_exact_continuation_and_explicit_full_discovery() -> Result<(), String> {
    let package = captured()?.package;
    let mut engine = Versions { calls: vec![], moved: false };
    let answer = fixture(versions_with_output_admission(
        &mut engine, &package, 2, None, || Ok(()),
        |answer| backend_present::encode_answer(answer, backend_present::Detail::Summary, None,
            backend_present::DEFAULT_RESPONSE_BUDGET_BYTES).map(|_| ()).map_err(backend_present::oversized_fault),
    ))?;
    assert_eq!(engine.calls.len(), 1, "summary formatting does not reread or resize a small page");
    let selected = selected_page(&answer);
    let next = selected.next_cursor.as_ref().ok_or("actual native next cursor")?;
    let claims = fixture(next.claims())?;
    assert_eq!(claims.limit, 2);
    assert_eq!(claims.offset, selected.entries.len() as u64);
    let summary = fixture(backend_present::encode_answer(&answer, backend_present::Detail::Summary,
        Some(next.as_str()), backend_present::MAX_RESPONSE_BUDGET_BYTES))?;
    let full = fixture(backend_present::encode_answer(&answer, backend_present::Detail::Full,
        Some(next.as_str()), backend_present::MAX_RESPONSE_BUDGET_BYTES))?;
    let projected: Value = fixture(serde_json::from_slice(&summary.bytes))?;
    assert_eq!(projected["nextCursor"], next.as_str());
    assert!(projected.get("semantic_data").is_none());
    let display = &projected["semantic_versions_summary"];
    assert_eq!(display["entries"].as_array().ok_or("all summary rows")?.len(), selected.entries.len());
    assert_eq!(display["full_detail"]["scope"], "fresh-discovery");
    assert_eq!(display["full_detail"]["call"]["arguments"]["command"]["package"], json!(package));
    let words = vec!["semantic-versions".to_owned(), package.as_str().to_owned(),
        "--detail".to_owned(), "full".to_owned()];
    let full_invocation = fixture(crate::invoke::parse(&words, None))?;
    assert!(matches!(fixture(backend_present::lower(&full_invocation, "/fixture"))?,
        Request::Surface(command) if matches!(*command,SurfaceCommand::SemanticVersions { package: actual,cursor:None,.. } if actual == package)));
    let exact: Value = fixture(serde_json::from_slice(&full.bytes))?;
    let restored: SemanticVersionsPage = fixture(serde_json::from_value(exact["semantic_data"]["value"].clone()))?;
    assert_eq!(&restored, selected, "full output still owns every exact native reference");
    assert!(summary.bytes.len() < full.bytes.len());
    for rendered in [backend_present::text::answer_with_detail(&answer, backend_present::Theme::plain(),
        backend_present::Detail::Summary,None,backend_present::ContinuationTarget::CliOption),
        backend_present::markdown::answer_with_detail(&answer,backend_present::Detail::Summary,
        None,backend_present::ContinuationTarget::CliOption)] {
        assert!(rendered.contains("--detail full (fresh discovery)"));
        assert!(rendered.contains(next.as_str()), "exact original continuation survives");
    }
    Ok(())
}
