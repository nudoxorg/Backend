//! Written TypeScript headers and doc comments, as the grammar sees them.
//!
//! An object type inside a header is a type, not the body; documentation is a
//! JSDoc block, not a plain comment.
use backend_compile::{SourceAnalysis, SourceDeclaration, SyntaxError};
use std::path::Path;

fn analyze(source: &str) -> Result<SourceAnalysis, SyntaxError> {
    backend_frontend_typescript::syntax_frontend()?
        .analyze(Path::new("scheduler.ts"), source.as_bytes())
}

fn declaration<'a>(analysis: &'a SourceAnalysis, name: &str) -> &'a SourceDeclaration {
    analysis
        .declarations()
        .iter()
        .find(|row| row.name() == name)
        .expect("captured declaration")
}

#[test]
fn class_binding_extents_include_only_grammar_owned_decorators_and_terminators()
-> Result<(), SyntaxError> {
    use backend_compile::SourceBindingIdentity;
    let source = "class Example {\n  @Input() currentPage = 1 /* before terminator */;\n  automatic = 2\n  @Input() /* between decorator and method */ set ifAuthenticated(value: boolean) {}\n  plain() {};\n}\n";
    let analysis = analyze(source)?;
    for (name, expected) in [
        (
            "currentPage",
            "@Input() currentPage = 1 /* before terminator */;",
        ),
        ("automatic", "automatic = 2"),
        (
            "ifAuthenticated",
            "@Input() /* between decorator and method */ set ifAuthenticated(value: boolean) {}",
        ),
        ("plain", "plain() {}"),
    ] {
        let SourceBindingIdentity::Exact(capture) = declaration(&analysis, name).binding_identity()
        else {
            panic!("exact TypeScript binding");
        };
        let (start, end) = capture.owner();
        assert_eq!(&source[start as usize..end as usize], expected, "{name}");
    }
    Ok(())
}

#[test]
fn property_binding_extents_include_only_grammar_owned_type_member_separators()
-> Result<(), SyntaxError> {
    use backend_compile::SourceBindingIdentity;
    let source = concat!(
        "// λ keeps all captures in exact UTF-8 byte coordinates\n",
        "interface Debug {\n",
        "  optional?: string /* before terminator */;\n",
        "  comma: number,\n",
        "  automatic: boolean\n",
        "  next: number;\n",
        "}\n",
        "type RecordShape = { nested: { inner: string; }; noTerminator: number };\n",
        "function control() {};\n",
    );
    let analysis = analyze(source)?;
    for (name, expected) in [
        ("optional", "optional?: string /* before terminator */;"),
        ("comma", "comma: number,"),
        ("automatic", "automatic: boolean"),
        ("next", "next: number;"),
        ("nested", "nested: { inner: string; };"),
        ("inner", "inner: string;"),
        ("noTerminator", "noTerminator: number"),
        ("control", "function control() {}"),
    ] {
        let SourceBindingIdentity::Exact(capture) = declaration(&analysis, name).binding_identity()
        else {
            panic!("exact TypeScript binding");
        };
        let (start, end) = capture.owner();
        assert_eq!(&source[start as usize..end as usize], expected, "{name}");
        let (start, end) = capture.name();
        assert_eq!(&source[start as usize..end as usize], name);
    }
    Ok(())
}

#[test]
fn object_types_in_a_header_do_not_end_it() -> Result<(), SyntaxError> {
    let analysis = analyze(
        "export function run(x: { a: number }): { b: string } {\n  return { b: \"\" };\n}\n\n\
         export const handler = (request: Request): { status: number } => {\n  return { status: 1 };\n};\n\n\
         export class Box {\n  value = 1;\n}\n",
    )?;
    assert_eq!(
        declaration(&analysis, "run").signature(),
        "export function run(x: { a: number }): { b: string }"
    );
    assert_eq!(
        declaration(&analysis, "handler").signature(),
        "export const handler = (request: Request): { status: number } =>"
    );
    assert_eq!(
        declaration(&analysis, "Box").signature(),
        "export class Box"
    );
    Ok(())
}

#[test]
fn documentation_is_a_jsdoc_block_never_a_plain_comment() -> Result<(), SyntaxError> {
    let analysis = analyze(
        "// generateSchedule(old);\nexport function plain() {}\n\n\
         /**\n * Builds the schedule.\n * @param week the week\n */\nexport function documented(week: number) {}\n",
    )?;
    assert_eq!(declaration(&analysis, "plain").documentation(), "");
    assert_eq!(
        declaration(&analysis, "documented").documentation(),
        "Builds the schedule.\n@param week the week"
    );
    Ok(())
}

#[test]
fn import_bindings_retain_exact_statement_and_local_name_captures() -> Result<(), SyntaxError> {
    use backend_compile::{DeclarationKind, SourceBindingIdentity};
    let source = "// λ byte offsets\nimport Default, { ArticlesService, Other as Renamed } from './articles.service';\nimport * as Namespace from './other';\nimport { type Hidden } from './types';\n";
    let analysis = analyze(source)?;
    let imports = analysis
        .declarations()
        .iter()
        .filter(|site| site.kind() == DeclarationKind::Import)
        .collect::<Vec<_>>();
    assert_eq!(imports.len(), 4);
    let statement =
        "import Default, { ArticlesService, Other as Renamed } from './articles.service';";
    let start = source.find(statement).expect("statement") as u32;
    for name in ["Default", "ArticlesService", "Renamed", "Namespace"] {
        let site = imports
            .iter()
            .copied()
            .find(|site| site.name() == name)
            .expect("binding");
        let SourceBindingIdentity::Exact(capture) = site.binding_identity() else {
            panic!("exact import binding")
        };
        assert_eq!(
            &source[capture.name().0 as usize..capture.name().1 as usize],
            name
        );
        if name != "Namespace" {
            assert_eq!(capture.owner(), (start, start + statement.len() as u32));
        }
    }
    assert!(imports.iter().all(|site| site.name() != "Hidden"));
    Ok(())
}
