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
    // Java annotation declarations and their elements have separate grammar
    // nodes; retain them as interfaces/methods so attributed semantic rows can
    // bind to their exact authored source sites too.
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
(annotation_type_element_declaration name: (identifier) @name) @definition.method
"
    );
    SyntaxFrontend::new(
        SourceLanguage::Java,
        b"tree-sitter-java-0.23.5/tags-v4-annotation-sites",
        vec![GrammarVariant::new(
            &["java"],
            tree_sitter_java::LANGUAGE.into(),
            &tags,
        )?],
    )
}

#[cfg(test)]
mod annotation_source_tests {
    use backend_compile::DeclarationKind;
    use std::path::Path;

    #[test]
    fn annotation_types_and_elements_retain_authored_source_sites() -> Result<(), String> {
        let source = "// λ precedes the annotation source\npackage demo;\npublic @interface Nonnull {\n    String value() default \"yes\";\n    class Checker {}\n}\n";
        let analysis = super::syntax_frontend()
            .map_err(|error| error.to_string())?
            .analyze(Path::new("demo/Nonnull.java"), source.as_bytes())
            .map_err(|error| error.to_string())?;
        for (name, kind, line) in [
            ("Nonnull", DeclarationKind::Interface, 3),
            ("value", DeclarationKind::Method, 4),
            ("Checker", DeclarationKind::Class, 5),
        ] {
            let declaration = analysis
                .declarations()
                .iter()
                .find(|declaration| declaration.name() == name && declaration.kind() == kind)
                .ok_or_else(|| format!("missing authored Java declaration {name}"))?;
            assert_eq!(declaration.kind(), kind);
            assert_eq!(declaration.line(), line);
            assert_eq!(declaration.location().path(), "demo/Nonnull.java");
            assert!(declaration.signature().contains(name));
            assert!(
                declaration
                    .source_excerpt()
                    .text()
                    .is_some_and(|text| text.contains(name))
            );
        }
        Ok(())
    }
}
