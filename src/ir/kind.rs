#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

// MARK: - SumVariant

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct SumVariant {
    /// The variant/tag name (e.g., "Some", "None", "Ok", "Err")
    pub name: String,
    /// Associated types for this variant (None for unit variants)
    pub types: Option<Vec<Typee>>,
}

// MARK: - Path

// MARK: - DynTrait

// MARK: - QualifiedPath

// MARK: - GenericArgs

// MARK: Kind Types

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct DocsFunction {
    pub input_parameters: Option<Vec<Parameter>>,
    pub output_parameters: Option<Vec<Parameter>>,
    pub attributes: Option<Vec<FunctionAttribute>>,
    pub generics: Option<Generics>,
    pub name: String,
    /// Is there a function body?
    pub implemented: bool,
    pub visibility: Option<Visibility>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct DocsRecord {
    /// Optional name of the record (e.g., "User", "Point").
    /// Anonymous records (like tuples or JS objects) may omit this.
    pub name: Option<String>,

    /// Optional generic parameters (e.g., <T, U>).
    pub generics: Option<Vec<GenericArg>>,

    /// The kind of record (named, tuple, unit, dynamic).
    pub kind: RecordKind,

    /// The fields of the record (if applicable).
    pub fields: Option<Vec<RecordField>>,

    /// The visibility of the record
    pub visibility: Option<Visibility>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum Visibility {
    #[cfg_attr(feature = "serde", serde(rename = "public"))]
    Public,
    #[cfg_attr(feature = "serde", serde(rename = "private"))]
    Private,
    #[cfg_attr(feature = "serde", serde(rename = "protected"))]
    Protected,
    #[cfg_attr(feature = "serde", serde(rename = "internal"))]
    Internal,
    Package,
}

/// The various attributes a function can have
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum FunctionAttribute {
    /// Takes an arbitrary amount of arguments
    Variadic,
    /// Determined at compile-time
    Const,
    /// No side-effects
    Pure,
    /// Runs asynchronously
    Async,
    /// For Rust, happens within an unsafe context
    Unsafe,
}

/// Represents different kinds of code elements in a programming language or API.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "kind", content = "value"))]
pub enum Kind {
    /// A namespace, package, or module.
    Module,

    /// Struct, class, record, or data class.
    #[cfg_attr(feature = "serde", serde(rename = "recordType"))]
    RecordType(DocsRecord),

    /// Unlinked documentation
    Info,

    /// Union type
    #[cfg_attr(feature = "serde", serde(rename = "unionType"))]
    UnionType(Vec<Type>),

    /// Trait/protocol/interface definition
    #[cfg_attr(feature = "serde", serde(rename = "traitDef"))]
    TraitDef(TraitDef),

    /// Trait/protocol implementation
    #[cfg_attr(feature = "serde", serde(rename = "traitImpl"))]
    TraitImpl(TraitImpl),

    /// Enum, algebraic data type, discriminated union.
    #[cfg_attr(feature = "serde", serde(rename = "sumType"))]
    SumType(Vec<SumVariant>),

    /// Trait, interface, abstract base class.
    #[cfg_attr(feature = "serde", serde(rename = "interfaceType"))]
    InterfaceType,

    /// Function, method, lambda (with metadata).
    Function(DocsFunction),

    /// Type alias, typedef, using alias.
    #[cfg_attr(feature = "serde", serde(rename = "typeAlias"))]
    TypeAlias,

    /// Constant or immutable global.
    Constant,

    /// Mutable global/static variable.
    Variable,

    /// Macro, template, codegen hook.
    Macro,

    /// Built‑in primitive type.
    #[cfg_attr(feature = "serde", serde(rename = "primitiveType"))]
    PrimitiveType,

    /// Field or property of a type.
    Field,

    /// Event, signal, or callback definition.
    Event,
}

// Implementing `Display` for `Kind` to replace the Swift `description` computed property.
use std::fmt;

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let description = match self {
            Kind::Module => "A namespace, package, or module.",
            Kind::RecordType(_) => "Struct, class, record, or data class.",
            Kind::Info => "A piece of unlinked documentation",
            Kind::UnionType(_) => "Union type (C, C++, Rust, etc.).",
            Kind::TraitDef(_) => "Trait def",
            Kind::TraitImpl(_) => "Trait impl",
            Kind::SumType(_) => "Enum, algebraic data type, discriminated union.",
            Kind::InterfaceType => "Trait, interface, abstract base class.",
            Kind::Function(_) => "Function, method, lambda (with metadata).",
            Kind::TypeAlias => "Type alias, typedef, using alias.",
            Kind::Constant => "Constant or immutable global.",
            Kind::Variable => "Mutable global/static variable.",
            Kind::Macro => "Macro, template, codegen hook.",
            Kind::PrimitiveType => "Built‑in primitive type.",
            Kind::Field => "Field or property of a type.",
            Kind::Event => "Event, signal, or callback definition.",
        };
        write!(f, "{}", description)
    }
}
