This module magically transforms compiler IR into JSON-LD to be dumped into a RDF triple based DB (**TerminusDB**)

**Core goal**: *flatten nested Rust structs into RDF triple-based nodes* with stable `@id` values and minimal nesting and explicit edges (`@id` ref) instead of nested objects

# Concepts

## Entry
An **Entry** is the symbol for the "demographics"/metadata of an API symbol
Think name, path, fq_name, documentation, visibility and edges to related symbols.

Example
```json
{
  "@id":"Entry/rust/axum/axum",
  "@type":"Entry",
  "aliases":[
    "axum::serve::serve"
  ],
  "documentation":"Serve the service with the supplied listener.\n\nThis method of running a service is SHORTNED FOR READABILITY SAKE",
  "fq_name":"axum",
  "kind":"Function/rust/axum/axum",
  "members":[
    "Entry/AFakePointerToOtherEntry"
  ],
  "name":"serve",
  "path":["axum"],
  "visibility":"public"
}
```
## Kind
A **Kind** structurally represents a symbol. Think Function, Struct, Enum, Module, etc.
Kinds hold payload fields that descrie the symbol’s behavior/shape (params/return types for functions).

There are two main categories as of now.
- **Hub-like kinds**: Skeleton like kind (think `Module`) that contains little to no information. The Entry would contain the significant information for a Module (members, documentation, etc.). There is a lot of room to shape this in ways that can be useful for Agents (Devs + LLM Agents)
- **Authority kinds**: Lots of data within this kind (`Function`, `Struct`, `Enum`) where Kind describes the shape of this symbol

For Shape Lookup, Kind will have some @key - hashed field, derived from contents of kind that can be used to find similar Kinds. Ignore for now, but *a clever solution could allow us to query different languages for crates that have similar solutions to a problem*.

# Identity + URI Rules
Requirement -> **any referencing node must be able to compute the URI of what it references** without relying on discovery order. An entry that is a `Function` that has an input parameter of Type `MyType` should be able to have the link to the Kind associated to `MyType`'s Entry.

## Symbol identity (`symbol_id`)
The symbol id is currently derived from the path or fq_name of that symbol. What changed for kind and Entry is that Entry will always be prefixed by Entry, whereas Kind's prefix will be the type of kind_tag in PascalCase

URI @ids:
- **Entry** `{"@id": "Entry/{lang}/{crate_name}/{symbol_id}}"`
- **KindVariants**  `{"@id": "KindTag/{lang}/{crate_name}/{kind_tag}/{symbol_id}}"`

# Architecture
The core idea is to store a Map (BTreeMap for deterministic iteration by URI value) of URI -> Json Values, representing the symbol/kind/etc URI and the corresponding values. Some context will be passed to contain URIs, metadata, etc. to include on various types/enum variants for Kind

# TerminusDB *WIP* Schema
This will need to be consistently updated
- Each variant of Kind inherits from the abstract Kind class, which contains only the field kind_tag -> more can be added, maybe a back link to the original Entry, etc.
- Additionally, All non-implented variants of kind simply inherit from the Kind class.
- **MISSING**
  - Structural Hashes for lookup

```json
[
  {
    "@type": "@context",
    "@schema": "terminusdb:///schema#",
    "@base": "terminusdb:///data/",
    "xsd": "http://www.w3.org/2001/XMLSchema#",
    "sys": "http://terminusdb.com/schema/sys#"
  },
  {
    "@id": "Entry",
    "@type": "Class",
    "aliases": {
      "@type": "Set",
      "@class": "xsd:string"
    },
    "documentation": {
      "@class": "xsd:string",
      "@type": "Optional"
    },
    "fq_name": "xsd:string",
    "kind": "Kind",
    "members": { "@type": "Set", "@class": "Entry" },
    "name": "xsd:string",
    "path": { "@type": "List", "@class": "xsd:string" },
    "visibility": "xsd:string"
  },
  
  {
    "@id": "Kind",
    "@type": "Class",
    "@abstract": [],
    "kind_tag": "xsd:string"
  },
  {
    "@id": "Module",
    "@type": "Class",
    "@inherits": ["Kind"]
  },
  {
    "@id": "Info",
    "@type": "Class",
    "@inherits": ["Kind"]
  },
  {
    "@id": "UnionType",
    "@type": "Class",
    "@inherits": ["Kind"]
  },
  {
    "@id": "TraitDef",
    "@type": "Class",
    "@inherits": ["Kind"]
  },
  {
    "@id": "TraitImpl",
    "@type": "Class",
    "@inherits": ["Kind"]
  },
  {
    "@id": "SumType",
    "@type": "Class",
    "@inherits": ["Kind"]
  },
  {
    "@id": "InterfaceType",
    "@type": "Class",
    "@inherits": ["Kind"]
  },
  {
    "@id": "Function",
    "@type": "Class",
    "@inherits": ["Kind"]
  },
  {
    "@id": "TypeAlias",
    "@type": "Class",
    "@inherits": ["Kind"]
  },
  {
    "@id": "Constant",
    "@type": "Class",
    "@inherits": ["Kind"]
  },
  {
    "@id": "Variable",
    "@type": "Class",
    "@inherits": ["Kind"]
  },
  {
    "@id": "Macro",
    "@type": "Class",
    "@inherits": ["Kind"]
  },
  {
    "@id": "PrimitiveType",
    "@type": "Class",
    "@inherits": ["Kind"]
  },
  {
    "@id": "Field",
    "@type": "Class",
    "@inherits": ["Kind"]
  },
  {
    "@id": "Event",
    "@type": "Class",
    "@inherits": ["Kind"]
  },
  {
    "@id": "RecordType",
    "@type": "Class",
    "@inherits": ["Kind"],
    "fields": "sys:JSON",
    "name": "xsd:string",
    "record_kind": "xsd:string",
    "visibility": "xsd:string"
  }
]
```
