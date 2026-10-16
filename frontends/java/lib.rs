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
(annotation_type_declaration name: (identifier) @name) @definition.interface
"
    );
    SyntaxFrontend::new(
        SourceLanguage::Java,
        b"tree-sitter-java-0.23.5/tags-v3",
        vec![GrammarVariant::new(
            &["java"],
            tree_sitter_java::LANGUAGE.into(),
            &tags,
        )?],
    )
}

#[cfg(test)]
mod tests {
    use super::syntax_frontend;
    use backend_compile::DeclarationKind;
    use std::path::Path;

    #[test]
    fn annotation_interfaces_and_nested_classes_are_structural_declarations() {
        let source = br#"
package javax.annotation;
public @interface Nonnull {
    class Checker {}
}
"#;
        let analysis = syntax_frontend()
            .expect("Java syntax frontend")
            .analyze(Path::new("javax/annotation/Nonnull.java"), source)
            .expect("valid annotation source");
        let declarations = analysis.declarations();

        assert!(declarations.iter().any(|declaration| {
            declaration.name() == "Nonnull" && declaration.kind() == DeclarationKind::Interface
        }));
        assert!(declarations.iter().any(|declaration| {
            declaration.name() == "Checker" && declaration.kind() == DeclarationKind::Class
        }));
    }
}
