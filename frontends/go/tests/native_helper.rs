//! End-to-end Go semantic helper integration.

use backend_compile::{
    Authority, FactKind, FlowSchema, ProfileSchema, SemanticBasisSchema, SessionKey, typed_of,
};
use backend_frontend_go::GoFrontend;
use std::{error::Error, path::PathBuf};

#[test]
fn go_packages_helper_emits_admitted_semantics() -> Result<(), Box<dyn Error>> {
    let Some(go) = find_executable("go") else {
        return Ok(());
    };
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

fn find_executable(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
        .and_then(|candidate| candidate.canonicalize().ok())
}

fn assert_semantics(frontend: &GoFrontend) -> Result<(), Box<dyn Error>> {
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
