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
(type_alias name: (type_identifier) @name) @definition.type
(source_file (const_declaration (const_spec name: (identifier) @name) @definition.constant))
(source_file (var_declaration (var_spec name: (identifier) @name) @definition.variable))
(source_file (var_declaration (var_spec_list (var_spec name: (identifier) @name) @definition.variable)))
"
    );
    SyntaxFrontend::new(
        SourceLanguage::Go,
        b"tree-sitter-go-0.25.0/tags-exact-declaration-extents-v4",
        vec![GrammarVariant::new(
            &["go"],
            tree_sitter_go::LANGUAGE.into(),
            &tags,
        )?],
    )
}

#[cfg(test)]
mod exact_extent_tests {
    use backend_compile::SourceBindingIdentity;

    #[test]
    fn syntax_extents_follow_go_authority_specs_bindings_and_utf8_bytes() -> Result<(), String> {
        let source = "package demo\n// λ shifts every following byte offset\ntype One struct{}\ntype (Two int; Three string)\ntype Alias = One\nvar Solo = 1\nvar (Grouped = 2; Other = 3)\nvar Left, Right = 4, 5\nconst A, B = 6, 7\nconst (Only = 8)\nfunc Café() {}\nfunc (One) Shared() {}\nfunc (Two) Shared() {}\n";
        let analysis = super::syntax_frontend().map_err(|error| error.to_string())?
            .analyze(std::path::Path::new("main.go"), source.as_bytes())
            .map_err(|error| error.to_string())?;
        for (name, extent) in [
            ("One", "type One struct{}"), ("Two", "Two int"), ("Three", "Three string"),
            ("Alias", "type Alias = One"), ("Solo", "var Solo = 1"),
            ("Grouped", "Grouped = 2"), ("Other", "Other = 3"),
            ("Left", "Left"), ("Right", "Right"), ("A", "A"), ("B", "B"),
            ("Only", "const (Only = 8)"), ("Café", "func Café() {}"),
        ] {
            let declaration = analysis.declarations().iter().find(|site| site.name() == name)
                .ok_or_else(|| format!("missing {name}"))?;
            let SourceBindingIdentity::Exact(capture) = declaration.binding_identity() else {
                return Err(format!("{name} lost exact capture"));
            };
            let (start, end) = capture.owner();
            assert_eq!(&source.as_bytes()[start as usize..end as usize], extent.as_bytes(), "{name}");
            let (start, end) = capture.name();
            assert_eq!(&source.as_bytes()[start as usize..end as usize], name.as_bytes(), "name token {name}");
        }
        let shared = analysis.declarations().iter().filter(|site| site.name() == "Shared")
            .map(|site| site.binding_identity()).collect::<Vec<_>>();
        assert_eq!(shared.len(), 2, "two receiver methods remain two declarations");
        assert_ne!(shared[0], shared[1]);
        Ok(())
    }
}
