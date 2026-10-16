//! Public structural extraction regression from the pinned SearXNG corpus.
//! The imported module is a reference; only the grammar's `name` field binds.
use backend_compile::{DeclarationKind, SourceBindingIdentity};
use backend_frontend_python::legacy::{self, DeclarationKind as RuffKind};
use backend_semantic::vocabulary::PythonVersion;
use std::path::Path;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn searxng_from_imports_emit_one_binding_without_a_module_reference_declaration() -> TestResult {
    // Original byte identities are pinned in searxng-captured-frontier.tsv.
    // Runtime4's distinct symbol pairs came from these exact source lines.
    let cases = [
        ("tests/unit/test_search.py", 4, "copy", "copy"),
        (
            "searx/engines/360search_videos.py",
            6,
            "datetime",
            "datetime",
        ),
        (
            "searx/search/processors/abstract.py",
            14,
            "searx.engines",
            "engines",
        ),
        (
            "searx/search/processors/online_dictionary.py",
            7,
            "searx.sxng_locales",
            "sxng_locales",
        ),
    ];
    let frontend = backend_frontend_python::syntax_frontend()?;
    for (path, line, module, binding) in cases {
        let prefix = format!("# λ source byte offsets\n{}", "\n".repeat(line - 2));
        let statement = format!("from {module} import {binding}");
        let source = format!("{prefix}{statement}\n");
        let syntax = frontend.analyze(Path::new(path), source.as_bytes())?;
        let imports = syntax
            .declarations()
            .iter()
            .filter(|d| d.kind() == DeclarationKind::Import)
            .collect::<Vec<_>>();
        assert_eq!(
            imports.len(),
            1,
            "the module reference is not a second import declaration: {path}"
        );
        assert_eq!(imports[0].name(), binding);
        assert_eq!(imports[0].line(), line as u32);
        assert_eq!(
            imports[0].signature(),
            format!("value\n{module}\n{binding}")
        );
        assert_eq!(imports[0].source_excerpt().text(), Some(statement.as_str()));
        assert_eq!(
            imports[0].binding_identity(),
            SourceBindingIdentity::Legacy,
            "this repair does not invent exact identity for the legacy Python structural lane"
        );
        // Independent Ruff AST proves one introduced binding at the final
        // identifier extent, distinct from the earlier module reference.
        let facts = legacy::extract(source.as_bytes(), PythonVersion::Python314)?;
        let aliases = facts
            .declarations
            .iter()
            .filter(|d| d.kind == RuffKind::Alias)
            .collect::<Vec<_>>();
        assert_eq!(aliases.len(), 1);
        let expected_start = prefix.len() + statement.rfind(binding).expect("binding");
        assert_eq!(
            (aliases[0].name_span.start, aliases[0].name_span.end),
            (
                expected_start as u32,
                (expected_start + binding.len()) as u32
            )
        );
        assert_eq!(
            &source.as_bytes()
                [aliases[0].name_span.start as usize..aliases[0].name_span.end as usize],
            binding.as_bytes()
        );
    }
    Ok(())
}

#[test]
fn from_import_fields_preserve_aliases_relative_multiline_and_real_repeated_bindings() -> TestResult
{
    let source = "# 🐍 UTF-8 and CRLF\r\nfrom phantom_module import item, item as renamed\r\nfrom .relative_module import (\r\n    other,\r\n    value as alias,\r\n)\r\nfrom wildcard_module import *\r\nfrom copy import copy; from copy import copy\r\nfrom copy import copy, copy\r\n";
    let syntax = backend_frontend_python::syntax_frontend()?
        .analyze(Path::new("imports.py"), source.as_bytes())?;
    let imports = syntax
        .declarations()
        .iter()
        .filter(|d| d.kind() == DeclarationKind::Import)
        .collect::<Vec<_>>();
    assert_eq!(
        imports.iter().map(|d| d.name()).collect::<Vec<_>>(),
        [
            "item", "renamed", "other", "alias", "copy", "copy", "copy", "copy"
        ]
    );
    assert!(
        imports
            .iter()
            .all(|d| !["phantom_module", "relative_module", "wildcard_module"].contains(&d.name()))
    );
    assert_eq!(imports[1].signature(), "value\nphantom_module\nitem");
    assert_eq!(imports[2].signature(), "value\n/relative_module\nother");
    assert_eq!(imports[3].signature(), "value\n/relative_module\nvalue");
    // Distinct source sites sharing one line/name/signature stay distinct.
    // Search must not deduplicate them by display coordinate.
    assert_eq!(
        imports
            .iter()
            .filter(|d| d.name() == "copy" && d.line() == 8)
            .count(),
        2
    );
    assert_eq!(
        imports
            .iter()
            .filter(|d| d.name() == "copy" && d.line() == 9)
            .count(),
        2
    );
    let facts = legacy::extract(source.as_bytes(), PythonVersion::Python314)?;
    let copies = facts
        .declarations
        .iter()
        .filter(|d| d.kind == RuffKind::Alias && d.name == "copy")
        .collect::<Vec<_>>();
    assert_eq!(copies.len(), 4);
    let spans = copies
        .iter()
        .map(|d| (d.name_span.start, d.name_span.end))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        spans.len(),
        4,
        "real repeated bindings have four distinct AST extents"
    );
    Ok(())
}
