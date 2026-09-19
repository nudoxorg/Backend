//! C# Roslyn native authority adapter.
#![forbid(unsafe_code)]

use backend_compile::{GrammarVariant, SourceLanguage, SyntaxError, SyntaxFrontend};

#[path = "src/legacy/mod.rs"]
pub mod legacy;

/// Builds the zero-toolchain local C# syntax frontend.
///
/// This structural baseline never claims native semantic authority.
///
/// # Errors
/// Returns an error when the embedded grammar query cannot be admitted.
pub fn syntax_frontend() -> Result<SyntaxFrontend, SyntaxError> {
    // The stock query selects classes, interfaces, methods, and namespaces
    // only. A C# field carries its name two levels down, in the variable
    // declarator, while the declaration itself owns the line.
    let tags = format!(
        "{}\n{}",
        tree_sitter_c_sharp::TAGS_QUERY,
        r"
(field_declaration (variable_declaration (variable_declarator name: (identifier) @name))) @definition.field
(property_declaration name: (identifier) @name) @definition.property
(enum_member_declaration name: (identifier) @name) @definition.variant
(constructor_declaration name: (identifier) @name) @definition.constructor
(enum_declaration name: (identifier) @name) @definition.enum
(struct_declaration name: (identifier) @name) @definition.struct
(record_declaration name: (identifier) @name) @definition.struct
"
    );
    SyntaxFrontend::new(
        SourceLanguage::CSharp,
        b"tree-sitter-c-sharp-0.23.5/tags-v2",
        vec![GrammarVariant::new(
            &["cs"],
            tree_sitter_c_sharp::LANGUAGE.into(),
            &tags,
        )?],
    )
}
