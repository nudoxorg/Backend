//! Exercises the hermetic legacy envelope only as a compatibility contract;
//! semantic parity itself is covered by the Ruff and Pyrefly authorities.

use backend_compile::{
    Authority, Coverage, FactKind, FlowSchema, ProfileSchema, SemanticBasisSchema, SessionKey,
    typed_of,
};
use backend_frontend_python::PythonFrontend;
use std::{error::Error, path::PathBuf};

#[test]
fn bundled_python_helper_emits_admitted_semantics() -> Result<(), Box<dyn Error>> {
    let Some(python) = find_executable("python3") else {
        return Ok(());
    };
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
    assert_eq!(extraction.coverage().state(), Coverage::Complete);
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

fn find_executable(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
        .and_then(|candidate| candidate.canonicalize().ok())
}
