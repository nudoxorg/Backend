//! Roslyn schema records, typed faults, and deterministic tool probing.
//! Every decode rejection retains the original transcript bytes.
//! Finished collections use boxed slices to bound retained ownership.

use std::env;
use std::path::PathBuf;
use std::process::Command;

use serde::Deserialize;

/// Roslyn's three meaningful reference-type nullability states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Nullability {
    /// Explicit `?` annotation.
    Annotated,
    /// Explicitly non-null in an enabled context.
    NotAnnotated,
    /// No nullable context annotation was available (the oracle's `none` cell).
    #[serde(rename = "none")]
    Oblivious,
}

/// One labelled tuple element: the producer emits `Item1`-style positional
/// labels as an explicit `null` name rather than omitting the key.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TupleElement {
    /// Author-written label, or `None` when positional.
    pub name: Option<String>,
    /// The element type.
    pub r#type: Type,
}

/// A closed C# type node retained by the decoder.
///
/// The producer writes the enum-internal keys `type_kind`, `owner_kind`,
/// `call_conv`, and `unmanaged_call_convs` in snake_case while every
/// surrounding struct is camelCase, so this enum deliberately carries no
/// field rename.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum Type {
    /// A (possibly generic) named type use.
    #[serde(rename = "named")]
    Named {
        /// Metadata fully-qualified name with arity backticks kept.
        name: String,
        /// Type arguments in source order.
        args: Box<[Type]>,
        /// The containing generic type, when nested in one.
        owner: Option<Box<Type>>,
        /// The meaningful nullability cell.
        nullable: Nullability,
        /// Roslyn's `TypeKind` spelling (`Class`, `Struct`, ...).
        type_kind: String,
    },
    /// An array type of any rank.
    #[serde(rename = "array")]
    Array {
        /// The element type.
        element: Box<Type>,
        /// Array rank (1 for `T[]`).
        rank: u32,
        /// The meaningful nullability cell.
        nullable: Nullability,
    },
    /// A raw pointer's pointee (`T*`).
    #[serde(rename = "pointer")]
    Pointer {
        /// The pointed-at type.
        pointee: Box<Type>,
    },
    /// `Nullable<T>` collapsed to its own node.
    #[serde(rename = "nullableValue")]
    NullableValue {
        /// The wrapped value type.
        inner: Box<Type>,
    },
    /// A tuple type with labelled or positional elements.
    #[serde(rename = "tuple")]
    Tuple {
        /// Elements in positional order.
        elements: Box<[TupleElement]>,
        /// The meaningful nullability cell.
        nullable: Nullability,
    },
    /// A function pointer signature.
    #[serde(rename = "funcPtr")]
    FunctionPointer {
        /// Parameter types in declaration order.
        params: Box<[Type]>,
        /// The return type, or `None` for `void`.
        #[serde(rename = "return")]
        return_type: Option<Box<Type>>,
        /// `managed` or `unmanaged`.
        call_conv: String,
        /// Bare unmanaged convention names (`Cdecl`, `StdCall`, ...).
        unmanaged_call_convs: Box<[String]>,
    },
    /// A generic parameter use.
    #[serde(rename = "typeParam")]
    TypeParameter {
        /// The parameter name.
        name: String,
        /// `method` or `type` owner.
        owner_kind: String,
        /// The meaningful nullability cell.
        nullable: Nullability,
    },
    /// A C# `dynamic` use.
    #[serde(rename = "dynamic")]
    Dynamic,
    /// A type Roslyn could not bind.
    #[serde(rename = "error")]
    Error {
        /// The best available source spelling.
        name: String,
    },
}

/// One extracted declaration. Every producer field is retained as a typed
/// record; nothing is discarded through an untyped `Value` tree.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct TypeDeclaration {
    #[serde(rename = "docId")]
    /// Roslyn's documentation-comment id.
    pub doc_id: String,
    /// The metadata fully-qualified name.
    pub qualified_name: String,
    /// The type's own name without namespace or arity.
    pub simple_name: String,
    /// The declaration kind token (`CLASS`, ...).
    pub kind: String,
    /// The containing namespace's dotted name.
    pub namespace: String,
    /// The enclosing type's doc-id, when nested.
    pub enclosing: Option<String>,
    /// Source-written modifiers, access modifier first.
    pub modifiers: Box<[String]>,
    /// Declaration-site generic parameters.
    pub type_params: Box<[GenericParameter]>,
    #[serde(rename = "baseType")]
    /// The base type, when one exists.
    pub base_type: Option<Type>,
    /// Directly implemented interfaces.
    pub interfaces: Box<[Type]>,
    #[serde(rename = "enumUnderlying")]
    /// The enum's underlying type, for enum declarations only.
    pub enum_underlying: Option<Type>,
    #[serde(rename = "delegateSig")]
    /// The delegate invocation signature, for delegate declarations only.
    pub delegate_sig: Option<DelegateSignature>,
    /// Every applied attribute.
    pub attributes: Box<[Attribute]>,
    /// The `[Obsolete]` fact, when present.
    pub deprecated: Option<Deprecation>,
    /// Whether `[EditorBrowsable(Never)]` is present.
    pub hidden: bool,
    /// Whether the type arrives through a type forwarder.
    pub forwarded: bool,
    /// Documentation text, when the producer reports one.
    pub doc: Option<String>,
    #[serde(rename = "docInherited")]
    /// Whether the doc text came from an ancestor.
    pub doc_inherited: bool,
    #[serde(rename = "docLinks")]
    /// Every `cref` in the comment, as an identity map.
    pub doc_links: Option<std::collections::BTreeMap<String, String>>,
    /// The UTF-8-byte span, when the symbol has source.
    pub location: Option<Location>,
    #[serde(rename = "extensionReceiver")]
    /// The C# 14 extension-block receiver, when present.
    pub extension_receiver: Option<Type>,
    pub members: Members,
}

/// A declaration-site generic parameter, including C# variance and constraints.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct GenericParameter {
    /// The assembly's simple name.
    pub name: String,
    pub variance: String,
    pub constraints: Constraints,
}

/// Generic parameter constraints.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct Constraints {
    #[serde(rename = "referenceType")]
    pub reference_type: bool,
    #[serde(rename = "valueType")]
    pub value_type: bool,
    #[serde(rename = "notNull")]
    pub not_null: bool,
    /// Whether an `unmanaged` constraint is present.
    pub unmanaged: bool,
    /// Whether a `new()` constraint is present.
    pub constructor: bool,
    #[serde(rename = "allowsRefLike")]
    /// Whether C# 13's `allows ref struct` is present.
    pub allows_ref_like: bool,
    pub types: Box<[Type]>,
}

/// A delegate invocation signature (`delegateSig`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct DelegateSignature {
    /// Delegate parameter facts in declaration order.
    pub params: Box<[Parameter]>,
    #[serde(rename = "return")]
    /// The delegate return type.
    pub return_type: Type,
}

/// An applied attribute with its constructor and named arguments.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct Attribute {
    /// The attribute class's metadata fully-qualified name.
    pub r#type: String,
    /// Constructor arguments as C# source spellings.
    pub args: Box<[String]>,
    /// Named arguments as C# source spellings.
    pub named: std::collections::BTreeMap<String, String>,
}

/// An `[Obsolete]` fact.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct Deprecation {
    /// The diagnostic message, when one was written.
    pub message: Option<String>,
    /// Whether using the symbol is an error.
    pub is_error: bool,
}

/// A UTF-8-byte source span with 0-based line/column facts.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct Location {
    /// The source file path as the producer saw it.
    pub file: String,
    /// Inclusive UTF-8 byte start.
    pub start: u32,
    /// Exclusive UTF-8 byte end.
    pub end: u32,
    /// Zero-based start line.
    pub start_line: u32,
    /// Zero-based start column.
    pub start_column: u32,
    /// Zero-based end line.
    pub end_line: u32,
    /// Zero-based end column.
    pub end_column: u32,
}

/// Member groups emitted by Roslyn.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct Members {
    /// Declared fields.
    pub fields: Box<[Field]>,
    /// Declared non-indexer properties.
    pub properties: Box<[Property]>,
    /// Declared events.
    pub events: Box<[Event]>,
    /// Declared constructors.
    pub constructors: Box<[Method]>,
    /// Declared ordinary methods.
    pub methods: Box<[Method]>,
    /// Declared user-defined operators.
    pub operators: Box<[Method]>,
    /// Declared implicit and explicit conversions.
    pub conversions: Box<[Method]>,
    /// Declared indexers.
    pub indexers: Box<[Property]>,
    /// Doc-ids of emitted nested types.
    pub nested: Box<[String]>,
}

/// A field fact.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub name: String,
    #[serde(rename = "docId")]
    pub doc_id: String,
    pub r#type: Type,
    /// The declared accessibility token.
    pub accessibility: String,
    /// Whether the field is `const`.
    pub is_const: bool,
    /// The constant value's C# spelling, when one exists.
    pub constant: Option<String>,
    /// Whether the member is `readonly`.
    pub is_readonly: bool,
    /// Whether the field is `volatile`.
    pub is_volatile: bool,
    /// Whether the member is `required`.
    pub is_required: bool,
    /// Whether the member is `static`.
    pub is_static: bool,
    pub attributes: Box<[Attribute]>,
    pub deprecated: Option<Deprecation>,
    pub hidden: bool,
    pub doc: Option<String>,
    #[serde(rename = "docInherited")]
    pub doc_inherited: bool,
    #[serde(rename = "docLinks")]
    pub doc_links: Option<std::collections::BTreeMap<String, String>>,
    pub location: Option<Location>,
}

/// A property or indexer fact.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct Property {
    pub name: String,
    #[serde(rename = "docId")]
    pub doc_id: String,
    pub r#type: Type,
    pub accessibility: String,
    #[serde(rename = "getAccessibility")]
    /// The getter's accessibility, when one exists.
    pub get_accessibility: Option<String>,
    #[serde(rename = "setAccessibility")]
    /// The setter's accessibility, when one exists.
    pub set_accessibility: Option<String>,
    #[serde(rename = "setKind")]
    /// `none`, `set`, or `init`.
    pub set_kind: String,
    pub is_required: bool,
    pub is_static: bool,
    /// Whether this property is an indexer (`this[...]`).
    pub is_indexer: bool,
    /// Parameter facts in declaration order.
    pub parameters: Box<[Parameter]>,
    /// Whether the member returns by reference.
    pub returns_by_ref: bool,
    /// Whether the member returns by read-only reference.
    pub returns_by_ref_readonly: bool,
    pub attributes: Box<[Attribute]>,
    pub deprecated: Option<Deprecation>,
    pub hidden: bool,
    pub doc: Option<String>,
    #[serde(rename = "docInherited")]
    pub doc_inherited: bool,
    #[serde(rename = "docLinks")]
    pub doc_links: Option<std::collections::BTreeMap<String, String>>,
    pub location: Option<Location>,
}

/// An event fact.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub name: String,
    #[serde(rename = "docId")]
    pub doc_id: String,
    pub r#type: Type,
    pub accessibility: String,
    #[serde(rename = "addAccessibility")]
    /// The add accessor's accessibility, when one exists.
    pub add_accessibility: Option<String>,
    #[serde(rename = "removeAccessibility")]
    /// The remove accessor's accessibility, when one exists.
    pub remove_accessibility: Option<String>,
    pub is_static: bool,
    pub attributes: Box<[Attribute]>,
    pub deprecated: Option<Deprecation>,
    pub hidden: bool,
    pub doc: Option<String>,
    #[serde(rename = "docInherited")]
    pub doc_inherited: bool,
    #[serde(rename = "docLinks")]
    pub doc_links: Option<std::collections::BTreeMap<String, String>>,
    pub location: Option<Location>,
}

/// A method, constructor, operator, or conversion fact.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct Method {
    pub name: String,
    #[serde(rename = "docId")]
    pub doc_id: String,
    #[serde(rename = "methodKind")]
    /// Roslyn's `MethodKind` spelling.
    pub method_kind: String,
    pub accessibility: String,
    pub is_static: bool,
    /// Whether the method is `abstract`.
    pub is_abstract: bool,
    /// Whether the method is `virtual`.
    pub is_virtual: bool,
    /// Whether the method is an `override`.
    pub is_override: bool,
    /// Whether the method is `sealed`.
    pub is_sealed: bool,
    /// Whether the method is `extern`.
    pub is_extern: bool,
    /// Whether the method is `async`.
    pub is_async: bool,
    /// Whether the method body contains a `yield`.
    pub is_iterator: bool,
    /// Whether the method is an extension method.
    pub is_extension_method: bool,
    pub is_readonly: bool,
    pub type_params: Box<[GenericParameter]>,
    pub parameters: Box<[Parameter]>,
    /// Exception types named by `<exception cref>` documentation.
    #[serde(default)]
    pub throws: Box<[Type]>,
    #[serde(rename = "returnType")]
    pub return_type: Option<Type>,
    pub returns_by_ref: bool,
    pub returns_by_ref_readonly: bool,
    /// The implemented interface's qualified name, when explicit.
    pub explicit_interface: Option<String>,
    #[serde(rename = "operatorKind")]
    /// `none`, `implicit`, `explicit`, or `checked`.
    pub operator_kind: String,
    pub attributes: Box<[Attribute]>,
    pub deprecated: Option<Deprecation>,
    pub hidden: bool,
    pub doc: Option<String>,
    #[serde(rename = "docInherited")]
    pub doc_inherited: bool,
    #[serde(rename = "docLinks")]
    pub doc_links: Option<std::collections::BTreeMap<String, String>>,
    pub location: Option<Location>,
}

/// A parameter fact.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct Parameter {
    pub name: String,
    pub r#type: Type,
    #[serde(rename = "refKind")]
    /// `none`, `ref`, `out`, `in`, or `refReadonly`.
    pub ref_kind: String,
    /// Whether the parameter is `params`.
    pub is_params: bool,
    /// Whether the parameter declares a default value.
    pub has_default: bool,
    /// The default value's C# spelling, when one exists.
    pub default: Option<String>,
    /// Whether the parameter carries a `scoped` modifier.
    pub scoped: bool,
    pub attributes: Box<[Attribute]>,
    pub location: Option<Location>,
}

/// The extracted source assembly header.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct Assembly {
    pub name: String,
    /// The assembly identity version.
    pub version: String,
    /// The target framework moniker.
    pub tfm: String,
    /// Forwarded type names (empty for source extractions).
    pub forwarded_types: Box<[String]>,
    /// `InternalsVisibleTo` targets.
    pub ivt: Box<[String]>,
}

/// One namespace emitted by the extraction.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct NamespaceInfo {
    pub name: String,
    pub doc: Option<String>,
}

/// One invocation resolved to a declaration id inside the source assembly.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct Reference {
    /// The invoking method's declaration id.
    pub owner: String,
    /// The invoked method's declaration id.
    pub target: String,
    pub file: String,
    pub start: u32,
    pub end: u32,
}

/// Extraction health counters.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct Diagnostics {
    /// How many type signatures degraded to error nodes.
    pub error_type_count: u64,
    /// How many compilation errors the producer counted.
    pub error_count: u64,
    /// Whether configured source generators could be applied.
    pub generator_support: String,
}

/// The complete Roslyn document.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct CSharpOutput {
    /// The producer schema cell; only 1 is accepted.
    pub format: u32,
    /// The runtime version that produced the document.
    pub dotnet_version: String,
    /// The Roslyn version that produced the document.
    pub roslyn: String,
    /// The extraction mode (`source`).
    pub mode: String,
    /// The extracted source assembly header.
    pub assembly: Assembly,
    /// Every namespace containing an emitted type.
    pub namespaces: Box<[NamespaceInfo]>,
    /// Every emitted type declaration.
    pub types: Box<[TypeDeclaration]>,
    /// Source-resolved invocations inside the assembly.
    pub references: Box<[Reference]>,
    /// Extraction health counters.
    pub diagnostics: Diagnostics,
}

/// A tool was unavailable before source work began.
#[derive(Debug, thiserror::Error)]
#[error("{language} tooling unavailable: {tool:?}: {cause}")]
pub struct ToolingUnavailable {
    /// Language name.
    pub language: &'static str,
    /// Requested executable.
    pub tool: PathBuf,
    /// Stable OS/process cause.
    pub cause: ToolingCause,
}

#[derive(Debug, thiserror::Error)]
pub enum ToolingCause {
    /// The tool ran and exited unsuccessfully.
    #[error("process exited with {status}")]
    ExitStatus {
        /// The child's exit status.
        status: std::process::ExitStatus,
    },
    /// The tool could not be started.
    #[error("process could not be started: {source}")]
    Io {
        /// The operating-system cause.
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeCause {
    /// Structured JSON decoding failed.
    #[error("JSON decoding failed: {0}")]
    Json(
        /// The underlying JSON error.
        #[source]
        serde_json::Error,
    ),
    /// The `format` field is missing.
    #[error("the format field is missing")]
    MissingFormat,
}

/// A source-preserving protocol rejection.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    /// JSON had an unknown field.
    #[error("unknown field `{field}` in C# transcript")]
    UnknownField {
        /// The rejected field's name.
        field: String,
        /// The exact transcript bytes.
        source_payload: Box<[u8]>,
    },
    /// A type discriminator was not in the closed schema.
    #[error("unknown type kind `{kind}` in C# transcript")]
    UnknownTypeKind {
        /// The rejected discriminator.
        kind: String,
        /// The exact transcript bytes.
        source_payload: Box<[u8]>,
    },
    /// The producer schema cell is not accepted.
    #[error("stale C# transcript schema `{found}`")]
    StaleSchema {
        /// The rejected schema cell, untruncated.
        found: u64,
        /// The exact transcript bytes.
        source_payload: Box<[u8]>,
    },
    /// The record ended before a complete JSON value.
    #[error("truncated C# transcript")]
    TruncatedRecord {
        /// The exact transcript bytes.
        source_payload: Box<[u8]>,
    },
    /// A structurally valid JSON document failed typed decoding.
    #[error("invalid C# transcript: {cause}")]
    Invalid {
        /// The typed decode cause.
        cause: DecodeCause,
        /// The exact transcript bytes.
        source_payload: Box<[u8]>,
    },
}

fn owned(input: &[u8]) -> Box<[u8]> {
    input.into()
}

/// Decode one complete Roslyn transcript without running a compiler.
pub fn decode(input: &[u8]) -> Result<CSharpOutput, DecodeError> {
    let value: serde_json::Value = serde_json::from_slice(input).map_err(|error| {
        if error.is_eof() {
            DecodeError::TruncatedRecord {
                source_payload: owned(input),
            }
        } else {
            DecodeError::Invalid {
                cause: DecodeCause::Json(error),
                source_payload: owned(input),
            }
        }
    })?;
    // The cell stays a full u64 through the staleness comparison: a
    // truncating `as u32` would fold 2^32+1 onto 1 and admit a future
    // schema as the current one.
    let format = value
        .get("format")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| DecodeError::Invalid {
            cause: DecodeCause::MissingFormat,
            source_payload: owned(input),
        })?;
    if format != 1 {
        return Err(DecodeError::StaleSchema {
            found: format,
            source_payload: owned(input),
        });
    }
    if let Some(kind) = find_unknown_kind(&value) {
        return Err(DecodeError::UnknownTypeKind {
            kind,
            source_payload: owned(input),
        });
    }
    serde_json::from_value(value).map_err(|error| {
        let text = error.to_string();
        if text.contains("unknown field") {
            let mut quoted = text.split('`');
            quoted.next();
            match quoted.next() {
                Some(field) => DecodeError::UnknownField {
                    field: field.to_owned(),
                    source_payload: owned(input),
                },
                None => DecodeError::Invalid {
                    cause: DecodeCause::Json(error),
                    source_payload: owned(input),
                },
            }
        } else {
            DecodeError::Invalid {
                cause: DecodeCause::Json(error),
                source_payload: owned(input),
            }
        }
    })
}

fn find_unknown_kind(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(kind) = map.get("kind").and_then(serde_json::Value::as_str) {
                let declaration = matches!(
                    kind,
                    "CLASS"
                        | "STRUCT"
                        | "INTERFACE"
                        | "ENUM"
                        | "DELEGATE"
                        | "RECORD"
                        | "RECORD_STRUCT"
                );
                if !declaration
                    && !matches!(
                        kind,
                        "named"
                            | "array"
                            | "pointer"
                            | "nullableValue"
                            | "tuple"
                            | "funcPtr"
                            | "typeParam"
                            | "dynamic"
                            | "error"
                    )
                {
                    return Some(kind.to_owned());
                }
            }
            map.values().find_map(find_unknown_kind)
        }
        serde_json::Value::Array(items) => items.iter().find_map(find_unknown_kind),
        _ => None,
    }
}

/// Probe the configured dotnet executable before any source or transcript work.
pub fn probe_dotnet() -> Result<PathBuf, ToolingUnavailable> {
    let configured =
        env::var_os("COMPILER_CSHARP_COMPILER").or_else(|| env::var_os("NUDOX_CSHARP_DOTNET"));
    let tool = match configured {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from("dotnet"),
    };
    probe_dotnet_path(tool)
}

/// Probe an explicit executable, useful for deterministic unavailable tests.
pub fn probe_dotnet_path(tool: PathBuf) -> Result<PathBuf, ToolingUnavailable> {
    let result = Command::new(&tool).arg("--version").output();
    match result {
        Ok(output) if output.status.success() => Ok(tool),
        Ok(output) => Err(ToolingUnavailable {
            language: "csharp",
            tool,
            cause: ToolingCause::ExitStatus {
                status: output.status,
            },
        }),
        Err(error) => Err(ToolingUnavailable {
            language: "csharp",
            tool,
            cause: ToolingCause::Io { source: error },
        }),
    }
}
