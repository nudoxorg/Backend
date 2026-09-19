//! Go `go/packages` native authority adapter.
#![forbid(unsafe_code)]

use backend_compile::{GrammarVariant, SourceLanguage, SyntaxError, SyntaxFrontend};

#[path = "src/legacy/mod.rs"]
pub mod legacy;

/// Builds the zero-toolchain local Go syntax frontend.
///
/// This structural baseline never claims native semantic authority.
///
/// # Errors
/// Returns an error when the embedded grammar query cannot be admitted.
pub fn syntax_frontend() -> Result<SyntaxFrontend, SyntaxError> {
    // The stock query selects package-level consts and vars without tagging
    // them a definition, so they were extracted and then dropped. They are
    // anchored at `source_file` here: a function-local `var` is not an
    // outline entry. Interface methods are `method_elem` in this grammar.
    let tags = format!(
        "{}\n{}",
        tree_sitter_go::TAGS_QUERY,
        r"
(field_declaration name: (field_identifier) @name) @definition.field
(method_elem name: (field_identifier) @name) @definition.method
(source_file (const_declaration (const_spec name: (identifier) @name) @definition.constant))
(source_file (var_declaration (var_spec name: (identifier) @name) @definition.variable))
"
    );
    SyntaxFrontend::new(
        SourceLanguage::Go,
        b"tree-sitter-go-0.25.0/tags-v2",
        vec![GrammarVariant::new(
            &["go"],
            tree_sitter_go::LANGUAGE.into(),
            &tags,
        )?],
    )
}
