//! Serde mirror of the oracle's JSON document (format version 1).
//!
//! The vendored doclet (`Extractor.java`) prints one JSON object per invocation;
//! every struct/enum here mirrors that shape exactly. The oracle always emits
//! every key (using `null` for absent values), but `#[serde(default)]` is
//! applied liberally so schema evolution on the Java side degrades gracefully
//! instead of failing deserialization.
//!
//! **Memory discipline:** all string-heavy types borrow from the deserialized
//! JSON where possible. The outer container (`Extraction`) owns its JSON `String`
//! through serde_json; inner fields use `Box<str>` for finished-length strings
//! (avoids over-allocation from `String`'s capacity headroom). Recursive trees
//! (`TypeMirror`, `Value`) are fully owned — they build their own sub-trees
//! during deserialization and cannot borrow parent slices.

use std::collections::BTreeMap;

use serde::Deserialize;

// ── Top-level document ─────────────────────────────────────────────────────

/// The whole oracle output: one document per `javadoc` invocation.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Extraction {
    /// Schema version stamped by the oracle (currently `1`).
    pub format: u32,
    /// `java.version` of the JVM the oracle ran on.
    pub java_version: Box<str>,
    /// JPMS modules (empty for classpath-style projects).
    #[serde(default)]
    pub modules: Box<[Module]>,
    /// Every included package, with `package-info.java` docs when present.
    #[serde(default)]
    pub packages: Box<[Package]>,
    /// Every included type declaration (flat list; nesting via `TypeDecl::enclosing`).
    #[serde(default)]
    pub types: Box<[TypeDecl]>,
}

// ── Module ─────────────────────────────────────────────────────────────────

/// A JPMS module (`module-info.java`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Module {
    pub name: Box<str>,
    pub open: bool,
    pub doc: Option<Box<str>>,
    pub doc_kind: Option<Box<str>>,
    #[serde(default)]
    pub annotations: Box<[Annotation]>,
    #[serde(default)]
    pub directives: Box<[Directive]>,
    pub position: Option<Position>,
}

/// One `module-info` directive.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Directive {
    Requires {
        module: Box<str>,
        transitive: bool,
        #[serde(rename = "static")]
        is_static: bool,
    },
    Exports {
        package: Box<str>,
        /// Qualified-export targets; `None` for an unqualified export.
        to: Option<Box<[Box<str>]>>,
    },
    Opens {
        package: Box<str>,
        to: Option<Box<[Box<str>]>>,
    },
    Uses {
        service: Box<str>,
    },
    Provides {
        service: Box<str>,
        #[serde(default)]
        implementations: Box<[Box<str>]>,
    },
}

// ── Package ─────────────────────────────────────────────────────────────────

/// A package, including `package-info.java` documentation and annotations.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Package {
    pub name: Box<str>,
    pub doc: Option<Box<str>>,
    pub doc_kind: Option<Box<str>>,
    #[serde(default)]
    pub annotations: Box<[Annotation]>,
    pub position: Option<Position>,
}

// ── TypeDecl ────────────────────────────────────────────────────────────────

/// A type declaration: class, interface, enum, record, or annotation type.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeDecl {
    /// Fully qualified name with dotted nesting (`com.example.Outer.Inner`).
    pub qualified_name: Box<str>,
    pub simple_name: Box<str>,
    /// `CLASS` | `INTERFACE` | `ENUM` | `RECORD` | `ANNOTATION_TYPE`.
    pub kind: TypeDeclKind,
    /// The declaring package (empty string for the unnamed package).
    pub package: Box<str>,
    /// The declaring JPMS module, if named.
    pub module: Option<Box<str>>,
    /// Qualified name of the enclosing type, for nested declarations.
    pub enclosing: Option<Box<str>>,
    /// `TOP_LEVEL` | `MEMBER` | `LOCAL` | `ANONYMOUS`.
    pub nesting: NestingKind,
    #[serde(default)]
    pub modifiers: Box<[Modifier]>,
    #[serde(default)]
    pub type_params: Box<[TypeParam]>,
    pub superclass: Option<TypeMirror>,
    #[serde(default)]
    pub interfaces: Box<[TypeMirror]>,
    /// Permitted direct subtypes, for `sealed` declarations.
    #[serde(default)]
    pub permits: Box<[TypeMirror]>,
    #[serde(default)]
    pub record_components: Box<[RecordComponent]>,
    #[serde(default)]
    pub annotations: Box<[Annotation]>,
    #[serde(default)]
    pub deprecated: bool,
    pub doc: Option<Box<str>>,
    pub doc_kind: Option<Box<str>>,
    pub position: Option<Position>,
    #[serde(default)]
    pub fields: Box<[Field]>,
    #[serde(default)]
    pub enum_constants: Box<[EnumConstant]>,
    #[serde(default)]
    pub constructors: Box<[Method]>,
    #[serde(default)]
    pub methods: Box<[Method]>,
    /// Qualified names of directly nested type declarations.
    #[serde(default)]
    pub nested: Box<[Box<str>]>,
}

/// The element kind of a type declaration, as emitted by the oracle.
///
/// Using a typed enum rather than `String` ensures exhaustive handling;
/// unknown variants cause a descriptive deserialization error rather than
/// silent incorrect behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TypeDeclKind {
    Class,
    Interface,
    Enum,
    Record,
    AnnotationType,
}

/// Nesting kind of a type declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum NestingKind {
    TopLevel,
    Member,
    Local,
    Anonymous,
}

/// A Java access/non-access modifier.
///
/// `#[serde(other)]` on `Unknown` lets the type absorb future JDK modifiers
/// (e.g. `sealed`, `non-sealed`, `strictfp`) without breaking deserialization,
/// while the exhaustive `match` in lowering will still catch the ones we know.
///
/// Modifiers the oracle emits as strings; we enumerate the ones we care about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Modifier {
    Public,
    Protected,
    Private,
    Static,
    Final,
    Abstract,
    Default,
    Synchronized,
    Native,
    Strictfp,
    Transient,
    Volatile,
    Sealed,
    #[serde(rename = "non-sealed")]
    NonSealed,
    #[serde(other)]
    Unknown,
}

// ── Members ──────────────────────────────────────────────────────────────────

/// A declaration-site type parameter (`<T extends A & B>`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeParam {
    pub name: Box<str>,
    #[serde(default)]
    pub bounds: Box<[TypeMirror]>,
    #[serde(default)]
    pub annotations: Box<[Annotation]>,
}

/// One component of a `record` declaration.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordComponent {
    pub name: Box<str>,
    #[serde(rename = "type")]
    pub ty: TypeMirror,
    pub accessor: Option<Box<str>>,
    #[serde(default)]
    pub annotations: Box<[Annotation]>,
    pub doc: Option<Box<str>>,
    pub doc_kind: Option<Box<str>>,
}

/// A field declaration.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Field {
    pub name: Box<str>,
    #[serde(rename = "type")]
    pub ty: TypeMirror,
    #[serde(default)]
    pub modifiers: Box<[Modifier]>,
    /// The compile-time constant value, for `static final` constants.
    pub constant: Option<Value>,
    #[serde(default)]
    pub annotations: Box<[Annotation]>,
    #[serde(default)]
    pub deprecated: bool,
    /// `EXPLICIT` | `MANDATED` | `SYNTHETIC` (javac element origin).
    pub origin: Option<ElementOrigin>,
    pub doc: Option<Box<str>>,
    pub doc_kind: Option<Box<str>>,
    pub position: Option<Position>,
}

/// An enum constant.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnumConstant {
    pub name: Box<str>,
    #[serde(default)]
    pub annotations: Box<[Annotation]>,
    #[serde(default)]
    pub deprecated: bool,
    pub doc: Option<Box<str>>,
    pub doc_kind: Option<Box<str>>,
    /// A raw source window starting at the constant's declaration. Constructor
    /// arguments and constant class bodies exist only in source (javadoc's
    /// attributed trees drop the initializer), so the Rust side parses the
    /// balanced `(args)` group and `{` body opener out of this window.
    pub source: Option<Box<str>>,
    pub position: Option<Position>,
}

/// A method or constructor.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Method {
    /// Simple name; `<init>` for constructors.
    pub name: Box<str>,
    #[serde(default)]
    pub modifiers: Box<[Modifier]>,
    #[serde(default)]
    pub type_params: Box<[TypeParam]>,
    #[serde(default)]
    pub params: Box<[Param]>,
    /// `None` for constructors; `TypeMirror::Void` for `void` methods.
    #[serde(rename = "return")]
    pub return_type: Option<TypeMirror>,
    /// Declared `throws` types (checked and unchecked alike).
    #[serde(default)]
    pub thrown: Box<[TypeMirror]>,
    #[serde(default)]
    pub varargs: bool,
    /// Whether this is an interface `default` method.
    #[serde(rename = "default", default)]
    pub is_default: bool,
    pub receiver: Option<TypeMirror>,
    /// The default of an annotation-interface element (`int x() default 3`).
    pub annotation_default: Option<Value>,
    #[serde(default)]
    pub annotations: Box<[Annotation]>,
    #[serde(default)]
    pub deprecated: bool,
    pub origin: Option<ElementOrigin>,
    pub doc: Option<Box<str>>,
    pub doc_kind: Option<Box<str>>,
    pub position: Option<Position>,
}

/// A formal parameter of a method or constructor.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Param {
    pub name: Box<str>,
    #[serde(rename = "type")]
    pub ty: TypeMirror,
    #[serde(default)]
    pub annotations: Box<[Annotation]>,
}

/// Element origin as reported by `elements.getOrigin(e)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ElementOrigin {
    Explicit,
    Mandated,
    Synthetic,
}

// ── Annotations and values ────────────────────────────────────────────────────

/// An annotation use, with explicitly supplied values only.
#[derive(Debug, Clone, Deserialize)]
pub struct Annotation {
    #[serde(rename = "type")]
    pub ty: Box<str>,
    #[serde(default)]
    pub values: BTreeMap<Box<str>, Value>,
}

/// Source position.
#[derive(Debug, Deserialize)]
pub struct Position {
    pub file: Box<str>,
    pub line: Option<u64>,
}

// ── TypeMirror ───────────────────────────────────────────────────────────────

/// A recursive structural type mirror (a type *use*), tagged by `"kind"`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TypeMirror {
    Primitive {
        name: Box<str>,
        #[serde(default)]
        annotations: Vec<Annotation>,
    },
    Void,
    Declared {
        /// The fully qualified element name (`java.util.List`).
        name: Box<str>,
        #[serde(default)]
        args: Vec<TypeMirror>,
        owner: Option<Box<TypeMirror>>,
        #[serde(default)]
        annotations: Vec<Annotation>,
    },
    Array {
        component: Box<TypeMirror>,
        #[serde(default)]
        annotations: Vec<Annotation>,
    },
    Typevar {
        name: Box<str>,
        #[serde(default)]
        annotations: Vec<Annotation>,
    },
    /// `?`, `? extends X`, or `? super X`.
    Wildcard {
        #[serde(rename = "extends")]
        extends_bound: Option<Box<TypeMirror>>,
        #[serde(rename = "super")]
        super_bound: Option<Box<TypeMirror>>,
    },
    Intersection {
        #[serde(default)]
        bounds: Vec<TypeMirror>,
    },
    Union {
        #[serde(default)]
        alternatives: Vec<TypeMirror>,
    },
    Error {
        name: Box<str>,
    },
    None,
    Null,
    Other {
        repr: Box<str>,
    },
}

impl TypeMirror {
    /// Returns `true` when this mirror is `Void` (used to elide void returns).
    pub fn is_void(&self) -> bool {
        matches!(self, TypeMirror::Void)
    }

    /// Returns `true` when this mirror is `SYNTHETIC` origin — compiler artifact.
    /// This is a shorthand check on `ElementOrigin`.
    #[allow(dead_code)]
    pub fn declared_name(&self) -> Option<&str> {
        match self {
            TypeMirror::Declared { name, .. } | TypeMirror::Error { name } => {
                Some(name.as_ref())
            }
            _ => None,
        }
    }
}

// ── Value (compile-time constants / annotation values) ─────────────────────

/// A compile-time constant or annotation value.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Value {
    String { value: Box<str> },
    Boolean { value: bool },
    /// A `char`, transported as a one-character string.
    Char { value: Box<str> },
    /// `float` and `double` alike.
    Double { value: f64 },
    /// All integral types (`byte` … `long`), widened.
    Int { value: i64 },
    /// An enum-constant reference.
    Enum {
        #[serde(rename = "type")]
        ty: Box<str>,
        name: Box<str>,
    },
    /// A class literal (`String.class`).
    Type { value: TypeMirror },
    Array {
        #[serde(default)]
        values: Vec<Value>,
    },
    Annotation { value: Annotation },
    Other { repr: Box<str> },
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Returns `true` when the origin marks a compiler artifact that should be
/// dropped (SYNTHETIC). MANDATED members (default constructors, enum
/// `values`/`valueOf`, record members) are kept — they are real API.
pub fn is_synthetic(origin: Option<ElementOrigin>) -> bool {
    origin == Some(ElementOrigin::Synthetic)
}

/// Whether a modifier slice contains the given modifier.
pub fn has_modifier(modifiers: &[Modifier], m: Modifier) -> bool {
    modifiers.contains(&m)
}
