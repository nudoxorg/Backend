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
            "item", "renamed", "alias", "other", "copy", "copy", "copy", "copy"
        ]
    );
    assert!(
        imports
            .iter()
            .all(|d| !["phantom_module", "relative_module", "wildcard_module"].contains(&d.name()))
    );
    // Public extraction sorts (statement line, kind, name). Multiline
    // imports share the statement line, so alias precedes other.
    for (name, signature) in [
        ("renamed", "value\nphantom_module\nitem"),
        ("other", "value\n/relative_module\nother"),
        ("alias", "value\n/relative_module\nvalue"),
    ] {
        assert_eq!(
            imports
                .iter()
                .find(|row| row.name() == name)
                .expect("binding")
                .signature(),
            signature
        );
    }
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
    assert_copy_alias_extents(source, &copies)?;
    Ok(())
}

fn assert_copy_alias_extents(source: &str, copies: &[&legacy::DeclarationFact]) -> TestResult {
    let mut expected = source
        .match_indices("from copy import copy")
        .map(|(start, _)| {
            let start = start + "from copy import ".len();
            (start as u32, (start + "copy".len()) as u32)
        })
        .collect::<Vec<_>>();
    let last = source.rfind(", copy").ok_or("last repeated binding")? + 2;
    expected.push((last as u32, (last + "copy".len()) as u32));
    assert_eq!(
        copies
            .iter()
            .map(|row| (row.name_span.start, row.name_span.end))
            .collect::<Vec<_>>(),
        expected,
        "each range selects the introduced name, excluding its module reference"
    );
    for (index, row) in copies.iter().enumerate() {
        let name = &source.as_bytes()[row.name_span.start as usize..row.name_span.end as usize];
        let owner = &source.as_bytes()[row.span.start as usize..row.span.end as usize];
        assert_eq!(name, b"copy");
        assert_eq!(row.span, row.binding_span);
        assert_eq!(
            owner,
            if index < 2 {
                b"from copy import copy".as_slice()
            } else {
                b"from copy import copy, copy".as_slice()
            }
        );
    }
    Ok(())
}

#[test]
fn repeated_import_sites_do_not_relax_other_module_rebinding_or_overload_boundaries() -> TestResult
{
    let source = "# λ\r\nimport pkg.child\r\nimport other.module as pkg\r\nfrom third import value as pkg\r\npkg = 42\r\nvalue = 1\r\nvalue = 2\r\ntype Alias = int\r\ntype Alias = str\r\ndef overloaded(): pass\r\ndef overloaded(): pass\r\nfrom fourth import overloaded\r\ndef overloaded(): pass\r\n";
    let facts = legacy::extract(source.as_bytes(), PythonVersion::Python314)?;
    let imports = facts
        .declarations
        .iter()
        .filter(|row| row.kind == RuffKind::Alias && row.name == "pkg")
        .collect::<Vec<_>>();
    assert_eq!(imports.len(), 3);
    let expected = [
        "import pkg.child",
        "import other.module as pkg",
        "from third import value as pkg",
    ];
    for (row, statement) in imports.iter().zip(expected) {
        assert_eq!(
            &source.as_bytes()[row.name_span.start as usize..row.name_span.end as usize],
            b"pkg"
        );
        assert_eq!(
            &source[row.span.start as usize..row.span.end as usize],
            statement
        );
    }
    assert!(
        !facts
            .declarations
            .iter()
            .any(|row| row.kind == RuffKind::Constant && row.name == "pkg")
    );
    let constants = facts
        .declarations
        .iter()
        .filter(|row| row.kind == RuffKind::Constant && row.name == "value")
        .collect::<Vec<_>>();
    assert_eq!(
        constants.len(),
        1,
        "ordinary rebinding keeps one module declaration"
    );
    assert_eq!(
        &source[constants[0].span.start as usize..constants[0].span.end as usize],
        "value = 1"
    );
    assert_eq!(
        &source[constants[0].binding_span.start as usize..constants[0].binding_span.end as usize],
        "value = 2"
    );
    assert_eq!(
        facts
            .declarations
            .iter()
            .filter(|row| row.kind == RuffKind::Alias && row.name == "Alias")
            .count(),
        1,
        "PEP695 aliases still use the ordinary declaration policy"
    );
    assert_eq!(
        facts
            .declarations
            .iter()
            .filter(|row| row.kind == RuffKind::Function && row.name == "overloaded")
            .count(),
        2,
        "consecutive overloads survive; an intervening import ends their run"
    );
    Ok(())
}

#[test]
fn import_site_retention_does_not_promote_function_or_class_imports_to_module_facts() -> TestResult
{
    let source = "import module as visible\nclass Container:\n    import module as class_only\ndef local_scope():\n    import module as local_only\n    local_only()\n";
    let facts = legacy::extract(source.as_bytes(), PythonVersion::Python314)?;
    let aliases = facts
        .declarations
        .iter()
        .filter(|row| row.kind == RuffKind::Alias)
        .map(|row| row.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(aliases, ["visible"]);
    assert!(
        facts
            .binding_scopes
            .iter()
            .any(|scope| scope.locals.iter().any(|name| name == "local_only")),
        "function imports remain local lexical bindings"
    );
    assert!(
        facts.declared_member_inventories.is_empty(),
        "class imports still prevent a complete class member inventory claim"
    );
    Ok(())
}
