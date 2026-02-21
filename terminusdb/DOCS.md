IR Types in TerminusDB
======================

These docs describe how the IR is stored in terminusdb. From a top, there are
two main kinds of nodes:

 -  Entry -> Purely symbolic and meta-informative. This describes things like name,
    documentation, direct links, etc.
 -  Kind -> Abstract, structural representation of the underlying data type. If
    there is a function, this would contain info like function name, input
    parameters, output parameters, etc. For types like Modules, this can hold
    only the kind\_tag, but there are plans to use sparse Kind variants to
    store more information.

   - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - -


Entry
-----

Entry is the node you usually want to fetch first. It is the indexable unit:
names, docs, navigation, and the stable place to hang “what is this symbol”
metadata. Structural meaning lives behind `kind`.

### Entry fields

 -  `aliases: Set<xsd:string>`
     -  Alternate fully-qualified names that should resolve to the same Entry.
     -  This matters because one underlying symbol can appear under multiple public
        names (re-exports, renamed items, trait method paths, different import
        surfaces). Clients should treat aliases as equivalent lookup keys for
        the same thing.
     -  It is a Set, so there is no ordering and duplicates are not meaningful.

 -  `documentation: Optional<xsd:string>`
     -  Documentation string for the symbol, if present.
     -  This is the most “human” payload in the graph: it’s what you show in UI and
        what you use for search relevance and summary generation. If absent,
        there was no doc available at ingest time (not an empty string).
     -  This is stored at Entry because it’s tied to the symbol identity, not the
        structural shape.

 -  `fq_name: xsd:string`
     -  Fully-qualified name string used for display and lookup.
     -  This is the main canonical label for the Entry. Even if URIs or paths shift
        over time, fq\_name is the thing clients can show everywhere and use as
        a stable external handle.
     -  If you are building search / linking, this is typically the string you keep in
        a UI index.

 -  `kind: Kind`
     -  Reference to the structural node for this Entry.
     -  This is the gateway from “symbol” to “shape”. Clients use it when they need
        details like signatures, generics, fields, variants, or trait
        relationships.
     -  Without following `kind`, you cannot reliably answer “what is it” beyond
        surface metadata.

 -  `members: Set<Entry>`
     -  Child Entries contained by this Entry.
     -  This is the navigation skeleton of the symbol graph. For a module it is “what’s
        inside”. For a type it can be associated items, nested definitions, or
        anything that the IR considers contained by that symbol.
     -  Because it’s a Set, treat it as an unordered collection and apply your own
        sorting rules (alphabetical, visibility-first, kind\_tag grouping,
        etc.) in the client.

 -  `name: xsd:string`
     -  The short name of the symbol (non-qualified).
     -  This is what you show in compact lists and breadcrumbs. It is also the piece
        most often matched by quick search queries.
     -  It is intentionally separate from fq\_name so clients don’t need to parse
        fq\_name for basic display.

 -  `path: List<xsd:string>`
     -  Segmented path for the symbol.
     -  This gives you a structured way to build breadcrumbs, do hierarchical grouping,
        and build deterministic routes in client apps (segment-by-segment
        navigation).
     -  Because it is a List, ordering is meaningful and represents containment: outer
        namespace first, symbol last.

 -  `visibility: xsd:string`
     -  Visibility qualifier for the symbol.
     -  This is important for client filtering (public API browsing vs internal-only
        browsing) and for generating “what should be surfaced” views.
     -  Kept as a string so language/tooling rules can evolve without forcing a schema
        migration.

   - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - -


Kind
----

Kind is the node you fetch when you need the actual shape of the symbol. If
Entry answers “what is it called and where is it”, Kind answers “what is it
structurally”.

### Kind base class fields

 -  `kind_tag: xsd:string`
     -  Discriminator tag for what the Kind represents.
     -  This is the fast switch for clients: it tells you which renderer/view to use
        and what additional fields you should expect to exist.
     -  It also supports analytics-style queries: “count all Function kinds”, “list all
        RecordType kinds”, etc.

### sys:JSON payload fields

Some Kind variants store structured payloads as `sys:JSON`. This is not random
JSON storage, it is a deliberate way to preserve rich IR structures (types,
signatures, bounds) without baking in a rigid schema too early.

Implications:

 -  You can render these blobs directly if you need to ship quickly.
 -  If you want higher-quality UX, you define an application-level schema for these
    JSON objects and render them as typed views (types with links, param
    tables, generic constraints sections, etc.).
 -  Expect these blobs to be the most “valuable” information for clients even
    though they are not strongly typed in Terminus.

   - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - -


Kind variants
-------------

### Module

Module is the structural node for namespace-like containers.

 -  There are no extra fields because the important content is modeled through the
    Entry tree (`members`), not inside the Kind itself.
 -  The significance of Module is that it defines a browsing boundary. If you are
    building an API browser, Module + members gives you the primary navigation
    experience.

### Info

Info is a structural placeholder for nodes where we want a Kind target but do
not want to commit to a heavier shape yet.

 -  In practice this lets the dataset stay consistent (“every Entry has a Kind”)
    even when the symbol doesn’t map cleanly to a structural schema.
 -  This is useful for incremental modeling: Info nodes can later be upgraded into
    richer variants without breaking Entry identity.

### UnionType

UnionType represents a “one of these types” structure.

 -  `types: List<sys:JSON>`
     -  The list of type expressions that participate in the union.
     -  The payload matters because union types are a major driver for type reasoning
        and compatibility checks. Clients can use this to render “accepted
        types”, “returned types”, or “variant-of” summaries.
     -  Stored as JSON because type expressions can be nested (paths, generics,
        lifetimes, trait objects, etc.).

### SumType

SumType represents enum-like structures: a finite set of named variants.

 -  `variants: List<sys:JSON>`
     -  Each variant should capture the variant name plus its payload shape (unit /
        tuple / struct-like fields) and any attributes/docs if available.
     -  The significance is high for client rendering because SumType enables a full
        “shape viewer”: variant list, payload expansion, and links to types
        used in each variant.

### RecordType

RecordType represents struct/record-like types: named containers of fields.

 -  `fields: List<sys:JSON>`
     -  Each field payload generally includes at least name + type, and can include
        visibility, attributes, docs, and default/value metadata depending on
        IR support.
     -  This is one of the most important structures for clients because it drives most
        “what does this type look like” UI: field tables, doc browsing, and
        compatibility diffs.

 -  `generics: Optional<sys:JSON>`
     -  Generic parameters and constraints for the record.
     -  This is essential for accurate type display because many record shapes are only
        meaningful when you show the generic arguments and bounds.

 -  `name: xsd:string`
     -  Display name for the record type (short name).

 -  `record_kind: xsd:string`
     -  Category of record shape (struct-like / tuple-like / unit-like, depending on
        the language/tooling).
     -  This matters because it tells a client how to render the shape and what syntax
        to use when generating examples.

 -  `visibility: xsd:string`
     -  Visibility qualifier for the type.

### Function

Function represents callable symbols with signature data and minimal state.

 -  `name: xsd:string`
     -  Function name (short name). This is what you display as the symbol title.

 -  `visibility: xsd:string`
     -  Visibility qualifier. This is a key filter for “public API surface”.

 -  `implemented: xsd:boolean`
     -  Indicates whether the IR captured an implementation body or treats this as
        declaration-only.
     -  This is significant for API browsing because it distinguishes “real behavior
        lives here” from “this is only a contract” (externs, trait
        requirements, stubs).

 -  `input_parameters: Optional<sys:JSON>`
     -  Structured parameter list.
     -  This is the heart of a function signature. Even if you don’t parse the full
        type system, the parameter list is what users expect to see first.

 -  `output_parameters: Optional<sys:JSON>`
     -  Return representation.
     -  This is equally important for callers: it defines what the function yields and
        often encodes Result/Option-like structures.

 -  `attributes: Optional<sys:JSON>`
     -  Attributes/annotations applied to the function.
     -  This matters for surface-level behavior flags: async-ness, inline hints,
        exported ABI, test markers, deprecations, and other “this changes how I
        should call it” metadata.

 -  `generics: Optional<sys:JSON>`
     -  Function generics and constraints.
     -  Critical for correct signature display and for linking type parameters into
        parameter/return types.

### TraitDef

TraitDef represents an interface contract: a set of required/provided items and
relationships.

 -  `name: xsd:string`
     -  Trait name (short name).

 -  `visibility: xsd:string`
     -  Visibility qualifier.

 -  `generics: Optional<sys:JSON>`
     -  Trait generic parameters and bounds.
     -  This is core to understanding the trait because trait bounds define where it
        applies.

 -  `super_traits: Optional<sys:JSON>`
     -  Parent traits / trait bounds.
     -  This defines the inheritance/constraint chain and is crucial for clients to
        render “this trait implies these traits”.

 -  `associated_types: Optional<sys:JSON>`
     -  Associated type declarations.
     -  These often act like “type slots” that implementers must fill, and clients need
        this for both display and constraint reasoning.

 -  `required_methods: Optional<sys:JSON>`
     -  Method declarations that must be implemented.
     -  Highly significant for “what do I need to implement” views.

 -  `provided_methods: Optional<sys:JSON>`
     -  Methods that come with default implementations.
     -  Significant because they define usable behavior without requiring
        implementation.

 -  `required_constants: Optional<sys:JSON>`
     -  Required associated constants.
     -  This is a part of the trait contract and is important for implementers and
        users of the trait.

 -  `docs: Optional<xsd:string>`
     -  Trait-level docs when present separately from Entry documentation.
     -  If both exist, Entry documentation is typically the primary display doc, and
        this can be treated as supplemental/merged content.

### TraitImpl

TraitImpl represents a specific implementation of a trait for a target type.

 -  `trait_ref: sys:JSON`
     -  The trait being implemented.
     -  This is how a client answers “what trait is this impl for” and can be used to
        link to the TraitDef.

 -  `for_type: sys:JSON`
     -  The target type receiving the implementation.
     -  This is how a client answers “who gets these methods”.

 -  `visibility: xsd:string`
     -  Visibility qualifier.

 -  `generics: Optional<sys:JSON>`
     -  Implementation generics and constraints.
     -  This matters because many impls apply only under specific bounds (“blanket impl
        under constraints”).

 -  `methods: Optional<sys:JSON>`
     -  Implemented methods and their details.
     -  This is important for clients who want to show “what changes” vs the trait
        contract and what concrete behavior exists.

 -  `associated_types: Optional<sys:JSON>`
     -  Concrete bindings for associated types.
     -  Often this is the key detail users look for: “what is Item for this impl”.

 -  `associated_constants: Optional<sys:JSON>`
     -  Concrete bindings for associated constants.

 -  `is_negative: xsd:boolean`
     -  True if negative impl (explicitly stating something does not implement a trait).
     -  This is rare but significant for reasoning and conflict detection.

 -  `is_blanket: xsd:boolean`
     -  True if this is a blanket impl (applies broadly across many types).
     -  Significant because blanket impls define large parts of behavior surface in
        languages like Rust.

 -  `is_unsafe: xsd:boolean`
     -  True if the impl is marked unsafe.
     -  Significant because it signals stricter correctness/safety requirements and
        often changes how clients want to display warnings.

 -  `docs: Optional<xsd:string>`
     -  Implementation docs if available.

### TypeAlias

TypeAlias represents a named synonym for another type expression.

 -  `aliased_type: sys:JSON`
     -  The underlying type expression.
     -  This is significant because type aliases are extremely common in public APIs:
        they simplify signatures, encode domain meanings, and can hide
        complexity. Clients should render these as `type X = ...` style views
        and ideally link through to underlying referenced types.

### InterfaceType

InterfaceType is currently a placeholder for interface-like constructs that are
not modeled as TraitDef.

 -  There are no extra fields yet, but the existence of this variant is important
    because it allows the dataset to distinguish “interface-like” categories in
    a way that can later be expanded.

### PrimitiveType

PrimitiveType represents language built-in primitive types.

 -  There are no extra fields because the “shape” is not decomposable in the same
    way as records/enums/functions.
 -  The significance is that primitive types anchor type graphs. When rendering
    type expressions, clients need a consistent way to display primitives and
    treat them as terminal nodes.

### Constant

Constant represents constant symbols. This is intentionally sparse right now.

 -  The significance is that constants often carry important API meaning (default
    values, config flags, version markers). Even without payload fields, the
    node exists so clients can navigate and attach docs, and so future schema
    can add type/value/attributes without breaking identity.

### Variable

Variable represents variable-like symbols (globals, statics, etc.) depending on
the IR.

 -  Same significance pattern as Constant: exists for navigation and future
    enrichment (mutability, type, initializer, thread-safety flags).

### Field

Field represents field-like symbols (often inside record/struct contexts).

 -  Even though RecordType also has a `fields` payload, having Field as a Kind
    category is useful for situations where fields are addressed as first-class
    symbols (docs, references, or separate indexing).
 -  This is a natural expansion point for making fields fully linkable (type,
    attributes, offset/layout, docs).

### Macro

Macro represents macro-like symbols.

 -  Macros can behave like functions syntactically but have different expansion
    semantics and often different call rules. Even if the payload is sparse
    now, distinguishing Macro allows clients to render correct UX and avoid
    treating macros as normal functions.

### Event

Event represents event-like symbols (framework/language dependent).

 -  This exists as a category marker so clients can group and render “event
    surfaces” differently from plain functions or records once payload is added
    (event name, payload type, subscription pattern, etc.).
