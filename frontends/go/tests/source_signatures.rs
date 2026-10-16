//! Written Go headers and doc comments, as the grammar sees them.
//!
//! A `{` inside a header is a type (`interface{}`), not the body, and a doc
//! comment is the block written directly above the declaration.
use backend_compile::{SourceAnalysis, SourceDeclaration, SyntaxError};
use std::path::Path;

fn analyze(source: &str) -> Result<SourceAnalysis, SyntaxError> {
    backend_frontend_go::syntax_frontend()?.analyze(Path::new("pkg.go"), source.as_bytes())
}

fn declaration<'a>(analysis: &'a SourceAnalysis, name: &str) -> &'a SourceDeclaration {
    analysis
        .declarations()
        .iter()
        .find(|row| row.name() == name)
        .expect("captured declaration")
}

#[test]
fn an_empty_interface_in_a_header_does_not_end_it() -> Result<(), SyntaxError> {
    let analysis = analyze(
        "package pkg\n\n\
         func Marshal(v interface{}) ([]byte, error) {\n\treturn nil, nil\n}\n\n\
         func (p *Properties) Decode(x interface{}) error {\n\treturn nil\n}\n\n\
         func Any() interface{} {\n\treturn nil\n}\n\n\
         func Pair(m map[string]struct{}, f func(a interface{}) bool) {\n}\n",
    )?;
    assert_eq!(
        declaration(&analysis, "Marshal").signature(),
        "func Marshal(v interface{}) ([]byte, error)"
    );
    assert_eq!(
        declaration(&analysis, "Decode").signature(),
        "func (p *Properties) Decode(x interface{}) error"
    );
    assert_eq!(
        declaration(&analysis, "Any").signature(),
        "func Any() interface{}"
    );
    assert_eq!(
        declaration(&analysis, "Pair").signature(),
        "func Pair(m map[string]struct{}, f func(a interface{}) bool)"
    );
    Ok(())
}

#[test]
fn a_type_declaration_ends_where_its_body_begins() -> Result<(), SyntaxError> {
    let analysis = analyze(
        "package pkg\n\n\
         type Decoder struct {\n\tcache map[string]int\n}\n\n\
         type Reader interface {\n\tRead(p []byte) (int, error)\n}\n\n\
         type Marker struct{}\n",
    )?;
    assert_eq!(
        declaration(&analysis, "Decoder").signature(),
        "Decoder struct"
    );
    assert_eq!(
        declaration(&analysis, "Reader").signature(),
        "Reader interface"
    );
    assert_eq!(
        declaration(&analysis, "Marker").signature(),
        "Marker struct{}"
    );
    Ok(())
}

#[test]
fn documentation_is_the_comment_block_directly_above() -> Result<(), SyntaxError> {
    let analysis = analyze(
        "package pkg\n\n\
         // Expand expands the path\n// to include the home directory.\nfunc Expand() {}\n\n\
         // Detached: a blank line separates this from its declaration.\n\nfunc Detached() {}\n\n\
         var a = 1 // trails a, documents nothing\nfunc After() {}\n",
    )?;
    assert_eq!(
        declaration(&analysis, "Expand").documentation(),
        "Expand expands the path\nto include the home directory."
    );
    assert_eq!(declaration(&analysis, "Detached").documentation(), "");
    assert_eq!(declaration(&analysis, "After").documentation(), "");
    Ok(())
}
