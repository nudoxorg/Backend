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
