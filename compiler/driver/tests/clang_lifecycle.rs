#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use compiler_driver::{
    DatabaseCompileFailure, ResolvedToolchain, compile_database_translation_unit,
};
use compiler_vocabulary::{CStandard, LanguageProfile, NativeTool, Stage};
use heart_identity::ContentId;
use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

fn toolchain() -> Result<ResolvedToolchain<'static>, compiler_driver::ToolchainResolutionError> {
    ResolvedToolchain::from_identity(
        NativeTool::Clang,
        Path::new("/usr/bin/clang"),
        ContentId::from_canonical_bytes(b"clang-lifecycle-toolchain"),
    )
}

#[test]
fn cancelled_database_entry_does_not_open_or_publish() -> Result<(), Box<dyn std::error::Error>> {
    let cancelled = AtomicBool::new(true);
    let mut output = [0xa5_u8; 4096];
    let result = compile_database_translation_unit(
        Path::new("/missing/database"),
        Path::new("main.c"),
        LanguageProfile::C(CStandard::C23),
        Stage::LowerIr,
        b"int main(void) { return 0; }\n",
        toolchain()?,
        &cancelled,
        &mut output,
    );
    assert!(matches!(
        result,
        Err(DatabaseCompileFailure::Cancelled { .. })
    ));
    assert!(output.iter().all(|byte| *byte == 0xa5));
    assert!(cancelled.load(Ordering::Acquire));
    Ok(())
}

#[test]
fn absent_database_is_retained_as_typed_terminal() -> Result<(), Box<dyn std::error::Error>> {
    let cancelled = AtomicBool::new(false);
    let mut output = [0xa5_u8; 4096];
    let result = compile_database_translation_unit(
        Path::new("/definitely/no/compile_commands-here"),
        Path::new("main.c"),
        LanguageProfile::C(CStandard::C23),
        Stage::LowerIr,
        b"int main(void) { return 0; }\n",
        toolchain()?,
        &cancelled,
        &mut output,
    );
    assert!(matches!(result, Err(DatabaseCompileFailure::Database(_))));
    Ok(())
}
