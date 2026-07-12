//! Tier C via tsz — proves the in-process tsz checker oracle ENRICHES the IR
//! beyond the syntactic pass: it recovers checker-inferred return types that
//! have no source annotation (only a type checker can produce them).

use std::fs;
use std::path::Path;

use compiler::compile::typescript::oracle::tsz;
use ir::kind::Entry;
use ir::parameter::Parameter;
use ir::ty::Type;
use tempfile::TempDir;

fn write(dir: &Path, rel: &str, body: &str) {
    fs::write(dir.join(rel), body).expect("write fixture");
}

/// `compute` has NO return annotation — syntactic extraction cannot know its
/// return type. The tsz checker infers `number`; after declaration emit +
/// re-extraction the IR must carry that inferred output type.
#[test]
fn tsz_oracle_recovers_inferred_return_type() {
    let dir = TempDir::new().expect("tempdir");
    write(dir.path(), "mod.ts", "export function compute(a: number, b: number) { return a + b; }\n");
    write(dir.path(), "package.json", r#"{"name":"tszfix","types":"mod.ts"}"#);

    let index = match tsz::normalize(dir.path(), "tszfix") {
        Ok(i) => i,
        Err(e) => panic!("tsz oracle normalize failed (should emit .d.ts for a trivial fn): {e}"),
    };

    let compute = index
        .entries_by_path
        .values()
        .find(|e| e.name() == "compute")
        .unwrap_or_else(|| panic!("no `compute` entry; have {:?}",
            index.entries_by_path.values().map(|e| e.name()).collect::<Vec<_>>()));

    let func = match compute {
        Entry::Function(sym) => &sym.inner,
        other => panic!("compute should be Entry::Function, got {other}"),
    };

    let outputs = func
        .output_parameters
        .as_ref()
        .expect("tsz-inferred return type must be present (syntactic pass has none)");
    let has_numeric = outputs.iter().any(|p| matches!(
        p,
        Parameter::Literal(l) if matches!(l.r#type, Some(Type::Primitive(_)))
    ));
    assert!(
        has_numeric,
        "compute's inferred return type should lower to a primitive (number); got {outputs:?}"
    );
}
