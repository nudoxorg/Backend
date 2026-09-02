//! Javadoc schema records, source-preserving faults, and UTF-16 conversion.
//! The adapter never substitutes source scanning for doclet facts.
//! Tool probing is performed before extraction work.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;
use std::process::Command;

/// A Java source span expressed in UTF-16 code units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct Utf16Span {
    /// Inclusive UTF-16 start.
    pub start: usize,
    /// Exclusive UTF-16 end.
    pub end: usize,
}

/// One applied annotation; element values are kept as JSON values because
/// their shape recurses through constants, arrays, nested annotations, enum
/// constants, and class literals.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Annotation {
    /// The annotation type's fully-qualified name.
    pub r#type: String,
    /// Explicit element values by simple name.
    pub values: BTreeMap<String, serde_json::Value>,
}

/// A compile-time constant cell written by the doclet.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum Constant {
    /// A `String` constant.
    #[serde(rename = "string")]
    String {
        /// The constant text.
        value: String,
    },
    /// A `boolean` constant.
    #[serde(rename = "boolean")]
    Boolean {
        /// The constant value.
        value: bool,
    },
    /// A `char` constant, spelled as its one-character string.
    #[serde(rename = "char")]
    Char {
        /// The constant character.
        value: String,
    },
    /// A finite floating-point constant.
    #[serde(rename = "double")]
    Double {
        /// The constant value.
        value: f64,
    },
    /// An integral constant widened to `long`.
    #[serde(rename = "int")]
    Int {
        /// The constant value.
        value: i64,
    },
    /// Anything unrepresentable in the other cells.
    #[serde(rename = "other")]
    Other {
        /// The best available spelling.
        repr: String,
    },
}

/// A closed Java type mirror sufficient for signatures and overload identity.
///
/// The doclet emits `annotations` only on the mirror kinds produced from a
/// `TypeMirror`'s own annotation mirrors (primitive, declared, array,
/// typevar); the remaining kinds carry no annotations key.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum TypeMirror {
    /// A primitive type.
    #[serde(rename = "primitive")]
    Primitive {
        /// The primitive keyword (`int`, `boolean`, ...).
        name: String,
        /// Type-use annotations.
        annotations: Box<[Annotation]>,
    },
    /// The `void` pseudo-type.
    #[serde(rename = "void")]
    Void,
    /// A class or interface use.
    #[serde(rename = "declared")]
    Declared {
        /// The fully-qualified name.
        name: String,
        /// Type arguments in source order.
        args: Box<[TypeMirror]>,
        /// The containing generic type, when nested in one.
        owner: Option<Box<TypeMirror>>,
        /// Type-use annotations.
        annotations: Box<[Annotation]>,
    },
    /// An array type.
    #[serde(rename = "array")]
    Array {
        /// The component type.
        component: Box<TypeMirror>,
        /// Type-use annotations.
        annotations: Box<[Annotation]>,
    },
    /// A type-variable use.
    #[serde(rename = "typevar")]
    TypeVariable {
        /// The type parameter's simple name.
        name: String,
        /// Type-use annotations.
        annotations: Box<[Annotation]>,
    },
    /// A wildcard (`?`, `? extends`, or `? super`).
    #[serde(rename = "wildcard")]
    Wildcard {
        /// The upper bound, when present.
        extends: Option<Box<TypeMirror>>,
        /// The lower bound, when present.
        #[serde(rename = "super")]
        super_bound: Option<Box<TypeMirror>>,
    },
    /// An intersection bound of a type parameter.
    #[serde(rename = "intersection")]
    Intersection {
        /// The intersected bounds.
        bounds: Box<[TypeMirror]>,
    },
    /// A multi-catch union type.
    #[serde(rename = "union")]
    Union {
        /// The alternatives.
        alternatives: Box<[TypeMirror]>,
    },
    /// A type javac could not resolve.
    #[serde(rename = "error")]
    Error {
        /// The best available spelling.
        name: String,
    },
    /// The `NONE` pseudo-type (e.g. no superclass).
    #[serde(rename = "none")]
    None,
    /// The `NULL` pseudo-type.
    #[serde(rename = "null")]
    Null,
    /// Any mirror kind outside the closed cases.
    #[serde(rename = "other")]
    Other {
        /// The mirror's `toString` spelling.
        repr: String,
    },
}

/// A declaration-site type parameter with its bounds.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JavaTypeParameter {
    /// The parameter's simple name.
    pub name: String,
    /// The declared bounds (an intersection node when there are several).
    pub bounds: Box<[TypeMirror]>,
    /// Annotations on the declaration.
    pub annotations: Box<[Annotation]>,
}

/// A record component fact.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordComponent {
    /// The element's simple name.
    pub name: String,
    /// The element's type.
    pub r#type: TypeMirror,
    /// The component accessor's simple name.
    pub accessor: Option<String>,
    /// Type-use annotations.
    pub annotations: Box<[Annotation]>,
    /// The raw doc comment, when present.
    pub doc: Option<String>,
    /// The doc flavor, when the JDK reports one.
    pub doc_kind: Option<String>,
}

/// A field fact.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JavaField {
    /// The element's simple name.
    pub name: String,
    /// The element's type.
    pub r#type: TypeMirror,
    /// The declared modifiers in model order.
    pub modifiers: Box<[String]>,
    /// The compile-time constant, when one exists.
    pub constant: Option<Constant>,
    /// Type-use annotations.
    pub annotations: Box<[Annotation]>,
    /// Whether the element is deprecated.
    pub deprecated: bool,
    /// The element origin token (`EXPLICIT`, ...).
    pub origin: String,
    /// The raw doc comment, when present.
    pub doc: Option<String>,
    /// The doc flavor, when the JDK reports one.
    pub doc_kind: Option<String>,
    /// The source position, when resolvable.
    pub position: Option<Position>,
}

/// An enum-constant fact, retaining the raw declaration source slice.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnumConstant {
    /// The element's simple name.
    pub name: String,
    /// Type-use annotations.
    pub annotations: Box<[Annotation]>,
    /// Whether the element is deprecated.
    pub deprecated: bool,
    /// The raw doc comment, when present.
    pub doc: Option<String>,
    /// The doc flavor, when the JDK reports one.
    pub doc_kind: Option<String>,
    /// The raw declaration source slice, when available.
    pub source: Option<String>,
    /// The source position, when resolvable.
    pub position: Option<Position>,
}

/// A method or constructor fact, including every thrown type.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JavaExecutable {
    /// The element's simple name.
    pub name: String,
    /// The declared modifiers in model order.
    pub modifiers: Box<[String]>,
    /// Declaration-site type parameters.
    pub type_params: Box<[JavaTypeParameter]>,
    /// Parameter facts in declaration order.
    pub params: Box<[JavaParameter]>,
    /// The return type, or `None` for constructors.
    #[serde(rename = "return")]
    pub return_type: Option<TypeMirror>,
    /// Every declared thrown type, in declaration order.
    pub thrown: Box<[TypeMirror]>,
    /// Whether the executable is variable-arity.
    pub varargs: bool,
    #[serde(rename = "default")]
    pub default_method: bool,
    /// The receiver type, when present.
    pub receiver: Option<TypeMirror>,
    /// The annotation-interface default value, when declared.
    pub annotation_default: Option<serde_json::Value>,
    /// Type-use annotations.
    pub annotations: Box<[Annotation]>,
    /// Whether the element is deprecated.
    pub deprecated: bool,
    /// The element origin token (`EXPLICIT`, ...).
    pub origin: String,
    /// The raw doc comment, when present.
    pub doc: Option<String>,
    /// The doc flavor, when the JDK reports one.
    pub doc_kind: Option<String>,
    /// The source position, when resolvable.
    pub position: Option<Position>,
}

/// A method parameter.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JavaParameter {
    /// The element's simple name.
    pub name: String,
    /// The element's type.
    pub r#type: TypeMirror,
    /// Type-use annotations.
    pub annotations: Box<[Annotation]>,
}

/// A typed Java declaration retained from the doclet, with every producer
/// field represented.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JavaType {
    /// The fully-qualified name.
    pub qualified_name: String,
    /// The simple name.
    pub simple_name: String,
    /// The rejected discriminator.
    pub kind: String,
    /// The containing package's fully-qualified name.
    pub package: String,
    /// The containing module, when named.
    pub module: Option<String>,
    /// The enclosing type's qualified name, when nested.
    pub enclosing: Option<String>,
    /// The nesting kind token (`TOP_LEVEL`, ...).
    pub nesting: String,
    /// The declared modifiers in model order.
    pub modifiers: Box<[String]>,
    /// Declaration-site type parameters.
    pub type_params: Box<[JavaTypeParameter]>,
    /// The superclass, when one exists.
    pub superclass: Option<TypeMirror>,
    /// Directly implemented interfaces.
    pub interfaces: Box<[TypeMirror]>,
    /// Permitted subclasses for sealed declarations.
    pub permits: Box<[TypeMirror]>,
    /// Record components, in declaration order.
    pub record_components: Box<[RecordComponent]>,
    /// Type-use annotations.
    pub annotations: Box<[Annotation]>,
    /// Whether the element is deprecated.
    pub deprecated: bool,
    /// Declared fields.
    pub fields: Box<[JavaField]>,
    /// Declared enum constants, in declaration order.
    pub enum_constants: Box<[EnumConstant]>,
    /// Declared constructors.
    pub constructors: Box<[JavaExecutable]>,
    /// Declared methods.
    pub methods: Box<[JavaExecutable]>,
    /// Qualified names of emitted nested types.
    pub nested: Box<[String]>,
    /// The raw doc comment, when present.
    pub doc: Option<String>,
    /// The doc flavor, when the JDK reports one.
    pub doc_kind: Option<String>,
    /// The source position, when resolvable.
    pub position: Option<Position>,
}

/// A source position from javadoc.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Position {
    /// The source file path as the producer saw it.
    pub file: String,
    /// The one-based line, when resolvable.
    pub line: Option<u32>,
}

/// Complete doclet output.
///
/// Modules and packages are retained as JSON values: their directive and
/// annotation trees share the typed fragments above only in part, and the
/// protocol's identity-carrying facts (types, references) are fully typed.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JavaOutput {
    /// The producer schema cell; only 1 is accepted.
    pub format: u32,
    /// The JDK version that produced the document.
    pub java_version: String,
    /// Named module facts.
    pub modules: Box<[serde_json::Value]>,
    /// Package facts.
    pub packages: Box<[serde_json::Value]>,
    /// Every emitted type declaration.
    pub types: Box<[JavaType]>,
    /// Source-resolved calls with UTF-16 coordinates.
    pub references: Box<[Reference]>,
}

/// A source-resolved call, retaining the helper's UTF-16 coordinates.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reference {
    /// The calling executable's id.
    pub owner: String,
    /// The called executable's id.
    pub target: String,
    /// The source file path as the producer saw it.
    pub file: String,
    /// The rejected UTF-16 start.
    pub start: usize,
    /// The rejected UTF-16 end.
    pub end: usize,
}

/// Tooling was unavailable before extraction started.
#[derive(Debug, thiserror::Error)]
#[error("{language} tooling unavailable: {tool:?}: {cause}")]
pub struct ToolingUnavailable {
    /// Language.
    pub language: &'static str,
    /// Executable.
    pub tool: PathBuf,
    /// Process cause.
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

/// A source-preserving decoder rejection.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    /// Unknown JSON field.
    #[error("unknown field `{field}` in Java transcript")]
    UnknownField {
        /// The rejected field's name.
        field: String,
        /// The exact transcript bytes.
        source_payload: Box<[u8]>,
    },
    /// Unknown discriminator.
    #[error("unknown Java type kind `{kind}`")]
    UnknownKind {
        /// The rejected discriminator.
        kind: String,
        /// The exact transcript bytes.
        source_payload: Box<[u8]>,
    },
    /// Old schema.
    #[error("stale Java transcript schema `{found}`")]
    StaleSchema {
        /// The rejected schema cell, untruncated.
        found: u64,
        /// The exact transcript bytes.
        source_payload: Box<[u8]>,
    },
    /// Incomplete JSON.
    #[error("truncated Java transcript")]
    Truncated {
        /// The exact transcript bytes.
        source_payload: Box<[u8]>,
    },
    /// Invalid UTF-16 boundary.
    #[error("surrogate-split UTF-16 span `{start}..{end}`")]
    SurrogateSplit {
        /// The rejected UTF-16 start.
        start: usize,
        /// The rejected UTF-16 end.
        end: usize,
        /// The exact transcript bytes.
        source_payload: Box<[u8]>,
    },
    /// Other typed failure.
    #[error("invalid Java transcript: {cause}")]
    Invalid {
        /// The typed decode cause.
        cause: DecodeCause,
        /// The exact transcript bytes.
        source_payload: Box<[u8]>,
    },
}

/// Every `kind` discriminator the doclet can write: declaration element
/// kinds, module directive kinds, type-mirror kinds, and the constant and
/// annotation-value wrapper kinds.
const CLOSED_KINDS: &[&str] = &[
    // Type declarations (`ElementKind.name()` of a `TypeElement`).
    "CLASS",
    "INTERFACE",
    "ENUM",
    "RECORD",
    "ANNOTATION_TYPE",
    // Module directives.
    "requires",
    "exports",
    "opens",
    "uses",
    "provides",
    // Type mirrors.
    "primitive",
    "void",
    "declared",
    "array",
    "typevar",
    "wildcard",
    "intersection",
    "union",
    "error",
    "none",
    "null",
    "other",
    // Compile-time constant cells.
    "string",
    "boolean",
    "char",
    "double",
    "int",
    // Annotation-value wrappers.
    "annotation",
    "type",
    "enum",
];

fn find_unknown_kind(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(kind) = map.get("kind").and_then(serde_json::Value::as_str)
                && !CLOSED_KINDS.contains(&kind)
            {
                return Some(kind.to_owned());
            }
            map.values().find_map(find_unknown_kind)
        }
        serde_json::Value::Array(items) => items.iter().find_map(find_unknown_kind),
        _ => None,
    }
}

/// Decode a recorded doclet document.
pub fn decode(input: &[u8]) -> Result<JavaOutput, DecodeError> {
    let value: serde_json::Value = serde_json::from_slice(input).map_err(|error| {
        if error.is_eof() {
            DecodeError::Truncated {
                source_payload: input.into(),
            }
        } else {
            DecodeError::Invalid {
                cause: DecodeCause::Json(error),
                source_payload: input.into(),
            }
        }
    })?;
    // The cell stays a full u64 through the staleness comparison so a
    // truncating `as u32` can never fold a future schema onto the current
    // one.
    let format = value
        .get("format")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| DecodeError::Invalid {
            cause: DecodeCause::MissingFormat,
            source_payload: input.into(),
        })?;
    if format != 1 {
        return Err(DecodeError::StaleSchema {
            found: format,
            source_payload: input.into(),
        });
    }
    if let Some(kind) = find_unknown_kind(&value) {
        return Err(DecodeError::UnknownKind {
            kind,
            source_payload: input.into(),
        });
    }
    serde_json::from_value(value).map_err(|error| {
        let text = error.to_string();
        if text.contains("unknown field") {
            let mut quoted = text.split('`');
            quoted.next();
            match quoted.next() {
                Some(field) => DecodeError::UnknownField {
                    field: field.into(),
                    source_payload: input.into(),
                },
                None => DecodeError::Invalid {
                    cause: DecodeCause::Json(error),
                    source_payload: input.into(),
                },
            }
        } else {
            DecodeError::Invalid {
                cause: DecodeCause::Json(error),
                source_payload: input.into(),
            }
        }
    })
}

/// Convert a helper UTF-16 span to an exact UTF-8 byte span.
pub fn utf16_span_to_utf8(
    source: &str,
    span: Utf16Span,
) -> Result<std::ops::Range<usize>, DecodeError> {
    let mut offsets = Vec::with_capacity(source.encode_utf16().count() + 1);
    offsets.push(0);
    for (byte, unit) in source.char_indices() {
        let width = unit.len_utf16();
        if width == 2 {
            offsets.push(byte);
        }
        offsets.push(byte + unit.len_utf8());
    }
    let split_start = span.start > 0 && offsets.get(span.start) == offsets.get(span.start - 1);
    let split_end = span.end > 0 && offsets.get(span.end) == offsets.get(span.end - 1);
    if span.start >= offsets.len() || span.end >= offsets.len() || split_start || split_end {
        return Err(DecodeError::SurrogateSplit {
            start: span.start,
            end: span.end,
            source_payload: source.as_bytes().into(),
        });
    }
    Ok(offsets[span.start]..offsets[span.end])
}

/// Probe the configured javadoc executable.
pub fn probe_javadoc() -> Result<PathBuf, ToolingUnavailable> {
    let tool = env::var_os("COMPILER_JAVA_COMPILER")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("javadoc"));
    probe_javadoc_path(tool)
}

/// Probe an explicit executable, useful for deterministic unavailable tests.
pub fn probe_javadoc_path(tool: PathBuf) -> Result<PathBuf, ToolingUnavailable> {
    let result = Command::new(&tool).arg("--version").output();
    match result {
        Ok(output) if output.status.success() => Ok(tool),
        Ok(output) => Err(ToolingUnavailable {
            language: "java",
            tool,
            cause: ToolingCause::ExitStatus {
                status: output.status,
            },
        }),
        Err(error) => Err(ToolingUnavailable {
            language: "java",
            tool,
            cause: ToolingCause::Io { source: error },
        }),
    }
}
