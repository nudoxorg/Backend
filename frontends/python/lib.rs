//! Python Ruff/pyrefly native authority adapter.
#![forbid(unsafe_code)]

use backend_compile::{GrammarVariant, SourceLanguage, SyntaxError, SyntaxFrontend};

#[path = "src/legacy/mod.rs"]
pub mod legacy;

/// Static packaging declarations using the shared Ruff parser.
pub mod metadata;

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
(module (expression_statement (assignment left: (identifier) @name) @definition.variable))
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
        b"tree-sitter-python-0.25.0/tags-v7-python-module-assignment-source",
        vec![GrammarVariant::new(
            &["py", "pyi", "pyw"],
            tree_sitter_python::LANGUAGE.into(),
            &tags,
        )?],
    )
}

#[cfg(test)]
mod source_excerpt_tests {
    use std::path::Path;

    #[test]
    fn structural_python_source_retains_function_body_and_class_field() -> Result<(), String> {
        let source = "# λ before the declaration\napp = make_app()\nclass App:\n    field = 1\n\n@app.get(\"/api/health\")\ndef health() -> dict[str, str]:\n    return {\"status\": \"ok\"}\n";
        let analysis = super::syntax_frontend().map_err(|error| error.to_string())?
            .analyze(Path::new("backend/main.py"), source.as_bytes())
            .map_err(|error| error.to_string())?;
        for (name, expected) in [
            ("health", "@app.get(\"/api/health\")\ndef health() -> dict[str, str]:\n    return {\"status\": \"ok\"}"),
            ("app", "app = make_app()"),
            ("field", "field = 1"),
        ] {
            let declaration = analysis.declarations().iter().find(|row| row.name() == name)
                .ok_or_else(|| format!("missing {name}"))?;
            assert_eq!(declaration.source_excerpt().text(), Some(expected));
            assert_eq!(declaration.source_excerpt().extent(), Some(backend_compile::SourceExcerptExtent::Complete));
        }
        Ok(())
    }
}
