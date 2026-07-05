//! Serde mirror of the oracle's JSON document.
//!
//! The vendored doclet (`oracle/nudox/oracle/Extractor.java`) prints one JSON
//! object per invocation; every struct/enum here mirrors that shape exactly.
//! The oracle always emits every key (using `null` for absent values), but
//! `#[serde(default)]` is applied liberally so schema evolution on the Java
//! side degrades gracefully instead of failing deserialization.
//!
//! The two recursive shapes are [`TypeMirror`] (a structural type use,
//! mirroring `javax.lang.model.type.TypeMirror`) and [`Value`] (a compile-time
//! constant or annotation value). Both are internally tagged by `"kind"`.

use std::collections::BTreeMap;

use serde::Deserialize;

/// The whole oracle output: one document per `javadoc` invocation.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Extraction {
	/// Schema version stamped by the oracle (currently `1`).
	pub format: u32,
	/// `java.version` of the JVM the oracle ran on.
	pub java_version: String,
	/// JPMS modules (empty for classpath-style projects).
	#[serde(default)]
	pub modules: Vec<Module>,
	/// Every included package, with `package-info.java` docs when present.
	#[serde(default)]
	pub packages: Vec<Package>,
	/// Every included type declaration, nested types included (flat list;
	/// nesting is expressed via [`TypeDecl::enclosing`]).
	#[serde(default)]
	pub types: Vec<TypeDecl>,
}

/// A JPMS module (`module-info.java`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Module {
	pub name: String,
	pub open: bool,
	pub doc: Option<String>,
	pub doc_kind: Option<String>,
	#[serde(default)]
	pub annotations: Vec<Annotation>,
	#[serde(default)]
	pub directives: Vec<Directive>,
	pub position: Option<Position>,
}

/// One `module-info` directive.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Directive {
	Requires {
		module: String,
		transitive: bool,
		#[serde(rename = "static")]
		is_static: bool,
	},
	Exports {
		package: String,
		/// Qualified-export targets; `None` for an unqualified export.
		to: Option<Vec<String>>,
	},
	Opens {
		package: String,
		to: Option<Vec<String>>,
	},
	Uses {
		service: String,
	},
	Provides {
		service: String,
		implementations: Vec<String>,
	},
}

/// A package, including `package-info.java` documentation and annotations.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Package {
	pub name: String,
	pub doc: Option<String>,
	pub doc_kind: Option<String>,
	#[serde(default)]
	pub annotations: Vec<Annotation>,
	pub position: Option<Position>,
}

/// A type declaration: class, interface, enum, record, or annotation type.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeDecl {
	/// Fully qualified name with dotted nesting (`com.example.Outer.Inner`).
	pub qualified_name: String,
	pub simple_name: String,
	/// `CLASS` | `INTERFACE` | `ENUM` | `RECORD` | `ANNOTATION_TYPE`.
	pub kind: String,
	/// The declaring package (empty string for the unnamed package).
	pub package: String,
	/// The declaring JPMS module, if named.
	pub module: Option<String>,
	/// Qualified name of the enclosing type, for nested declarations.
	pub enclosing: Option<String>,
	/// `TOP_LEVEL` | `MEMBER` | `LOCAL` | `ANONYMOUS`.
	pub nesting: String,
	#[serde(default)]
	pub modifiers: Vec<String>,
	#[serde(default)]
	pub type_params: Vec<TypeParam>,
	pub superclass: Option<TypeMirror>,
	#[serde(default)]
	pub interfaces: Vec<TypeMirror>,
	/// Permitted direct subtypes, for `sealed` declarations.
	#[serde(default)]
	pub permits: Vec<TypeMirror>,
	#[serde(default)]
	pub record_components: Vec<RecordComponent>,
	#[serde(default)]
	pub annotations: Vec<Annotation>,
	#[serde(default)]
	pub deprecated: bool,
	/// Raw doc-comment text, verbatim (leading `*`/`///` already stripped).
	pub doc: Option<String>,
	/// `TRADITIONAL` | `END_OF_LINE` (JEP 467 Markdown) | `None` (pre-23 JDK).
	pub doc_kind: Option<String>,
	pub position: Option<Position>,
	#[serde(default)]
	pub fields: Vec<Field>,
	#[serde(default)]
	pub enum_constants: Vec<EnumConstant>,
	#[serde(default)]
	pub constructors: Vec<Method>,
	#[serde(default)]
	pub methods: Vec<Method>,
	/// Qualified names of directly nested type declarations.
	#[serde(default)]
	pub nested: Vec<String>,
}

/// A declaration-site type parameter (`<T extends A & B>`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeParam {
	pub name: String,
	#[serde(default)]
	pub bounds: Vec<TypeMirror>,
	#[serde(default)]
	pub annotations: Vec<Annotation>,
}

/// One component of a `record` declaration.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordComponent {
	pub name: String,
	#[serde(rename = "type")]
	pub ty: TypeMirror,
	/// The accessor method name (same as the component name in practice).
	pub accessor: Option<String>,
	#[serde(default)]
	pub annotations: Vec<Annotation>,
	pub doc: Option<String>,
	pub doc_kind: Option<String>,
}

/// A field declaration.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Field {
	pub name: String,
	#[serde(rename = "type")]
	pub ty: TypeMirror,
	#[serde(default)]
	pub modifiers: Vec<String>,
	/// The compile-time constant value, for `static final` constants.
	pub constant: Option<Value>,
	#[serde(default)]
	pub annotations: Vec<Annotation>,
	#[serde(default)]
	pub deprecated: bool,
	/// `EXPLICIT` | `MANDATED` | `SYNTHETIC` (javac element origin).
	pub origin: Option<String>,
	pub doc: Option<String>,
	pub doc_kind: Option<String>,
	pub position: Option<Position>,
}

/// An enum constant.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnumConstant {
	pub name: String,
	#[serde(default)]
	pub annotations: Vec<Annotation>,
	#[serde(default)]
	pub deprecated: bool,
	pub doc: Option<String>,
	pub doc_kind: Option<String>,
	/// A raw source window starting at the constant's declaration. Constructor
	/// arguments and constant class bodies exist only in source (javadoc's
	/// attributed trees drop the initializer), so the Rust side parses the
	/// balanced `(args)` group and `{` body opener out of this window.
	pub source: Option<String>,
	pub position: Option<Position>,
}

/// A method or constructor.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Method {
	/// Simple name; `<init>` for constructors.
	pub name: String,
	#[serde(default)]
	pub modifiers: Vec<String>,
	#[serde(default)]
	pub type_params: Vec<TypeParam>,
	#[serde(default)]
	pub params: Vec<Param>,
	/// `None` for constructors; `TypeMirror::Void` for `void` methods.
	#[serde(rename = "return")]
	pub return_type: Option<TypeMirror>,
	/// Declared `throws` types (checked and unchecked alike).
	#[serde(default)]
	pub thrown: Vec<TypeMirror>,
	#[serde(default)]
	pub varargs: bool,
	/// Whether this is an interface `default` method.
	#[serde(rename = "default", default)]
	pub is_default: bool,
	/// The receiver type, when explicit (annotated `this`, inner-class
	/// constructor outer instance).
	pub receiver: Option<TypeMirror>,
	/// The default of an annotation-interface element (`int x() default 3`).
	pub annotation_default: Option<Value>,
	#[serde(default)]
	pub annotations: Vec<Annotation>,
	#[serde(default)]
	pub deprecated: bool,
	pub origin: Option<String>,
	pub doc: Option<String>,
	pub doc_kind: Option<String>,
	pub position: Option<Position>,
}

/// A formal parameter of a method or constructor.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Param {
	pub name: String,
	#[serde(rename = "type")]
	pub ty: TypeMirror,
	#[serde(default)]
	pub annotations: Vec<Annotation>,
}

/// An annotation use, with explicitly supplied values only (defaults live on
/// the annotation declaration).
#[derive(Debug, Clone, Deserialize)]
pub struct Annotation {
	#[serde(rename = "type")]
	pub ty: String,
	#[serde(default)]
	pub values: BTreeMap<String, Value>,
}

/// A source position.
#[derive(Debug, Clone, Deserialize)]
pub struct Position {
	pub file: String,
	pub line: Option<u64>,
}

/// A recursive structural type mirror (a type *use*).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TypeMirror {
	/// `int`, `boolean`, … — with any type-use annotations.
	Primitive {
		name: String,
		#[serde(default)]
		annotations: Vec<Annotation>,
	},
	Void,
	/// A class or interface type, with type arguments and (when the enclosing
	/// type is itself parameterised, `Outer<T>.Inner`) its generic owner.
	Declared {
		/// The fully qualified element name (`java.util.List`).
		name: String,
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
		name: String,
		#[serde(default)]
		annotations: Vec<Annotation>,
	},
	/// `?`, `? extends X`, or `? super X` (never both bounds at once).
	Wildcard {
		#[serde(rename = "extends")]
		extends_bound: Option<Box<TypeMirror>>,
		#[serde(rename = "super")]
		super_bound: Option<Box<TypeMirror>>,
	},
	/// A type-variable bound intersection (`T extends A & B`).
	Intersection {
		#[serde(default)]
		bounds: Vec<TypeMirror>,
	},
	/// A multi-catch union (`catch (A | B e)`); rare in API positions.
	Union {
		#[serde(default)]
		alternatives: Vec<TypeMirror>,
	},
	/// An unresolvable type; `name` is the source text.
	Error {
		name: String,
	},
	None,
	Null,
	/// Anything the oracle could not classify, kept as its display form.
	Other {
		repr: String,
	},
}

/// A compile-time constant or annotation value.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Value {
	String { value: String },
	Boolean { value: bool },
	/// A `char`, transported as a one-character string.
	Char { value: String },
	/// `float` and `double` alike.
	Double { value: f64 },
	/// All integral types (`byte` … `long`), widened.
	Int { value: i64 },
	/// An enum-constant reference.
	Enum {
		#[serde(rename = "type")]
		ty: String,
		name: String,
	},
	/// A class literal (`String.class`).
	Type { value: TypeMirror },
	Array {
		#[serde(default)]
		values: Vec<Value>,
	},
	Annotation { value: Annotation },
	Other { repr: String },
}

impl TypeMirror {
	/// The qualified element name for declared/error mirrors, when meaningful.
	pub fn declared_name(&self) -> Option<&str> {
		match self {
			TypeMirror::Declared { name, .. } => Some(name),
			TypeMirror::Error { name } => Some(name),
			_ => None,
		}
	}
}
