//! Contract tests for the closed language, tool, and diagnostic-capacity vocabulary.
//! Literal arrays pin canonical order because recipe reports depend on that order remaining stable.
//! Capacity assertions prevent transport adapters from silently selecting divergent limits.
use compiler_vocabulary::{
    CSharpVersion, CStandard, CxxStandard, GoVersion, JavaRelease, Language, LanguageProfile,
    MAX_NATIVE_DIAGNOSTIC_BYTES, MAX_NATIVE_WORKER_PANIC_BYTES, NativeTool, NativeWorker,
    NativeWorkerPanic, NativeWorkerPanicClass, PythonVersion, RegistryEcosystem, RustEdition,
    TypeScriptSource,
};

#[test]
fn registry_ecosystems_have_one_stable_identity_and_spelling() {
    let ecosystems = [
        RegistryEcosystem::Cargo,
        RegistryEcosystem::Npm,
        RegistryEcosystem::Pypi,
        RegistryEcosystem::Maven,
        RegistryEcosystem::Nuget,
        RegistryEcosystem::Golang,
        RegistryEcosystem::Cpp,
    ];
    let spellings = ["cargo", "npm", "pypi", "maven", "nuget", "golang", "cpp"];

    for (index, (ecosystem, spelling)) in ecosystems.into_iter().zip(spellings).enumerate() {
        assert_eq!(ecosystem.as_str(), spelling);
        assert_eq!(RegistryEcosystem::parse_canonical(spelling), Ok(ecosystem));
        assert_eq!(
            RegistryEcosystem::try_from((index + 1) as u8),
            Ok(ecosystem)
        );
        assert_eq!(u8::from(ecosystem), (index + 1) as u8);
    }
}

#[test]
fn registry_aliases_are_outer_input_only() {
    assert_eq!("python".parse(), Ok(RegistryEcosystem::Pypi));
    assert_eq!("go".parse(), Ok(RegistryEcosystem::Golang));
    assert!(RegistryEcosystem::parse_canonical("python").is_err());
    assert!(RegistryEcosystem::parse_canonical("go").is_err());
}

#[test]
/// Pins the complete seven-language schedule and its canonical iteration order.
fn compiler_corpus_language_families_are_closed_and_complete() {
    assert_eq!(
        Language::ALL,
        [
            Language::Rust,
            Language::TypeScript,
            Language::Python,
            Language::Go,
            Language::Java,
            Language::CSharp,
            Language::Clang,
        ]
    );
}

#[test]
/// Pins the one-to-one native-tool schedule used by compiler capability reports.
fn compiler_native_tool_families_are_closed_and_canonically_ordered() {
    assert_eq!(
        NativeTool::ALL,
        [
            NativeTool::Rustc,
            NativeTool::Clang,
            NativeTool::Python,
            NativeTool::TypeScriptCompiler,
            NativeTool::GoCompiler,
            NativeTool::JavaCompiler,
            NativeTool::CSharpCompiler,
        ]
    );
}

#[test]
/// Every supported profile must retain its dialect identity through the wire code.
fn every_language_profile_round_trips_and_c_distinguishes_cxx() {
    let profiles = [
        LanguageProfile::Rust(RustEdition::Rust2015),
        LanguageProfile::Rust(RustEdition::Rust2018),
        LanguageProfile::Rust(RustEdition::Rust2021),
        LanguageProfile::Rust(RustEdition::Rust2024),
        LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
        LanguageProfile::TypeScript(TypeScriptSource::Tsx),
        LanguageProfile::Python(PythonVersion::Python310),
        LanguageProfile::Python(PythonVersion::Python311),
        LanguageProfile::Python(PythonVersion::Python312),
        LanguageProfile::Python(PythonVersion::Python313),
        LanguageProfile::Python(PythonVersion::Python314),
        LanguageProfile::Go(GoVersion::Go122),
        LanguageProfile::Go(GoVersion::Go123),
        LanguageProfile::Go(GoVersion::Go124),
        LanguageProfile::Go(GoVersion::Go125),
        LanguageProfile::Java(JavaRelease::Java8),
        LanguageProfile::Java(JavaRelease::Java11),
        LanguageProfile::Java(JavaRelease::Java17),
        LanguageProfile::Java(JavaRelease::Java21),
        LanguageProfile::Java(JavaRelease::Java25),
        LanguageProfile::CSharp(CSharpVersion::CSharp10),
        LanguageProfile::CSharp(CSharpVersion::CSharp11),
        LanguageProfile::CSharp(CSharpVersion::CSharp12),
        LanguageProfile::CSharp(CSharpVersion::CSharp13),
        LanguageProfile::CSharp(CSharpVersion::CSharp14),
        LanguageProfile::C(CStandard::C11),
        LanguageProfile::C(CStandard::C17),
        LanguageProfile::C(CStandard::C23),
        LanguageProfile::Cxx(CxxStandard::Cxx17),
        LanguageProfile::Cxx(CxxStandard::Cxx20),
        LanguageProfile::Cxx(CxxStandard::Cxx23),
        LanguageProfile::Cxx(CxxStandard::Cxx26),
    ];
    assert_eq!(profiles.len(), 32);
    for profile in profiles {
        let encoded = <[u8; 2]>::from(profile);
        assert_eq!(LanguageProfile::try_from(encoded), Ok(profile));
    }
    assert_ne!(
        <[u8; 2]>::from(LanguageProfile::C(CStandard::C11)),
        <[u8; 2]>::from(LanguageProfile::Cxx(CxxStandard::Cxx17))
    );
}

#[test]
/// Pins the single portable diagnostic retention limit shared by every frontend.
fn compiler_native_diagnostic_retention_has_one_closed_portable_capacity() {
    assert_eq!(MAX_NATIVE_DIAGNOSTIC_BYTES, 256);
}

#[test]
fn worker_panic_capture_retains_worker_class_message_and_utf8_boundary() {
    let message = "x".repeat(MAX_NATIVE_WORKER_PANIC_BYTES - 1) + "é";
    let payload: Box<dyn core::any::Any + Send> = Box::new(message);
    let cause = NativeWorkerPanic::capture(NativeWorker::StandardOutputReader, payload.as_ref());

    assert_eq!(cause.worker, NativeWorker::StandardOutputReader);
    assert_eq!(cause.class, NativeWorkerPanicClass::OwnedMessage);
    assert_eq!(cause.message.byte_len, MAX_NATIVE_WORKER_PANIC_BYTES - 1);
    assert!(cause.message.truncated);
    assert!(matches!(
        cause.message.bytes.get(..cause.message.byte_len),
        Some(bytes) if core::str::from_utf8(bytes).is_ok()
    ));
    assert!(std::error::Error::source(&cause).is_none());
}

#[test]
fn opaque_worker_panic_is_distinct_from_an_empty_text_payload() {
    let opaque_payload: Box<dyn core::any::Any + Send> = Box::new(17_u32);
    let text_payload: Box<dyn core::any::Any + Send> = Box::new("");
    let opaque =
        NativeWorkerPanic::capture(NativeWorker::StandardErrorReader, opaque_payload.as_ref());
    let text = NativeWorkerPanic::capture(NativeWorker::StandardErrorReader, text_payload.as_ref());

    assert_eq!(opaque.class, NativeWorkerPanicClass::Opaque);
    assert_eq!(text.class, NativeWorkerPanicClass::StaticMessage);
    assert_ne!(opaque, text);
}
