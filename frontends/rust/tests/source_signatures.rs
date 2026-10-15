//! Written Rust headers and doc comments, as the grammar sees them.
//!
//! A header is everything before the body, however many lines it spans; and
//! documentation is `///`, `/** */`, or `#[doc = "…"]`, never a plain `//`
//! comment, even a commented-out line of code.
use backend_compile::{SourceAnalysis, SourceDeclaration, SyntaxError};
use std::path::Path;

fn analyze(source: &str) -> Result<SourceAnalysis, SyntaxError> {
    backend_frontend_rust::syntax_frontend()?.analyze(Path::new("lib.rs"), source.as_bytes())
}

fn declaration<'a>(analysis: &'a SourceAnalysis, name: &str) -> &'a SourceDeclaration {
    analysis
        .declarations()
        .iter()
        .find(|row| row.name() == name)
        .expect("captured declaration")
}

#[test]
fn a_header_runs_to_its_body_over_every_line_it_spans() -> Result<(), SyntaxError> {
    let analysis = analyze(
        "pub fn build<'a, T>(\n    input: &'a str,\n    limit: [u8; 4],\n) -> Result<T, Error>\nwhere\n    T: Clone,\n{\n    todo!()\n}\n\n\
         pub struct Wrapper(\n    pub u32,\n    pub String,\n);\n\n\
         pub fn lifetimes<'a>(x: &'a str) -> &'a str {\n    x\n}\n",
    )?;
    assert_eq!(
        declaration(&analysis, "build").signature(),
        "pub fn build<'a, T>(\n    input: &'a str,\n    limit: [u8; 4],\n) -> Result<T, Error>\nwhere\n    T: Clone,"
    );
    assert_eq!(
        declaration(&analysis, "Wrapper").signature(),
        "pub struct Wrapper(\n    pub u32,\n    pub String,\n);"
    );
    assert_eq!(
        declaration(&analysis, "lifetimes").signature(),
        "pub fn lifetimes<'a>(x: &'a str) -> &'a str"
    );
    Ok(())
}

#[test]
fn documentation_is_a_doc_comment_never_a_commented_out_line() -> Result<(), SyntaxError> {
    let analysis = analyze(
        "// mod support;\n// use support::*;\npub struct Iter;\n\n\
         /// Returns the count.\n///\n/// See [`other`] too.\npub fn documented() {}\n\n\
         // plain note\n/// Doc kept.\n// another plain note\npub fn mixed() {}\n\n\
         /** Block doc. */\npub fn block() {}\n\n\
         #[doc = \"Attribute doc.\"]\n#[inline]\npub fn attribute() {}\n",
    )?;
    assert_eq!(declaration(&analysis, "Iter").documentation(), "");
    assert_eq!(
        declaration(&analysis, "documented").documentation(),
        "Returns the count.\n\nSee [`other`] too."
    );
    assert_eq!(declaration(&analysis, "mixed").documentation(), "Doc kept.");
    assert_eq!(
        declaration(&analysis, "block").documentation(),
        "Block doc."
    );
    assert_eq!(
        declaration(&analysis, "attribute").documentation(),
        "Attribute doc."
    );
    Ok(())
}
