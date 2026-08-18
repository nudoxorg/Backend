//! Serde mirror of the C# Roslyn oracle's JSON document.
//!
//! The oracle (`oracle/Program.cs` + friends) prints one JSON object per
//! invocation; every struct/enum here mirrors that shape exactly (camelCase,
//! see CSHARP-PLAN §2.6). The oracle always emits every key (using `null` /
//! `[]` for absent values), but `#[serde(default)]` is applied liberally so
//! schema evolution on the C# side degrades gracefully instead of failing
//! deserialization.
//!
//! The one recursive shape is [`TypeSig`] (a structural type *use*, mirroring
//! Roslyn's `ITypeSymbol`), internally tagged by `"kind"`. String-typed
//! enumerations (accessibility, nullability, ref-kind, …) stay `String` so a
//! new oracle value never breaks the parse — the lowering interprets them.
//!
//! **Memory note:** all `String` fields here are owned because serde_json's
//! from_slice allocates from its own scratch; the raw bytes are dropped after
//! parse. A future pass could gate borrow behind `#[serde(borrow)] &'de str`
//! but that requires pinning the original JSON bytes for the lifetime of the
//! Extraction, which complicates the Producer::oracle() → lower() contract.
//! For now, owned strings are the honest choice.

use std::collections::BTreeMap;

use serde::Deserialize;

/// The oracle emits `null` for many absent string fields. `#[serde(default)]`
/// only covers *missing* keys — present-null still fails. This accepts both.
fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// The whole oracle output: one document per invocation.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Extraction {
    /// The payload schema this build reads.
    ///
    /// `0` when the field is absent, which is what any `oracle.dll` built
    /// before this handshake existed emits — see [`Extraction::staleness`].
    /// The oracle (`Extractor.WriteExtraction` in
    /// `oracle/csharp/Extractor.cs`) stamps this unconditionally, never
    /// omitted.
    #[serde(default)]
    pub format: u32,
    /// The .NET runtime version the oracle ran on (`"10.0"`).
    #[serde(default, deserialize_with = "null_as_default")]
    pub dotnet_version: String,
    /// The Roslyn version (`"5.6.0"`).
    #[serde(default, deserialize_with = "null_as_default")]
    pub roslyn: String,
    /// `"metadata"` or `"source"`.
    #[serde(default, deserialize_with = "null_as_default")]
    pub mode: String,
    /// The extracted assembly's identity + facade metadata.
    pub assembly: Assembly,
    /// Compilation diagnostics (error-type + total error counts) for tiering.
    #[serde(default)]
    pub diagnostics: Diagnostics,
    /// Every namespace inhabited by an extracted type, with its `<summary>`
    /// when a `namespace-info`-style doc exists (rare in C#).
    #[serde(default)]
    pub namespaces: Vec<Namespace>,
    /// Every included type declaration; nesting is expressed via
    /// [`TypeDecl::enclosing`] (a flat list).
    #[serde(default)]
    pub types: Vec<TypeDecl>,
    /// Roslyn-bound same-assembly method calls.
    #[serde(default)]
    pub references: Vec<Reference>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reference {
    pub owner: String,
    pub target: String,
    pub file: String,
    pub start: usize,
    pub end: usize,
}

/// Why an oracle payload cannot be trusted to be complete.
///
/// Mirrors `crate::go::oracle::Staleness` — see that type's doc comment for
/// the field report that motivated the mechanism. `NUDOX_CSHARP_ORACLE` is
/// the C# analogue of `NUDOX_GO_ORACLE_BIN`: `lindsey.app` ships neither
/// oracle, so a user points either variable at whatever prebuilt copy they
/// find.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Staleness {
    /// The oracle predates a field — or a field's scope — this build reads.
    #[error(
        "the C# oracle is out of date: it speaks payload schema {found}, but \
         this build reads schema {required}. Data added since (currently: \
         `references`, the Roslyn-bound same-assembly call graph that backs \
         `refs`) is absent or incomplete in its output, so `refs` will look \
         empty for every C# package rather than genuinely reference-free. \
         Rebuild the oracle (`dotnet publish -c Release --no-self-contained -o \
         publish` in workspace/compiler/languages/oracle/csharp) and point \
         NUDOX_CSHARP_ORACLE at the result."
    )]
    OlderThanRequired {
        /// What the oracle declared (`0` when it declared nothing).
        found: u32,
        /// What this build needs.
        required: u32,
    },
}

impl Extraction {
    /// The payload schema this build reads.
    ///
    /// Bump this in lockstep with the `format` argument
    /// `Extractor.WriteExtraction` is called with (`oracle/csharp/Extractor.cs`)
    /// whenever the Rust side starts *reading* a field the oracle only
    /// recently started emitting — that is exactly the moment an older
    /// `oracle.dll` begins under-reporting it. Currently `1`: every field this
    /// build reads (including `references`) has been part of `format` 1 since
    /// it was introduced, so nothing has forced a bump yet.
    pub const REQUIRED_SCHEMA_VERSION: u32 = 1;

    /// Whether the oracle that produced this payload is older than this build
    /// expects.
    ///
    /// # Why this is not a deserialization error
    ///
    /// Every field on [`Extraction`] is `#[serde(default)]`, deliberately: a
    /// *newer* oracle adding fields must not break an older reader. The cost
    /// is that an *older* oracle omitting fields is indistinguishable from a
    /// package that genuinely has none — both arrive as an empty `Vec`. See
    /// `crate::go::oracle::Output::staleness` for the incident that made this
    /// worth guarding before a C#-specific field report is what surfaces it.
    ///
    /// Returns `None` for a current *or newer* oracle: only older
    /// under-reports.
    pub fn staleness(&self) -> Option<Staleness> {
        (self.format < Self::REQUIRED_SCHEMA_VERSION).then_some(Staleness::OlderThanRequired {
            found: self.format,
            required: Self::REQUIRED_SCHEMA_VERSION,
        })
    }
}

/// The assembly identity and facade metadata.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Assembly {
    #[serde(default, deserialize_with = "null_as_default")]
    pub name: String,
    pub version: Option<String>,
    pub tfm: Option<String>,
    /// Type-forwarded names (facade packages like `System.Memory`).
    #[serde(default)]
    pub forwarded_types: Vec<String>,
    /// `InternalsVisibleTo` targets (metadata only).
    #[serde(default)]
    pub ivt: Vec<String>,
}

/// Compilation diagnostics summary.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostics {
    /// Count of `TypeKind.Error` symbols encountered (missing-ref fidelity).
    #[serde(default)]
    pub error_type_count: u64,
    /// Total compilation error count.
    #[serde(default)]
    pub error_count: u64,
    /// Whether source generators contributed to this extraction.
    ///
    /// `Applied` means configured source generators contributed syntax;
    /// `Unavailable` means configured inputs could not be loaded; `Unknown`
    /// means no generator configuration was present or the field was omitted
    /// by an older oracle.
    #[serde(default)]
    pub generator_support: GeneratorSupport,
}

/// Source-generator support reported by the oracle.
///
/// This is deliberately typed rather than an advisory string: consumers must
/// be able to distinguish an extraction that included generated API from one
/// that could not evaluate generators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GeneratorSupport {
    /// A configured source generator could not be loaded or executed.
    Unavailable,
    /// Configured source generators were loaded and their output was applied.
    Applied,
    /// A legacy document omitted the field.
    Unknown,
}

impl Default for GeneratorSupport {
    fn default() -> Self {
        Self::Unknown
    }
}

/// A namespace, with a doc summary when present.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Namespace {
    pub name: String,
    pub doc: Option<String>,
}

/// Where a declaration lives in its source file.
///
/// `start`/`end` are **UTF-8 byte offsets**, matching
/// [`nudox_ir::entry::Symbol::span`]'s documented unit. Roslyn's own
/// `Location.SourceSpan` is in UTF-16 code units (a .NET `string` is UTF-16),
/// so the oracle converts before ever writing this out — see
/// `Extractor.Utf8ByteOffsets` in `oracle/Extractor.cs`. Doing the conversion
/// oracle-side means every consumer of this schema, in Rust or otherwise,
/// gets a byte range it can slice `Field::type`'s file directly with no unit
/// bug to rediscover.
///
/// [`nudox_ir::entry::Symbol::span`]: nudox_ir::entry::Symbol
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    /// Absolute path to the source file, as Roslyn's `SyntaxTree.FilePath`
    /// reports it (the same path the oracle was pointed at under `--root`).
    pub file: String,
    /// Inclusive-start, exclusive-end UTF-8 byte offset into `file`.
    pub start: usize,
    pub end: usize,
    /// Zero-based line and UTF-8 byte column of the start.
    #[serde(default)]
    pub start_line: u32,
    #[serde(default)]
    pub start_column: u32,
    /// Zero-based line and UTF-8 byte column of the end.
    #[serde(default)]
    pub end_line: u32,
    #[serde(default)]
    pub end_column: u32,
}

/// A type declaration: class, struct, interface, enum, delegate, or record.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeDecl {
    /// The Roslyn documentation-comment id (`T:System.String`) — the join key
    /// for cref/doc-link resolution and future occurrence work.
    #[serde(default, deserialize_with = "null_as_default")]
    pub doc_id: String,
    /// Fully qualified metadata name with arity backticks kept
    /// (`System.Collections.Generic.List\`1`).
    pub qualified_name: String,
    pub simple_name: String,
    /// `CLASS` | `STRUCT` | `INTERFACE` | `ENUM` | `DELEGATE` | `RECORD` |
    /// `RECORD_STRUCT`.
    pub kind: String,
    /// The declaring namespace (empty string for the global namespace).
    #[serde(default, deserialize_with = "null_as_default")]
    pub namespace: String,
    /// The enclosing type's doc-id, for nested declarations.
    pub enclosing: Option<String>,
    #[serde(default)]
    pub modifiers: Vec<String>,
    #[serde(default)]
    pub type_params: Vec<TypeParam>,
    pub base_type: Option<TypeSig>,
    #[serde(default)]
    pub interfaces: Vec<TypeSig>,
    /// The underlying integral type of an `enum`.
    pub enum_underlying: Option<TypeSig>,
    /// The invoke signature of a `delegate`.
    pub delegate_sig: Option<DelegateSig>,
    #[serde(default)]
    pub attributes: Vec<Attr>,
    pub deprecated: Option<Deprecated>,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub forwarded: bool,
    pub doc: Option<String>,
    #[serde(default)]
    pub doc_inherited: bool,
    pub doc_links: Option<BTreeMap<String, String>>,
    /// The receiver type for a C# 14 extension block.
    pub extension_receiver: Option<TypeSig>,
    /// Where the type is declared. `#[serde(default)]` so hand-written
    /// fixtures that predate this field still parse — they simply lower
    /// with no location, same as a symbol the oracle itself could not place.
    #[serde(default)]
    pub location: Option<Location>,
    #[serde(default)]
    pub members: Members,
}

/// A delegate's invoke signature.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegateSig {
    #[serde(default)]
    pub params: Vec<Param>,
    #[serde(rename = "return")]
    pub return_type: Option<TypeSig>,
}

/// The members of a type declaration, split by member kind.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Members {
    #[serde(default)]
    pub fields: Vec<Field>,
    #[serde(default)]
    pub properties: Vec<Property>,
    #[serde(default)]
    pub events: Vec<Event>,
    #[serde(default)]
    pub constructors: Vec<Method>,
    #[serde(default)]
    pub methods: Vec<Method>,
    #[serde(default)]
    pub operators: Vec<Method>,
    #[serde(default)]
    pub conversions: Vec<Method>,
    #[serde(default)]
    pub indexers: Vec<Property>,
    /// Directly nested type declarations. The oracle emits **doc-ids**
    /// (`T:Ns.Outer+Inner`) when available, falling back to the metadata
    /// qualified name.
    #[serde(default)]
    pub nested: Vec<String>,
}

/// A declaration-site type parameter (`<T>` / `<in T>` / `<out T>`), with
/// its constraint clause.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeParam {
    pub name: String,
    /// `"none"` | `"in"` (contravariant) | `"out"` (covariant).
    #[serde(default, deserialize_with = "null_as_default")]
    pub variance: String,
    #[serde(default)]
    pub constraints: TypeParamConstraints,
}

/// The constraint clause of a type parameter (`where T : class, IFoo, new()`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeParamConstraints {
    /// `class` constraint.
    #[serde(default)]
    pub reference_type: bool,
    /// `struct` constraint.
    #[serde(default)]
    pub value_type: bool,
    /// `notnull` constraint.
    #[serde(default)]
    pub not_null: bool,
    /// `unmanaged` constraint.
    #[serde(default)]
    pub unmanaged: bool,
    /// `new()` constructor constraint.
    #[serde(default)]
    pub constructor: bool,
    /// `allows ref struct` (C# 13).
    #[serde(default)]
    pub allows_ref_like: bool,
    /// Explicit type constraints (`where T : Base`).
    #[serde(default)]
    pub types: Vec<TypeSig>,
}

/// A formal parameter.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Param {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: TypeSig,
    /// `"none"` | `"ref"` | `"out"` | `"in"` | `"refReadonly"`.
    #[serde(default, deserialize_with = "null_as_default")]
    pub ref_kind: String,
    #[serde(default)]
    pub is_params: bool,
    #[serde(default)]
    pub has_default: bool,
    pub default: Option<String>,
    #[serde(default)]
    pub scoped: bool,
    #[serde(default)]
    pub attributes: Vec<Attr>,
    #[serde(default)]
    pub location: Option<Location>,
}

/// A field declaration.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Field {
    pub name: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub doc_id: String,
    #[serde(rename = "type")]
    pub ty: TypeSig,
    #[serde(default, deserialize_with = "null_as_default")]
    pub accessibility: String,
    #[serde(default)]
    pub is_const: bool,
    /// The compile-time constant value's display text, for `const` fields /
    /// enum members.
    pub constant: Option<String>,
    #[serde(default)]
    pub is_readonly: bool,
    #[serde(default)]
    pub is_volatile: bool,
    #[serde(default)]
    pub is_required: bool,
    #[serde(default)]
    pub is_static: bool,
    #[serde(default)]
    pub attributes: Vec<Attr>,
    pub deprecated: Option<Deprecated>,
    #[serde(default)]
    pub hidden: bool,
    pub doc: Option<String>,
    #[serde(default)]
    pub doc_inherited: bool,
    pub doc_links: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub location: Option<Location>,
}

/// A property or indexer.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Property {
    pub name: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub doc_id: String,
    #[serde(rename = "type")]
    pub ty: TypeSig,
    #[serde(default, deserialize_with = "null_as_default")]
    pub accessibility: String,
    /// The getter's own accessibility, when it differs (asymmetric accessors).
    pub get_accessibility: Option<String>,
    /// The setter's own accessibility, when it differs.
    pub set_accessibility: Option<String>,
    /// `"none"` (read-only) | `"set"` | `"init"`.
    #[serde(default, deserialize_with = "null_as_default")]
    pub set_kind: String,
    #[serde(default)]
    pub is_required: bool,
    #[serde(default)]
    pub is_static: bool,
    #[serde(default)]
    pub is_indexer: bool,
    /// Indexer parameters (empty for ordinary properties).
    #[serde(default)]
    pub parameters: Vec<Param>,
    #[serde(default)]
    pub returns_by_ref: bool,
    #[serde(default)]
    pub returns_by_ref_readonly: bool,
    #[serde(default)]
    pub attributes: Vec<Attr>,
    pub deprecated: Option<Deprecated>,
    #[serde(default)]
    pub hidden: bool,
    pub doc: Option<String>,
    #[serde(default)]
    pub doc_inherited: bool,
    pub doc_links: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub location: Option<Location>,
}

/// An event declaration.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub name: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub doc_id: String,
    /// The event's delegate type.
    #[serde(rename = "type")]
    pub ty: TypeSig,
    #[serde(default, deserialize_with = "null_as_default")]
    pub accessibility: String,
    pub add_accessibility: Option<String>,
    pub remove_accessibility: Option<String>,
    #[serde(default)]
    pub is_static: bool,
    #[serde(default)]
    pub attributes: Vec<Attr>,
    pub deprecated: Option<Deprecated>,
    #[serde(default)]
    pub hidden: bool,
    pub doc: Option<String>,
    #[serde(default)]
    pub doc_inherited: bool,
    pub doc_links: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub location: Option<Location>,
}

/// A method, constructor, operator, or conversion.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Method {
    /// The metadata name (`.ctor`, `op_Addition`, `get_Item`, `M`).
    pub name: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub doc_id: String,
    /// Roslyn `MethodKind` (`Ordinary`, `Constructor`, `UserDefinedOperator`,
    /// `Conversion`, `ExplicitInterfaceImplementation`, …).
    #[serde(default, deserialize_with = "null_as_default")]
    pub method_kind: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub accessibility: String,
    #[serde(default)]
    pub is_static: bool,
    #[serde(default)]
    pub is_abstract: bool,
    #[serde(default)]
    pub is_virtual: bool,
    #[serde(default)]
    pub is_override: bool,
    #[serde(default)]
    pub is_sealed: bool,
    #[serde(default)]
    pub is_extern: bool,
    #[serde(default)]
    pub is_async: bool,
    #[serde(default)]
    pub is_iterator: bool,
    #[serde(default)]
    pub is_extension_method: bool,
    #[serde(default)]
    pub is_readonly: bool,
    #[serde(default)]
    pub type_params: Vec<TypeParam>,
    #[serde(default)]
    pub parameters: Vec<Param>,
    /// `None` for constructors and `void` returns is a
    /// `named{name:"System.Void"}`.
    pub return_type: Option<TypeSig>,
    #[serde(default)]
    pub returns_by_ref: bool,
    #[serde(default)]
    pub returns_by_ref_readonly: bool,
    /// The explicitly-implemented interface member (`IFoo.Bar`), if any.
    pub explicit_interface: Option<String>,
    /// `"none"` | `"implicit"` | `"explicit"` | `"checked"` (conversions/ops).
    /// Oracle emits JSON `null` for non-operators; treat as default
    /// (empty/`none`).
    #[serde(default, deserialize_with = "null_as_default")]
    pub operator_kind: String,
    #[serde(default)]
    pub attributes: Vec<Attr>,
    pub deprecated: Option<Deprecated>,
    #[serde(default)]
    pub hidden: bool,
    pub doc: Option<String>,
    #[serde(default)]
    pub doc_inherited: bool,
    pub doc_links: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub location: Option<Location>,
}

/// An attribute use (`[Foo(1, Name = "x")]`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attr {
    /// The attribute class's metadata name (`System.ObsoleteAttribute`).
    #[serde(rename = "type")]
    pub ty: String,
    /// Positional argument display strings.
    #[serde(default)]
    pub args: Vec<String>,
    /// Named-argument display strings, keyed by property name.
    #[serde(default)]
    pub named: BTreeMap<String, String>,
}

/// Deprecation (`[Obsolete]`) marker.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Deprecated {
    pub message: Option<String>,
    #[serde(default)]
    pub is_error: bool,
}

/// A recursive structural type use (mirrors CSHARP-PLAN §2.4). Internally
/// tagged by `"kind"`; every reference-type node carries 3-state
/// [`nullable`](TypeSig) annotation.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TypeSig {
    /// A named class / struct / interface / enum / delegate use.
    Named {
        /// Metadata FQN, arity backticks kept.
        name: String,
        #[serde(default)]
        args: Vec<TypeSig>,
        /// The generic owner for `Outer<T>.Inner` uses.
        owner: Option<Box<TypeSig>>,
        /// `"none"` (oblivious) | `"annotated"` (`T?`) | `"notAnnotated"`.
        #[serde(default)]
        nullable: String,
        /// Roslyn `TypeKind`
        /// (`Class`/`Struct`/`Interface`/`Enum`/`Delegate`/…).
        #[serde(default)]
        type_kind: String,
    },
    /// A generic type-parameter reference.
    TypeParam {
        name: String,
        /// `"type"` | `"method"`.
        #[serde(default)]
        owner_kind: String,
        #[serde(default)]
        nullable: String,
    },
    /// An array (`T[]`, `T[,]`, jagged = nested).
    Array {
        element: Box<TypeSig>,
        /// The array rank (1 = SZ vector; >1 = multidimensional).
        #[serde(default = "one")]
        rank: u32,
        #[serde(default)]
        nullable: String,
    },
    /// An unmanaged pointer (`T*`).
    Pointer { pointee: Box<TypeSig> },
    /// A function pointer (`delegate*<...>`).
    FuncPtr {
        #[serde(default)]
        params: Vec<TypeSig>,
        #[serde(rename = "return")]
        return_type: Option<Box<TypeSig>>,
        #[serde(default)]
        call_conv: String,
        #[serde(default)]
        unmanaged_call_convs: Vec<String>,
    },
    /// A value tuple (`(int, string)` / `(int x, string y)`).
    Tuple {
        #[serde(default)]
        elements: Vec<TupleElement>,
        #[serde(default)]
        nullable: String,
    },
    /// `dynamic`.
    Dynamic {},
    /// `Nullable<T>` — a nullable *value* type.
    NullableValue { inner: Box<TypeSig> },
    /// An unresolvable type; `name` is the source/metadata text.
    Error { name: String },
}

/// One element of a value tuple.
#[derive(Debug, Clone, Deserialize)]
pub struct TupleElement {
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub ty: TypeSig,
}

fn one() -> u32 {
    1
}

impl TypeSig {
    /// The metadata name for named/error uses, when meaningful.
    pub fn named_name(&self) -> Option<&str> {
        match self {
            TypeSig::Named { name, .. } | TypeSig::Error { name } => Some(name),
            _ => None,
        }
    }
}

/// The 3-state nullability of a reference-type node (never collapse oblivious
/// and non-null — pitfall #6/#10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nullability {
    /// No annotation context (oblivious).
    Oblivious,
    /// `T?` — annotated nullable.
    Annotated,
    /// `T` under an enabled nullable context — non-null.
    NotAnnotated,
}

impl Nullability {
    /// Interpret the oracle's `nullable` string.
    pub fn parse(s: &str) -> Self {
        match s {
            "annotated" => Nullability::Annotated,
            "notAnnotated" => Nullability::NotAnnotated,
            _ => Nullability::Oblivious,
        }
    }
}
