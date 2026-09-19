//! Java `javac`/doclet native authority adapter.
#![forbid(unsafe_code)]

use backend_compile::{GrammarVariant, SourceLanguage, SyntaxError, SyntaxFrontend};

#[path = "src/legacy/mod.rs"]
pub mod legacy;

/// Builds the zero-toolchain local Java syntax frontend.
///
/// This structural baseline never claims native semantic authority.
///
/// # Errors
/// Returns an error when the embedded grammar query cannot be admitted.
pub fn syntax_frontend() -> Result<SyntaxFrontend, SyntaxError> {
    // The stock query selects classes, interfaces, and methods only, so a
    // class arrived without its fields and an enum without its constants.
    let tags = format!(
        "{}\n{}",
        tree_sitter_java::TAGS_QUERY,
        r"
(field_declaration declarator: (variable_declarator name: (identifier) @name)) @definition.field
(enum_constant name: (identifier) @name) @definition.variant
(constructor_declaration name: (identifier) @name) @definition.constructor
(enum_declaration name: (identifier) @name) @definition.enum
(record_declaration name: (identifier) @name) @definition.struct
"
    );
    SyntaxFrontend::new(
        SourceLanguage::Java,
        b"tree-sitter-java-0.23.5/tags-v2",
        vec![GrammarVariant::new(
            &["java"],
            tree_sitter_java::LANGUAGE.into(),
            &tags,
        )?],
    )
}
