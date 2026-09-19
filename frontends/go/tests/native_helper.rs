//! End-to-end Go semantic helper integration.
//!
//! The Go authority test requires an explicitly admitted compiler. A missing
//! or unusable compiler is a typed [`NativeHelperError`] terminal, never a
//! silent pass, so the suite cannot report green without its toolchain.

use backend_compile::{
    Authority, AuthorityError, FactKind, FlowSchema, ProfileSchema, SemanticBasisSchema,
    SessionKey, typed_of,
};
use backend_frontend_go::GoFrontend;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
enum NativeHelperError {
    #[error("COMPILER_GO_COMPILER must name the Go compiler used for the real Go authority test")]
    MissingCompiler,
    #[error("configured Go compiler is not a file: {path:?}")]
    CompilerNotAFile {
        /// Rejected compiler path.
        path: PathBuf,
    },
    #[error(transparent)]
    Authority(#[from] AuthorityError),
}

#[test]
fn go_packages_helper_emits_admitted_semantics() -> Result<(), NativeHelperError> {
    let go = configured_compiler()?;
    let source = br#"package fixture

import "fmt"

// Node retains one value.
type Node[T any] struct { Value T }

func Render(value Node[int]) string { return fmt.Sprint(value.Value) }
"#;
    let bundled = GoFrontend::new(
        source.to_vec(),
        go.to_string_lossy(),
        b"module example.test/fixture\n\ngo 1.23\n".to_vec(),
        "linux",
    )?;
    assert_semantics(&bundled)?;
    if let Ok(helper) = std::env::var("BACKEND_GO_SEMANTIC_HELPER") {
        let cold = GoFrontend::with_helper(
            source.to_vec(),
            go.to_string_lossy(),
            &helper,
            b"module example.test/fixture\n\ngo 1.23\n".to_vec(),
            "linux",
        )?;
        assert_semantics(&cold)?;
        let persistent = GoFrontend::with_persistent_helper(
            source.to_vec(),
            go.to_string_lossy(),
            helper,
            b"module example.test/fixture\n\ngo 1.23\n".to_vec(),
            "linux",
        )?;
        assert_semantics(&persistent)?;
    }
    Ok(())
}

/// Resolves the explicitly configured compiler or returns a typed terminal.
fn configured_compiler() -> Result<PathBuf, NativeHelperError> {
    let path = std::env::var_os("COMPILER_GO_COMPILER")
        .map(PathBuf::from)
        .ok_or(NativeHelperError::MissingCompiler)?;
    if !path.is_file() {
        return Err(NativeHelperError::CompilerNotAFile { path });
    }
    Ok(path)
}

fn assert_semantics(frontend: &GoFrontend) -> Result<(), NativeHelperError> {
    let snapshot = frontend.discover()?;
    let key = SessionKey::new(
        frontend.identity(),
        snapshot.manifest(),
        typed_of::<ProfileSchema>(b"go-1.23"),
        typed_of::<FlowSchema>(b"semantic-index"),
        typed_of::<SemanticBasisSchema>(b"go/packages"),
    );
    let extraction = frontend.extract(&snapshot, key)?;
    let records = extraction.records();
    assert!(records.iter().any(|record| {
        record.kind() == FactKind::Declaration
            && record.key_bytes().ends_with(b"/Node")
            && record.value().starts_with(b"go-semantic-v1\0")
    }));
    assert!(records.iter().any(|record| {
        record.kind() == FactKind::Type && record.key_bytes().ends_with(b"/Render")
    }));
    assert!(records.iter().any(|record| {
        record.kind() == FactKind::Edge && record.key_bytes().windows(2).any(|part| part == b"->")
    }));
    assert!(records.iter().any(|record| {
        record.kind() == FactKind::Dependency && record.key_bytes() == b"go/fmt"
    }));
    Ok(())
}
