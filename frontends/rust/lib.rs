//! Rust frontend surface: the zero-toolchain syntax frontend plus the legacy
//! Cargo/rust-analyzer authority lane the engine drives.
#![forbid(unsafe_code)]

use backend_compile::{GrammarVariant, SourceLanguage, SyntaxError, SyntaxFrontend};

#[path = "src/legacy/mod.rs"]
pub mod legacy;

/// Builds the zero-toolchain local Rust syntax frontend.
///
/// This structural baseline never claims native semantic authority.
///
/// # Errors
/// Returns an error when the embedded grammar query cannot be admitted.
pub fn syntax_frontend() -> Result<SyntaxFrontend, SyntaxError> {
    // The stock tags query selects only items a reference can point at, so a
    // struct arrived with no fields and an enum with no variants. These
    // patterns complete the declaration floor a product outline renders.
    let tags = format!(
        "{}\n{}",
        tree_sitter_rust::TAGS_QUERY,
        r"
(field_declaration name: (field_identifier) @name) @definition.field
(enum_variant name: (identifier) @name) @definition.variant
(const_item name: (identifier) @name) @definition.constant
(static_item name: (identifier) @name) @definition.variable
(function_signature_item name: (identifier) @name) @definition.method
(associated_type name: (type_identifier) @name) @definition.type
"
    );
    SyntaxFrontend::new(
        SourceLanguage::Rust,
        b"tree-sitter-rust-0.24.2/tags-v2",
        vec![GrammarVariant::new(
            &["rs"],
            tree_sitter_rust::LANGUAGE.into(),
            &tags,
        )?],
    )
}
