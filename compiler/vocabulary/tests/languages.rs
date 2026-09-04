//! Contract tests for the closed language, tool, and diagnostic-capacity vocabulary.
//! Literal arrays pin canonical order because recipe reports depend on that order remaining stable.
//! Capacity assertions prevent transport adapters from silently selecting divergent limits.
use compiler_vocabulary::{
    Language, MAX_NATIVE_DIAGNOSTIC_BYTES, MAX_NATIVE_WORKER_PANIC_BYTES, NativeTool,
    NativeWorker, NativeWorkerPanic, NativeWorkerPanicClass,
};

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
/// Pins the single portable diagnostic retention limit shared by every frontend.
fn compiler_native_diagnostic_retention_has_one_closed_portable_capacity() {
    assert_eq!(MAX_NATIVE_DIAGNOSTIC_BYTES, 256);
}

#[test]
fn worker_panic_capture_retains_worker_class_message_and_utf8_boundary() {
    let message = "x".repeat(MAX_NATIVE_WORKER_PANIC_BYTES - 1) + "é";
    let payload: Box<dyn core::any::Any + Send> = Box::new(message);
    let cause = NativeWorkerPanic::capture(
        NativeWorker::StandardOutputReader,
        payload.as_ref(),
    );

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
    let opaque = NativeWorkerPanic::capture(
        NativeWorker::StandardErrorReader,
        opaque_payload.as_ref(),
    );
    let text = NativeWorkerPanic::capture(
        NativeWorker::StandardErrorReader,
        text_payload.as_ref(),
    );

    assert_eq!(opaque.class, NativeWorkerPanicClass::Opaque);
    assert_eq!(text.class, NativeWorkerPanicClass::StaticMessage);
    assert_ne!(opaque, text);
}
