use crate::CompilerAuthorityDiagnosticFacts;
use crate::interface::{
    TypeScriptCompilerAdmissionCause, TypeScriptCompilerAdmissionDetail,
    TypeScriptCompilerPackageRole, TypeScriptCompilerVersion,
};

fn rejected<T, E: std::fmt::Display>(
    result: Result<T, E>,
    message: &'static str,
) -> Result<String, Box<dyn std::error::Error>> {
    match result {
        Err(error) => Ok(error.to_string()),
        Ok(_) => Err(std::io::Error::other(message).into()),
    }
}

#[test]
fn typescript_admission_current_dto_refuses_old28_before_populated_fact_shape()
-> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        super::DTO_VERSION,
        35,
        "coverage and populated TypeScript causes require the current matched cohort"
    );
    let detail = TypeScriptCompilerAdmissionDetail::new(
        TypeScriptCompilerPackageRole::SelectedCompiler,
        Some(
            TypeScriptCompilerVersion::new("5.9.3")
                .ok_or_else(|| std::io::Error::other("valid compiler version must be admitted"))?,
        ),
        TypeScriptCompilerAdmissionCause::RuntimeFiles {
            observed: 257,
            maximum: 256,
        },
    )
    .ok_or_else(|| std::io::Error::other("runtime-file overage must form admission facts"))?;
    let facts = CompilerAuthorityDiagnosticFacts {
        retained_bytes: 0,
        observed_bytes: 0,
        truncated: false,
        python_failure: None,
        python_configuration: None,
        typescript_failure: Some(detail.failure_kind()),
        typescript_stop_site: None,
        typescript_admission: Some(detail),
        typescript_cause: None,
        go_failure: None,
    };
    let populated = serde_json::to_value(&facts)?;
    assert_eq!(
        serde_json::from_value::<CompilerAuthorityDiagnosticFacts>(populated.clone())?,
        facts
    );

    // This exact closed DTO28 field vocabulary supplies a shape-law witness,
    // not an old-binary process replay or producer-authority fixture.
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct DiagnosticFacts28 {
        retained_bytes: u32,
        observed_bytes: u64,
        truncated: bool,
        #[serde(default)]
        python_failure: Option<serde_json::Value>,
        #[serde(default)]
        python_configuration: Option<serde_json::Value>,
        #[serde(default)]
        typescript_failure: Option<serde_json::Value>,
        #[serde(default)]
        go_failure: Option<serde_json::Value>,
    }
    let old_shape = rejected(
        serde_json::from_value::<DiagnosticFacts28>(populated.clone()),
        "DTO28 shape must reject populated admission facts",
    )?;
    assert!(old_shape.contains("unknown field `typescript_admission`"));
    let mut legacy = populated.clone();
    legacy
        .as_object_mut()
        .ok_or_else(|| std::io::Error::other("serialized facts must be a JSON object"))?
        .remove("typescript_admission");
    assert!(serde_json::from_value::<DiagnosticFacts28>(legacy).is_ok());

    // The maintained header preflight runs before the strict nested reader.
    let bytes = serde_json::to_vec(&serde_json::json!({
        "version": super::DTO_VERSION, "reply": {"diagnostic": populated}
    }))?;
    let mismatch = rejected(
        super::super::codec::check_version_header_against(&bytes, "reply", 28),
        "DTO28 preflight must reject the current header",
    )?;
    assert!(mismatch.contains(&format!("reply DTO version {}", super::DTO_VERSION)));
    assert!(mismatch.contains("this build supports 28"));
    assert!(mismatch.contains("same build"));
    assert!(!mismatch.contains("unknown field"));
    let mut old_envelope: serde_json::Value = serde_json::from_slice(&bytes)?;
    old_envelope["version"] = serde_json::json!(28);
    let old_bytes = serde_json::to_vec(&old_envelope)?;
    let error = rejected(
        crate::wire::decode_reply_body(&old_bytes),
        "DTO28 live envelope must be rejected",
    )?;
    assert!(error.contains("reply DTO version 28"));
    assert!(!error.contains("unknown field"));
    Ok(())
}

#[test]
fn typescript_cause_requires_a_matched_cohort_before_strict_dto30_shape_admission()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::interface::{TypeScriptAuthorityFailureKind, TypeScriptFailureCause};
    assert_eq!(
        super::DTO_VERSION,
        35,
        "strict DTO30 readers reject the populated cause field"
    );
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct DiagnosticFacts30 {
        retained_bytes: u32,
        observed_bytes: u64,
        truncated: bool,
        #[serde(default)]
        python_failure: Option<serde_json::Value>,
        #[serde(default)]
        python_configuration: Option<serde_json::Value>,
        #[serde(default)]
        typescript_failure: Option<serde_json::Value>,
        #[serde(default)]
        typescript_admission: Option<serde_json::Value>,
        #[serde(default)]
        go_failure: Option<serde_json::Value>,
    }
    let mut facts = plain_facts();
    // Absent: byte-for-byte the previous shape, readable by a reader without the field.
    let absent = serde_json::to_value(&facts)?;
    assert!(absent.get("typescript_cause").is_none());
    assert!(serde_json::from_value::<DiagnosticFacts30>(absent.clone()).is_ok());
    assert_eq!(
        serde_json::from_value::<CompilerAuthorityDiagnosticFacts>(absent)?,
        facts
    );
    facts.typescript_cause = TypeScriptFailureCause::new("tsconfig.json does not exist");
    let present = serde_json::to_value(&facts)?;
    assert_eq!(
        serde_json::from_value::<CompilerAuthorityDiagnosticFacts>(present.clone())?,
        facts
    );
    let old_shape = rejected(
        serde_json::from_value::<DiagnosticFacts30>(present.clone()),
        "DTO30 shape must reject populated cause",
    )?;
    assert!(old_shape.contains("unknown field `typescript_cause`"));
    let envelope = serde_json::to_vec(&serde_json::json!({
        "version": super::DTO_VERSION, "reply": {"diagnostic": present}
    }))?;
    let mismatch = rejected(
        super::super::codec::check_version_header_against(&envelope, "reply", 30),
        "DTO30 preflight must reject the current header",
    )?;
    assert!(mismatch.contains("this build supports 30"));
    assert!(mismatch.contains("same build"));
    assert!(!mismatch.contains("unknown field"));
    assert_eq!(
        facts.typescript_failure,
        Some(TypeScriptAuthorityFailureKind::HostConfigMissing)
    );
    Ok(())
}

fn plain_facts() -> CompilerAuthorityDiagnosticFacts {
    CompilerAuthorityDiagnosticFacts {
        retained_bytes: 0,
        observed_bytes: 0,
        truncated: false,
        python_failure: None,
        python_configuration: None,
        typescript_failure: Some(
            crate::interface::TypeScriptAuthorityFailureKind::HostConfigMissing,
        ),
        typescript_stop_site: None,
        typescript_admission: None,
        typescript_cause: None,
        go_failure: None,
    }
}

#[test]
fn typescript_stop_site_current_cohort_refuses_dto30_before_the_strict_field()
-> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(super::DTO_VERSION, 38);
    let mut facts = plain_facts();
    facts.typescript_failure = Some(
        crate::interface::TypeScriptAuthorityFailureKind::NativeProjectCheckWorkBudgetExhausted,
    );
    facts.typescript_stop_site = Some(crate::interface::TypeScriptStopSite::new(
        Some(crate::interface::TypeScriptStopPhase::FileCheck),
        Some("src/main.ts"),
        9,
        10,
    ));
    let present = serde_json::to_value(&facts)?;
    assert_eq!(
        serde_json::from_value::<CompilerAuthorityDiagnosticFacts>(present.clone())?,
        facts
    );
    // Its existence must be accounted for separately from the optional host cause.
    assert!(present.get("typescript_stop_site").is_some());
    assert!(present.get("typescript_cause").is_none());
    let envelope = serde_json::to_vec(&serde_json::json!({
        "version": super::DTO_VERSION, "reply": {"diagnostic": present}
    }))?;
    let refusal = rejected(
        super::super::codec::check_version_header_against(&envelope, "reply", 30),
        "DTO30 preflight must reject the current stop-site header",
    )?;
    assert!(refusal.contains("this build supports 30"));
    assert!(!refusal.contains("unknown field"));
    Ok(())
}

#[test]
fn native_resolution_dto32_refuses_dto31_before_closed_failure_kinds()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::interface::{
        AuthorityDiagnosticClass, AuthorityPhase, CompilerDiagnostic, CompilerTerminal,
        SourceAuthority, TypeScriptAuthorityFailureKind as F,
    };
    assert_eq!(super::DTO_VERSION, 38);
    for kind in [
        F::NativeSessionModuleResolution,
        F::NativeModuleInvalidReference,
        F::NativeProjectCheckModuleInvalidReference,
    ] {
        let source = b"import('./module')";
        let private = b"private /home/user/captured-reference.ts:17";
        let failure = crate::PackageCompilerFailure::from_package_terminal(
            "src/main.ts",
            &CompilerTerminal::AuthorityAdmission {
                source: SourceAuthority {
                    identity: backend_version::ContentId::from_canonical_bytes(source),
                    byte_len: u32::try_from(source.len())?,
                },
                language: backend_semantic::vocabulary::Language::TypeScript,
                stage: backend_semantic::vocabulary::Stage::LowerIr,
                phase: AuthorityPhase::Resolve,
                class: AuthorityDiagnosticClass::Authority,
                diagnostic: CompilerDiagnostic::from_native(private, private.len(), false)
                    .map(|diagnostic| diagnostic.with_typescript_failure(kind)),
            },
        )
        .map_err(|error| std::io::Error::other(error.to_string()))?
        .ok_or_else(|| std::io::Error::other("native resolution must become a refusal"))?;
        let reply = crate::ReplyDto::new(
            1,
            crate::CommandReply::Failed(crate::CommandFailure::CompilerRefused {
                detail: failure.detail().to_owned(),
                failure,
            }),
        );
        let bytes = serde_json::to_vec(&reply)?;
        assert_eq!(crate::decode_reply_body(&bytes)?, reply);
        assert!(!String::from_utf8_lossy(&bytes).contains("/home/user"));
        // The maintained preflight parameterized as a DTO31 reader
        // rejects this current payload before its closed enum is decoded.
        let old_peer = rejected(
            super::super::codec::check_version_header_against(&bytes, "reply", 31),
            "DTO31 peer must reject the new enum cohort first",
        )?;
        assert!(old_peer.contains("reply DTO version 38"));
        assert!(old_peer.contains("this build supports 31"));
        assert!(old_peer.contains("same build"));
        assert!(!old_peer.contains("unknown variant"));
        // An old live envelope cannot be relabelled as a new typed success.
        let mut old: serde_json::Value = serde_json::from_slice(&bytes)?;
        old["version"] = serde_json::json!(31);
        let old_bytes = serde_json::to_vec(&old)?;
        let error = rejected(
            crate::decode_reply_body(&old_bytes),
            "DTO31 relabelled live envelope must be refused",
        )?;
        assert!(error.contains("reply DTO version 31"));
        assert!(error.contains("this build supports 38"));
        assert!(!error.contains("unknown variant"));
    }
    Ok(())
}
