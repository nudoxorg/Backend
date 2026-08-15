//! Unit tests for the C# producer lowering — no oracle invocation.
//!
//! We embed a literal JSON fixture that covers:
//!   - a namespace with a class (with fields, a generic method, XML doc)
//!   - an interface
//!   - an enum with members
//!   - a generic method with a type constraint
//!   - a property
//!   - an XML doc comment with `<summary>` and `<see cref=…>`

use nudox_languages::csharp::{lower, parse_extraction};

const FIXTURE: &str = r#"{
  "format": 1,
  "dotnetVersion": "10.0",
  "roslyn": "5.6.0",
  "mode": "source",
  "assembly": { "name": "MyLib", "version": "1.0.0", "tfm": "net10.0" },
  "diagnostics": { "errorTypeCount": 0, "errorCount": 0 },
  "namespaces": [
    { "name": "MyLib", "doc": null }
  ],
  "types": [
    {
      "docId": "T:MyLib.IAnimal",
      "qualifiedName": "MyLib.IAnimal",
      "simpleName": "IAnimal",
      "kind": "INTERFACE",
      "namespace": "MyLib",
      "enclosing": null,
      "modifiers": ["public"],
      "typeParams": [],
      "baseType": null,
      "interfaces": [],
      "enumUnderlying": null,
      "delegateSig": null,
      "attributes": [],
      "deprecated": null,
      "hidden": false,
      "forwarded": false,
      "doc": "<summary>An animal interface with <see cref=\"M:MyLib.IAnimal.Speak\"/> method.</summary>",
      "docInherited": false,
      "docLinks": { "M:MyLib.IAnimal.Speak": "M:MyLib.IAnimal.Speak" },
      "extensionReceiver": null,
      "members": {
        "fields": [],
        "properties": [
          {
            "name": "Name",
            "docId": "P:MyLib.IAnimal.Name",
            "type": { "kind": "named", "name": "System.String", "args": [], "owner": null, "nullable": "notAnnotated", "typeKind": "Class" },
            "accessibility": "public",
            "getAccessibility": null,
            "setAccessibility": null,
            "setKind": "none",
            "isRequired": false,
            "isStatic": false,
            "isIndexer": false,
            "parameters": [],
            "returnsByRef": false,
            "returnsByRefReadonly": false,
            "attributes": [],
            "deprecated": null,
            "hidden": false,
            "doc": "<summary>The animal's name.</summary>",
            "docInherited": false,
            "docLinks": null
          }
        ],
        "events": [],
        "constructors": [],
        "methods": [
          {
            "name": "Speak",
            "docId": "M:MyLib.IAnimal.Speak",
            "methodKind": "Ordinary",
            "accessibility": "public",
            "isStatic": false,
            "isAbstract": true,
            "isVirtual": false,
            "isOverride": false,
            "isSealed": false,
            "isExtern": false,
            "isAsync": false,
            "isIterator": false,
            "isExtensionMethod": false,
            "isReadonly": false,
            "typeParams": [],
            "parameters": [],
            "returnType": { "kind": "named", "name": "System.Void", "args": [], "owner": null, "nullable": "none", "typeKind": "Void" },
            "returnsByRef": false,
            "returnsByRefReadonly": false,
            "explicitInterface": null,
            "operatorKind": null,
            "attributes": [],
            "deprecated": null,
            "hidden": false,
            "doc": null,
            "docInherited": false,
            "docLinks": null
          }
        ],
        "operators": [],
        "conversions": [],
        "indexers": [],
        "nested": []
      }
    },
    {
      "docId": "T:MyLib.Dog",
      "qualifiedName": "MyLib.Dog",
      "simpleName": "Dog",
      "kind": "CLASS",
      "namespace": "MyLib",
      "enclosing": null,
      "modifiers": ["public"],
      "typeParams": [],
      "baseType": { "kind": "named", "name": "System.Object", "args": [], "owner": null, "nullable": "none", "typeKind": "Class" },
      "interfaces": [
        { "kind": "named", "name": "MyLib.IAnimal", "args": [], "owner": null, "nullable": "none", "typeKind": "Interface" }
      ],
      "enumUnderlying": null,
      "delegateSig": null,
      "attributes": [],
      "deprecated": null,
      "hidden": false,
      "forwarded": false,
      "doc": "<summary>A dog that implements <see cref=\"T:MyLib.IAnimal\"/>.</summary><seealso cref=\"T:MyLib.IAnimal\"/>",
      "docInherited": false,
      "docLinks": { "T:MyLib.IAnimal": "T:MyLib.IAnimal" },
      "extensionReceiver": null,
      "members": {
        "fields": [
          {
            "name": "_name",
            "docId": "F:MyLib.Dog._name",
            "type": { "kind": "named", "name": "System.String", "args": [], "owner": null, "nullable": "notAnnotated", "typeKind": "Class" },
            "accessibility": "private",
            "isConst": false,
            "constant": null,
            "isReadonly": true,
            "isVolatile": false,
            "isRequired": false,
            "isStatic": false,
            "attributes": [],
            "deprecated": null,
            "hidden": false,
            "doc": null,
            "docInherited": false,
            "docLinks": null
          }
        ],
        "properties": [
          {
            "name": "Name",
            "docId": "P:MyLib.Dog.Name",
            "type": { "kind": "named", "name": "System.String", "args": [], "owner": null, "nullable": "notAnnotated", "typeKind": "Class" },
            "accessibility": "public",
            "getAccessibility": null,
            "setAccessibility": null,
            "setKind": "none",
            "isRequired": false,
            "isStatic": false,
            "isIndexer": false,
            "parameters": [],
            "returnsByRef": false,
            "returnsByRefReadonly": false,
            "attributes": [],
            "deprecated": null,
            "hidden": false,
            "doc": "<summary>The dog's name.</summary>",
            "docInherited": false,
            "docLinks": null
          }
        ],
        "events": [],
        "constructors": [
          {
            "name": ".ctor",
            "docId": "M:MyLib.Dog.#ctor(System.String)",
            "methodKind": "Constructor",
            "accessibility": "public",
            "isStatic": false,
            "isAbstract": false,
            "isVirtual": false,
            "isOverride": false,
            "isSealed": false,
            "isExtern": false,
            "isAsync": false,
            "isIterator": false,
            "isExtensionMethod": false,
            "isReadonly": false,
            "typeParams": [],
            "parameters": [
              {
                "name": "name",
                "type": { "kind": "named", "name": "System.String", "args": [], "owner": null, "nullable": "notAnnotated", "typeKind": "Class" },
                "refKind": "none",
                "isParams": false,
                "hasDefault": false,
                "default": null,
                "scoped": false,
                "attributes": []
              }
            ],
            "returnType": null,
            "returnsByRef": false,
            "returnsByRefReadonly": false,
            "explicitInterface": null,
            "operatorKind": null,
            "attributes": [],
            "deprecated": null,
            "hidden": false,
            "doc": "<summary>Creates a <see cref=\"T:MyLib.Dog\"/>.</summary><param name=\"name\">The dog's name.</param>",
            "docInherited": false,
            "docLinks": null
          }
        ],
        "methods": [
          {
            "name": "Speak",
            "docId": "M:MyLib.Dog.Speak",
            "methodKind": "Ordinary",
            "accessibility": "public",
            "isStatic": false,
            "isAbstract": false,
            "isVirtual": false,
            "isOverride": true,
            "isSealed": false,
            "isExtern": false,
            "isAsync": false,
            "isIterator": false,
            "isExtensionMethod": false,
            "isReadonly": false,
            "typeParams": [],
            "parameters": [],
            "returnType": { "kind": "named", "name": "System.Void", "args": [], "owner": null, "nullable": "none", "typeKind": "Void" },
            "returnsByRef": false,
            "returnsByRefReadonly": false,
            "explicitInterface": null,
            "operatorKind": null,
            "attributes": [],
            "deprecated": null,
            "hidden": false,
            "doc": null,
            "docInherited": false,
            "docLinks": null
          },
          {
            "name": "Find",
            "docId": "M:MyLib.Dog.Find``1(``0)",
            "methodKind": "Ordinary",
            "accessibility": "public",
            "isStatic": false,
            "isAbstract": false,
            "isVirtual": false,
            "isOverride": false,
            "isSealed": false,
            "isExtern": false,
            "isAsync": false,
            "isIterator": false,
            "isExtensionMethod": false,
            "isReadonly": false,
            "typeParams": [
              {
                "name": "T",
                "variance": "none",
                "constraints": {
                  "referenceType": true,
                  "valueType": false,
                  "notNull": false,
                  "unmanaged": false,
                  "constructor": false,
                  "allowsRefLike": false,
                  "types": [
                    { "kind": "named", "name": "MyLib.IAnimal", "args": [], "owner": null, "nullable": "none", "typeKind": "Interface" }
                  ]
                }
              }
            ],
            "parameters": [
              {
                "name": "criteria",
                "type": { "kind": "typeParam", "name": "T", "ownerKind": "method", "nullable": "none" },
                "refKind": "none",
                "isParams": false,
                "hasDefault": false,
                "default": null,
                "scoped": false,
                "attributes": []
              }
            ],
            "returnType": { "kind": "typeParam", "name": "T", "ownerKind": "method", "nullable": "none" },
            "returnsByRef": false,
            "returnsByRefReadonly": false,
            "explicitInterface": null,
            "operatorKind": null,
            "attributes": [],
            "deprecated": null,
            "hidden": false,
            "doc": null,
            "docInherited": false,
            "docLinks": null
          }
        ],
        "operators": [],
        "conversions": [],
        "indexers": [],
        "nested": []
      }
    },
    {
      "docId": "T:MyLib.AnimalKind",
      "qualifiedName": "MyLib.AnimalKind",
      "simpleName": "AnimalKind",
      "kind": "ENUM",
      "namespace": "MyLib",
      "enclosing": null,
      "modifiers": ["public"],
      "typeParams": [],
      "baseType": null,
      "interfaces": [],
      "enumUnderlying": { "kind": "named", "name": "System.Int32", "args": [], "owner": null, "nullable": "none", "typeKind": "Struct" },
      "delegateSig": null,
      "attributes": [],
      "deprecated": null,
      "hidden": false,
      "forwarded": false,
      "doc": "<summary>Kinds of animals.</summary>",
      "docInherited": false,
      "docLinks": null,
      "extensionReceiver": null,
      "members": {
        "fields": [
          {
            "name": "Dog",
            "docId": "F:MyLib.AnimalKind.Dog",
            "type": { "kind": "named", "name": "MyLib.AnimalKind", "args": [], "owner": null, "nullable": "none", "typeKind": "Enum" },
            "accessibility": "public",
            "isConst": true,
            "constant": "0",
            "isReadonly": false,
            "isVolatile": false,
            "isRequired": false,
            "isStatic": true,
            "attributes": [],
            "deprecated": null,
            "hidden": false,
            "doc": "<summary>A dog.</summary>",
            "docInherited": false,
            "docLinks": null
          },
          {
            "name": "Cat",
            "docId": "F:MyLib.AnimalKind.Cat",
            "type": { "kind": "named", "name": "MyLib.AnimalKind", "args": [], "owner": null, "nullable": "none", "typeKind": "Enum" },
            "accessibility": "public",
            "isConst": true,
            "constant": "1",
            "isReadonly": false,
            "isVolatile": false,
            "isRequired": false,
            "isStatic": true,
            "attributes": [],
            "deprecated": null,
            "hidden": false,
            "doc": "<summary>A cat.</summary>",
            "docInherited": false,
            "docLinks": null
          }
        ],
        "properties": [],
        "events": [],
        "constructors": [],
        "methods": [],
        "operators": [],
        "conversions": [],
        "indexers": [],
        "nested": []
      }
    }
  ]
}"#;

#[test]
fn parse_fixture_succeeds() {
    let extraction = parse_extraction(FIXTURE.as_bytes()).expect("fixture should parse");
    assert_eq!(extraction.format, 1);
    assert_eq!(extraction.assembly.name, "MyLib");
    assert_eq!(extraction.types.len(), 3);
}

#[test]
fn lower_fixture_succeeds() {
    let extraction = parse_extraction(FIXTURE.as_bytes()).expect("fixture should parse");
    let pkg = lower(&extraction).expect("lowering should succeed");

    // Count entries: root + 3 types + their members.
    let entries: Vec<_> = pkg.iter().collect();
    assert!(!entries.is_empty(), "package must have entries");

    // Check all three types are present by name.
    let names: Vec<&str> = entries.iter().map(|(_, e)| e.sym().name.as_str()).collect();

    assert!(names.contains(&"IAnimal"), "IAnimal must be present");
    assert!(names.contains(&"Dog"), "Dog must be present");
    assert!(names.contains(&"AnimalKind"), "AnimalKind must be present");
}

#[test]
fn interface_entry_is_present_and_named() {
    let extraction = parse_extraction(FIXTURE.as_bytes()).expect("fixture should parse");
    let pkg = lower(&extraction).expect("lowering should succeed");

    let ianimal = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "IAnimal")
        .expect("IAnimal entry not found");

    // Verify it's a Trait kind.
    use nudox_ir::kind::Kind;
    assert!(
        matches!(
            ianimal.1.kind(),
            nudox_ir::entry::EntryInner::Owned(Kind::Trait(_))
        ),
        "IAnimal must be a Trait kind"
    );
}

#[test]
fn enum_entry_has_variants() {
    let extraction = parse_extraction(FIXTURE.as_bytes()).expect("fixture should parse");
    let pkg = lower(&extraction).expect("lowering should succeed");

    let animal_kind = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "AnimalKind")
        .expect("AnimalKind entry not found");

    use nudox_ir::kind::Kind;
    assert!(
        matches!(
            animal_kind.1.kind(),
            nudox_ir::entry::EntryInner::Owned(Kind::Enum(_))
        ),
        "AnimalKind must be an Enum kind"
    );

    // Verify variant children exist.
    let variants = pkg
        .iter()
        .filter(|(_, e)| {
            matches!(
                e.kind(),
                nudox_ir::entry::EntryInner::Owned(Kind::Variant(_))
            )
        })
        .count();
    assert_eq!(
        variants, 2,
        "AnimalKind must have 2 variants (Dog, Cat)"
    );
}

#[test]
fn dog_has_fields_and_methods() {
    let extraction = parse_extraction(FIXTURE.as_bytes()).expect("fixture should parse");
    let pkg = lower(&extraction).expect("lowering should succeed");

    // Dog should have at least: _name (Field), Name (Field/property), Speak
    // (Function).
    let field_names: Vec<&str> = pkg
        .iter()
        .filter(|(_, e)| {
            matches!(
                e.kind(),
                nudox_ir::entry::EntryInner::Owned(nudox_ir::kind::Kind::Field(_))
            )
        })
        .map(|(_, e)| e.sym().name.as_str())
        .collect();

    assert!(
        field_names.contains(&"_name"),
        "_name field must be present"
    );
    assert!(
        field_names.contains(&"Name"),
        "Name property must be present"
    );
}

#[test]
fn generic_method_with_constraint_is_present() {
    let extraction = parse_extraction(FIXTURE.as_bytes()).expect("fixture should parse");
    let pkg = lower(&extraction).expect("lowering should succeed");

    let find = pkg.iter().find(|(_, e)| e.sym().name == "Find");
    assert!(find.is_some(), "Find generic method must be present");

    let (_, entry) = find.unwrap();
    use nudox_ir::kind::Kind;
    assert!(
        matches!(
            entry.kind(),
            nudox_ir::entry::EntryInner::Owned(Kind::Function(_))
        ),
        "Find must be a Function kind"
    );

    // The Function should have generics (at least 1 GenericParam for T).
    if let nudox_ir::entry::EntryInner::Owned(Kind::Function(f)) = entry.kind() {
        assert!(
            !f.generics.is_empty(),
            "Find must have at least one generic param"
        );
        assert!(
            !f.wheres.is_empty(),
            "Find must have at least one where predicate for `class` constraint"
        );
    }
}

#[test]
fn interface_doc_link_is_present() {
    let extraction = parse_extraction(FIXTURE.as_bytes()).expect("fixture should parse");
    let pkg = lower(&extraction).expect("lowering should succeed");

    let ianimal = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "IAnimal")
        .expect("IAnimal entry not found");

    // doc_links should contain the Speak method cref.
    assert!(
        !ianimal.1.sym().doc_links.is_empty(),
        "IAnimal must have at least one doc_link (Speak cref)"
    );
}

#[test]
fn dog_documentation_contains_summary() {
    let extraction = parse_extraction(FIXTURE.as_bytes()).expect("fixture should parse");
    let pkg = lower(&extraction).expect("lowering should succeed");

    let dog = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "Dog")
        .expect("Dog entry not found");

    assert!(
        dog.1.sym().documentation.contains("dog"),
        "Dog documentation must contain summary text"
    );
}

#[test]
fn unsupported_format_returns_error() {
    let bad_json = r#"{"format": 99, "assembly": {"name":"x"}, "diagnostics": {}, "types": [{"docId":"T:X","qualifiedName":"X","simpleName":"X","kind":"CLASS","namespace":"","modifiers":[],"typeParams":[],"interfaces":[],"attributes":[],"hidden":false,"forwarded":false,"docInherited":false,"members":{"fields":[],"properties":[],"events":[],"constructors":[],"methods":[],"operators":[],"conversions":[],"indexers":[],"nested":[]}}]}"#;
    let err = parse_extraction(bad_json.as_bytes()).expect_err("must fail");
    assert!(
        matches!(
            err,
            nudox_languages::csharp::Error::UnsupportedFormat { .. }
        ),
        "expected UnsupportedFormat error"
    );
}

#[test]
fn empty_types_returns_error() {
    let empty_json = r#"{"format": 1, "assembly": {"name":"x"}, "diagnostics": {}, "types": [], "namespaces": []}"#;
    let err = parse_extraction(empty_json.as_bytes()).expect_err("must fail for empty types");
    assert!(
        matches!(err, nudox_languages::csharp::Error::NoTypes),
        "expected NoTypes error"
    );
}

// ---------------------------------------------------------------------------
// New tests for richness regressions
// ---------------------------------------------------------------------------

/// A fixture with two classes where `Handler` has a property whose type is
/// `EventSource` — a named type declared in the same extraction.  After the
/// fix, that property's IR type must be `Type::Nominal(...)`, not `Type::Any`.
const NOMINAL_TYPE_FIXTURE: &str = r#"{
  "format": 1,
  "dotnetVersion": "10.0",
  "roslyn": "5.6.0",
  "mode": "source",
  "assembly": { "name": "NominalLib", "version": "1.0.0", "tfm": "net10.0" },
  "diagnostics": { "errorTypeCount": 0, "errorCount": 0 },
  "namespaces": [],
  "types": [
    {
      "docId": "T:NominalLib.EventSource",
      "qualifiedName": "NominalLib.EventSource",
      "simpleName": "EventSource",
      "kind": "CLASS",
      "namespace": "NominalLib",
      "enclosing": null,
      "modifiers": ["public"],
      "typeParams": [],
      "baseType": null,
      "interfaces": [],
      "enumUnderlying": null,
      "delegateSig": null,
      "attributes": [],
      "deprecated": null,
      "hidden": false,
      "forwarded": false,
      "doc": "<summary>An event source.</summary>",
      "docInherited": false,
      "docLinks": null,
      "extensionReceiver": null,
      "members": { "fields": [], "properties": [], "events": [], "constructors": [], "methods": [], "operators": [], "conversions": [], "indexers": [], "nested": [] }
    },
    {
      "docId": "T:NominalLib.Handler",
      "qualifiedName": "NominalLib.Handler",
      "simpleName": "Handler",
      "kind": "CLASS",
      "namespace": "NominalLib",
      "enclosing": null,
      "modifiers": ["public"],
      "typeParams": [],
      "baseType": null,
      "interfaces": [],
      "enumUnderlying": null,
      "delegateSig": null,
      "attributes": [],
      "deprecated": null,
      "hidden": false,
      "forwarded": false,
      "doc": null,
      "docInherited": false,
      "docLinks": null,
      "extensionReceiver": null,
      "members": {
        "fields": [],
        "properties": [
          {
            "name": "Source",
            "docId": "P:NominalLib.Handler.Source",
            "type": { "kind": "named", "name": "NominalLib.EventSource", "args": [], "owner": null, "nullable": "notAnnotated", "typeKind": "Class" },
            "accessibility": "public",
            "getAccessibility": null,
            "setAccessibility": null,
            "setKind": "set",
            "isRequired": false,
            "isStatic": false,
            "isIndexer": false,
            "parameters": [],
            "returnsByRef": false,
            "returnsByRefReadonly": false,
            "attributes": [],
            "deprecated": null,
            "hidden": false,
            "doc": null,
            "docInherited": false,
            "docLinks": null
          }
        ],
        "events": [],
        "constructors": [],
        "methods": [],
        "operators": [],
        "conversions": [],
        "indexers": [],
        "nested": []
      }
    }
  ]
}"#;

/// Property whose type is another class in the same extraction must resolve to
/// a nominal reference, not `Type::Any`.
///
/// The fixture uses `"nullable": "notAnnotated"` on the Source property type,
/// so the IR type is `Annotated { inner: Nominal(...), annotation: "notAnnotated" }`.
/// The test asserts two things:
///   1. The type is NOT `Type::Any` (the regression).
///   2. The inner type (stripping the nullability annotation) IS `Type::Nominal`.
#[test]
fn named_type_lowers_to_nominal_not_any() {
    use nudox_ir::{build::Type, entry::EntryInner, kind::Kind};

    let extraction =
        parse_extraction(NOMINAL_TYPE_FIXTURE.as_bytes()).expect("nominal fixture must parse");
    let pkg = lower(&extraction).expect("nominal fixture must lower");

    // Find the `Source` property (a Field entry).
    let source_entry = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "Source")
        .expect("Source property must be present");

    let ty = match source_entry.1.kind() {
        EntryInner::Owned(Kind::Field(f)) => f.ty.as_ref().expect("Source must have a type"),
        other => panic!("Source must be a Field, got {other:?}"),
    };

    // The nullable annotation ("notAnnotated") wraps the underlying Nominal in
    // Type::Annotated.  Peel the annotation layer to reach the base type.
    let base_ty = match ty {
        Type::Annotated { inner, annotation } => {
            assert_eq!(
                annotation.token, "notAnnotated",
                "annotation token must be 'notAnnotated'"
            );
            inner.as_ref()
        }
        // Oblivious nullability (no annotation) produces a bare type directly.
        other => other,
    };

    assert!(
        matches!(base_ty, Type::Nominal(_)),
        "Source.ty (after stripping nullability annotation) must be Type::Nominal, \
         not Type::Any (got {base_ty:?}); named types in the same extraction must \
         resolve to Nominal"
    );
}

/// A fixture with a generic method whose parameter uses a type parameter `T`.
/// After the fix, the parameter type must be `Type::TypeVar("T")`, not `Any`.
const TYPEVAR_FIXTURE: &str = r#"{
  "format": 1,
  "dotnetVersion": "10.0",
  "roslyn": "5.6.0",
  "mode": "source",
  "assembly": { "name": "GenLib", "version": "1.0.0", "tfm": "net10.0" },
  "diagnostics": { "errorTypeCount": 0, "errorCount": 0 },
  "namespaces": [],
  "types": [
    {
      "docId": "T:GenLib.Container",
      "qualifiedName": "GenLib.Container",
      "simpleName": "Container",
      "kind": "CLASS",
      "namespace": "GenLib",
      "enclosing": null,
      "modifiers": ["public"],
      "typeParams": [],
      "baseType": null,
      "interfaces": [],
      "enumUnderlying": null,
      "delegateSig": null,
      "attributes": [],
      "deprecated": null,
      "hidden": false,
      "forwarded": false,
      "doc": null,
      "docInherited": false,
      "docLinks": null,
      "extensionReceiver": null,
      "members": {
        "fields": [],
        "properties": [],
        "events": [],
        "constructors": [],
        "methods": [
          {
            "name": "Wrap",
            "docId": "M:GenLib.Container.Wrap``1(``0)",
            "methodKind": "Ordinary",
            "accessibility": "public",
            "isStatic": true,
            "isAbstract": false,
            "isVirtual": false,
            "isOverride": false,
            "isSealed": false,
            "isExtern": false,
            "isAsync": false,
            "isIterator": false,
            "isExtensionMethod": false,
            "isReadonly": false,
            "typeParams": [
              {
                "name": "T",
                "variance": "none",
                "constraints": {
                  "referenceType": false,
                  "valueType": false,
                  "notNull": false,
                  "unmanaged": false,
                  "constructor": false,
                  "allowsRefLike": false,
                  "types": []
                }
              }
            ],
            "parameters": [
              {
                "name": "value",
                "type": { "kind": "typeParam", "name": "T", "ownerKind": "method", "nullable": "none" },
                "refKind": "none",
                "isParams": false,
                "hasDefault": false,
                "default": null,
                "scoped": false,
                "attributes": []
              }
            ],
            "returnType": { "kind": "typeParam", "name": "T", "ownerKind": "method", "nullable": "none" },
            "returnsByRef": false,
            "returnsByRefReadonly": false,
            "explicitInterface": null,
            "operatorKind": null,
            "attributes": [],
            "deprecated": null,
            "hidden": false,
            "doc": null,
            "docInherited": false,
            "docLinks": null
          }
        ],
        "operators": [],
        "conversions": [],
        "indexers": [],
        "nested": []
      }
    }
  ]
}"#;

/// Generic method parameter typed as a type-var `T` must lower to
/// `Type::TypeVar("T")`, not `Type::Any`.
#[test]
fn type_param_use_lowers_to_typevar() {
    use nudox_ir::{build::Type, entry::EntryInner, kind::Kind};

    let extraction =
        parse_extraction(TYPEVAR_FIXTURE.as_bytes()).expect("typevar fixture must parse");
    let pkg = lower(&extraction).expect("typevar fixture must lower");

    // Find the `value` parameter of Wrap.
    let value_param = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "value")
        .expect("value parameter must be present");

    let ty = match value_param.1.kind() {
        EntryInner::Owned(Kind::Param(p)) => p.ty.as_ref().expect("value param must have a type"),
        other => panic!("value must be a Param, got {other:?}"),
    };

    assert!(
        matches!(ty, Type::TypeVar(n) if n == "T"),
        "value param type must be Type::TypeVar(\"T\"), got {ty:?}"
    );
}

/// A fixture with a method that has a `<exception cref>` doc tag and a
/// parameter with a default value.
const EXCEPTION_AND_DEFAULT_FIXTURE: &str = r#"{
  "format": 1,
  "dotnetVersion": "10.0",
  "roslyn": "5.6.0",
  "mode": "source",
  "assembly": { "name": "ExLib", "version": "1.0.0", "tfm": "net10.0" },
  "diagnostics": { "errorTypeCount": 0, "errorCount": 0 },
  "namespaces": [],
  "types": [
    {
      "docId": "T:ExLib.Parser",
      "qualifiedName": "ExLib.Parser",
      "simpleName": "Parser",
      "kind": "CLASS",
      "namespace": "ExLib",
      "enclosing": null,
      "modifiers": ["public"],
      "typeParams": [],
      "baseType": null,
      "interfaces": [],
      "enumUnderlying": null,
      "delegateSig": null,
      "attributes": [],
      "deprecated": null,
      "hidden": false,
      "forwarded": false,
      "doc": null,
      "docInherited": false,
      "docLinks": null,
      "extensionReceiver": null,
      "members": {
        "fields": [],
        "properties": [],
        "events": [
          {
            "name": "DataArrived",
            "docId": "E:ExLib.Parser.DataArrived",
            "type": { "kind": "named", "name": "System.EventHandler", "args": [], "owner": null, "nullable": "none", "typeKind": "Delegate" },
            "accessibility": "public",
            "addAccessibility": "internal",
            "removeAccessibility": null,
            "isStatic": false,
            "attributes": [],
            "deprecated": null,
            "hidden": false,
            "doc": null,
            "docInherited": false,
            "docLinks": null
          }
        ],
        "constructors": [],
        "methods": [
          {
            "name": "Parse",
            "docId": "M:ExLib.Parser.Parse(System.String,System.Int32)",
            "methodKind": "Ordinary",
            "accessibility": "public",
            "isStatic": false,
            "isAbstract": false,
            "isVirtual": false,
            "isOverride": false,
            "isSealed": false,
            "isExtern": false,
            "isAsync": false,
            "isIterator": false,
            "isExtensionMethod": false,
            "isReadonly": false,
            "typeParams": [],
            "parameters": [
              {
                "name": "input",
                "type": { "kind": "named", "name": "System.String", "args": [], "owner": null, "nullable": "notAnnotated", "typeKind": "Class" },
                "refKind": "none",
                "isParams": false,
                "hasDefault": false,
                "default": null,
                "scoped": false,
                "attributes": []
              },
              {
                "name": "maxLen",
                "type": { "kind": "named", "name": "System.Int32", "args": [], "owner": null, "nullable": "none", "typeKind": "Struct" },
                "refKind": "none",
                "isParams": false,
                "hasDefault": true,
                "default": "1024",
                "scoped": false,
                "attributes": []
              }
            ],
            "returnType": { "kind": "named", "name": "System.Boolean", "args": [], "owner": null, "nullable": "none", "typeKind": "Struct" },
            "returnsByRef": false,
            "returnsByRefReadonly": false,
            "explicitInterface": null,
            "operatorKind": null,
            "attributes": [],
            "deprecated": null,
            "hidden": false,
            "doc": "<summary>Parses the input.</summary><exception cref=\"T:System.ArgumentException\">on invalid input</exception>",
            "docInherited": false,
            "docLinks": null
          }
        ],
        "operators": [],
        "conversions": [],
        "indexers": [],
        "nested": []
      }
    }
  ]
}"#;

/// Exception `<exception cref>` tags must NOT appear in `output_params`.
/// They must appear as `doc_links` (with label "throws") on the method symbol
/// and as prose in the method documentation.
#[test]
fn exception_cref_is_not_output_param() {
    use nudox_ir::{entry::EntryInner, kind::Kind};

    let extraction = parse_extraction(EXCEPTION_AND_DEFAULT_FIXTURE.as_bytes())
        .expect("exception fixture must parse");
    let pkg = lower(&extraction).expect("exception fixture must lower");

    // Find the Parse function.
    let parse_entry = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "Parse")
        .expect("Parse method must be present");

    let fn_kind = match parse_entry.1.kind() {
        EntryInner::Owned(Kind::Function(f)) => f,
        other => panic!("Parse must be a Function, got {other:?}"),
    };

    // output_params must contain only the return value — not the exception.
    assert_eq!(
        fn_kind.output_params.len(),
        1,
        "Parse output_params must have exactly 1 entry (the return bool), \
         not the exception; exceptions must not be in output_params"
    );

    // The method's doc_links must contain a "throws" link for ArgumentException.
    let throws_link = parse_entry
        .1
        .sym()
        .doc_links
        .iter()
        .find(|dl| dl.label.as_deref() == Some("throws"));
    assert!(
        throws_link.is_some(),
        "Parse must have a 'throws' doc_link for ArgumentException"
    );

    // The exception description must appear in documentation prose.
    assert!(
        parse_entry
            .1
            .sym()
            .documentation
            .contains("ArgumentException"),
        "Parse documentation must mention ArgumentException"
    );
}

/// `Function::throws` must be populated from `<exception cref>` XML tags.
///
/// The fixture's `Parse` method documents one exception from a cross-package
/// type (`System.ArgumentException`).  Cross-package exception types fall back
/// to `Type::Any` (the same rule as other cross-package named types).
/// `output_params` must still contain only the return value.
#[test]
fn function_throws_is_wired() {
    use nudox_ir::{build::Type, entry::EntryInner, kind::Kind};

    let extraction = parse_extraction(EXCEPTION_AND_DEFAULT_FIXTURE.as_bytes())
        .expect("exception fixture must parse");
    let pkg = lower(&extraction).expect("exception fixture must lower");

    let parse_entry = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "Parse")
        .expect("Parse method must be present");

    let fn_kind = match parse_entry.1.kind() {
        EntryInner::Owned(Kind::Function(f)) => f,
        other => panic!("Parse must be a Function, got {other:?}"),
    };

    // throws must have exactly one entry (System.ArgumentException).
    assert_eq!(
        fn_kind.throws.len(),
        1,
        "Parse::throws must have 1 entry (System.ArgumentException), got {}",
        fn_kind.throws.len()
    );

    // `System.ArgumentException` is cross-package, so it is an *unresolved
    // external* that keeps its FQN — not `Type::Any`.
    //
    // Asserting on the name rather than on the variant is the whole point of
    // CC-2 here: `throws` exists to say *which* exception, and under
    // `Type::Any` a `<exception cref="ArgumentException">` and a
    // `<exception cref="IOException">` produced identical entries. A
    // `matches!(t, Type::Unknown(_))` assertion would still pass on that bug.
    assert_eq!(
        fn_kind.throws[0],
        Type::Unknown(nudox_ir::kinds::UnknownType::UnresolvedExternal {
            name: "System.ArgumentException".to_owned()
        }),
        "a cross-package exception type must name itself, got {:?}",
        fn_kind.throws[0]
    );
    assert_ne!(
        fn_kind.throws[0],
        Type::Any,
        "`object` is not what a missing exception reference means"
    );

    // output_params still has only the return value, not the exception.
    assert_eq!(
        fn_kind.output_params.len(),
        1,
        "output_params must have exactly 1 entry (the return bool), not the exception"
    );
}

/// When an exception type is declared within the same extraction, `throws`
/// must produce `Type::Nominal`, not `Type::Any`.
const INPACKAGE_THROWS_FIXTURE: &str = r#"{
  "format": 1,
  "dotnetVersion": "10.0",
  "roslyn": "5.6.0",
  "mode": "source",
  "assembly": { "name": "ThrowLib", "version": "1.0.0", "tfm": "net10.0" },
  "diagnostics": { "errorTypeCount": 0, "errorCount": 0 },
  "namespaces": [],
  "types": [
    {
      "docId": "T:ThrowLib.DomainException",
      "qualifiedName": "ThrowLib.DomainException",
      "simpleName": "DomainException",
      "kind": "CLASS",
      "namespace": "ThrowLib",
      "enclosing": null,
      "modifiers": ["public"],
      "typeParams": [],
      "baseType": null,
      "interfaces": [],
      "enumUnderlying": null,
      "delegateSig": null,
      "attributes": [],
      "deprecated": null,
      "hidden": false,
      "forwarded": false,
      "doc": null,
      "docInherited": false,
      "docLinks": null,
      "extensionReceiver": null,
      "members": { "fields": [], "properties": [], "events": [], "constructors": [], "methods": [], "operators": [], "conversions": [], "indexers": [], "nested": [] }
    },
    {
      "docId": "T:ThrowLib.Service",
      "qualifiedName": "ThrowLib.Service",
      "simpleName": "Service",
      "kind": "CLASS",
      "namespace": "ThrowLib",
      "enclosing": null,
      "modifiers": ["public"],
      "typeParams": [],
      "baseType": null,
      "interfaces": [],
      "enumUnderlying": null,
      "delegateSig": null,
      "attributes": [],
      "deprecated": null,
      "hidden": false,
      "forwarded": false,
      "doc": null,
      "docInherited": false,
      "docLinks": null,
      "extensionReceiver": null,
      "members": {
        "fields": [],
        "properties": [],
        "events": [],
        "constructors": [],
        "methods": [
          {
            "name": "Execute",
            "docId": "M:ThrowLib.Service.Execute",
            "methodKind": "Ordinary",
            "accessibility": "public",
            "isStatic": false,
            "isAbstract": false,
            "isVirtual": false,
            "isOverride": false,
            "isSealed": false,
            "isExtern": false,
            "isAsync": false,
            "isIterator": false,
            "isExtensionMethod": false,
            "isReadonly": false,
            "typeParams": [],
            "parameters": [],
            "returnType": { "kind": "named", "name": "System.Void", "args": [], "owner": null, "nullable": "none", "typeKind": "Void" },
            "returnsByRef": false,
            "returnsByRefReadonly": false,
            "explicitInterface": null,
            "operatorKind": null,
            "attributes": [],
            "deprecated": null,
            "hidden": false,
            "doc": "<summary>Executes the service.</summary><exception cref=\"T:ThrowLib.DomainException\">on domain error</exception>",
            "docInherited": false,
            "docLinks": null
          }
        ],
        "operators": [],
        "conversions": [],
        "indexers": [],
        "nested": []
      }
    }
  ]
}"#;

#[test]
fn inpackage_exception_type_lowers_to_nominal() {
    use nudox_ir::{build::Type, entry::EntryInner, kind::Kind};

    let extraction = parse_extraction(INPACKAGE_THROWS_FIXTURE.as_bytes())
        .expect("inpackage throws fixture must parse");
    let pkg = lower(&extraction).expect("inpackage throws fixture must lower");

    let execute = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "Execute")
        .expect("Execute method must be present");

    let fn_kind = match execute.1.kind() {
        EntryInner::Owned(Kind::Function(f)) => f,
        other => panic!("Execute must be a Function, got {other:?}"),
    };

    // throws must have 1 entry: DomainException, declared in the same extraction.
    assert_eq!(
        fn_kind.throws.len(),
        1,
        "Execute::throws must have 1 entry (DomainException)"
    );

    // DomainException is in the same extraction → must be Type::Nominal, not Any.
    assert!(
        matches!(fn_kind.throws[0], Type::Nominal(_)),
        "in-package exception type must lower to Type::Nominal, got {:?}",
        fn_kind.throws[0]
    );

    // output_params is empty (void return — no output param for void).
    assert_eq!(
        fn_kind.output_params.len(),
        0,
        "Execute has void return — output_params must be empty"
    );
}

/// A parameter with `hasDefault: true` and `default: "1024"` must have
/// `ParamAttribute::Optional` and its default text preserved in documentation.
#[test]
fn param_default_value_is_captured() {
    use nudox_ir::{build::ParamAttribute, entry::EntryInner, kind::Kind};

    let extraction = parse_extraction(EXCEPTION_AND_DEFAULT_FIXTURE.as_bytes())
        .expect("exception fixture must parse");
    let pkg = lower(&extraction).expect("exception fixture must lower");

    let max_len = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "maxLen")
        .expect("maxLen parameter must be present");

    let param_kind = match max_len.1.kind() {
        EntryInner::Owned(Kind::Param(p)) => p,
        other => panic!("maxLen must be a Param, got {other:?}"),
    };

    assert!(
        param_kind.attributes.contains(&ParamAttribute::Optional),
        "maxLen must carry ParamAttribute::Optional"
    );
    assert!(
        max_len.1.sym().documentation.contains("1024"),
        "maxLen documentation must contain the default value text '1024'"
    );
}

/// An event with asymmetric `add_accessibility` must surface that in its
/// documentation.
#[test]
fn event_accessor_accessibility_is_documented() {
    let extraction = parse_extraction(EXCEPTION_AND_DEFAULT_FIXTURE.as_bytes())
        .expect("exception fixture must parse");
    let pkg = lower(&extraction).expect("exception fixture must lower");

    let event_entry = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "DataArrived")
        .expect("DataArrived event must be present");

    assert!(
        event_entry.1.sym().documentation.contains("Add accessor"),
        "DataArrived documentation must mention asymmetric Add accessor accessibility"
    );
    assert!(
        event_entry.1.sym().documentation.contains("internal"),
        "DataArrived documentation must contain 'internal' (the add_accessibility value)"
    );
}

// ---------------------------------------------------------------------------
// New tests for features adopted from nudox-ir
// ---------------------------------------------------------------------------

/// A fixture that exercises declaration-site variance on generic type parameters.
///
/// `IEnumerable<out T>` (covariant) and `IComparer<in T>` (contravariant) are
/// canonical C# examples.  The oracle emits `"out"` / `"in"` / `"none"` as the
/// `variance` token on each TypeParam.
const VARIANCE_FIXTURE: &str = r#"{
  "format": 1,
  "dotnetVersion": "10.0",
  "roslyn": "5.6.0",
  "mode": "source",
  "assembly": { "name": "VarLib", "version": "1.0.0", "tfm": "net10.0" },
  "diagnostics": { "errorTypeCount": 0, "errorCount": 0 },
  "namespaces": [],
  "types": [
    {
      "docId": "T:VarLib.IReadable`1",
      "qualifiedName": "VarLib.IReadable`1",
      "simpleName": "IReadable",
      "kind": "INTERFACE",
      "namespace": "VarLib",
      "enclosing": null,
      "modifiers": ["public"],
      "typeParams": [
        {
          "name": "T",
          "variance": "out",
          "constraints": {
            "referenceType": false, "valueType": false, "notNull": false,
            "unmanaged": false, "constructor": false, "allowsRefLike": false, "types": []
          }
        }
      ],
      "baseType": null,
      "interfaces": [],
      "enumUnderlying": null,
      "delegateSig": null,
      "attributes": [],
      "deprecated": null,
      "hidden": false,
      "forwarded": false,
      "doc": null,
      "docInherited": false,
      "docLinks": null,
      "extensionReceiver": null,
      "members": { "fields": [], "properties": [], "events": [], "constructors": [],
                   "methods": [], "operators": [], "conversions": [], "indexers": [], "nested": [] }
    },
    {
      "docId": "T:VarLib.IWritable`1",
      "qualifiedName": "VarLib.IWritable`1",
      "simpleName": "IWritable",
      "kind": "INTERFACE",
      "namespace": "VarLib",
      "enclosing": null,
      "modifiers": ["public"],
      "typeParams": [
        {
          "name": "T",
          "variance": "in",
          "constraints": {
            "referenceType": false, "valueType": false, "notNull": false,
            "unmanaged": false, "constructor": false, "allowsRefLike": false, "types": []
          }
        }
      ],
      "baseType": null,
      "interfaces": [],
      "enumUnderlying": null,
      "delegateSig": null,
      "attributes": [],
      "deprecated": null,
      "hidden": false,
      "forwarded": false,
      "doc": null,
      "docInherited": false,
      "docLinks": null,
      "extensionReceiver": null,
      "members": { "fields": [], "properties": [], "events": [], "constructors": [],
                   "methods": [], "operators": [], "conversions": [], "indexers": [], "nested": [] }
    }
  ]
}"#;

/// Declaration-site variance (`out T` → Covariant, `in T` → Contravariant) must
/// be preserved in `GenericParam::Type::variance`.
#[test]
fn variance_round_trip() {
    use nudox_ir::{
        entry::EntryInner, kind::Kind, kinds::generics::GenericParam, kinds::ty::Variance,
    };

    let extraction =
        parse_extraction(VARIANCE_FIXTURE.as_bytes()).expect("variance fixture must parse");
    let pkg = lower(&extraction).expect("variance fixture must lower");

    // IReadable<out T> → Covariant.
    let readable = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "IReadable")
        .expect("IReadable must be present");

    let readable_generics = match readable.1.kind() {
        EntryInner::Owned(Kind::Trait(t)) => &t.generics,
        other => panic!("IReadable must be a Trait, got {other:?}"),
    };
    assert_eq!(
        readable_generics.len(),
        1,
        "IReadable must have exactly one generic param"
    );
    match &readable_generics[0] {
        GenericParam::Type { name, variance, .. } => {
            assert_eq!(name, "T");
            assert_eq!(
                *variance,
                Some(Variance::Covariant),
                "IReadable<out T>: T must be Covariant, got {variance:?}"
            );
        }
        other => panic!("expected GenericParam::Type, got {other:?}"),
    }

    // IWritable<in T> → Contravariant.
    let writable = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "IWritable")
        .expect("IWritable must be present");

    let writable_generics = match writable.1.kind() {
        EntryInner::Owned(Kind::Trait(t)) => &t.generics,
        other => panic!("IWritable must be a Trait, got {other:?}"),
    };
    assert_eq!(
        writable_generics.len(),
        1,
        "IWritable must have exactly one generic param"
    );
    match &writable_generics[0] {
        GenericParam::Type { name, variance, .. } => {
            assert_eq!(name, "T");
            assert_eq!(
                *variance,
                Some(Variance::Contravariant),
                "IWritable<in T>: T must be Contravariant, got {variance:?}"
            );
        }
        other => panic!("expected GenericParam::Type, got {other:?}"),
    }
}

/// A fixture with a delegate type that has a structured invoke signature.
const DELEGATE_FIXTURE: &str = r#"{
  "format": 1,
  "dotnetVersion": "10.0",
  "roslyn": "5.6.0",
  "mode": "source",
  "assembly": { "name": "DelLib", "version": "1.0.0", "tfm": "net10.0" },
  "diagnostics": { "errorTypeCount": 0, "errorCount": 0 },
  "namespaces": [],
  "types": [
    {
      "docId": "T:DelLib.Transformer",
      "qualifiedName": "DelLib.Transformer",
      "simpleName": "Transformer",
      "kind": "DELEGATE",
      "namespace": "DelLib",
      "enclosing": null,
      "modifiers": ["public"],
      "typeParams": [],
      "baseType": null,
      "interfaces": [],
      "enumUnderlying": null,
      "delegateSig": {
        "params": [
          {
            "name": "input",
            "type": { "kind": "named", "name": "System.String", "args": [], "owner": null, "nullable": "notAnnotated", "typeKind": "Class" },
            "refKind": "none",
            "isParams": false,
            "hasDefault": false,
            "default": null,
            "scoped": false,
            "attributes": []
          }
        ],
        "return": { "kind": "named", "name": "System.Int32", "args": [], "owner": null, "nullable": "none", "typeKind": "Struct" }
      },
      "attributes": [],
      "deprecated": null,
      "hidden": false,
      "forwarded": false,
      "doc": null,
      "docInherited": false,
      "docLinks": null,
      "extensionReceiver": null,
      "members": { "fields": [], "properties": [], "events": [], "constructors": [],
                   "methods": [], "operators": [], "conversions": [], "indexers": [], "nested": [] }
    }
  ]
}"#;

/// A delegate must lower to `Alias { target: Some(Type::FunctionPointer { ... }) }`.
/// Previously delegates produced `Alias { target: None }`, discarding all type info.
#[test]
fn delegate_lowers_to_function_pointer_alias() {
    use nudox_ir::{build::Type, entry::EntryInner, kind::Kind};

    let extraction =
        parse_extraction(DELEGATE_FIXTURE.as_bytes()).expect("delegate fixture must parse");
    let pkg = lower(&extraction).expect("delegate fixture must lower");

    let transformer = pkg
        .iter()
        .find(|(_, e)| e.sym().name == "Transformer")
        .expect("Transformer delegate must be present");

    let alias_body = match transformer.1.kind() {
        EntryInner::Owned(Kind::Alias(a)) => a,
        other => panic!("Transformer must be an Alias, got {other:?}"),
    };

    let target = alias_body
        .target
        .as_ref()
        .expect("Transformer alias must have a structural target (was None before)");

    match target {
        Type::FunctionPointer { params, ret, abi } => {
            assert_eq!(
                params.len(),
                1,
                "Transformer has one input parameter (string)"
            );
            // Return type: System.Int32 → I32 primitive.
            assert!(ret.is_some(), "Transformer has a return type (int)");
            assert!(
                abi.is_none(),
                "managed delegate must have abi = None, got {abi:?}"
            );
        }
        other => panic!("Transformer alias target must be Type::FunctionPointer, got {other:?}"),
    }
}

/// A fixture with a labelled value tuple `(int start, int end)`.
const LABELLED_TUPLE_FIXTURE: &str = r#"{
  "format": 1,
  "dotnetVersion": "10.0",
  "roslyn": "5.6.0",
  "mode": "source",
  "assembly": { "name": "TupLib", "version": "1.0.0", "tfm": "net10.0" },
  "diagnostics": { "errorTypeCount": 0, "errorCount": 0 },
  "namespaces": [],
  "types": [
    {
      "docId": "T:TupLib.RangeUtil",
      "qualifiedName": "TupLib.RangeUtil",
      "simpleName": "RangeUtil",
      "kind": "CLASS",
      "namespace": "TupLib",
      "enclosing": null,
      "modifiers": ["public"],
      "typeParams": [],
      "baseType": null,
      "interfaces": [],
      "enumUnderlying": null,
      "delegateSig": null,
      "attributes": [],
      "deprecated": null,
      "hidden": false,
      "forwarded": false,
      "doc": null,
      "docInherited": false,
      "docLinks": null,
      "extensionReceiver": null,
      "members": {
        "fields": [],
        "properties": [],
        "events": [],
        "constructors": [],
        "methods": [
          {
            "name": "GetRange",
            "docId": "M:TupLib.RangeUtil.GetRange",
            "methodKind": "Ordinary",
            "accessibility": "public",
            "isStatic": true,
            "isAbstract": false,
            "isVirtual": false,
            "isOverride": false,
            "isSealed": false,
            "isExtern": false,
            "isAsync": false,
            "isIterator": false,
            "isExtensionMethod": false,
            "isReadonly": false,
            "typeParams": [],
            "parameters": [],
            "returnType": {
              "kind": "tuple",
              "elements": [
                { "name": "start", "type": { "kind": "named", "name": "System.Int32", "args": [], "owner": null, "nullable": "none", "typeKind": "Struct" } },
                { "name": "end",   "type": { "kind": "named", "name": "System.Int32", "args": [], "owner": null, "nullable": "none", "typeKind": "Struct" } }
              ],
              "nullable": ""
            },
            "returnsByRef": false,
            "returnsByRefReadonly": false,
            "explicitInterface": null,
            "operatorKind": null,
            "attributes": [],
            "deprecated": null,
            "hidden": false,
            "doc": null,
            "docInherited": false,
            "docLinks": null
          }
        ],
        "operators": [],
        "conversions": [],
        "indexers": [],
        "nested": []
      }
    }
  ]
}"#;

/// A method returning `(int start, int end)` must lower to a `Type::Tuple`
/// whose elements are `TupleElement::Named`, preserving the labels.
/// Previously all labels were dropped (all elements became `Positional`).
#[test]
fn labelled_tuple_preserves_labels() {
    use nudox_ir::{build::Type, entry::EntryInner, kind::Kind, kinds::ty::TupleElement};

    let extraction = parse_extraction(LABELLED_TUPLE_FIXTURE.as_bytes())
        .expect("labelled-tuple fixture must parse");
    let pkg = lower(&extraction).expect("labelled-tuple fixture must lower");

    // The return Param of GetRange is an anonymous entry (name = "") whose
    // type is the labelled tuple.  Since this is the only method in the
    // fixture, the only anonymous Param is the return value of GetRange.
    // Strategy: find the Param entry whose type is a Tuple.
    let return_param = pkg
        .iter()
        .find(|(_, e)| {
            if let EntryInner::Owned(Kind::Param(p)) = e.kind() {
                matches!(p.ty.as_ref(), Some(Type::Tuple(_)))
            } else {
                false
            }
        })
        .expect("must find a Param whose type is a Tuple (the GetRange return)");

    let ty = match return_param.1.kind() {
        EntryInner::Owned(Kind::Param(p)) => p.ty.as_ref().expect("return param must have a type"),
        other => panic!("return param must be a Param, got {other:?}"),
    };

    match ty {
        Type::Tuple(elems) => {
            assert_eq!(elems.len(), 2, "tuple must have 2 elements");
            match &elems[0] {
                TupleElement::Named { label, .. } => {
                    assert_eq!(label, "start", "first element label must be 'start'");
                }
                TupleElement::Positional(_) => {
                    panic!("first element must be Named{{start}}, not Positional")
                }
            }
            match &elems[1] {
                TupleElement::Named { label, .. } => {
                    assert_eq!(label, "end", "second element label must be 'end'");
                }
                TupleElement::Positional(_) => {
                    panic!("second element must be Named{{end}}, not Positional")
                }
            }
        }
        other => panic!("return type of GetRange must be Type::Tuple, got {other:?}"),
    }
}
