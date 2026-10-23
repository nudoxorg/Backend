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
    // declarator, while the declaration itself owns the line. Records default
    // to classes; the shared node-shape refinement recognizes record structs.
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
(record_declaration name: (identifier) @name) @definition.class
"
    );
    SyntaxFrontend::new(
        SourceLanguage::CSharp,
        b"tree-sitter-c-sharp-0.23.5/tags-v4-record-kinds",
        vec![GrammarVariant::new(
            &["cs"],
            tree_sitter_c_sharp::LANGUAGE.into(),
            &tags,
        )?],
    )
}

#[cfg(test)]
mod record_kind_tests {
    use backend_compile::DeclarationKind;
    use std::path::Path;

    #[test]
    fn record_classes_and_record_structs_retain_their_authored_kinds() -> Result<(), String> {
        let source = r#"namespace Demo {
public record Plain(int Value);
public record class Explicit(int Value);
public record struct Point(int X, int Y);
public readonly record struct ReadonlyPoint(int X);
[System.Obsolete("record struct")]
public record Decorated(int Value) { public struct Nested {} }
public record /* struct */ Commented(int Value);
public record @struct(int Value);
public class OrdinaryClass {}
public struct OrdinaryStruct {}
}
"#;
        let analysis = super::syntax_frontend()
            .map_err(|error| error.to_string())?
            .analyze(Path::new("Records.cs"), source.as_bytes())
            .map_err(|error| error.to_string())?;
        for (name, kind, line) in [
            ("Plain", DeclarationKind::Class, 2),
            ("Explicit", DeclarationKind::Class, 3),
            ("Point", DeclarationKind::Struct, 4),
            ("ReadonlyPoint", DeclarationKind::Struct, 5),
            ("Decorated", DeclarationKind::Class, 6),
            ("Nested", DeclarationKind::Struct, 7),
            ("Commented", DeclarationKind::Class, 8),
            ("@struct", DeclarationKind::Class, 9),
            ("OrdinaryClass", DeclarationKind::Class, 10),
            ("OrdinaryStruct", DeclarationKind::Struct, 11),
        ] {
            let declarations = analysis
                .declarations()
                .iter()
                .filter(|declaration| declaration.name() == name)
                .collect::<Vec<_>>();
            assert_eq!(declarations.len(), 1, "one authored declaration for {name}");
            let declaration = declarations[0];
            assert_eq!(declaration.kind(), kind, "{name}");
            assert_eq!(declaration.line(), line, "{name}");
            assert_eq!(declaration.location().path(), "Records.cs");
            assert!(declaration.signature().contains(name), "{name}");
        }
        Ok(())
    }
}
