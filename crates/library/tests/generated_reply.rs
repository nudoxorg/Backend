//! Exercises the `backend-library` tests generated-reply contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//! Cross-adapter falsifiers for generated artifacts and typed compiler terminals.

use std::time::Duration;

use backend_library::interface::{
    ApplicationOutcome, ApplicationReply, CompilerAttempt, CompilerCause, CompilerDiagnostic,
    CompilerFragmentFailure, CompilerTerminal, CorrelationId, Diagnostic, DiagnosticCode,
    DiagnosticDetail, DurableReceiptAuthority, GeneratedArtifact, GenerationAuthority,
    NativeIoFact, NativeIoPhase, PublicationAuthority, ReplyBody, SemanticImageAuthority,
    SourceAuthority,
};
use backend_library::protocol::encode_cli_reply;
use backend_semantic::ir::{
    AtomId, BuildError, CanonicalDataError, DataResource, EntityId, EntityNameFault,
    EntityRecordFault, FragmentError, LayoutStep, PrepareError,
};
use backend_semantic::vocabulary::{
    AuthorityDiagnosticClass, AuthorityPhase, CompileRecipeFact, Language, LanguageProfile,
    NativeTool, RustEdition, Stage,
};
use backend_version::{
    ArtifactId, CompilePublicationDomain, CompilePublicationEncoding, ContentId,
    DependencySetDomain, IrFragmentDomain, IrFragmentEncoding, IrManifestDomain,
    IrManifestEncoding, IrSemanticImageDomain, IrSemanticImageEncoding, SourceFactDomain,
    ToolchainDomain,
};
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error("reply encoding failed: {0}")]
    Encode(#[from] std::io::Error),
    #[error("reply JSON projection failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("compiler diagnostic constructor returned no retained fact")]
    MissingDiagnostic,
    #[error("projection mismatch on {channel}: {observed:?}")]
    Projection {
        channel: &'static str,
        observed: Value,
    },
}

#[allow(clippy::needless_pass_by_value)]
fn expect_projection(
    channel: &'static str,
    observed: &Value,
    expected: Value,
) -> Result<(), TestError> {
    if observed == &expected {
        Ok(())
    } else {
        Err(TestError::Projection {
            channel,
            observed: observed.clone(),
        })
    }
}

fn generated_artifact() -> GeneratedArtifact {
    let source_identity = ContentId::<SourceFactDomain>::from_canonical_bytes(b"wire-source");
    let toolchain = ContentId::<ToolchainDomain>::from_canonical_bytes(b"wire-toolchain");
    GeneratedArtifact {
        source: SourceAuthority {
            identity: source_identity,
            byte_len: 11,
        },
        recipe: CompileRecipeFact::derive(
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            NativeTool::Rustc,
            source_identity,
            toolchain,
        ),
        fragment: ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_encoded_bytes(
            b"wire-fragment",
        ),
        semantic_image: SemanticImageAuthority {
            identity: ArtifactId::<IrSemanticImageEncoding, IrSemanticImageDomain>::from_encoded_bytes(
                b"wire-semantic-image",
            ),
            byte_len: 19,
        },
        publication: PublicationAuthority {
            generation: GenerationAuthority {
                pinned_root: backend_library::interface::GenerationId::from_canonical_bytes(
                    b"wire-root",
                ),
                dep_set: ContentId::<DependencySetDomain>::from_canonical_bytes(b"wire-deps"),
            },
            manifest: ArtifactId::<IrManifestEncoding, IrManifestDomain>::from_encoded_bytes(
                b"wire-manifest",
            ),
            binding: ArtifactId::<CompilePublicationEncoding, CompilePublicationDomain>::from_encoded_bytes(
                b"wire-binding",
            ),
            receipt: DurableReceiptAuthority {
                sequence: 7,
                durable_end: 4_096,
                immutable_checksum: [0x11; 16],
                head_checksum: [0x22; 16],
            },
        },
    }
}

fn generated_reply() -> ApplicationReply {
    ApplicationReply {
        correlation: CorrelationId(401),
        outcome: ApplicationOutcome::Resolved(ReplyBody::Generated(generated_artifact())),
    }
}

fn compiler_failure_reply() -> Result<ApplicationReply, TestError> {
    let artifact = generated_artifact();
    let attempted = CompilerAttempt {
        source: artifact.source,
        recipe: artifact.recipe.identity,
    };
    let diagnostic = CompilerDiagnostic::from_native(b"bad source", 300, true)
        .ok_or(TestError::MissingDiagnostic)?;
    let terminal = CompilerTerminal::Compile {
        attempted,
        cause: CompilerCause::NativeRejected {
            code: Some(17),
            diagnostic: Some(diagnostic),
        },
    };
    Ok(ApplicationReply {
        correlation: CorrelationId(402),
        outcome: ApplicationOutcome::Failed {
            diagnostic: Diagnostic {
                code: DiagnosticCode::CompilerTerminal,
                detail: DiagnosticDetail::Compiler(terminal),
            },
        },
    })
}

fn authority_failure_reply() -> Result<ApplicationReply, TestError> {
    let artifact = generated_artifact();
    let attempted = CompilerAttempt {
        source: artifact.source,
        recipe: artifact.recipe.identity,
    };
    let diagnostic =
        CompilerDiagnostic::from_native(b"syntax", 6, false).ok_or(TestError::MissingDiagnostic)?;
    let terminal = CompilerTerminal::Compile {
        attempted,
        cause: CompilerCause::Authority {
            phase: AuthorityPhase::Parse,
            class: AuthorityDiagnosticClass::Syntax,
            diagnostic: Some(diagnostic),
        },
    };
    Ok(ApplicationReply {
        correlation: CorrelationId(405),
        outcome: ApplicationOutcome::Failed {
            diagnostic: Diagnostic {
                code: DiagnosticCode::CompilerTerminal,
                detail: DiagnosticDetail::Compiler(terminal),
            },
        },
    })
}

fn fragment_failure_reply(failure: CompilerFragmentFailure) -> ApplicationReply {
    let artifact = generated_artifact();
    ApplicationReply {
        correlation: CorrelationId(406),
        outcome: ApplicationOutcome::Failed {
            diagnostic: Diagnostic {
                code: DiagnosticCode::CompilerTerminal,
                detail: DiagnosticDetail::Compiler(CompilerTerminal::Compile {
                    attempted: CompilerAttempt {
                        source: artifact.source,
                        recipe: artifact.recipe.identity,
                    },
                    cause: CompilerCause::FragmentFailure(failure),
                }),
            },
        },
    }
}

#[test]
fn generated_reply_projects_every_retained_artifact_fact() -> Result<(), TestError> {
    let cli: Value = serde_json::from_slice(&encode_cli_reply(generated_reply())?)?;

    expect_projection(
        "generated correlation",
        &cli["correlation"],
        Value::from(401),
    )?;
    expect_projection(
        "generated body kind",
        &cli["body"]["kind"],
        Value::from("generated"),
    )?;
    expect_projection(
        "generated source byte length",
        &cli["body"]["artifact"]["source"]["byte_len"],
        Value::from(11),
    )?;
    if !cli["body"]["artifact"]["source"]["identity"]
        .as_str()
        .is_some_and(|identity| identity.starts_with("content:"))
    {
        return Err(TestError::Projection {
            channel: "generated source identity",
            observed: cli["body"]["artifact"]["source"]["identity"].clone(),
        });
    }
    expect_projection(
        "generated recipe profile",
        &cli["body"]["artifact"]["recipe"]["profile"],
        serde_json::json!({ "rust": "rust2024" }),
    )?;
    expect_projection(
        "generated recipe stage",
        &cli["body"]["artifact"]["recipe"]["stage"],
        Value::from("lower-ir"),
    )?;
    expect_projection(
        "generated recipe tool",
        &cli["body"]["artifact"]["recipe"]["tool"],
        Value::from("rustc"),
    )?;
    if !cli["body"]["artifact"]["fragment"]
        .as_str()
        .is_some_and(|fragment| fragment.starts_with("artifact:"))
    {
        return Err(TestError::Projection {
            channel: "generated fragment identity",
            observed: cli["body"]["artifact"]["fragment"].clone(),
        });
    }
    if !cli["body"]["artifact"]["publication"]["manifest"]
        .as_str()
        .is_some_and(|manifest| manifest.starts_with("artifact:"))
    {
        return Err(TestError::Projection {
            channel: "generated manifest identity",
            observed: cli["body"]["artifact"]["publication"]["manifest"].clone(),
        });
    }
    if !cli["body"]["artifact"]["publication"]["binding"]
        .as_str()
        .is_some_and(|binding| binding.starts_with("artifact:"))
    {
        return Err(TestError::Projection {
            channel: "generated binding identity",
            observed: cli["body"]["artifact"]["publication"]["binding"].clone(),
        });
    }
    expect_projection(
        "generated durable receipt sequence",
        &cli["body"]["artifact"]["publication"]["receipt"]["sequence"],
        Value::from(7),
    )?;
    expect_projection(
        "generated durable receipt end",
        &cli["body"]["artifact"]["publication"]["receipt"]["durable_end"],
        Value::from(4_096),
    )?;
    if cli["body"]["artifact"]["publication"]["receipt"]["immutable_checksum"]
        .as_array()
        .is_none_or(|checksum| checksum.len() != 16)
        || cli["body"]["artifact"]["publication"]["receipt"]["head_checksum"]
            .as_array()
            .is_none_or(|checksum| checksum.len() != 16)
    {
        return Err(TestError::Projection {
            channel: "generated durable receipt checksums",
            observed: cli["body"]["artifact"]["publication"]["receipt"].clone(),
        });
    }
    if let Some(package) = cli["body"]["artifact"].get("package") {
        return Err(TestError::Projection {
            channel: "generated package absence",
            observed: package.clone(),
        });
    }
    Ok(())
}

#[test]
fn compiler_terminal_keeps_typed_attempt_and_bounded_native_diagnostic() -> Result<(), TestError> {
    let encoded: Value = serde_json::from_slice(&encode_cli_reply(compiler_failure_reply()?)?)?;
    let terminal = &encoded["diagnostic"]["detail"]["terminal"];
    let cause = &terminal["cause"];
    let native_diagnostic = &cause["diagnostic"];

    expect_projection(
        "compiler failure correlation",
        &encoded["correlation"],
        Value::from(402),
    )?;
    expect_projection(
        "compiler failure terminal",
        &encoded["terminal"]["kind"],
        Value::from("failed"),
    )?;
    expect_projection(
        "compiler failure body",
        &encoded["body"]["kind"],
        Value::from("rejected"),
    )?;
    expect_projection(
        "compiler diagnostic code",
        &encoded["diagnostic"]["code"],
        Value::from("compiler_terminal"),
    )?;
    expect_projection(
        "compiler diagnostic detail",
        &encoded["diagnostic"]["detail"]["kind"],
        Value::from("compiler"),
    )?;
    expect_projection(
        "compiler terminal kind",
        &terminal["kind"],
        Value::from("compile"),
    )?;
    expect_projection(
        "compiler attempted source length",
        &terminal["attempted"]["source"]["byte_len"],
        Value::from(11),
    )?;
    if !terminal["attempted"]["recipe"]
        .as_str()
        .is_some_and(|recipe| recipe.starts_with("content:"))
    {
        return Err(TestError::Projection {
            channel: "compiler attempted recipe identity",
            observed: terminal["attempted"]["recipe"].clone(),
        });
    }
    expect_projection(
        "native rejection kind",
        &cause["kind"],
        Value::from("native_rejected"),
    )?;
    expect_projection("native rejection code", &cause["code"], Value::from(17))?;
    expect_projection(
        "native diagnostic byte length",
        &native_diagnostic["byte_len"],
        Value::from(10),
    )?;
    expect_projection(
        "native diagnostic observed",
        &native_diagnostic["observed"],
        Value::from(300),
    )?;
    expect_projection(
        "native diagnostic truncation",
        &native_diagnostic["truncated"],
        Value::from(true),
    )?;
    if native_diagnostic["bytes"]
        .as_array()
        .is_none_or(|bytes| bytes.len() != 10)
    {
        return Err(TestError::Projection {
            channel: "native diagnostic bounded bytes",
            observed: native_diagnostic["bytes"].clone(),
        });
    }
    if let Some(message) = cause.get("message") {
        return Err(TestError::Projection {
            channel: "native cause erased message",
            observed: message.clone(),
        });
    }
    if let Some(message) = native_diagnostic.get("message") {
        return Err(TestError::Projection {
            channel: "native diagnostic erased message",
            observed: message.clone(),
        });
    }
    Ok(())
}

#[test]
fn fragment_failure_projects_source_span_coordinates_without_authority_reclassification()
-> Result<(), TestError> {
    let failure = CompilerFragmentFailure::build(BuildError::InvalidOccurrenceSpan {
        owner: EntityId::new(7),
        start: 18,
        end: 24,
    });
    let encoded: Value =
        serde_json::from_slice(&encode_cli_reply(fragment_failure_reply(failure))?)?;
    let cause = &encoded["diagnostic"]["detail"]["terminal"]["cause"];
    let fault = &cause["fault"];

    expect_projection(
        "fragment compiler cause kind",
        &cause["kind"],
        Value::from("fragment"),
    )?;
    expect_projection(
        "fragment prepare phase",
        &cause["cause"],
        Value::from("prepare"),
    )?;
    expect_projection(
        "IR build fault family",
        &fault["family"],
        Value::from("build"),
    )?;
    expect_projection(
        "specific source recovery fault tag",
        &fault["kind"],
        Value::from("build_invalid_occurrence_span"),
    )?;
    expect_projection(
        "source recovery fault class",
        &fault["facts"]["kind"],
        Value::from("occurrence_span"),
    )?;
    expect_projection("occurrence owner", &fault["facts"]["owner"], Value::from(7))?;
    expect_projection(
        "occurrence start",
        &fault["facts"]["start"],
        Value::from(18),
    )?;
    expect_projection("occurrence end", &fault["facts"]["end"], Value::from(24))?;
    if !fault["detail"].as_str().is_some_and(|detail| {
        detail.contains("occurrence span")
            && detail.contains("\"owner\":7")
            && detail.contains("\"end\":24")
    }) {
        return Err(TestError::Projection {
            channel: "fragment human detail",
            observed: fault["detail"].clone(),
        });
    }
    expect_projection(
        "bounded detail truncation",
        &fault["detail_truncated"],
        Value::from(false),
    )?;
    let wire = encoded.to_string();
    if wire.contains("wire-source") || wire.contains("bad source") {
        return Err(TestError::Projection {
            channel: "fragment output leaked source or native text",
            observed: Value::String(wire),
        });
    }
    Ok(())
}

#[test]
fn fragment_layout_overflow_keeps_the_lane_and_count_operands() -> Result<(), TestError> {
    let failure = CompilerFragmentFailure::prepare(PrepareError::LayoutOverflow {
        step: LayoutStep::SemanticData,
        entity_count: 23,
        type_node_count: 41,
    });
    let encoded: Value =
        serde_json::from_slice(&encode_cli_reply(fragment_failure_reply(failure))?)?;
    let fault = &encoded["diagnostic"]["detail"]["terminal"]["cause"]["fault"];

    expect_projection(
        "fragment prepare family",
        &fault["family"],
        Value::from("prepare"),
    )?;
    expect_projection(
        "specific overflow fault tag",
        &fault["kind"],
        Value::from("prepare_layout_overflow"),
    )?;
    expect_projection(
        "layout overflow class",
        &fault["facts"]["kind"],
        Value::from("layout_overflow"),
    )?;
    expect_projection(
        "layout overflow step",
        &fault["facts"]["step"],
        Value::from("semantic_data"),
    )?;
    expect_projection(
        "layout entity count",
        &fault["facts"]["entity_count"],
        Value::from(23),
    )?;
    expect_projection(
        "layout type count",
        &fault["facts"]["type_node_count"],
        Value::from(41),
    )?;
    if fault["detail"].as_str().is_none_or(|detail| {
        detail.len() > backend_library::interface::MAX_COMPILER_FRAGMENT_DETAIL_BYTES
            || !detail.contains("overflow")
    }) {
        return Err(TestError::Projection {
            channel: "bounded layout detail",
            observed: fault["detail"].clone(),
        });
    }
    Ok(())
}

#[test]
fn fragment_prepare_count_projects_exact_kind_and_wire_capacity() -> Result<(), TestError> {
    let actual =
        usize::try_from(u64::from(u32::MAX) + 1).map_err(|error| TestError::Projection {
            channel: "host must represent the wire-count overflow fixture",
            observed: Value::String(error.to_string()),
        })?;
    let source = u32::try_from(actual).expect_err("count exceeds the fragment field width");
    let failure = CompilerFragmentFailure::prepare(PrepareError::Count {
        lane: LayoutStep::TypeNodeLane,
        actual,
        source,
    });
    let encoded: Value =
        serde_json::from_slice(&encode_cli_reply(fragment_failure_reply(failure))?)?;
    let fault = &encoded["diagnostic"]["detail"]["terminal"]["cause"]["fault"];

    expect_projection(
        "specific count fault tag",
        &fault["kind"],
        Value::from("prepare_count"),
    )?;
    expect_projection(
        "count lane",
        &fault["facts"]["lane"],
        Value::from("type_node_lane"),
    )?;
    expect_projection(
        "count overflow actual",
        &fault["facts"]["actual"],
        Value::from(actual as u64),
    )?;
    expect_projection(
        "count wire maximum",
        &fault["facts"]["maximum"],
        Value::from(u32::MAX),
    )?;
    Ok(())
}

#[test]
fn fragment_entity_subcause_keeps_exact_nested_tag_and_coordinates() -> Result<(), TestError> {
    let failure = CompilerFragmentFailure::prepare(PrepareError::Entity {
        ordinal: EntityId::new(2),
        fault: EntityRecordFault::Name(EntityNameFault {
            target: AtomId::new(9),
            atom_count: 4,
        }),
    });
    let encoded: Value =
        serde_json::from_slice(&encode_cli_reply(fragment_failure_reply(failure))?)?;
    let fault = &encoded["diagnostic"]["detail"]["terminal"]["cause"]["fault"];

    expect_projection(
        "entity prepare outer fault",
        &fault["kind"],
        Value::from("prepare_entity"),
    )?;
    expect_projection(
        "entity exact nested cause",
        &fault["facts"]["fault"]["family"],
        Value::from("entity_record"),
    )?;
    expect_projection(
        "entity exact nested variant",
        &fault["facts"]["fault"]["fault"]["fault"],
        Value::from("name_reference"),
    )?;
    expect_projection(
        "entity coordinate",
        &fault["facts"]["fault"]["fault"]["ordinal"],
        Value::from(2),
    )?;
    expect_projection(
        "rejected atom coordinate",
        &fault["facts"]["fault"]["fault"]["target"],
        Value::from(9),
    )?;
    expect_projection(
        "available atom count",
        &fault["facts"]["fault"]["fault"]["atom_count"],
        Value::from(4),
    )?;
    if fault["detail"].as_str().is_none_or(|detail| {
        detail.len() > backend_library::interface::MAX_COMPILER_FRAGMENT_DETAIL_BYTES
            || !detail.contains("prepare entity")
            || !detail.contains("name_reference")
            || !detail.contains("atom_count")
    }) {
        return Err(TestError::Projection {
            channel: "entity fragment detail omits the nested cause",
            observed: fault["detail"].clone(),
        });
    }
    Ok(())
}

#[test]
fn fragment_semantic_budget_subcause_keeps_resource_and_operands() -> Result<(), TestError> {
    let failure = CompilerFragmentFailure::prepare(PrepareError::SemanticData {
        cause: CanonicalDataError::BudgetExceeded {
            resource: DataResource::Work,
            observed: 117,
            limit: 100,
        },
    });
    let encoded: Value =
        serde_json::from_slice(&encode_cli_reply(fragment_failure_reply(failure))?)?;
    let fault = &encoded["diagnostic"]["detail"]["terminal"]["cause"]["fault"];

    expect_projection(
        "semantic prepare outer fault",
        &fault["kind"],
        Value::from("prepare_semantic_data"),
    )?;
    expect_projection(
        "semantic exact nested cause",
        &fault["facts"]["fault"]["family"],
        Value::from("canonical_data"),
    )?;
    expect_projection(
        "semantic exact nested variant",
        &fault["facts"]["fault"]["fault"]["fault"],
        Value::from("budget_exceeded"),
    )?;
    expect_projection(
        "semantic budget resource",
        &fault["facts"]["fault"]["fault"]["resource"],
        Value::from("work"),
    )?;
    expect_projection(
        "observed work",
        &fault["facts"]["fault"]["fault"]["observed"],
        Value::from(117),
    )?;
    expect_projection(
        "work budget",
        &fault["facts"]["fault"]["fault"]["limit"],
        Value::from(100),
    )?;
    if fault["detail"].as_str().is_none_or(|detail| {
        detail.len() > backend_library::interface::MAX_COMPILER_FRAGMENT_DETAIL_BYTES
            || !detail.contains("budget_exceeded")
            || !detail.contains("\"limit\":100")
            || !detail.contains("\"observed\":117")
    }) {
        return Err(TestError::Projection {
            channel: "semantic fragment detail omits the nested cause",
            observed: fault["detail"].clone(),
        });
    }
    Ok(())
}

#[test]
fn fragment_validate_fault_has_a_distinct_stable_tag() -> Result<(), TestError> {
    let failure = CompilerFragmentFailure::validate(FragmentError::TruncatedHeader {
        required: 32,
        actual: 7,
    });
    let encoded: Value =
        serde_json::from_slice(&encode_cli_reply(fragment_failure_reply(failure))?)?;
    let cause = &encoded["diagnostic"]["detail"]["terminal"]["cause"];

    expect_projection("validation phase", &cause["cause"], Value::from("validate"))?;
    expect_projection(
        "specific validation error tag",
        &cause["fault"]["kind"],
        Value::from("validate_truncated_header"),
    )?;
    let facts = &cause["fault"]["facts"];
    if facts["fault"]["family"] != "validation"
        || facts["fault"]["fault"]["fault"] != "truncated_header"
        || facts["fault"]["fault"]["required"] != 32
        || facts["fault"]["fault"]["actual"] != 7
        || cause["fault"]["detail"].as_str().is_none_or(|detail| {
            !detail.contains("\"required\":32") || !detail.contains("\"actual\":7")
        })
    {
        return Err(TestError::Projection {
            channel: "validation detail omits required/actual byte lengths",
            observed: cause["fault"]["detail"].clone(),
        });
    }
    Ok(())
}

#[test]
fn authority_terminal_keeps_class_count_and_primary_bytes() -> Result<(), TestError> {
    let reply = authority_failure_reply()?;
    let cli: Value = serde_json::from_slice(&encode_cli_reply(reply)?)?;
    let cause = &cli["diagnostic"]["detail"]["terminal"]["cause"];

    expect_projection(
        "authority correlation",
        &cli["correlation"],
        Value::from(405),
    )?;
    expect_projection("authority kind", &cause["kind"], Value::from("authority"))?;
    expect_projection("authority phase", &cause["phase"], Value::from("parse"))?;
    expect_projection("authority class", &cause["class"], Value::from("syntax"))?;
    expect_projection(
        "authority diagnostic byte length",
        &cause["diagnostic"]["byte_len"],
        Value::from(6),
    )?;
    expect_projection(
        "authority diagnostic observed",
        &cause["diagnostic"]["observed"],
        Value::from(6),
    )?;
    expect_projection(
        "authority diagnostic truncation",
        &cause["diagnostic"]["truncated"],
        Value::from(false),
    )?;
    expect_projection(
        "authority primary bytes",
        &cause["diagnostic"]["bytes"],
        serde_json::json!([115, 121, 110, 116, 97, 120]),
    )?;
    if let Some(message) = cause.get("message") {
        return Err(TestError::Projection {
            channel: "authority cause erased message",
            observed: message.clone(),
        });
    }
    Ok(())
}

#[test]
fn native_io_fact_keeps_closed_kind_and_platform_code() -> Result<(), TestError> {
    let artifact = generated_artifact();
    let reply = ApplicationReply {
        correlation: CorrelationId(403),
        outcome: ApplicationOutcome::Failed {
            diagnostic: Diagnostic {
                code: DiagnosticCode::CompilerTerminal,
                detail: DiagnosticDetail::Compiler(CompilerTerminal::Compile {
                    attempted: CompilerAttempt {
                        source: artifact.source,
                        recipe: artifact.recipe.identity,
                    },
                    cause: CompilerCause::NativeIo {
                        phase: NativeIoPhase::Input,
                        cause: NativeIoFact {
                            kind: std::io::ErrorKind::PermissionDenied,
                            raw_os_code: Some(13),
                        },
                    },
                }),
            },
        },
    };
    let encoded: Value = serde_json::from_slice(&encode_cli_reply(reply)?)?;
    let cause = &encoded["diagnostic"]["detail"]["terminal"]["cause"];

    expect_projection(
        "native I/O cause kind",
        &cause["kind"],
        Value::from("native_io"),
    )?;
    expect_projection("native I/O phase", &cause["phase"], Value::from("input"))?;
    expect_projection(
        "native I/O error kind",
        &cause["cause"]["kind"],
        Value::from("permission_denied"),
    )?;
    expect_projection(
        "native I/O platform code",
        &cause["cause"]["raw_os_code"],
        Value::from(13),
    )?;
    if let Some(message) = cause["cause"].get("message") {
        return Err(TestError::Projection {
            channel: "native I/O erased message",
            observed: message.clone(),
        });
    }
    Ok(())
}

#[test]
fn deadline_construction_terminal_retains_typed_timeout() -> Result<(), TestError> {
    let artifact = generated_artifact();
    let reply = ApplicationReply {
        correlation: CorrelationId(404),
        outcome: ApplicationOutcome::Failed {
            diagnostic: Diagnostic {
                code: DiagnosticCode::CompilerTerminal,
                detail: DiagnosticDetail::Compiler(CompilerTerminal::DeadlineConstruction {
                    source: artifact.source,
                    language: Language::Rust,
                    stage: Stage::LowerIr,
                    timeout: Duration::new(7, 11),
                }),
            },
        },
    };
    let encoded: Value = serde_json::from_slice(&encode_cli_reply(reply)?)?;
    let terminal = &encoded["diagnostic"]["detail"]["terminal"];

    expect_projection(
        "deadline terminal kind",
        &terminal["kind"],
        Value::from("deadline_construction"),
    )?;
    expect_projection(
        "deadline source byte length",
        &terminal["source"]["byte_len"],
        Value::from(11),
    )?;
    expect_projection(
        "deadline language",
        &terminal["language"],
        Value::from("rust"),
    )?;
    expect_projection(
        "deadline stage",
        &terminal["stage"],
        Value::from("lower-ir"),
    )?;
    expect_projection(
        "deadline timeout seconds",
        &terminal["timeout"]["secs"],
        Value::from(7),
    )?;
    expect_projection(
        "deadline timeout nanoseconds",
        &terminal["timeout"]["nanos"],
        Value::from(11),
    )?;
    Ok(())
}
