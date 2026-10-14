use super::*;
use crate::interface::{
    CompilerDiagnostic, CompilerTerminal, SourceAuthority, TypeScriptAuthorityFailureKind as K,
    TypeScriptCompilerAdmissionCause as C, TypeScriptCompilerAdmissionDetail as D,
    TypeScriptCompilerPackageRole as R, TypeScriptCompilerVersion as V, TypeScriptFailureCause,
};
use backend_semantic::vocabulary::{Language, Stage};

fn failure_with(
    kind: K,
    cause: Option<&str>,
    path: &str,
    admission: bool,
) -> PackageCompilerFailure {
    let native = b"private /home/user/sdk authority.secret and raw compiler transcript";
    let mut diagnostic = CompilerDiagnostic::from_native(native, native.len(), false)
        .unwrap()
        .with_typescript_failure(kind);
    if admission {
        diagnostic = diagnostic.with_typescript_admission(
            D::new(R::SelectedCompiler, V::new("5.9.3"), C::MissingApiEntry).unwrap(),
        );
    }
    if let Some(cause) = cause {
        diagnostic = diagnostic.with_typescript_cause(TypeScriptFailureCause::new(cause).unwrap());
    }
    PackageCompilerFailure::from_package_terminal(
        path,
        &CompilerTerminal::AuthorityAdmission {
            source: SourceAuthority {
                identity: ContentId::from_canonical_bytes(b"source"),
                byte_len: 6,
            },
            language: Language::TypeScript,
            stage: Stage::LowerIr,
            phase: crate::interface::AuthorityPhase::Resolve,
            class: crate::interface::AuthorityDiagnosticClass::Authority,
            diagnostic: Some(diagnostic),
        },
    )
    .unwrap()
    .unwrap()
}

#[test]
fn typescript_cause_extends_the_closed_kind_detail_and_roundtrips() {
    let cause = "TS5083: Cannot read file 'tsconfig.app.json'";
    let failure = failure_with(K::HostCompilerApiBridge, Some(cause), "src/main.tsx", false);
    assert_eq!(failure.kind_tag(), "typescript_host_compiler_api_bridge");
    assert_eq!(
        failure.detail(),
        format!("{}: {cause}", K::HostCompilerApiBridge.detail())
    );
    assert!(!failure.detail_truncated());
    assert!(failure.typescript_compiler_admission().is_none());
    let bytes = failure.encode_bounded_json().unwrap();
    let wire = String::from_utf8(bytes.clone()).unwrap();
    assert!(wire.contains("\"typescript_cause\""));
    for private in ["/home/user", "authority.secret", "raw compiler transcript"] {
        assert!(!wire.contains(private), "{private}");
    }
    let reopened = PackageCompilerFailure::decode_bounded_json(&bytes).unwrap();
    assert_eq!(reopened, failure);
    assert!(reopened.retained_diagnostic_for_local_debug().is_none());
    // The field is additive: a failure without cause keeps the exact fixed sentence.
    let plain = failure_with(K::HostCompilerApiBridge, None, "src/main.tsx", false);
    assert_eq!(plain.detail(), K::HostCompilerApiBridge.detail());
    assert!(
        !String::from_utf8(plain.encode_bounded_json().unwrap())
            .unwrap()
            .contains("typescript_cause")
    );
}

#[test]
fn typescript_cause_never_shadows_typed_admission_facts() {
    let failure = failure_with(K::HostCompilerApiBridge, Some("ignored text"), "a.ts", true);
    assert!(failure.typescript_compiler_admission().is_some());
    assert!(!failure.detail().contains("ignored text"));
    assert!(
        !String::from_utf8(failure.encode_bounded_json().unwrap())
            .unwrap()
            .contains("typescript_cause")
    );
}

#[test]
fn typescript_cause_forgeries_are_refused() {
    let failure = failure_with(
        K::HostConfigInvalid,
        Some("tsconfig.json is malformed"),
        "a.ts",
        false,
    );
    let value = serde_json::to_value(&failure).unwrap();
    let mut mutate = |edit: &dyn Fn(&mut serde_json::Value)| {
        let mut forged = value.clone();
        edit(&mut forged);
        serde_json::from_value::<PackageCompilerFailure>(forged).is_err()
    };
    // Text that disagrees with the displayed detail.
    assert!(mutate(&|forged| {
        forged["cause"]["fault"]["diagnostic"]["typescript_cause"] = serde_json::json!("other")
    }));
    assert!(mutate(&|forged| {
        forged["detail"] = serde_json::json!("unrelated sanitized explanation")
    }));
    // Unadmitted text: control characters, quotes, oversize.
    for bad in ["line\nbreak", "quo\"te", ""] {
        assert!(
            mutate(&|forged| {
                forged["cause"]["fault"]["diagnostic"]["typescript_cause"] = serde_json::json!(bad)
            }),
            "{bad:?}"
        );
    }
    let oversized = "x".repeat(TypeScriptFailureCause::MAX_BYTES + 1);
    assert!(mutate(&|forged| {
        forged["cause"]["fault"]["diagnostic"]["typescript_cause"] = serde_json::json!(oversized)
    }));
    // A cause with no closed kind to explain is contradictory.
    assert!(mutate(&|forged| {
        forged["cause"]["fault"]["diagnostic"]
            .as_object_mut()
            .unwrap()
            .remove("typescript_failure");
    }));
}

#[test]
fn typescript_cause_worst_case_packet_stays_within_the_wire_bound() {
    let cause = "c".repeat(TypeScriptFailureCause::MAX_BYTES);
    let path = "\"".repeat(PackageCompilerFailure::MAX_RELATIVE_PATH_BYTES);
    for kind in [
        K::HostNodeUnavailable,
        K::HostCompilerApiBridge,
        K::HostConfigInvalid,
    ] {
        let failure = failure_with(kind, Some(&cause), &path, false);
        assert!(failure.detail().len() <= MAX_COMPILER_FRAGMENT_DETAIL_BYTES);
        assert!(!failure.detail_truncated(), "{kind:?}");
        let bytes = failure.encode_bounded_json().unwrap();
        assert!(
            bytes.len() <= PackageCompilerFailure::MAX_ENCODED_BYTES,
            "{kind:?}"
        );
        assert_eq!(
            PackageCompilerFailure::decode_bounded_json(&bytes).unwrap(),
            failure
        );
    }
}
