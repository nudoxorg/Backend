//! Exercises the hermetic legacy envelope only as a compatibility contract;
//! semantic parity itself is covered by the Ruff and Pyrefly authorities.
//!
//! The Python authority test requires an explicitly admitted interpreter.
//! A missing or unusable interpreter is a typed failure, never a silent pass,
//! so the suite cannot report green without its toolchain.

use backend_compile::{
    Authority, AuthorityError, Coverage, FactKind, FlowSchema, ProfileSchema, SemanticBasisSchema,
    SessionKey, typed_of,
};
use backend_frontend_python::PythonFrontend;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
enum NativeHelperError {
    #[error(
        "COMPILER_PYTHON_COMPILER must name the Python interpreter used for the real Python authority test"
    )]
    MissingInterpreter,
    #[error("configured Python interpreter is not a file: {path:?}")]
    InterpreterNotAFile {
        /// Rejected interpreter path.
        path: PathBuf,
    },
    #[error("Python authority did not reach complete coverage for {path:?}")]
    AuthorityUnavailable {
        /// Configured interpreter path.
        path: PathBuf,
    },
    #[error(transparent)]
    Authority(#[from] AuthorityError),
}

#[test]
fn bundled_python_helper_emits_admitted_semantics() -> Result<(), NativeHelperError> {
    let python = configured_interpreter()?;
    let source = br#"from dataclasses import dataclass

@dataclass
class Model:
    value: str

def render(model: Model) -> str:
    """Render a model."""
    return model.value
"#;
    let frontend = PythonFrontend::bundled(
        source.to_vec(),
        python.to_string_lossy(),
        "3.13",
        b"{}".to_vec(),
    )?;
    let snapshot = frontend.discover()?;
    let key = SessionKey::new(
        frontend.identity(),
        snapshot.manifest(),
        typed_of::<ProfileSchema>(b"python-3.13"),
        typed_of::<FlowSchema>(b"semantic-index"),
        typed_of::<SemanticBasisSchema>(b"cpython-ast"),
    );
    let extraction = frontend.extract(&snapshot, key)?;
    if extraction.coverage().state() != Coverage::Complete {
        return Err(NativeHelperError::AuthorityUnavailable { path: python });
    }
    assert!(extraction.records().iter().any(|record| {
        record.kind() == FactKind::Declaration && record.key_bytes() == b"module.Model"
    }));
    assert!(extraction.records().iter().any(|record| {
        record.kind() == FactKind::Type && record.key_bytes() == b"module.Model.value"
    }));
    assert!(
        extraction
            .records()
            .iter()
            .any(|record| record.kind() == FactKind::Edge)
    );
    assert!(extraction.records().iter().any(|record| {
        record.kind() == FactKind::Dependency && record.key_bytes() == b"python/dataclasses"
    }));
    Ok(())
}

/// Resolves the explicitly configured interpreter or returns a typed terminal.
fn configured_interpreter() -> Result<PathBuf, NativeHelperError> {
    let path = std::env::var_os("COMPILER_PYTHON_COMPILER")
        .map(PathBuf::from)
        .ok_or(NativeHelperError::MissingInterpreter)?;
    if !path.is_file() {
        return Err(NativeHelperError::InterpreterNotAFile { path });
    }
    Ok(path)
}

/// The absent-toolchain terminal is itself observable: an authority whose
/// interpreter path does not exist must report `Coverage::Unavailable`
/// rather than a fabricated complete result or a silent skip.
#[test]
fn absent_interpreter_is_a_typed_unavailable_coverage_terminal() -> Result<(), NativeHelperError> {
    let frontend = PythonFrontend::new(
        b"x = 1\n".to_vec(),
        "/no/such/nudox-python".to_owned(),
        "/no/such/nudox-python".to_owned(),
        "3.13",
        b"{}".to_vec(),
    )?;
    let snapshot = frontend.discover()?;
    let key = SessionKey::new(
        frontend.identity(),
        snapshot.manifest(),
        typed_of::<ProfileSchema>(b"python-3.13"),
        typed_of::<FlowSchema>(b"semantic-index"),
        typed_of::<SemanticBasisSchema>(b"cpython-ast"),
    );
    let extraction = frontend.extract(&snapshot, key)?;
    assert_eq!(extraction.coverage().state(), Coverage::Unavailable);
    Ok(())
}
