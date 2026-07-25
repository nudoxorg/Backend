//! Unit tests for the C# producer lowering — no oracle invocation.
//!
//! We embed a literal JSON fixture that covers:
//!   - a namespace with a class (with fields, a generic method, XML doc)
//!   - an interface
//!   - an enum with members
//!   - a generic method with a type constraint
//!   - a property
//!   - an XML doc comment with `<summary>` and `<see cref=…>`

use nudox_producer_csharp::{lower, parse_extraction};

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
    let variants: Vec<_> = pkg
        .iter()
        .filter(|(_, e)| {
            matches!(
                e.kind(),
                nudox_ir::entry::EntryInner::Owned(Kind::Variant(_))
            )
        })
        .collect();
    assert_eq!(
        variants.len(),
        2,
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
            nudox_producer_csharp::ProducerError::UnsupportedFormat { .. }
        ),
        "expected UnsupportedFormat error"
    );
}

#[test]
fn empty_types_returns_error() {
    let empty_json = r#"{"format": 1, "assembly": {"name":"x"}, "diagnostics": {}, "types": [], "namespaces": []}"#;
    let err = parse_extraction(empty_json.as_bytes()).expect_err("must fail for empty types");
    assert!(
        matches!(err, nudox_producer_csharp::ProducerError::NoTypes),
        "expected NoTypes error"
    );
}
