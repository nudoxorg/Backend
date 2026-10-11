//! Tests for the ambient-module "export context" rule.
//!
//! An ambient module/namespace implicitly exports its non-`export`-marked
//! members only while its body contains no `ExportDeclaration`/`ExportAssignment`
//! statement (tsc's `setExportContextFlag`/`hasExportDeclarations`). Once such a
//! statement appears (`export { ... }`, `export * ...`, `export = ...`, or
//! `export default <expression>`), the body switches to explicit-export mode and
//! only the explicitly exported bindings (plus the synthesized `default`) are
//! visible.

use std::sync::Arc;
use tsz_binder::BinderState;
use tsz_parser::parser::ParserState;

fn bind_source(source: &str) -> BinderState {
    let mut parser = ParserState::new("test.d.ts".to_string(), source.to_string());
    let root = parser.parse_source_file();
    let arena = Arc::new(parser.get_arena().clone());
    let mut binder = BinderState::new();
    binder.bind_source_file(&arena, root);
    binder
}

fn exported_names(binder: &BinderState, module: &str) -> Vec<String> {
    let mut names: Vec<String> = binder
        .module_exports
        .get(module)
        .map(|table| table.iter().map(|(name, _)| name.clone()).collect())
        .unwrap_or_default();
    names.sort();
    names
}

#[test]
fn ambient_module_without_export_declarations_auto_exports_members() {
    // No export declaration anywhere: implicit export context stays on, so every
    // member is visible even without an `export` modifier.
    let binder = bind_source(
        r#"declare module "m" {
            const a: number;
            function b(): void;
        }"#,
    );
    let names = exported_names(&binder, "m");
    assert!(
        names.contains(&"a".to_string()),
        "expected `a`, got {names:?}"
    );
    assert!(
        names.contains(&"b".to_string()),
        "expected `b`, got {names:?}"
    );
}

#[test]
fn named_export_declaration_disables_implicit_export_of_siblings() {
    // `export { a }` switches the body to explicit-export mode: `a` is exported,
    // the unmarked `b` is module-local only.
    let binder = bind_source(
        r#"declare module "m" {
            export { a };
            const a: number;
            const b: number;
        }"#,
    );
    let names = exported_names(&binder, "m");
    assert!(
        names.contains(&"a".to_string()),
        "expected `a`, got {names:?}"
    );
    assert!(
        !names.contains(&"b".to_string()),
        "`b` is not exported, got {names:?}"
    );
}

#[test]
fn named_export_declaration_rule_is_not_name_specific() {
    // Same rule with completely different identifier spellings — proves the
    // behavior keys on structure, not on a hardcoded name.
    let binder = bind_source(
        r#"declare module "pkg" {
            export { keep };
            const keep: string;
            const hidden: string;
        }"#,
    );
    let names = exported_names(&binder, "pkg");
    assert!(
        names.contains(&"keep".to_string()),
        "expected `keep`, got {names:?}"
    );
    assert!(
        !names.contains(&"hidden".to_string()),
        "`hidden` is not exported, got {names:?}"
    );
}

#[test]
fn export_equals_disables_implicit_export_of_siblings() {
    // `export = x` is an export assignment that disables the implicit export
    // context, so the unmarked sibling `y` is not exported.
    let binder = bind_source(
        r#"declare module "m" {
            const x: { foo: number };
            const y: number;
            export = x;
        }"#,
    );
    let names = exported_names(&binder, "m");
    assert!(
        !names.contains(&"y".to_string()),
        "`y` is not exported, got {names:?}"
    );
}

#[test]
fn export_default_does_not_disable_implicit_export_context() {
    // `export default <…>` is intentionally NOT treated as disabling (see the
    // helper docs: the synthesized-`default` cross-file path is not yet modeled,
    // so we fall back to implicit export). The sibling `y` therefore stays
    // implicitly exported rather than being hidden.
    let binder = bind_source(
        r#"declare module "m" {
            const x: { foo: number };
            const y: number;
            export default x;
        }"#,
    );
    let names = exported_names(&binder, "m");
    assert!(
        names.contains(&"y".to_string()),
        "`y` stays implicitly exported under the default-export fallback, got {names:?}"
    );
}

#[test]
fn export_modifier_on_declaration_keeps_implicit_export_context() {
    // `export function`/`export const`/etc. are declarations with an export
    // modifier, NOT bare export declarations, so they must not flip the body to
    // explicit-export mode. The unmarked sibling `Promise` stays implicitly
    // exported alongside the explicitly exported `defer`. (Mirrors the regression
    // in tsc's `funduleUsedAcrossFileBoundary`, where the parser models
    // `export function` as an `EXPORT_DECLARATION` wrapping the function.)
    let binder = bind_source(
        r#"declare module "q" {
            interface Promise<T> { foo: string; }
            export function defer<T>(): string;
        }"#,
    );
    let names = exported_names(&binder, "q");
    assert!(
        names.contains(&"Promise".to_string()),
        "`Promise` stays implicitly exported, got {names:?}"
    );
    assert!(
        names.contains(&"defer".to_string()),
        "`defer` is explicitly exported, got {names:?}"
    );
}

#[test]
fn export_default_declaration_keeps_implicit_export_context() {
    // `export default function f() {}` is a default-exported *declaration*, not
    // an export assignment, so it does NOT disable the implicit export context:
    // the sibling `g` stays implicitly exported.
    let binder = bind_source(
        r#"declare module "m" {
            export default function f(): void;
            const g: number;
        }"#,
    );
    let names = exported_names(&binder, "m");
    assert!(
        names.contains(&"g".to_string()),
        "`g` stays implicitly exported, got {names:?}"
    );
}

#[test]
fn empty_ambient_surface_retains_its_declared_wildcard_owner() {
    let binder = bind_source(
        r#"declare module "foreign" { export type * from "./wrong"; }
           declare module "pkg" { export type * from "./right"; }"#,
    );
    for (owner, target) in [("foreign", "./wrong"), ("pkg", "./right")] {
        assert!(binder.declared_modules.contains(owner));
        let surface = binder.module_exports.get(owner).unwrap();
        assert!(surface.is_empty(), "a wildcard edge is not a direct member");
        let edges = binder.wildcard_reexports.get(owner).unwrap();
        assert_eq!(edges.iter().count(), 1);
        assert!(edges.iter().any(|(specifier, type_only)| {
            specifier == target && *type_only
        }));
    }
    assert!(!binder.module_exports.contains_key("src/pkg.ts"));
}

#[test]
fn empty_ambient_body_after_a_public_declaration_preserves_merged_members() {
    let binder = bind_source(
        r#"declare module "pkg" { export const value: number; }
           declare module "pkg" { export type * from "./right"; }"#,
    );
    let names = exported_names(&binder, "pkg");
    assert_eq!(names, vec!["value"]);
    assert!(binder.wildcard_reexports.get("pkg").is_some());
}

#[test]
fn empty_ambient_surface_does_not_replace_bodyless_any_semantics() {
    let binder = bind_source(r#"declare module "body" {} declare module "shorthand";"#);
    assert!(binder.module_exports.get("body").unwrap().is_empty());
    assert!(!binder.shorthand_ambient_modules.contains("body"));
    assert!(binder.shorthand_ambient_modules.contains("shorthand"));
    assert!(!binder.module_exports.contains_key("shorthand"));
}

#[test]
fn empty_ambient_surface_does_not_publish_an_augmentation_as_an_owner() {
    let binder = bind_source(
        r#"import { value } from "./base";
           declare module "pkg" {}"#,
    );
    assert!(binder.is_external_module);
    assert!(!binder.declared_modules.contains("pkg"));
    assert!(!binder.module_exports.contains_key("pkg"));
}

#[test]
fn augmentation_reexports_preserve_the_literal_owner_without_declaring_an_ambient_module() {
    for clause in ["export type { Shape }", "export type *"] {
        let binder = bind_source(&format!(
            "{clause} from 'dep'; declare module 'test.d.ts' {{ {clause} from 'other'; }}"
        ));
        assert!(binder.is_external_module);
        assert!(
            binder
                .module_augmentations
                .get("test.d.ts")
                .is_some_and(Vec::is_empty),
            "the augmentation body must retain its owner without inventing a named declaration"
        );
        assert!(!binder.declared_modules.contains("test.d.ts"));
        assert!(!binder.shorthand_ambient_modules.contains("test.d.ts"));
    }
}

#[test]
fn augmentation_owner_entries_preserve_members_and_exclude_bodyless_and_identifier_declarations() {
    let binder = bind_source(
        "export {}; declare module './target' { interface Existing { value: number } } \
         declare module './target' {} declare module './empty' {} \
         declare module '*.json'; namespace Identifier {}",
    );
    let entries = binder.module_augmentations.get("./target").unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "Existing");
    assert!(
        binder
            .module_augmentations
            .get("./empty")
            .is_some_and(Vec::is_empty)
    );
    assert!(!binder.declared_modules.contains("./empty"));
    assert!(!binder.module_exports.contains_key("./empty"));
    assert!(binder.shorthand_ambient_modules.contains("*.json"));
    assert!(!binder.module_augmentations.contains_key("*.json"));
    assert!(!binder.module_augmentations.contains_key("Identifier"));
}
