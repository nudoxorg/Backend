use serde::{Deserialize, Serialize};

use crate::ir::{
    generics::{ConstExpr, GenericArg},
    kind::Visibility,
    ty::Type,
};

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Record {
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
pub enum RecordKind {
    /// A record with no fields (unit struct, empty object).
    Unit,
    /// A record with ordered, positional fields (tuple, tuple struct).
    Tuple,
    /// A record with named fields (struct, class, record, object).
    Named,
    /// A record with dynamic/unknown fields (JS object, Python dict).
    Dynamic,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct RecordField {
    /// Field name (None if tuple-like).
    pub name: Option<String>,

    /// Type of the field (if known).
    pub ty: Option<Box<Type>>,
    /// The default value
    pub default_value: Option<ConstExpr>,

    /// Attributes on the field
    pub attributes: Option<FieldAttribute>,

    /// Visibility
    pub visibility: Option<Visibility>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum FieldAttribute {
    Mutable,
    Optional,
}

// Adding sum variants here for historical reasons

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct SumVariant {
    /// The variant/tag name (e.g., "Some", "None", "Ok", "Err")
    pub name: String,
    /// Associated types for this variant (None for unit variants)
    pub types: Option<Vec<Type>>,
}
