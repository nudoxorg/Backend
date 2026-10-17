use super::*;
use crate::interface::{
    CompilerDiagnostic, CompilerTerminal, SourceAuthority, TypeScriptAuthorityFailureKind as K,
    TypeScriptCompilerAdmissionCause as C, TypeScriptCompilerAdmissionDetail as D,
    TypeScriptCompilerPackageRole as R, TypeScriptCompilerVersion as V, TypeScriptFailureCause,
};
use backend_semantic::vocabulary::{Language, Stage};

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

fn required<T>(value: Option<T>, context: &'static str) -> TestResult<T> {
    value.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, context).into())
}

fn package_result<T>(value: Result<T, ProductAdmissionError>) -> TestResult<T> {
    value.map_err(|error| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()).into()
    })
}

fn failure_with(
    kind: K,
    cause: Option<&str>,
    path: &str,
    admission: bool,
) -> TestResult<PackageCompilerFailure> {
    let native = b"private /home/user/sdk authority.secret and raw compiler transcript";
    let mut diagnostic = required(
        CompilerDiagnostic::from_native(native, native.len(), false),
        "admit test compiler diagnostic",
    )?
    .with_typescript_failure(kind);
    if admission {
        let version = required(V::new("5.9.3"), "admit selected TypeScript version")?;
        let detail = required(
            D::new(R::SelectedCompiler, Some(version), C::MissingApiEntry),
            "admit selected TypeScript compiler facts",
        )?;
        diagnostic = diagnostic.with_typescript_admission(detail);
    }
    if let Some(cause) = cause {
        diagnostic = diagnostic.with_typescript_cause(required(
            TypeScriptFailureCause::new(cause),
            "admit TypeScript failure cause",
        )?);
    }
    let terminal = CompilerTerminal::AuthorityAdmission {
        source: SourceAuthority {
            identity: ContentId::from_canonical_bytes(b"source"),
            byte_len: 6,
        },
        language: Language::TypeScript,
        stage: Stage::LowerIr,
        phase: crate::interface::AuthorityPhase::Resolve,
        class: crate::interface::AuthorityDiagnosticClass::Authority,
        diagnostic: Some(diagnostic),
    };
    required(
        package_result(PackageCompilerFailure::from_package_terminal(
            path, &terminal,
        ))?,
        "project test terminal as package compiler failure",
    )
}

#[test]
fn typescript_cause_extends_the_closed_kind_detail_and_roundtrips() -> TestResult<()> {
    let cause = "TS5083: Cannot read file 'tsconfig.app.json'";
    let failure = failure_with(K::HostCompilerApiBridge, Some(cause), "src/main.tsx", false)?;
    assert_eq!(failure.kind_tag(), "typescript_host_compiler_api_bridge");
    assert_eq!(
        failure.detail(),
        format!("{}: {cause}", K::HostCompilerApiBridge.detail())
    );
    assert!(!failure.detail_truncated());
    assert!(failure.typescript_compiler_admission().is_none());
    let bytes = package_result(failure.encode_bounded_json())?;
    let wire = String::from_utf8(bytes.clone())?;
    assert!(wire.contains("\"typescript_cause\""));
    for private in ["/home/user", "authority.secret", "raw compiler transcript"] {
        assert!(!wire.contains(private), "{private}");
    }
    let reopened = package_result(PackageCompilerFailure::decode_bounded_json(&bytes))?;
    assert_eq!(reopened, failure);
    assert!(reopened.retained_diagnostic_for_local_debug().is_none());
    // The field is additive: a failure without cause keeps the exact fixed sentence.
    let plain = failure_with(K::HostCompilerApiBridge, None, "src/main.tsx", false)?;
    assert_eq!(plain.detail(), K::HostCompilerApiBridge.detail());
    let plain_wire = String::from_utf8(package_result(plain.encode_bounded_json())?)?;
    assert!(!plain_wire.contains("typescript_cause"));
    Ok(())
}

#[test]
fn typescript_cause_never_shadows_typed_admission_facts() -> TestResult<()> {
    let failure = failure_with(K::HostCompilerApiBridge, Some("ignored text"), "a.ts", true)?;
    assert!(failure.typescript_compiler_admission().is_some());
    assert!(!failure.detail().contains("ignored text"));
    let wire = String::from_utf8(package_result(failure.encode_bounded_json())?)?;
    assert!(!wire.contains("typescript_cause"));
    Ok(())
}

#[test]
fn typescript_cause_forgeries_are_refused() -> TestResult<()> {
    let failure = failure_with(
        K::HostConfigInvalid,
        Some("tsconfig.json is malformed"),
        "a.ts",
        false,
    )?;
    let value = serde_json::to_value(&failure)?;
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
    let mut forged = value.clone();
    let diagnostic = forged
        .pointer_mut("/cause/fault/diagnostic")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "serialized diagnostic is not an object",
            )
        })?;
    assert!(diagnostic.remove("typescript_failure").is_some());
    assert!(serde_json::from_value::<PackageCompilerFailure>(forged).is_err());
    Ok(())
}

#[test]
fn typescript_cause_worst_case_packet_stays_within_the_wire_bound() -> TestResult<()> {
    let cause = "c".repeat(TypeScriptFailureCause::MAX_BYTES);
    let path = "\"".repeat(PackageCompilerFailure::MAX_RELATIVE_PATH_BYTES);
    for kind in [
        K::HostNodeUnavailable,
        K::HostCompilerApiBridge,
        K::HostConfigInvalid,
    ] {
        let failure = failure_with(kind, Some(&cause), &path, false)?;
        assert!(failure.detail().len() <= MAX_COMPILER_FRAGMENT_DETAIL_BYTES);
        assert!(!failure.detail_truncated(), "{kind:?}");
        let bytes = package_result(failure.encode_bounded_json())?;
        assert!(
            bytes.len() <= PackageCompilerFailure::MAX_ENCODED_BYTES,
            "{kind:?}"
        );
        assert_eq!(
            package_result(PackageCompilerFailure::decode_bounded_json(&bytes))?,
            failure
        );
    }
    Ok(())
}
