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
