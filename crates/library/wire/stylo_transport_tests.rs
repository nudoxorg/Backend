//! CSS command wire admission and full reply envelope bounds.
use super::*;
use crate::stylo_query::*;
use crate::{CommandReply, PackageReference, SurfaceCommand, SurfaceReply};

fn gaps() -> Vec<StyloScopeGap> {
    vec![
        StyloScopeGap::AuthoredSourcePartial,
        StyloScopeGap::AuthoredReferenceSitesUnavailable,
        StyloScopeGap::OrdinaryVariableReferencesUnavailable,
        StyloScopeGap::NativeDeclarationNameDecodeUnavailable,
        StyloScopeGap::PreferenceStabilityUnknown,
        StyloScopeGap::ImportsNotResolved,
        StyloScopeGap::AncestorRuleContextNotReturned,
        StyloScopeGap::FrameworkNotInterpreted,
        StyloScopeGap::DomAndCascadeNotAttempted,
        StyloScopeGap::ProviderUnknown,
    ]
}
fn request() -> StyloQueryRequest {
    StyloQueryRequest {
        package: PackageReference::parse("/tmp/project").unwrap(),
        role: StyloNameRole::Class,
        name: "foo".into(),
        page_rows: 1,
        output_bytes: 131_072,
        scan_rows: 7,
        scan_bytes: 229_376,
        continuation: None,
    }
}
fn page(terminal: StyloQueryTerminal) -> StyloQueryPage {
    StyloQueryPage {
        basis: StyloQueryBasis {
            manifest_root: [1; 32],
            native_root: Some([2; 32]),
            attempt: StyloAttemptState::LatestObserved,
            worker_capability: StyloWorkerCapability::Unknown,
            host_terminal_summary: StyloHostTerminalSummary::Unknown,
            source_partial: true,
            gaps: gaps(),
            producer: Some([3; 32]),
            selection_complete: false,
        },
        hits: Vec::new(),
        terminal,
    }
}

#[test]
fn stylo_live_dto40_is_strict_and_journal39_remains_readable() {
    assert_eq!(crate::DTO_VERSION, 40);
    let command = CommandDto::new(17, crate::Command::Surface(SurfaceCommand::StyloQuery {
        request: request(),
    }));
    let current = serde_json::to_vec(&command).expect("current command");
    let mut old: serde_json::Value = serde_json::from_slice(&current).expect("command JSON");
    old["version"] = serde_json::json!(39);
    let error = crate::decode_command_body(&serde_json::to_vec(&old).unwrap())
        .expect_err("old live command must be refused");
    assert!(error.contains("command DTO version 39"));
    assert!(error.contains("this build supports 40"));
    let mut future = old;
    future["version"] = serde_json::json!(41);
    assert!(crate::decode_command_body(&serde_json::to_vec(&future).unwrap()).is_err());
    let mut unknown: serde_json::Value = serde_json::from_slice(&current).unwrap();
    unknown["unexpected"] = serde_json::json!(true);
    assert!(crate::decode_command_body(&serde_json::to_vec(&unknown).unwrap()).is_err());
    let text = String::from_utf8(current).unwrap();
    let duplicate = text.replacen("\"version\":40", "\"version\":40,\"version\":40", 1);
    assert_ne!(duplicate, text);
    assert!(crate::decode_command_body(duplicate.as_bytes()).is_err());
    let journal = crate::JournalViewGrammarV3::from_checked_container(3).unwrap();
    assert!(journal.check_envelope_version(35).is_ok());
    assert!(journal.check_envelope_version(39).is_ok());
    assert!(journal.check_envelope_version(40).is_ok());
    assert!(journal.check_envelope_version(41).is_err());
}
#[test]
fn native_wire_replies_keep_scope_and_fit_actual_envelope() {
    let request = request();
    let cursor = StyloQueryCursor::issue(
        StyloCursorClaims {
            manifest_root: [1; 32],
            native_root: [2; 32],
            project: [3; 32],
            role: request.role,
            name: request.name.clone(),
            page_rows: request.page_rows,
            output_bytes: request.output_bytes,
            scan_rows: request.scan_rows,
            scan_bytes: request.scan_bytes,
            after: [4; 32],
        },
        &[5; 32],
    )
    .unwrap();
    for terminal in [
        StyloQueryTerminal::Complete,
        StyloQueryTerminal::More {
            continuation: cursor,
            work_limited: true,
        },
    ] {
        let page = page(terminal);
        let command = SurfaceCommand::StyloQuery {
            request: request.clone(),
        };
        let reply = SurfaceReply::StyloQuery(page.clone());
        admit_surface_reply(&command, &reply).unwrap();
        let envelope = ReplyDto::new(u64::MAX, CommandReply::Surface(reply));
        let bytes = encode_reply_body_bounded(&envelope).unwrap();
        assert!(bytes.len() <= request.output_bytes as usize);
        assert!(bytes.len() <= serde_json::to_vec(&page).unwrap().len() + 256);
    }
    let mut forged = page(StyloQueryTerminal::Complete);
    forged.basis.gaps.clear();
    assert!(
        SurfaceReply::StyloQuery(forged)
            .admit(crate::CommandId::StyloQuery)
            .is_err()
    );
    let mut absent = page(StyloQueryTerminal::Complete);
    absent.basis.attempt = StyloAttemptState::NoSelectedAnalysis;
    absent.basis.producer = None;
    assert!(
        SurfaceReply::StyloQuery(absent)
            .admit(crate::CommandId::StyloQuery)
            .is_err()
    );
    let mut legacy = page(StyloQueryTerminal::Complete);
    legacy.basis.attempt = StyloAttemptState::UnknownLegacy;
    legacy.basis.native_root = None;
    assert!(
        SurfaceReply::StyloQuery(legacy)
            .admit(crate::CommandId::StyloQuery)
            .is_err()
    );
    let mut oversized = page(StyloQueryTerminal::Complete);
    oversized.hits.push(StyloQueryHit::NativeSelector {
        id: [6; 32],
        path: "site.css".into(),
        rule: backend_semantic::stylo_analysis::StyloRule {
            path: vec![0],
            kind: backend_semantic::stylo_analysis::StyloRuleKind::Style,
            native_kind: "Style".into(),
            condition: None,
            location: None,
        },
        selector: backend_semantic::stylo_analysis::StyloSelector {
            rule_path: vec![0],
            ordinal: 0,
            canonical: "x".repeat(4 * 1024 * 1024),
            specificity: 0,
            components: Vec::new(),
            component_coverage: backend_semantic::stylo_analysis::StyloDependencyCoverage::Unknown,
        },
    });
    assert!(
        SurfaceReply::StyloQuery(oversized)
            .admit(crate::CommandId::StyloQuery)
            .is_err()
    );
    let mut wrong_name = page(StyloQueryTerminal::Complete);
    wrong_name.hits.push(StyloQueryHit::NativeSelector {
        id: [7; 32],
        path: "site.css".into(),
        rule: backend_semantic::stylo_analysis::StyloRule {
            path: vec![0],
            kind: backend_semantic::stylo_analysis::StyloRuleKind::Style,
            native_kind: "Style".into(),
            condition: None,
            location: None,
        },
        selector: backend_semantic::stylo_analysis::StyloSelector {
            rule_path: vec![0],
            ordinal: 0,
            canonical: ".bar".into(),
            specificity: 0,
            components: vec![backend_semantic::stylo_analysis::StyloSelectorComponent {
                kind: backend_semantic::stylo_analysis::StyloSelectorComponentKind::Class,
                name: "bar".into(),
            }],
            component_coverage: backend_semantic::stylo_analysis::StyloDependencyCoverage::Unknown,
        },
    });
    let reply = SurfaceReply::StyloQuery(wrong_name);
    assert!(reply.admit(crate::CommandId::StyloQuery).is_ok());
    assert!(admit_surface_reply(&SurfaceCommand::StyloQuery { request }, &reply).is_err());
}
