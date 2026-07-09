//! Pipeline part: **TypeScript source → surface IR** (`compiler::languages::typescript`).
//!
//! TDD specs (`todo!()` bodies) for the deno-doc lowering: entry-point /
//! declaration-root discovery and the TS-specific lowerings the old
//! `producers::parse::typescript` path produced.

/// A regular module lowers exported items and drops private ones.
///
/// Arrange: `greeter.ts` (exported `DEFAULT_GREETING`, `Greeter`, `greet`;
///   private `SECRET_GREETING`, `WhisperGreeter`).
/// Assert: exported items appear; `SECRET_GREETING`/`WhisperGreeter` are
///   `Visibility::Private`.
#[ignore = "TDD stub — not yet implemented"]
#[test]
fn regular_module_lowers_exports() {
    todo!("lower greeter.ts and assert exported vs private items");
}

/// Namespaces, classes (with private `#fields`), and default exports lower.
///
/// Arrange: `toolkit.ts` (`namespace toolkit`, `Builder` with `#segments`,
///   default-export `install`).
/// Assert: the namespace becomes a `Module`, `Builder` a `RecordType`, and the
///   default export is reachable.
#[ignore = "TDD stub — not yet implemented"]
#[test]
fn namespaces_classes_and_default_exports_lower() {
    todo!("lower toolkit.ts and assert namespace/class/default-export entries");
}

/// An interface becomes a `TraitDef`, with call/index signatures as members.
///
/// Assert: `Greeter` lowers to `Entry::TraitDef`; a call signature lowers to a
///   `TraitMethod` named `__call`, an index signature to `__index`.
#[ignore = "TDD stub — not yet implemented"]
#[test]
fn interface_becomes_trait_def_with_signatures() {
    todo!("assert interface -> TraitDef with __call/__index members");
}

/// Declaration roots are discovered from `package.json` (types/typings/exports).
///
/// Assert: given a package whose `package.json` sets `types: "mod.ts"`, the
///   entry point resolves to `mod.ts` and its exports are lowered.
#[ignore = "TDD stub — not yet implemented"]
#[test]
fn declaration_roots_resolved_from_package_json() {
    todo!("assert package.json `types` drives the entry point");
}

/// TS structural types (keyof / mapped / conditional) are first-class IR types.
///
/// Assert: a `keyof` lowers to `Type::TypeOperator`, a mapped type to
///   `Type::Mapped`, a conditional to `Type::Conditional`.
#[ignore = "TDD stub — not yet implemented"]
#[test]
fn structural_types_are_first_class() {
    todo!("assert keyof/mapped/conditional lower to their IR Type variants");
}
