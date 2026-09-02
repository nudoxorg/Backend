//! Integration falsifiers for the TypeScript extraction boundary.
//! Each test names one preservation law and checks its exact observable fact.
//! The fixture is intentionally small and independent of the filesystem.

use compiler_languages_typescript::{
    extract_module,
    facts::{DeclarationKind, ExtractionError},
};

#[test]
fn declarations_keep_real_utf8_byte_spans() -> Result<(), ExtractionError> {
    let source = "// 😀\ninterface 用户 { value?: string }\n";
    let facts = extract_module("fixture.ts", source)?;
    assert!(!facts.declarations.is_empty());
    if let Some(declaration) = facts.declarations.first() {
        assert_eq!(declaration.kind, DeclarationKind::Interface);
        assert_eq!(declaration.name.as_ref(), "用户");
        assert_eq!(declaration.span.start.into_u32(), 8);
        assert_eq!(declaration.span.end.into_u32(), 43);
        let name_start = declaration.name.as_ref();
        assert_eq!(source.get(18..24), Some(name_start));
    }
    Ok(())
}

#[test]
fn malformed_source_retains_typed_parse_fault() {
    let result = extract_module("broken.ts", "interface User {");
    assert!(result.is_err(), "truncated source must reject");
    if let Err(ExtractionError::Parse {
        path, offending, ..
    }) = result
    {
        assert_eq!(path.to_string_lossy(), "broken.ts");
        assert!(!offending.is_empty());
    }
}

#[test]
fn annotation_user_is_nominal_not_type_variable() -> Result<(), ExtractionError> {
    let facts = extract_module("types.ts", "interface User { id: string }\nlet u: User;\n")?;
    assert!(
        matches!(facts.type_facts.first(), Some(compiler_languages_typescript::facts::TypeFact::Nominal { declaration, .. }) if declaration == "User")
    );
    Ok(())
}

#[test]
fn generic_parameter_use_is_type_variable() -> Result<(), ExtractionError> {
    let facts = extract_module("generic.ts", "const id = <T>(x: T): T => x;\n")?;
    assert!(
        matches!(facts.type_facts.first(), Some(compiler_languages_typescript::facts::TypeFact::TypeVar(name)) if name == "T")
    );
    Ok(())
}

#[test]
fn ast_lowering_preserves_import_export_and_members() -> Result<(), ExtractionError> {
    let source = "const a = 1;\nconst b = 2;\nimport { A } from \"./a\";\nexport function f(): void {}\nexport * from \"./m\";\ninterface Duo { a(): void; b(): void }";
    let facts = extract_module("fixture.ts", source)?;
    assert_eq!(facts.imports.len(), 1);
    let import = facts.imports.first();
    assert!(import.is_some());
    if let Some(import) = import {
        assert_eq!(import.request.as_ref(), "./a");
        assert_eq!(
            source
                .get(
                    usize::try_from(import.span.start.into_u32()).unwrap_or(0)
                        ..usize::try_from(import.span.end.into_u32()).unwrap_or(0),
                )
                .unwrap_or(""),
            "import { A } from \"./a\";"
        );
        assert!(
            matches!(&import.name, compiler_languages_typescript::facts::ImportName::Named { imported, local } if imported == "A" && local == "A")
        );
    }
    assert!(facts.exports.into_iter().any(|export| matches!(export.shape, compiler_languages_typescript::facts::ExportShape::Star { ref request, alias: None } if request == "./m")));
    let interface = facts
        .declarations
        .into_iter()
        .find(|declaration| declaration.kind == DeclarationKind::Interface);
    assert!(interface.is_some());
    let Some(interface) = interface else {
        return Ok(());
    };
    assert_eq!(interface.name.as_ref(), "Duo");
    assert_eq!(interface.members.len(), 2);
    assert_eq!(
        interface
            .members
            .into_iter()
            .map(|member| member.name.as_ref().to_owned())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
    Ok(())
}

#[test]
fn nominal_annotation_does_not_prefix_match() -> Result<(), ExtractionError> {
    let facts = extract_module(
        "types.ts",
        "interface User { id: string }\nlet u: UserService;\n",
    )?;
    assert!(!facts.type_facts.into_iter().any(|fact| matches!(
        fact,
        compiler_languages_typescript::facts::TypeFact::Nominal { .. }
    )));
    Ok(())
}

#[test]
fn declarations_and_specifiers_are_not_dropped() -> Result<(), ExtractionError> {
    let facts = extract_module(
        "fixture.ts",
        "export function f(): void {}\nimport D, { N } from \"./a\";\ninterface Repo<T> {}\nconst a = 1, b = 2;\n",
    )?;
    assert!(
        facts
            .exports
            .iter()
            .any(|export| export.name.as_ref() == "f")
    );
    assert_eq!(facts.imports.len(), 2);
    assert!(
        facts
            .declarations
            .iter()
            .any(|declaration| declaration.name.as_ref() == "a")
    );
    assert!(
        facts
            .declarations
            .iter()
            .any(|declaration| declaration.name.as_ref() == "b")
    );
    let repo = facts
        .declarations
        .iter()
        .find(|declaration| declaration.name.as_ref() == "Repo");
    assert!(repo.is_some());
    if let Some(repo) = repo {
        assert!(
            repo.type_parameters
                .iter()
                .any(|parameter| parameter.as_ref() == "T")
        );
    }
    Ok(())
}

#[test]
fn non_code_text_does_not_make_type_facts() -> Result<(), ExtractionError> {
    let facts = extract_module(
        "fixture.ts",
        "// use <T>\nconst text = \"interface Ghost\";\n",
    )?;
    assert!(facts.type_facts.is_empty());
    Ok(())
}

#[test]
fn uncommon_module_forms_keep_their_typed_shape() -> Result<(), ExtractionError> {
    let facts = extract_module(
        "fixture.ts",
        "import x = require(\"./a\");\nexport = Mod;\nexport * as ns from \"./m\";\n",
    )?;
    assert!(facts.declarations.is_empty());
    assert!(
        facts
            .imports
            .iter()
            .any(|import| import.request.as_ref() == "./a")
    );
    assert!(
        facts
            .exports
            .iter()
            .any(|export| export.name.as_ref() == "Mod")
    );
    assert!(facts.exports.iter().any(|export| matches!(
        &export.shape,
        compiler_languages_typescript::facts::ExportShape::Star { request, alias }
            if request == "./m" && alias.as_deref() == Some("ns")
    )));
    Ok(())
}
