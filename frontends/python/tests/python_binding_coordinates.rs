//! Rebinding sites retain exact native declaration extents and written owners.

use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use backend_frontend_python::legacy::checker::{
    NativePythonProjectAuthority, PythonProjectControl, PythonProjectSource, SymbolOutcome,
};
use backend_frontend_python::legacy::{DeclarationKind, extract};
use backend_semantic::ir::{PythonSourceCoordinate, SourceIdentity};
use backend_semantic::vocabulary::PythonVersion;

const SOURCE: &str = "# 🐍 exact UTF-8 and CRLF bytes\r\nimport sys\r\nvalue: int = 1\r\nvalue = 2\r\nif sys.version_info[0] < 3:\r\n    text_type = bytes\r\nelse:\r\n    text_type = str\r\n";

#[test]
fn rebinding_extent_follows_name_without_moving_first_written_owner() {
    for version in [PythonVersion::Python313, PythonVersion::Python314] {
        let facts = extract(SOURCE.as_bytes(), version).expect("syntax authority");
        for (name, first, last) in [
            ("value", "value: int = 1", "value = 2"),
            ("text_type", "text_type = bytes", "text_type = str"),
        ] {
            let declarations = facts
                .declarations
                .iter()
                .filter(|row| row.name == name)
                .collect::<Vec<_>>();
            assert_eq!(declarations.len(), 1, "existing module deduplication");
            let declaration = declarations[0];
            assert_eq!(declaration.kind, DeclarationKind::Constant);
            assert_eq!(
                &SOURCE[declaration.span.start as usize..declaration.span.end as usize],
                first,
                "the first written annotation and alias owner stays exact"
            );
            assert_eq!(
                &SOURCE[declaration.binding_span.start as usize
                    ..declaration.binding_span.end as usize],
                last
            );
            assert_eq!(declaration.name_span.start, declaration.binding_span.start);
            assert!(declaration.span.end < declaration.name_span.start);
        }
        let written = facts
            .annotations
            .iter()
            .find(|row| row.owner == "value")
            .expect("first written annotation");
        assert_eq!(
            &SOURCE[written.span.start as usize..written.span.end as usize],
            "int"
        );
    }
}

#[test]
fn native_rebound_constants_encode_the_exact_current_binding_source_site() {
    let root = std::env::temp_dir().join(format!(
        "nudox-python-rebound-coordinates-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("fixture root");
    let checker = NativePythonProjectAuthority::admit().expect("compiled native authority");
    let caller = "from . import compat\nvalue = compat.value\ntext_type = compat.text_type\n";
    let cancelled = AtomicBool::new(false);
    for version in [PythonVersion::Python313, PythonVersion::Python314] {
        for prefix in ["", "# fresh immutable source snapshot\r\n"] {
            let source = format!("{prefix}{SOURCE}");
            let sources = [
                PythonProjectSource {
                    relative_path: "pkg/__init__.py",
                    source: "",
                },
                PythonProjectSource {
                    relative_path: "pkg/compat.py",
                    source: &source,
                },
                PythonProjectSource {
                    relative_path: "pkg/client.py",
                    source: caller,
                },
            ];
            let control = || PythonProjectControl {
                cancelled: &cancelled,
                deadline: Instant::now() + Duration::from_secs(30),
            };
            let report = checker
                .analyze_project(&root, "pkg", &sources, version, control())
                .expect("fresh native rebinding transaction");
            let client = report.module("pkg/client.py").expect("caller report");
            for (name, statement) in [("value", "value = 2"), ("text_type", "text_type = str")] {
                let occurrence = caller.find(&format!("compat.{name}")).expect("caller") + 7;
                let target = client
                    .symbols
                    .iter()
                    .find_map(|symbol| match &symbol.outcome {
                        SymbolOutcome::Definition { target, .. }
                            if symbol.span.start as usize == occurrence =>
                        {
                            Some(target)
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| panic!("native exact target for {name}"));
                assert_eq!(target.relative_path.as_ref(), "pkg/compat.py");
                let coordinate = PythonSourceCoordinate::decode(&target.source_coordinate)
                    .expect("admissible native coordinate")
                    .0;
                let expected_start = source.rfind(statement).expect("last declaration") as u32;
                assert_eq!(coordinate.declaration_start, expected_start);
                assert_eq!(
                    coordinate.declaration_end,
                    expected_start + statement.len() as u32
                );
                assert_eq!(coordinate.name_start, target.name_span.start);
                assert_eq!(coordinate.name_start, expected_start);
                assert_eq!(
                    coordinate.source,
                    *SourceIdentity::from_bytes(source.as_bytes())
                        .expect("exact source identity")
                        .identity
                );
                assert_eq!(
                    &source[target.name_span.start as usize..target.name_span.end as usize],
                    name
                );
            }
            report
                .witness()
                .validate_current(control())
                .expect("current witness");
        }
    }
    std::fs::remove_dir_all(root).expect("fixture cleanup");
}
