//! Cross-adapter falsifiers for generated artifacts and typed compiler terminals.

use std::time::Duration;

use nudox_compile_vocab::{CompileRecipeFact, Language, NativeTool, Stage};
use nudox_id::{
    ArtifactId, CompilePublicationDomain, CompilePublicationEncoding, ContentId,
    DependencySetDomain, IrFragmentDomain, IrFragmentEncoding, IrManifestDomain,
    IrManifestEncoding, SourceFactDomain, ToolchainDomain,
};
use serde_json::Value;
use wave_application_core::{
    ApplicationOutcome, ApplicationReply, CompilerAttempt, CompilerCause, CompilerDiagnostic,
    CompilerTerminal, CorrelationId, Diagnostic, DiagnosticCode, DiagnosticDetail,
    GeneratedArtifact, GenerationAuthority, NativeIoFact, NativeIoPhase, PublicationAuthority,
    ReplyBody, SourceAuthority,
};
use wave_application_protocol::{encode_cli_reply, mcp_reply};

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
            Language::Rust,
            Stage::LowerIr,
            NativeTool::Rustc,
            source_identity,
            toolchain,
        ),
        fragment: ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_encoded_bytes(
            b"wire-fragment",
        ),
        publication: PublicationAuthority {
            generation: GenerationAuthority {
                pinned_root: wave_application_core::GenerationId::from_canonical_bytes(
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

#[test]
#[allow(clippy::too_many_lines)]
fn generated_reply_is_byte_for_byte_the_same_cli_body_and_mcp_structured_content()
-> Result<(), TestError> {
    let cli: Value = serde_json::from_slice(&encode_cli_reply(generated_reply())?)?;
    let request_id = Value::String(String::from("generated-request"));
    let mcp = serde_json::to_value(mcp_reply(&request_id, generated_reply()))?;
    let structured = &mcp["result"]["structuredContent"];

    expect_projection("cli/mcp body parity", &cli, structured.clone())?;
    expect_projection("mcp request id", &mcp["id"], request_id.clone())?;
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
        "generated recipe language",
        &cli["body"]["artifact"]["recipe"]["language"],
        Value::from("rust"),
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
    if let Some(package) = cli["body"]["artifact"].get("package") {
        return Err(TestError::Projection {
            channel: "generated package absence",
            observed: package.clone(),
        });
    }
    Ok(())
}

#[test]
#[allow(clippy::too_many_lines)]
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
