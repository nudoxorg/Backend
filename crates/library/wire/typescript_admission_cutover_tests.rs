use crate::CompilerAuthorityDiagnosticFacts;
use crate::interface::{
    TypeScriptCompilerAdmissionCause, TypeScriptCompilerAdmissionDetail,
    TypeScriptCompilerPackageRole, TypeScriptCompilerVersion,
};

#[test]
fn typescript_admission_current_dto_refuses_old28_before_populated_fact_shape() {
    assert_eq!(
        super::DTO_VERSION,
        31,
        "populated TypeScript causes require a matched DTO31 or newer cohort"
    );
    let detail = TypeScriptCompilerAdmissionDetail::new(
        TypeScriptCompilerPackageRole::SelectedCompiler,
        Some(TypeScriptCompilerVersion::new("5.9.3").unwrap()),
        TypeScriptCompilerAdmissionCause::RuntimeFiles {
            observed: 257,
            maximum: 256,
        },
    )
    .unwrap();
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
    let populated = serde_json::to_value(&facts).unwrap();
    assert_eq!(
        serde_json::from_value::<CompilerAuthorityDiagnosticFacts>(populated.clone()).unwrap(),
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
    let old_shape = serde_json::from_value::<DiagnosticFacts28>(populated.clone())
        .err()
        .unwrap()
        .to_string();
    assert!(old_shape.contains("unknown field `typescript_admission`"));
    let mut legacy = populated.clone();
    legacy
        .as_object_mut()
        .unwrap()
        .remove("typescript_admission");
    assert!(serde_json::from_value::<DiagnosticFacts28>(legacy).is_ok());

    // The maintained header preflight runs before the strict nested reader.
    let bytes = serde_json::to_vec(&serde_json::json!({
        "version": super::DTO_VERSION, "reply": {"diagnostic": populated}
    }))
    .unwrap();
    let mismatch =
        super::super::codec::check_version_header_against(&bytes, "reply", 28).unwrap_err();
    assert!(mismatch.contains(&format!("reply DTO version {}", super::DTO_VERSION)));
    assert!(mismatch.contains("this build supports 28"));
    assert!(mismatch.contains("same build"));
    assert!(!mismatch.contains("unknown field"));
    let mut old_envelope: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    old_envelope["version"] = serde_json::json!(28);
    let error =
        crate::wire::decode_reply_body(&serde_json::to_vec(&old_envelope).unwrap()).unwrap_err();
    assert!(error.contains("reply DTO version 28"));
    assert!(!error.contains("unknown field"));
}

#[test]
fn typescript_cause_requires_a_matched_cohort_before_strict_dto30_shape_admission() {
    use crate::interface::{TypeScriptAuthorityFailureKind, TypeScriptFailureCause};
    assert_eq!(
        super::DTO_VERSION,
        31,
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
    let absent = serde_json::to_value(&facts).unwrap();
    assert!(absent.get("typescript_cause").is_none());
    assert!(serde_json::from_value::<DiagnosticFacts30>(absent.clone()).is_ok());
    assert_eq!(
        serde_json::from_value::<CompilerAuthorityDiagnosticFacts>(absent).unwrap(),
        facts
    );
    facts.typescript_cause = TypeScriptFailureCause::new("tsconfig.json does not exist");
    let present = serde_json::to_value(&facts).unwrap();
    assert_eq!(
        serde_json::from_value::<CompilerAuthorityDiagnosticFacts>(present.clone()).unwrap(),
        facts
    );
    let old_shape = serde_json::from_value::<DiagnosticFacts30>(present.clone())
        .err()
        .unwrap()
        .to_string();
    assert!(old_shape.contains("unknown field `typescript_cause`"));
    let envelope = serde_json::to_vec(&serde_json::json!({
        "version": super::DTO_VERSION, "reply": {"diagnostic": present}
    })).unwrap();
    let mismatch = super::super::codec::check_version_header_against(&envelope, "reply", 30)
        .unwrap_err();
    assert!(mismatch.contains("this build supports 30"));
    assert!(mismatch.contains("same build"));
    assert!(!mismatch.contains("unknown field"));
    assert_eq!(
        facts.typescript_failure,
        Some(TypeScriptAuthorityFailureKind::HostConfigMissing)
    );
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
fn typescript_stop_site_is_a_new_strict_field_in_the_matched_dto31_cohort() {
    assert_eq!(super::DTO_VERSION, 31);
    let mut facts = plain_facts();
    facts.typescript_failure = Some(crate::interface::TypeScriptAuthorityFailureKind::NativeProjectCheckWorkBudgetExhausted);
    facts.typescript_stop_site = Some(crate::interface::TypeScriptStopSite::new(
        Some(crate::interface::TypeScriptStopPhase::FileCheck), Some("src/main.ts"), 9, 10,
    ));
    let present = serde_json::to_value(&facts).unwrap();
    assert_eq!(serde_json::from_value::<CompilerAuthorityDiagnosticFacts>(present.clone()).unwrap(), facts);
    // Its existence must be accounted for separately from the optional host cause.
    assert!(present.get("typescript_stop_site").is_some());
    assert!(present.get("typescript_cause").is_none());
    let envelope = serde_json::to_vec(&serde_json::json!({
        "version": super::DTO_VERSION, "reply": {"diagnostic": present}
    })).unwrap();
    let refusal = super::super::codec::check_version_header_against(&envelope, "reply", 30).unwrap_err();
    assert!(refusal.contains("this build supports 30"));
    assert!(!refusal.contains("unknown field"));
}
