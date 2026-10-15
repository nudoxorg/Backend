use crate::CompilerAuthorityDiagnosticFacts;
use crate::interface::{
    TypeScriptCompilerAdmissionCause, TypeScriptCompilerAdmissionDetail,
    TypeScriptCompilerPackageRole, TypeScriptCompilerVersion,
};

#[test]
fn typescript_admission_current_dto_refuses_old28_before_populated_fact_shape() {
    assert_eq!(
        super::DTO_VERSION,
        30,
        "catalog windows require a matched DTO30 cohort"
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
        typescript_admission: Some(detail),
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
