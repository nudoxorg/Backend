//! Python Ruff/pyrefly native authority adapter.
#![forbid(unsafe_code)]

use backend_compile::{GrammarVariant, SourceLanguage, SyntaxError, SyntaxFrontend};

#[path = "src/legacy/mod.rs"]
pub mod legacy;

/// Builds the zero-toolchain local Python syntax frontend.
///
/// This structural baseline never claims native semantic authority.
///
/// # Errors
/// Returns an error when the embedded grammar query cannot be admitted.
pub fn syntax_frontend() -> Result<SyntaxFrontend, SyntaxError> {
    // Python has no field syntax, so a class's data members are exactly its
    // class-body assignments; anchoring on `class_definition` keeps a
    // function-local assignment out of the outline. A `@property` accessor is
    // a field to a reader and is selected by its decorator's text.
    let tags = format!(
        "{}\n{}",
        tree_sitter_python::TAGS_QUERY,
        r#"
(class_definition
  body: (block (expression_statement (assignment left: (identifier) @name) @definition.field)))
(decorated_definition
  (decorator (identifier) @_decorator)
  definition: (function_definition name: (identifier) @name) @definition.property
  (#eq? @_decorator "property"))
"#
    );
    SyntaxFrontend::new(
        SourceLanguage::Python,
        b"tree-sitter-python-0.25.0/tags-v2",
        vec![GrammarVariant::new(
            &["py", "pyi", "pyw"],
            tree_sitter_python::LANGUAGE.into(),
            &tags,
        )?],
    )
}
