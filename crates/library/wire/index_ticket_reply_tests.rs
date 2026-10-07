//! Shape fixtures carry no runtime authority. Each negative is valid on its own
//! and decoded through the public wire boundary before caller admission rejects it.

use crate::{
    Command, CommandDto, CommandReply, IndexCancelReceipt, IndexCancelStatus, IndexJobObservation,
    IndexJobOutcome, IndexJobPartialPublication, IndexJobProgressEvent, IndexJobProgressKind,
    IndexJobStage, IndexJobTerminal, IndexJobTicket, IndexOperationObservation,
    IndexOperationProfileRefusal, IndexOperationPublicationReceipt, IndexOperationSemanticCoverage,
    IndexOperationSemanticProfileState, IndexOperationSemanticUnavailableReason,
    IndexOperationSourceProfile, IndexProgressPage, IndexSourceCaptureSummary, PackageReference,
    ProductText, ReplyDto, SemanticLanguageProfile, SurfaceCommand, SurfaceReply,
};

fn ticket(counter: u64, epoch: u8, package: &str) -> IndexJobTicket {
    IndexJobTicket::new(
        core::num::NonZeroU64::new(counter).expect("nonzero counter"),
        [epoch; 16],
        PackageReference::parse(package).expect("package"),
    )
}

fn partial(ticket: &IndexJobTicket) -> IndexJobPartialPublication {
    let basis = crate::Basis::new(
        crate::view_state_root(&[]),
        crate::object_version(b"ticket-admission-source"),
    );
    let view = crate::ViewRoot::new_incomplete(
        crate::view_key(b"ticket-admission-view"),
        basis,
        crate::Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
        Vec::new(),
        Vec::new(),
    )
    .expect("explicitly incomplete fixture view");
    let refused = SemanticLanguageProfile::from_name("typescript").expect("TypeScript profile");
    let mut profiles = vec![
        IndexOperationSourceProfile {
            profile: SemanticLanguageProfile::from_name("python").expect("Python profile"),
            source_version: [4; 32],
            input_digest: [5; 32],
            observation_sequence: 2,
            source_count: 2,
            state: IndexOperationSemanticProfileState::Published {
                generation: [6; 32],
                coverage: IndexOperationSemanticCoverage::Complete,
            },
        },
        IndexOperationSourceProfile {
            profile: refused,
            source_version: [4; 32],
            input_digest: [7; 32],
            observation_sequence: 2,
            source_count: 3,
            state: IndexOperationSemanticProfileState::Unavailable {
                reason: IndexOperationSemanticUnavailableReason::Rejected,
            },
        },
    ];
    profiles.sort_by_key(|profile| profile.profile);
    IndexJobPartialPublication {
        package: ticket.package().clone(),
        receipt: IndexOperationPublicationReceipt::from_published_view(
            Some([8; 32]),
            [9; 32],
            [10; 32],
            3,
            &view,
            crate::Cursor::for_view_root(&view),
        )
        .expect("structural publication receipt"),
        source_capture: IndexSourceCaptureSummary {
            producer_package: ticket.package().clone(),
            request_identity: [1; 32],
            commit_identity: [2; 32],
            workspace_root: [3; 32],
            workspace_sequence: 2,
            profiles: profiles.into_boxed_slice(),
        },
        refused_profiles: vec![IndexOperationProfileRefusal {
            profile: refused,
            reason: IndexOperationSemanticUnavailableReason::Rejected,
            compiler_failure: None,
        }]
        .into_boxed_slice(),
    }
}

fn outcomes(ticket: &IndexJobTicket) -> Vec<IndexJobOutcome> {
    use crate::interface::{CompilerAttempt, CompilerFragmentFailure, SourceAuthority};
    use backend_semantic::ir::{BuildError, EntityId};
    let attempt = CompilerAttempt {
        source: SourceAuthority {
            identity: backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(b"source"),
            byte_len: 6,
        },
        recipe: backend_version::ContentId::<backend_version::CompileRecipeDomain>::from_canonical_bytes(b"recipe"),
    };
    let fault = CompilerFragmentFailure::build(BuildError::InvalidOccurrenceSpan {
        owner: EntityId::new(7),
        start: 18,
        end: 24,
    });
    vec![
        IndexJobOutcome::Published,
        IndexJobOutcome::PartiallyPublished(partial(ticket)),
        IndexJobOutcome::Refused(ProductText::from_static("compiler refused this job")),
        IndexJobOutcome::RefusedWithCompilerFailure {
            detail: ProductText::from_static("compiler refused src/recovery.ts"),
            failure: crate::PackageCompilerFailure::from_fragment_failure(
                "src/recovery.ts",
                attempt,
                &fault,
            )
            .expect("typed compiler refusal"),
        },
        IndexJobOutcome::Cancelled,
        IndexJobOutcome::Failed(ProductText::from_static("job failed")),
    ]
}

fn replies(ticket: &IndexJobTicket) -> Vec<(SurfaceCommand, SurfaceReply)> {
    let await_command = SurfaceCommand::IndexAwait {
        ticket: ticket.clone(),
    };
    let progress_command = SurfaceCommand::IndexProgress {
        ticket: ticket.clone(),
        after_sequence: 1,
    };
    let cancel_command = SurfaceCommand::IndexCancel {
        ticket: ticket.clone(),
    };
    let mut replies = Vec::new();
    for outcome in outcomes(ticket) {
        let terminal = IndexJobTerminal {
            ticket: ticket.clone(),
            outcome,
        };
        replies.push((
            await_command.clone(),
            SurfaceReply::IndexTerminal(terminal.clone()),
        ));
        replies.push((
            progress_command.clone(),
            SurfaceReply::IndexProgress(IndexJobObservation::Terminal(terminal.clone())),
        ));
        replies.push((
            cancel_command.clone(),
            SurfaceReply::IndexCancellation(IndexCancelReceipt {
                ticket: ticket.clone(),
                status: IndexCancelStatus::Terminal(terminal),
            }),
        ));
    }
    replies.push((
        progress_command.clone(),
        SurfaceReply::IndexProgress(IndexJobObservation::Pending(IndexProgressPage {
            ticket: ticket.clone(),
            stage: IndexJobStage::Compiling,
            events: vec![IndexJobProgressEvent {
                ticket: ticket.clone(),
                sequence: 2,
                kind: IndexJobProgressKind::StageChanged {
                    stage: IndexJobStage::Compiling,
                },
            }]
            .into_boxed_slice(),
            next_sequence: 2,
            truncated: false,
            has_more: false,
        })),
    ));
    replies.push((
        progress_command,
        SurfaceReply::IndexProgress(IndexJobObservation::Unknown {
            ticket: ticket.clone(),
            current_owner_epoch: [99; 16],
        }),
    ));
    for status in [IndexCancelStatus::Requested, IndexCancelStatus::Unknown] {
        replies.push((
            cancel_command.clone(),
            SurfaceReply::IndexCancellation(IndexCancelReceipt {
                ticket: ticket.clone(),
                status,
            }),
        ));
    }
    replies
}

fn decoded(reply: SurfaceReply) -> ReplyDto {
    let envelope = ReplyDto::new(42, CommandReply::Surface(reply));
    crate::decode_reply_body(&serde_json::to_vec(&envelope).expect("reply bytes"))
        .expect("strict wire decode of internally valid reply")
}

#[test]
fn index_ticket_replies_admit_exact_callers_including_refusals_partial_and_restart_unknown() {
    let ticket = ticket(7, 3, "/workspace/demo");
    for (command, reply) in replies(&ticket) {
        reply.admit(command.id()).expect("self admission");
        let decoded = decoded(reply);
        crate::admit_reply(&CommandDto::new(42, Command::Surface(command)), &decoded)
            .expect("exact ticket through shared client admission");
    }
}

#[test]
fn index_ticket_replies_reject_foreign_package_epoch_and_counter_after_valid_decode() {
    let requested = ticket(7, 3, "/workspace/demo");
    for foreign in [
        ticket(7, 3, "/workspace/foreign"),
        ticket(7, 4, "/workspace/demo"),
        ticket(8, 3, "/workspace/demo"),
    ] {
        for (command, reply) in replies(&foreign) {
            reply
                .admit(command.id())
                .expect("foreign reply is self consistent");
            let requested_command = match command {
                SurfaceCommand::IndexAwait { .. } => SurfaceCommand::IndexAwait {
                    ticket: requested.clone(),
                },
                SurfaceCommand::IndexProgress { after_sequence, .. } => {
                    SurfaceCommand::IndexProgress {
                        ticket: requested.clone(),
                        after_sequence,
                    }
                }
                SurfaceCommand::IndexCancel { .. } => SurfaceCommand::IndexCancel {
                    ticket: requested.clone(),
                },
                _ => unreachable!("fixture commands are ticket operations"),
            };
            let decoded = decoded(reply);
            let result = crate::admit_reply(
                &CommandDto::new(42, Command::Surface(requested_command)),
                &decoded,
            );
            assert!(
                matches!(result, Err(crate::ReplyAdmissionError::Protocol(_))),
                "foreign ticket must not cross caller admission: {foreign:?}"
            );
        }
    }
}

#[test]
fn index_ticket_progress_rejects_durable_operation_reply_with_the_same_command_id() {
    let command = SurfaceCommand::IndexProgress {
        ticket: ticket(7, 3, "/workspace/demo"),
        after_sequence: 1,
    };
    let reply = SurfaceReply::IndexOperationStatus(IndexOperationObservation::Unknown {
        operation_key: crate::IndexOperationKey::from_bytes([7; 32]).expect("operation key"),
    });
    reply
        .admit(command.id())
        .expect("shared command ID alone admits this family");
    assert!(matches!(
        crate::admit_reply(
            &CommandDto::new(42, Command::Surface(command)),
            &decoded(reply)
        ),
        Err(crate::ReplyAdmissionError::Protocol(_))
    ));
}

#[test]
fn index_ticket_commands_keep_identity_free_failures_as_failures() {
    let requested = ticket(7, 3, "/workspace/demo");
    for command in [
        SurfaceCommand::IndexAwait {
            ticket: requested.clone(),
        },
        SurfaceCommand::IndexProgress {
            ticket: requested.clone(),
            after_sequence: 1,
        },
        SurfaceCommand::IndexCancel { ticket: requested },
    ] {
        for failure in [
            CommandReply::Failed(crate::CommandFailure::InvalidQuery(
                "request refused".to_owned(),
            )),
            CommandReply::Error("transport failed".to_owned()),
        ] {
            let envelope = ReplyDto::new(42, failure);
            let decoded =
                crate::decode_reply_body(&serde_json::to_vec(&envelope).expect("failure bytes"))
                    .expect("wire failure decode");
            crate::admit_reply(
                &CommandDto::new(42, Command::Surface(command.clone())),
                &decoded,
            )
            .expect("identity-free failure makes no foreign ticket claim");
        }
    }
}
