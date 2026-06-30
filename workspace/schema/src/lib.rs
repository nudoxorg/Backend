#![allow(non_camel_case_types)]

#[cfg(feature = "serde")]
mod serde_utils;
pub mod poly;
pub mod poly_containers;

#[cfg(feature = "serde")]
use serde_yml as _ ;
#[cfg(feature = "serde")]
use serde::{Deserialize,Serialize,de::IntoDeserializer};
use serde_value::Value;
#[cfg(feature = "serde")]
use serde_path_to_error;
use std::collections::HashMap;
use std::collections::BTreeMap;

// Types

pub type string = String;
pub type integer = String;
pub type boolean = String;
pub type float = f64;
pub type double = f64;
pub type decimal = String;
pub type time = String;
pub type date = String;
pub type datetime = String;
pub type date_or_datetime = String;
pub type uriorcurie = String;
pub type curie = String;
pub type uri = String;
pub type ncname = String;
pub type objectidentifier = String;
pub type nodeidentifier = String;
pub type jsonpointer = String;
pub type jsonpath = String;
pub type sparqlpath = String;

// Slots

pub type id = uriorcurie;
pub type type_designator = String;
pub type name = String;
pub type fq_name = String;
pub type aliases = Vec<String>;
pub type documentation = String;
pub type visibility = Visibility;
pub type path = Vec<String>;
pub type members = Vec<String>;
pub type implemented_protocols = Vec<String>;
pub type kind = String;
pub type generics = Generics;
pub type implemented = bool;
pub type input_parameters = Vec<Parameter>;
pub type output_parameters = Vec<Parameter>;
pub type attributes = Vec<FunctionAttribute>;
pub type fields = Vec<RecordField>;
pub type record_kind = RecordKind;
pub type variants = Vec<SumVariant>;
pub type types = Vec<TypeExpression>;
pub type super_traits = Vec<TraitRef>;
pub type associated_types = Vec<AssociatedType>;
pub type required_methods = Vec<TraitMethod>;
pub type provided_methods = Vec<TraitMethod>;
pub type required_constants = Vec<TraitConstant>;
pub type docs = String;
pub type trait_ref = TraitRef;
pub type for_type = TypeExpression;
pub type methods = Vec<FunctionDetail>;
pub type associated_constants = Vec<TraitConstant>;
pub type is_negative = bool;
pub type is_blanket = bool;
pub type is_unsafe = bool;
pub type aliased_type = TypeExpression;
pub type ty = String;
pub type default_value = String;
pub type description = String;
pub type type_entry_id = isize;
pub type type_tag = String;
pub type generic_args = Vec<GenericArg>;
pub type param_name = String;
pub type primitive = PrimitiveKind;
pub type inner_type = TypeExpression;
pub type element_types = Vec<TypeExpression>;
pub type array_length = isize;
pub type is_mutable = bool;
pub type lifetime = String;
pub type fn_inputs = Vec<Parameter>;
pub type fn_outputs = Vec<Parameter>;
pub type fn_generic_params = Vec<TypeParam>;
pub type fn_attributes = Vec<FunctionAttribute>;
pub type dyn_traits = Vec<PolyTrait>;
pub type sum_variants = Vec<SumVariant>;
pub type generic_bounds = Vec<GenericBound>;
pub type qualified_name = String;
pub type self_type = TypeExpression;
pub type trait_path = String;
pub type arg_kind = String;
pub type type_value = TypeExpression;
pub type const_expr = String;
pub type type_params = Vec<TypeParam>;
pub type const_params = Vec<ConstParam>;
pub type lifetime_params = Vec<LifetimeParam>;
pub type constraints = Vec<ConstraintExpr>;
pub type variance = String;
pub type default_type = String;
pub type constraint_kind = String;
pub type param = String;
pub type assoc_name = String;
pub type bound = TypeExprSimple;
pub type kind_signature = String;
pub type shorter = String;
pub type longer = String;
pub type predicate_expr = String;
pub type args = String;
pub type bounds = Vec<GenericBound>;
pub type bound_kind = String;
pub type parameters = Vec<Parameter>;
pub type return_type = TypeExpression;
pub type receiver = ReceiverKind;
pub type has_default_implementation = bool;
pub type custom_name = String;
pub type custom_args = Vec<String>;
pub type tr = TraitRef;
pub type lifetimes = Vec<String>;

// Enums

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
#[cfg_attr(feature = "serde", serde(rename = "package"))]
    Package,
}

impl core::fmt::Display for Visibility {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Visibility::Public => f.write_str("public"),
            Visibility::Private => f.write_str("private"),
            Visibility::Protected => f.write_str("protected"),
            Visibility::Internal => f.write_str("internal"),
            Visibility::Package => f.write_str("package"),
        }
    }
}


#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum RecordKind {
#[cfg_attr(feature = "serde", serde(rename = "Unit"))]
    Unit,
#[cfg_attr(feature = "serde", serde(rename = "Tuple"))]
    Tuple,
#[cfg_attr(feature = "serde", serde(rename = "Named"))]
    Named,
#[cfg_attr(feature = "serde", serde(rename = "Dynamic"))]
    Dynamic,
}

impl core::fmt::Display for RecordKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            RecordKind::Unit => f.write_str("Unit"),
            RecordKind::Tuple => f.write_str("Tuple"),
            RecordKind::Named => f.write_str("Named"),
            RecordKind::Dynamic => f.write_str("Dynamic"),
        }
    }
}


#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum FunctionAttribute {
#[cfg_attr(feature = "serde", serde(rename = "Variadic"))]
    Variadic,
#[cfg_attr(feature = "serde", serde(rename = "Const"))]
    Const,
#[cfg_attr(feature = "serde", serde(rename = "Pure"))]
    Pure,
#[cfg_attr(feature = "serde", serde(rename = "Async"))]
    Async,
#[cfg_attr(feature = "serde", serde(rename = "Unsafe"))]
    Unsafe,
}

impl core::fmt::Display for FunctionAttribute {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            FunctionAttribute::Variadic => f.write_str("Variadic"),
            FunctionAttribute::Const => f.write_str("Const"),
            FunctionAttribute::Pure => f.write_str("Pure"),
            FunctionAttribute::Async => f.write_str("Async"),
            FunctionAttribute::Unsafe => f.write_str("Unsafe"),
        }
    }
}


#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum ParameterAttribute {
#[cfg_attr(feature = "serde", serde(rename = "Inout"))]
    Inout,
#[cfg_attr(feature = "serde", serde(rename = "Mutable"))]
    Mutable,
#[cfg_attr(feature = "serde", serde(rename = "Consuming"))]
    Consuming,
#[cfg_attr(feature = "serde", serde(rename = "Borrowing"))]
    Borrowing,
#[cfg_attr(feature = "serde", serde(rename = "Isolated"))]
    Isolated,
#[cfg_attr(feature = "serde", serde(rename = "Variadic"))]
    Variadic,
#[cfg_attr(feature = "serde", serde(rename = "Optional"))]
    Optional,
}

impl core::fmt::Display for ParameterAttribute {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ParameterAttribute::Inout => f.write_str("Inout"),
            ParameterAttribute::Mutable => f.write_str("Mutable"),
            ParameterAttribute::Consuming => f.write_str("Consuming"),
            ParameterAttribute::Borrowing => f.write_str("Borrowing"),
            ParameterAttribute::Isolated => f.write_str("Isolated"),
            ParameterAttribute::Variadic => f.write_str("Variadic"),
            ParameterAttribute::Optional => f.write_str("Optional"),
        }
    }
}


#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum FieldAttribute {
#[cfg_attr(feature = "serde", serde(rename = "Mutable"))]
    Mutable,
#[cfg_attr(feature = "serde", serde(rename = "Optional"))]
    Optional,
}

impl core::fmt::Display for FieldAttribute {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            FieldAttribute::Mutable => f.write_str("Mutable"),
            FieldAttribute::Optional => f.write_str("Optional"),
        }
    }
}


#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum Variance {
#[cfg_attr(feature = "serde", serde(rename = "Covariant"))]
    Covariant,
#[cfg_attr(feature = "serde", serde(rename = "Contravariant"))]
    Contravariant,
#[cfg_attr(feature = "serde", serde(rename = "Invariant"))]
    Invariant,
#[cfg_attr(feature = "serde", serde(rename = "Bivariant"))]
    Bivariant,
}

impl core::fmt::Display for Variance {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Variance::Covariant => f.write_str("Covariant"),
            Variance::Contravariant => f.write_str("Contravariant"),
            Variance::Invariant => f.write_str("Invariant"),
            Variance::Bivariant => f.write_str("Bivariant"),
        }
    }
}


#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum TypeKind {
#[cfg_attr(feature = "serde", serde(rename = "Type"))]
    Type,
#[cfg_attr(feature = "serde", serde(rename = "HigherKinded"))]
    HigherKinded,
#[cfg_attr(feature = "serde", serde(rename = "Associated"))]
    Associated,
}

impl core::fmt::Display for TypeKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            TypeKind::Type => f.write_str("Type"),
            TypeKind::HigherKinded => f.write_str("HigherKinded"),
            TypeKind::Associated => f.write_str("Associated"),
        }
    }
}


#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum ReceiverKind {
#[cfg_attr(feature = "serde", serde(rename = "Owned"))]
    Owned,
#[cfg_attr(feature = "serde", serde(rename = "SharedRef"))]
    SharedRef,
#[cfg_attr(feature = "serde", serde(rename = "MutRef"))]
    MutRef,
#[cfg_attr(feature = "serde", serde(rename = "Static"))]
    Static,
#[cfg_attr(feature = "serde", serde(rename = "Arbitrary"))]
    Arbitrary,
}

impl core::fmt::Display for ReceiverKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ReceiverKind::Owned => f.write_str("Owned"),
            ReceiverKind::SharedRef => f.write_str("SharedRef"),
            ReceiverKind::MutRef => f.write_str("MutRef"),
            ReceiverKind::Static => f.write_str("Static"),
            ReceiverKind::Arbitrary => f.write_str("Arbitrary"),
        }
    }
}


#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum TraitAttributeKind {
#[cfg_attr(feature = "serde", serde(rename = "Marker"))]
    Marker,
#[cfg_attr(feature = "serde", serde(rename = "Auto"))]
    Auto,
#[cfg_attr(feature = "serde", serde(rename = "Unsafe"))]
    Unsafe,
#[cfg_attr(feature = "serde", serde(rename = "ObjectSafe"))]
    ObjectSafe,
#[cfg_attr(feature = "serde", serde(rename = "Sealed"))]
    Sealed,
#[cfg_attr(feature = "serde", serde(rename = "Functional"))]
    Functional,
}

impl core::fmt::Display for TraitAttributeKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            TraitAttributeKind::Marker => f.write_str("Marker"),
            TraitAttributeKind::Auto => f.write_str("Auto"),
            TraitAttributeKind::Unsafe => f.write_str("Unsafe"),
            TraitAttributeKind::ObjectSafe => f.write_str("ObjectSafe"),
            TraitAttributeKind::Sealed => f.write_str("Sealed"),
            TraitAttributeKind::Functional => f.write_str("Functional"),
        }
    }
}


#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum PrimitiveKind {
#[cfg_attr(feature = "serde", serde(rename = "Int8"))]
    Int8,
#[cfg_attr(feature = "serde", serde(rename = "Int16"))]
    Int16,
#[cfg_attr(feature = "serde", serde(rename = "Int"))]
    Int,
#[cfg_attr(feature = "serde", serde(rename = "Int64"))]
    Int64,
#[cfg_attr(feature = "serde", serde(rename = "Int128"))]
    Int128,
#[cfg_attr(feature = "serde", serde(rename = "UInt8"))]
    UInt8,
#[cfg_attr(feature = "serde", serde(rename = "UInt16"))]
    UInt16,
#[cfg_attr(feature = "serde", serde(rename = "UInt"))]
    UInt,
#[cfg_attr(feature = "serde", serde(rename = "UInt64"))]
    UInt64,
#[cfg_attr(feature = "serde", serde(rename = "UInt128"))]
    UInt128,
#[cfg_attr(feature = "serde", serde(rename = "F16"))]
    F16,
#[cfg_attr(feature = "serde", serde(rename = "Float"))]
    Float,
#[cfg_attr(feature = "serde", serde(rename = "Double"))]
    Double,
#[cfg_attr(feature = "serde", serde(rename = "Bool"))]
    Bool,
#[cfg_attr(feature = "serde", serde(rename = "String"))]
    String,
#[cfg_attr(feature = "serde", serde(rename = "Char"))]
    Char,
#[cfg_attr(feature = "serde", serde(rename = "None"))]
    None,
#[cfg_attr(feature = "serde", serde(rename = "Date"))]
    Date,
#[cfg_attr(feature = "serde", serde(rename = "Data"))]
    Data,
}

impl core::fmt::Display for PrimitiveKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PrimitiveKind::Int8 => f.write_str("Int8"),
            PrimitiveKind::Int16 => f.write_str("Int16"),
            PrimitiveKind::Int => f.write_str("Int"),
            PrimitiveKind::Int64 => f.write_str("Int64"),
            PrimitiveKind::Int128 => f.write_str("Int128"),
            PrimitiveKind::UInt8 => f.write_str("UInt8"),
            PrimitiveKind::UInt16 => f.write_str("UInt16"),
            PrimitiveKind::UInt => f.write_str("UInt"),
            PrimitiveKind::UInt64 => f.write_str("UInt64"),
            PrimitiveKind::UInt128 => f.write_str("UInt128"),
            PrimitiveKind::F16 => f.write_str("F16"),
            PrimitiveKind::Float => f.write_str("Float"),
            PrimitiveKind::Double => f.write_str("Double"),
            PrimitiveKind::Bool => f.write_str("Bool"),
            PrimitiveKind::String => f.write_str("String"),
            PrimitiveKind::Char => f.write_str("Char"),
            PrimitiveKind::None => f.write_str("None"),
            PrimitiveKind::Date => f.write_str("Date"),
            PrimitiveKind::Data => f.write_str("Data"),
        }
    }
}



// Classes

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Entry {
    #[cfg_attr(feature = "serde", serde(rename = "@id"))]
    pub id: uriorcurie,
    #[cfg_attr(feature = "serde", serde(default))]
    #[cfg_attr(feature = "serde", serde(rename = "@type"))]
    pub type_designator: Option<String>,
    pub name: String,
    #[cfg_attr(feature = "serde", serde(default))]
    pub fq_name: Option<String>,
    #[cfg_attr(feature = "serde", serde(
        deserialize_with = "serde_utils::deserialize_primitive_list_or_single_value_optional",
        serialize_with = "serde_utils::serialize_primitive_list_or_single_value_optional"
    ))]
    #[cfg_attr(feature = "serde", serde(default))]
    pub aliases: Option<Vec<String>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub documentation: Option<String>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub visibility: Option<Visibility>,
    #[cfg_attr(feature = "serde", serde(
        deserialize_with = "serde_utils::deserialize_primitive_list_or_single_value_optional",
        serialize_with = "serde_utils::serialize_primitive_list_or_single_value_optional"
    ))]
    #[cfg_attr(feature = "serde", serde(default))]
    pub path: Option<Vec<String>>,
    #[cfg_attr(feature = "serde", serde(
        deserialize_with = "serde_utils::deserialize_primitive_list_or_single_value_optional",
        serialize_with = "serde_utils::serialize_primitive_list_or_single_value_optional"
    ))]
    #[cfg_attr(feature = "serde", serde(default))]
    pub members: Option<Vec<String>>,
    #[cfg_attr(feature = "serde", serde(
        deserialize_with = "serde_utils::deserialize_primitive_list_or_single_value_optional",
        serialize_with = "serde_utils::serialize_primitive_list_or_single_value_optional"
    ))]
    #[cfg_attr(feature = "serde", serde(default))]
    pub implemented_protocols: Option<Vec<String>>,
    pub kind: String
}


#[cfg(feature = "serde")]
impl serde_utils::InlinedPair for Entry {
    type Key   = uriorcurie;
    type Value = Value;
    type Error = String;

    fn extract_key(&self) -> &Self::Key {
        return &self.id;
    }

    fn from_pair_mapping(k: Self::Key, v: Value) -> Result<Self,Self::Error> {
        let mut map = match v {
            Value::Map(m) => m,
            _ => return Err("ClassDefinition must be a mapping".into()),
        };
        let key_value = serde_value::to_value(k.clone())
            .map_err(|e| format!("unable to serialize key: {}", e))?;
        map.insert(Value::String("id".into()), key_value);
        let de          = Value::Map(map).into_deserializer();
        match serde_path_to_error::deserialize(de) {
            Ok(ok)  => Ok(ok),
            Err(e)  => Err(format!("at `{}`: {}", e.path(), e.inner())),
        }
    }


    fn from_pair_simple(_k: Self::Key, _v: Value) -> Result<Self,Self::Error> {
        Err("Cannot create a Entry from a primitive value!".into())
    }


    fn compact_value(&self) -> Option<Value> {
        let value = match serde_value::to_value(self) {
            Ok(v) => v,
            Err(_) => return None,
        };
        match value {
            Value::Map(mut map) => {
                map.remove(&Value::String("id".into()));
                Some(Value::Map(map))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Kind {
    #[cfg_attr(feature = "serde", serde(rename = "@id"))]
    pub id: uriorcurie,
    #[cfg_attr(feature = "serde", serde(default))]
    #[cfg_attr(feature = "serde", serde(rename = "@type"))]
    pub type_designator: Option<String>
}


#[cfg(feature = "serde")]
impl serde_utils::InlinedPair for Kind {
    type Key   = uriorcurie;
    type Value = String;
    type Error = String;

    fn extract_key(&self) -> &Self::Key {
        return &self.id;
    }

    fn from_pair_mapping(k: Self::Key, v: Value) -> Result<Self,Self::Error> {
        let mut map = match v {
            Value::Map(m) => m,
            _ => return Err("ClassDefinition must be a mapping".into()),
        };
        let key_value = serde_value::to_value(k.clone())
            .map_err(|e| format!("unable to serialize key: {}", e))?;
        map.insert(Value::String("id".into()), key_value);
        let de          = Value::Map(map).into_deserializer();
        match serde_path_to_error::deserialize(de) {
            Ok(ok)  => Ok(ok),
            Err(e)  => Err(format!("at `{}`: {}", e.path(), e.inner())),
        }
    }


    fn from_pair_simple(k: Self::Key, v: Value) -> Result<Self,Self::Error> {
        let mut map:  BTreeMap<Value, Value> = BTreeMap::new();
        let key_value = serde_value::to_value(k.clone())
            .map_err(|e| format!("unable to serialize key: {}", e))?;
        map.insert(Value::String("id".into()), key_value);
        map.insert(Value::String("type_designator".into()), v);
        let de          = Value::Map(map).into_deserializer();
        match serde_path_to_error::deserialize(de) {
            Ok(ok)  => Ok(ok),
            Err(e)  => Err(format!("at `{}`: {}", e.path(), e.inner())),
        }

    }

    fn simple_value(&self) -> Option<&Self::Value> {
        self.type_designator.as_ref()
    }

    fn compact_value(&self) -> Option<Value> {
        let value = match serde_value::to_value(self) {
            Ok(v) => v,
            Err(_) => return None,
        };
        match value {
            Value::Map(mut map) => {
                map.remove(&Value::String("id".into()));
                Some(Value::Map(map))
            }
            _ => None,
        }
    }
}
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature="serde", serde(tag = "@type"))]
pub enum KindOrSubtype {    #[serde(rename = "Module",   )]
    Module(Module),     #[serde(rename = "Info",   )]
    Info(Info),     #[serde(rename = "InterfaceType",   )]
    InterfaceType(InterfaceType),     #[serde(rename = "PrimitiveType",   )]
    PrimitiveType(PrimitiveType),     #[serde(rename = "Constant",   )]
    Constant(Constant),     #[serde(rename = "Variable",   )]
    Variable(Variable),     #[serde(rename = "Macro",   )]
    Macro(Macro),     #[serde(rename = "FieldKind",   )]
    FieldKind(FieldKind),     #[serde(rename = "Event",   )]
    Event(Event),     #[serde(rename = "Function",   )]
    Function(Function),     #[serde(rename = "RecordType",   )]
    RecordType(RecordType),     #[serde(rename = "SumType",   )]
    SumType(SumType),     #[serde(rename = "UnionType",   )]
    UnionType(UnionType),     #[serde(rename = "TraitDef",   )]
    TraitDef(TraitDef),     #[serde(rename = "TraitImpl",   )]
    TraitImpl(TraitImpl),     #[serde(rename = "TypeAlias",   )]
    TypeAlias(TypeAlias)}

impl From<Module>   for KindOrSubtype { fn from(x: Module)   -> Self { Self::Module(x) } }
impl From<Info>   for KindOrSubtype { fn from(x: Info)   -> Self { Self::Info(x) } }
impl From<InterfaceType>   for KindOrSubtype { fn from(x: InterfaceType)   -> Self { Self::InterfaceType(x) } }
impl From<PrimitiveType>   for KindOrSubtype { fn from(x: PrimitiveType)   -> Self { Self::PrimitiveType(x) } }
impl From<Constant>   for KindOrSubtype { fn from(x: Constant)   -> Self { Self::Constant(x) } }
impl From<Variable>   for KindOrSubtype { fn from(x: Variable)   -> Self { Self::Variable(x) } }
impl From<Macro>   for KindOrSubtype { fn from(x: Macro)   -> Self { Self::Macro(x) } }
impl From<FieldKind>   for KindOrSubtype { fn from(x: FieldKind)   -> Self { Self::FieldKind(x) } }
impl From<Event>   for KindOrSubtype { fn from(x: Event)   -> Self { Self::Event(x) } }
impl From<Function>   for KindOrSubtype { fn from(x: Function)   -> Self { Self::Function(x) } }
impl From<RecordType>   for KindOrSubtype { fn from(x: RecordType)   -> Self { Self::RecordType(x) } }
impl From<SumType>   for KindOrSubtype { fn from(x: SumType)   -> Self { Self::SumType(x) } }
impl From<UnionType>   for KindOrSubtype { fn from(x: UnionType)   -> Self { Self::UnionType(x) } }
impl From<TraitDef>   for KindOrSubtype { fn from(x: TraitDef)   -> Self { Self::TraitDef(x) } }
impl From<TraitImpl>   for KindOrSubtype { fn from(x: TraitImpl)   -> Self { Self::TraitImpl(x) } }
impl From<TypeAlias>   for KindOrSubtype { fn from(x: TypeAlias)   -> Self { Self::TypeAlias(x) } }


#[cfg(feature = "serde")]
impl serde_utils::InlinedPair for KindOrSubtype {
    type Key       = uriorcurie;
    type Value     = serde_value::Value;
    type Error     = String;

    fn from_pair_mapping(k: Self::Key, v: Self::Value) -> Result<Self, Self::Error> {
        if let Ok(x) = Module::from_pair_mapping(k.clone(), v.clone()) {
            return Ok(KindOrSubtype::Module(x));
        }
        if let Ok(x) = Info::from_pair_mapping(k.clone(), v.clone()) {
            return Ok(KindOrSubtype::Info(x));
        }
        if let Ok(x) = InterfaceType::from_pair_mapping(k.clone(), v.clone()) {
            return Ok(KindOrSubtype::InterfaceType(x));
        }
        if let Ok(x) = PrimitiveType::from_pair_mapping(k.clone(), v.clone()) {
            return Ok(KindOrSubtype::PrimitiveType(x));
        }
        if let Ok(x) = Constant::from_pair_mapping(k.clone(), v.clone()) {
            return Ok(KindOrSubtype::Constant(x));
        }
        if let Ok(x) = Variable::from_pair_mapping(k.clone(), v.clone()) {
            return Ok(KindOrSubtype::Variable(x));
        }
        if let Ok(x) = Macro::from_pair_mapping(k.clone(), v.clone()) {
            return Ok(KindOrSubtype::Macro(x));
        }
        if let Ok(x) = FieldKind::from_pair_mapping(k.clone(), v.clone()) {
            return Ok(KindOrSubtype::FieldKind(x));
        }
        if let Ok(x) = Event::from_pair_mapping(k.clone(), v.clone()) {
            return Ok(KindOrSubtype::Event(x));
        }
        if let Ok(x) = Function::from_pair_mapping(k.clone(), v.clone()) {
            return Ok(KindOrSubtype::Function(x));
        }
        if let Ok(x) = RecordType::from_pair_mapping(k.clone(), v.clone()) {
            return Ok(KindOrSubtype::RecordType(x));
        }
        if let Ok(x) = SumType::from_pair_mapping(k.clone(), v.clone()) {
            return Ok(KindOrSubtype::SumType(x));
        }
        if let Ok(x) = UnionType::from_pair_mapping(k.clone(), v.clone()) {
            return Ok(KindOrSubtype::UnionType(x));
        }
        if let Ok(x) = TraitDef::from_pair_mapping(k.clone(), v.clone()) {
            return Ok(KindOrSubtype::TraitDef(x));
        }
        if let Ok(x) = TraitImpl::from_pair_mapping(k.clone(), v.clone()) {
            return Ok(KindOrSubtype::TraitImpl(x));
        }
        if let Ok(x) = TypeAlias::from_pair_mapping(k.clone(), v.clone()) {
            return Ok(KindOrSubtype::TypeAlias(x));
        }
        Err("none of the variants matched the mapping form".into())
    }

    fn from_pair_simple(k: Self::Key, v: Self::Value) -> Result<Self, Self::Error> {
        if let Ok(x) = Module::from_pair_simple(k.clone(), v.clone()) {
            return Ok(KindOrSubtype::Module(x));
        }
        if let Ok(x) = Info::from_pair_simple(k.clone(), v.clone()) {
            return Ok(KindOrSubtype::Info(x));
        }
        if let Ok(x) = InterfaceType::from_pair_simple(k.clone(), v.clone()) {
            return Ok(KindOrSubtype::InterfaceType(x));
        }
        if let Ok(x) = PrimitiveType::from_pair_simple(k.clone(), v.clone()) {
            return Ok(KindOrSubtype::PrimitiveType(x));
        }
        if let Ok(x) = Constant::from_pair_simple(k.clone(), v.clone()) {
            return Ok(KindOrSubtype::Constant(x));
        }
        if let Ok(x) = Variable::from_pair_simple(k.clone(), v.clone()) {
            return Ok(KindOrSubtype::Variable(x));
        }
        if let Ok(x) = Macro::from_pair_simple(k.clone(), v.clone()) {
            return Ok(KindOrSubtype::Macro(x));
        }
        if let Ok(x) = FieldKind::from_pair_simple(k.clone(), v.clone()) {
            return Ok(KindOrSubtype::FieldKind(x));
        }
        if let Ok(x) = Event::from_pair_simple(k.clone(), v.clone()) {
            return Ok(KindOrSubtype::Event(x));
        }
        if let Ok(x) = Function::from_pair_simple(k.clone(), v.clone()) {
            return Ok(KindOrSubtype::Function(x));
        }
        if let Ok(x) = RecordType::from_pair_simple(k.clone(), v.clone()) {
            return Ok(KindOrSubtype::RecordType(x));
        }
        if let Ok(x) = SumType::from_pair_simple(k.clone(), v.clone()) {
            return Ok(KindOrSubtype::SumType(x));
        }
        if let Ok(x) = UnionType::from_pair_simple(k.clone(), v.clone()) {
            return Ok(KindOrSubtype::UnionType(x));
        }
        if let Ok(x) = TraitDef::from_pair_simple(k.clone(), v.clone()) {
            return Ok(KindOrSubtype::TraitDef(x));
        }
        if let Ok(x) = TraitImpl::from_pair_simple(k.clone(), v.clone()) {
            return Ok(KindOrSubtype::TraitImpl(x));
        }
        if let Ok(x) = TypeAlias::from_pair_simple(k.clone(), v.clone()) {
            return Ok(KindOrSubtype::TypeAlias(x));
        }
        Err("none of the variants support the primitive form".into())
    }

    fn extract_key(&self) -> &Self::Key {
        match self {
            KindOrSubtype::Module(inner) => inner.extract_key(),
            KindOrSubtype::Info(inner) => inner.extract_key(),
            KindOrSubtype::InterfaceType(inner) => inner.extract_key(),
            KindOrSubtype::PrimitiveType(inner) => inner.extract_key(),
            KindOrSubtype::Constant(inner) => inner.extract_key(),
            KindOrSubtype::Variable(inner) => inner.extract_key(),
            KindOrSubtype::Macro(inner) => inner.extract_key(),
            KindOrSubtype::FieldKind(inner) => inner.extract_key(),
            KindOrSubtype::Event(inner) => inner.extract_key(),
            KindOrSubtype::Function(inner) => inner.extract_key(),
            KindOrSubtype::RecordType(inner) => inner.extract_key(),
            KindOrSubtype::SumType(inner) => inner.extract_key(),
            KindOrSubtype::UnionType(inner) => inner.extract_key(),
            KindOrSubtype::TraitDef(inner) => inner.extract_key(),
            KindOrSubtype::TraitImpl(inner) => inner.extract_key(),
            KindOrSubtype::TypeAlias(inner) => inner.extract_key(),
        }
    }
}



#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Module {
    #[cfg_attr(feature = "serde", serde(rename = "@id"))]
    pub id: uriorcurie,
    #[cfg_attr(feature = "serde", serde(default))]
    #[cfg_attr(feature = "serde", serde(rename = "@type"))]
    #[cfg_attr(feature = "serde", serde(skip_serializing))]
    pub type_designator: Option<String>
}


#[cfg(feature = "serde")]
impl serde_utils::InlinedPair for Module {
    type Key   = uriorcurie;
    type Value = String;
    type Error = String;

    fn extract_key(&self) -> &Self::Key {
        return &self.id;
    }

    fn from_pair_mapping(k: Self::Key, v: Value) -> Result<Self,Self::Error> {
        let mut map = match v {
            Value::Map(m) => m,
            _ => return Err("ClassDefinition must be a mapping".into()),
        };
        let key_value = serde_value::to_value(k.clone())
            .map_err(|e| format!("unable to serialize key: {}", e))?;
        map.insert(Value::String("id".into()), key_value);
        let de          = Value::Map(map).into_deserializer();
        match serde_path_to_error::deserialize(de) {
            Ok(ok)  => Ok(ok),
            Err(e)  => Err(format!("at `{}`: {}", e.path(), e.inner())),
        }
    }


    fn from_pair_simple(k: Self::Key, v: Value) -> Result<Self,Self::Error> {
        let mut map:  BTreeMap<Value, Value> = BTreeMap::new();
        let key_value = serde_value::to_value(k.clone())
            .map_err(|e| format!("unable to serialize key: {}", e))?;
        map.insert(Value::String("id".into()), key_value);
        map.insert(Value::String("type_designator".into()), v);
        let de          = Value::Map(map).into_deserializer();
        match serde_path_to_error::deserialize(de) {
            Ok(ok)  => Ok(ok),
            Err(e)  => Err(format!("at `{}`: {}", e.path(), e.inner())),
        }

    }

    fn simple_value(&self) -> Option<&Self::Value> {
        self.type_designator.as_ref()
    }

    fn compact_value(&self) -> Option<Value> {
        let value = match serde_value::to_value(self) {
            Ok(v) => v,
            Err(_) => return None,
        };
        match value {
            Value::Map(mut map) => {
                map.remove(&Value::String("id".into()));
                Some(Value::Map(map))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Info {
    #[cfg_attr(feature = "serde", serde(rename = "@id"))]
    pub id: uriorcurie,
    #[cfg_attr(feature = "serde", serde(default))]
    #[cfg_attr(feature = "serde", serde(rename = "@type"))]
    #[cfg_attr(feature = "serde", serde(skip_serializing))]
    pub type_designator: Option<String>
}


#[cfg(feature = "serde")]
impl serde_utils::InlinedPair for Info {
    type Key   = uriorcurie;
    type Value = String;
    type Error = String;

    fn extract_key(&self) -> &Self::Key {
        return &self.id;
    }

    fn from_pair_mapping(k: Self::Key, v: Value) -> Result<Self,Self::Error> {
        let mut map = match v {
            Value::Map(m) => m,
            _ => return Err("ClassDefinition must be a mapping".into()),
        };
        let key_value = serde_value::to_value(k.clone())
            .map_err(|e| format!("unable to serialize key: {}", e))?;
        map.insert(Value::String("id".into()), key_value);
        let de          = Value::Map(map).into_deserializer();
        match serde_path_to_error::deserialize(de) {
            Ok(ok)  => Ok(ok),
            Err(e)  => Err(format!("at `{}`: {}", e.path(), e.inner())),
        }
    }


    fn from_pair_simple(k: Self::Key, v: Value) -> Result<Self,Self::Error> {
        let mut map:  BTreeMap<Value, Value> = BTreeMap::new();
        let key_value = serde_value::to_value(k.clone())
            .map_err(|e| format!("unable to serialize key: {}", e))?;
        map.insert(Value::String("id".into()), key_value);
        map.insert(Value::String("type_designator".into()), v);
        let de          = Value::Map(map).into_deserializer();
        match serde_path_to_error::deserialize(de) {
            Ok(ok)  => Ok(ok),
            Err(e)  => Err(format!("at `{}`: {}", e.path(), e.inner())),
        }

    }

    fn simple_value(&self) -> Option<&Self::Value> {
        self.type_designator.as_ref()
    }

    fn compact_value(&self) -> Option<Value> {
        let value = match serde_value::to_value(self) {
            Ok(v) => v,
            Err(_) => return None,
        };
        match value {
            Value::Map(mut map) => {
                map.remove(&Value::String("id".into()));
                Some(Value::Map(map))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct InterfaceType {
    #[cfg_attr(feature = "serde", serde(rename = "@id"))]
    pub id: uriorcurie,
    #[cfg_attr(feature = "serde", serde(default))]
    #[cfg_attr(feature = "serde", serde(rename = "@type"))]
    #[cfg_attr(feature = "serde", serde(skip_serializing))]
    pub type_designator: Option<String>
}


#[cfg(feature = "serde")]
impl serde_utils::InlinedPair for InterfaceType {
    type Key   = uriorcurie;
    type Value = String;
    type Error = String;

    fn extract_key(&self) -> &Self::Key {
        return &self.id;
    }

    fn from_pair_mapping(k: Self::Key, v: Value) -> Result<Self,Self::Error> {
        let mut map = match v {
            Value::Map(m) => m,
            _ => return Err("ClassDefinition must be a mapping".into()),
        };
        let key_value = serde_value::to_value(k.clone())
            .map_err(|e| format!("unable to serialize key: {}", e))?;
        map.insert(Value::String("id".into()), key_value);
        let de          = Value::Map(map).into_deserializer();
        match serde_path_to_error::deserialize(de) {
            Ok(ok)  => Ok(ok),
            Err(e)  => Err(format!("at `{}`: {}", e.path(), e.inner())),
        }
    }


    fn from_pair_simple(k: Self::Key, v: Value) -> Result<Self,Self::Error> {
        let mut map:  BTreeMap<Value, Value> = BTreeMap::new();
        let key_value = serde_value::to_value(k.clone())
            .map_err(|e| format!("unable to serialize key: {}", e))?;
        map.insert(Value::String("id".into()), key_value);
        map.insert(Value::String("type_designator".into()), v);
        let de          = Value::Map(map).into_deserializer();
        match serde_path_to_error::deserialize(de) {
            Ok(ok)  => Ok(ok),
            Err(e)  => Err(format!("at `{}`: {}", e.path(), e.inner())),
        }

    }

    fn simple_value(&self) -> Option<&Self::Value> {
        self.type_designator.as_ref()
    }

    fn compact_value(&self) -> Option<Value> {
        let value = match serde_value::to_value(self) {
            Ok(v) => v,
            Err(_) => return None,
        };
        match value {
            Value::Map(mut map) => {
                map.remove(&Value::String("id".into()));
                Some(Value::Map(map))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct PrimitiveType {
    #[cfg_attr(feature = "serde", serde(rename = "@id"))]
    pub id: uriorcurie,
    #[cfg_attr(feature = "serde", serde(default))]
    #[cfg_attr(feature = "serde", serde(rename = "@type"))]
    #[cfg_attr(feature = "serde", serde(skip_serializing))]
    pub type_designator: Option<String>
}


#[cfg(feature = "serde")]
impl serde_utils::InlinedPair for PrimitiveType {
    type Key   = uriorcurie;
    type Value = String;
    type Error = String;

    fn extract_key(&self) -> &Self::Key {
        return &self.id;
    }

    fn from_pair_mapping(k: Self::Key, v: Value) -> Result<Self,Self::Error> {
        let mut map = match v {
            Value::Map(m) => m,
            _ => return Err("ClassDefinition must be a mapping".into()),
        };
        let key_value = serde_value::to_value(k.clone())
            .map_err(|e| format!("unable to serialize key: {}", e))?;
        map.insert(Value::String("id".into()), key_value);
        let de          = Value::Map(map).into_deserializer();
        match serde_path_to_error::deserialize(de) {
            Ok(ok)  => Ok(ok),
            Err(e)  => Err(format!("at `{}`: {}", e.path(), e.inner())),
        }
    }


    fn from_pair_simple(k: Self::Key, v: Value) -> Result<Self,Self::Error> {
        let mut map:  BTreeMap<Value, Value> = BTreeMap::new();
        let key_value = serde_value::to_value(k.clone())
            .map_err(|e| format!("unable to serialize key: {}", e))?;
        map.insert(Value::String("id".into()), key_value);
        map.insert(Value::String("type_designator".into()), v);
        let de          = Value::Map(map).into_deserializer();
        match serde_path_to_error::deserialize(de) {
            Ok(ok)  => Ok(ok),
            Err(e)  => Err(format!("at `{}`: {}", e.path(), e.inner())),
        }

    }

    fn simple_value(&self) -> Option<&Self::Value> {
        self.type_designator.as_ref()
    }

    fn compact_value(&self) -> Option<Value> {
        let value = match serde_value::to_value(self) {
            Ok(v) => v,
            Err(_) => return None,
        };
        match value {
            Value::Map(mut map) => {
                map.remove(&Value::String("id".into()));
                Some(Value::Map(map))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Constant {
    #[cfg_attr(feature = "serde", serde(rename = "@id"))]
    pub id: uriorcurie,
    #[cfg_attr(feature = "serde", serde(default))]
    #[cfg_attr(feature = "serde", serde(rename = "@type"))]
    #[cfg_attr(feature = "serde", serde(skip_serializing))]
    pub type_designator: Option<String>
}


#[cfg(feature = "serde")]
impl serde_utils::InlinedPair for Constant {
    type Key   = uriorcurie;
    type Value = String;
    type Error = String;

    fn extract_key(&self) -> &Self::Key {
        return &self.id;
    }

    fn from_pair_mapping(k: Self::Key, v: Value) -> Result<Self,Self::Error> {
        let mut map = match v {
            Value::Map(m) => m,
            _ => return Err("ClassDefinition must be a mapping".into()),
        };
        let key_value = serde_value::to_value(k.clone())
            .map_err(|e| format!("unable to serialize key: {}", e))?;
        map.insert(Value::String("id".into()), key_value);
        let de          = Value::Map(map).into_deserializer();
        match serde_path_to_error::deserialize(de) {
            Ok(ok)  => Ok(ok),
            Err(e)  => Err(format!("at `{}`: {}", e.path(), e.inner())),
        }
    }


    fn from_pair_simple(k: Self::Key, v: Value) -> Result<Self,Self::Error> {
        let mut map:  BTreeMap<Value, Value> = BTreeMap::new();
        let key_value = serde_value::to_value(k.clone())
            .map_err(|e| format!("unable to serialize key: {}", e))?;
        map.insert(Value::String("id".into()), key_value);
        map.insert(Value::String("type_designator".into()), v);
        let de          = Value::Map(map).into_deserializer();
        match serde_path_to_error::deserialize(de) {
            Ok(ok)  => Ok(ok),
            Err(e)  => Err(format!("at `{}`: {}", e.path(), e.inner())),
        }

    }

    fn simple_value(&self) -> Option<&Self::Value> {
        self.type_designator.as_ref()
    }

    fn compact_value(&self) -> Option<Value> {
        let value = match serde_value::to_value(self) {
            Ok(v) => v,
            Err(_) => return None,
        };
        match value {
            Value::Map(mut map) => {
                map.remove(&Value::String("id".into()));
                Some(Value::Map(map))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Variable {
    #[cfg_attr(feature = "serde", serde(rename = "@id"))]
    pub id: uriorcurie,
    #[cfg_attr(feature = "serde", serde(default))]
    #[cfg_attr(feature = "serde", serde(rename = "@type"))]
    #[cfg_attr(feature = "serde", serde(skip_serializing))]
    pub type_designator: Option<String>
}


#[cfg(feature = "serde")]
impl serde_utils::InlinedPair for Variable {
    type Key   = uriorcurie;
    type Value = String;
    type Error = String;

    fn extract_key(&self) -> &Self::Key {
        return &self.id;
    }

    fn from_pair_mapping(k: Self::Key, v: Value) -> Result<Self,Self::Error> {
        let mut map = match v {
            Value::Map(m) => m,
            _ => return Err("ClassDefinition must be a mapping".into()),
        };
        let key_value = serde_value::to_value(k.clone())
            .map_err(|e| format!("unable to serialize key: {}", e))?;
        map.insert(Value::String("id".into()), key_value);
        let de          = Value::Map(map).into_deserializer();
        match serde_path_to_error::deserialize(de) {
            Ok(ok)  => Ok(ok),
            Err(e)  => Err(format!("at `{}`: {}", e.path(), e.inner())),
        }
    }


    fn from_pair_simple(k: Self::Key, v: Value) -> Result<Self,Self::Error> {
        let mut map:  BTreeMap<Value, Value> = BTreeMap::new();
        let key_value = serde_value::to_value(k.clone())
            .map_err(|e| format!("unable to serialize key: {}", e))?;
        map.insert(Value::String("id".into()), key_value);
        map.insert(Value::String("type_designator".into()), v);
        let de          = Value::Map(map).into_deserializer();
        match serde_path_to_error::deserialize(de) {
            Ok(ok)  => Ok(ok),
            Err(e)  => Err(format!("at `{}`: {}", e.path(), e.inner())),
        }

    }

    fn simple_value(&self) -> Option<&Self::Value> {
        self.type_designator.as_ref()
    }

    fn compact_value(&self) -> Option<Value> {
        let value = match serde_value::to_value(self) {
            Ok(v) => v,
            Err(_) => return None,
        };
        match value {
            Value::Map(mut map) => {
                map.remove(&Value::String("id".into()));
                Some(Value::Map(map))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Macro {
    #[cfg_attr(feature = "serde", serde(rename = "@id"))]
    pub id: uriorcurie,
    #[cfg_attr(feature = "serde", serde(default))]
    #[cfg_attr(feature = "serde", serde(rename = "@type"))]
    #[cfg_attr(feature = "serde", serde(skip_serializing))]
    pub type_designator: Option<String>
}


#[cfg(feature = "serde")]
impl serde_utils::InlinedPair for Macro {
    type Key   = uriorcurie;
    type Value = String;
    type Error = String;

    fn extract_key(&self) -> &Self::Key {
        return &self.id;
    }

    fn from_pair_mapping(k: Self::Key, v: Value) -> Result<Self,Self::Error> {
        let mut map = match v {
            Value::Map(m) => m,
            _ => return Err("ClassDefinition must be a mapping".into()),
        };
        let key_value = serde_value::to_value(k.clone())
            .map_err(|e| format!("unable to serialize key: {}", e))?;
        map.insert(Value::String("id".into()), key_value);
        let de          = Value::Map(map).into_deserializer();
        match serde_path_to_error::deserialize(de) {
            Ok(ok)  => Ok(ok),
            Err(e)  => Err(format!("at `{}`: {}", e.path(), e.inner())),
        }
    }


    fn from_pair_simple(k: Self::Key, v: Value) -> Result<Self,Self::Error> {
        let mut map:  BTreeMap<Value, Value> = BTreeMap::new();
        let key_value = serde_value::to_value(k.clone())
            .map_err(|e| format!("unable to serialize key: {}", e))?;
        map.insert(Value::String("id".into()), key_value);
        map.insert(Value::String("type_designator".into()), v);
        let de          = Value::Map(map).into_deserializer();
        match serde_path_to_error::deserialize(de) {
            Ok(ok)  => Ok(ok),
            Err(e)  => Err(format!("at `{}`: {}", e.path(), e.inner())),
        }

    }

    fn simple_value(&self) -> Option<&Self::Value> {
        self.type_designator.as_ref()
    }

    fn compact_value(&self) -> Option<Value> {
        let value = match serde_value::to_value(self) {
            Ok(v) => v,
            Err(_) => return None,
        };
        match value {
            Value::Map(mut map) => {
                map.remove(&Value::String("id".into()));
                Some(Value::Map(map))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct FieldKind {
    #[cfg_attr(feature = "serde", serde(rename = "@id"))]
    pub id: uriorcurie,
    #[cfg_attr(feature = "serde", serde(default))]
    #[cfg_attr(feature = "serde", serde(rename = "@type"))]
    #[cfg_attr(feature = "serde", serde(skip_serializing))]
    pub type_designator: Option<String>
}


#[cfg(feature = "serde")]
impl serde_utils::InlinedPair for FieldKind {
    type Key   = uriorcurie;
    type Value = String;
    type Error = String;

    fn extract_key(&self) -> &Self::Key {
        return &self.id;
    }

    fn from_pair_mapping(k: Self::Key, v: Value) -> Result<Self,Self::Error> {
        let mut map = match v {
            Value::Map(m) => m,
            _ => return Err("ClassDefinition must be a mapping".into()),
        };
        let key_value = serde_value::to_value(k.clone())
            .map_err(|e| format!("unable to serialize key: {}", e))?;
        map.insert(Value::String("id".into()), key_value);
        let de          = Value::Map(map).into_deserializer();
        match serde_path_to_error::deserialize(de) {
            Ok(ok)  => Ok(ok),
            Err(e)  => Err(format!("at `{}`: {}", e.path(), e.inner())),
        }
    }


    fn from_pair_simple(k: Self::Key, v: Value) -> Result<Self,Self::Error> {
        let mut map:  BTreeMap<Value, Value> = BTreeMap::new();
        let key_value = serde_value::to_value(k.clone())
            .map_err(|e| format!("unable to serialize key: {}", e))?;
        map.insert(Value::String("id".into()), key_value);
        map.insert(Value::String("type_designator".into()), v);
        let de          = Value::Map(map).into_deserializer();
        match serde_path_to_error::deserialize(de) {
            Ok(ok)  => Ok(ok),
            Err(e)  => Err(format!("at `{}`: {}", e.path(), e.inner())),
        }

    }

    fn simple_value(&self) -> Option<&Self::Value> {
        self.type_designator.as_ref()
    }

    fn compact_value(&self) -> Option<Value> {
        let value = match serde_value::to_value(self) {
            Ok(v) => v,
            Err(_) => return None,
        };
        match value {
            Value::Map(mut map) => {
                map.remove(&Value::String("id".into()));
                Some(Value::Map(map))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Event {
    #[cfg_attr(feature = "serde", serde(rename = "@id"))]
    pub id: uriorcurie,
    #[cfg_attr(feature = "serde", serde(default))]
    #[cfg_attr(feature = "serde", serde(rename = "@type"))]
    #[cfg_attr(feature = "serde", serde(skip_serializing))]
    pub type_designator: Option<String>
}


#[cfg(feature = "serde")]
impl serde_utils::InlinedPair for Event {
    type Key   = uriorcurie;
    type Value = String;
    type Error = String;

    fn extract_key(&self) -> &Self::Key {
        return &self.id;
    }

    fn from_pair_mapping(k: Self::Key, v: Value) -> Result<Self,Self::Error> {
        let mut map = match v {
            Value::Map(m) => m,
            _ => return Err("ClassDefinition must be a mapping".into()),
        };
        let key_value = serde_value::to_value(k.clone())
            .map_err(|e| format!("unable to serialize key: {}", e))?;
        map.insert(Value::String("id".into()), key_value);
        let de          = Value::Map(map).into_deserializer();
        match serde_path_to_error::deserialize(de) {
            Ok(ok)  => Ok(ok),
            Err(e)  => Err(format!("at `{}`: {}", e.path(), e.inner())),
        }
    }


    fn from_pair_simple(k: Self::Key, v: Value) -> Result<Self,Self::Error> {
        let mut map:  BTreeMap<Value, Value> = BTreeMap::new();
        let key_value = serde_value::to_value(k.clone())
            .map_err(|e| format!("unable to serialize key: {}", e))?;
        map.insert(Value::String("id".into()), key_value);
        map.insert(Value::String("type_designator".into()), v);
        let de          = Value::Map(map).into_deserializer();
        match serde_path_to_error::deserialize(de) {
            Ok(ok)  => Ok(ok),
            Err(e)  => Err(format!("at `{}`: {}", e.path(), e.inner())),
        }

    }

    fn simple_value(&self) -> Option<&Self::Value> {
        self.type_designator.as_ref()
    }

    fn compact_value(&self) -> Option<Value> {
        let value = match serde_value::to_value(self) {
            Ok(v) => v,
            Err(_) => return None,
        };
        match value {
            Value::Map(mut map) => {
                map.remove(&Value::String("id".into()));
                Some(Value::Map(map))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Function {
    pub name: String,
    #[cfg_attr(feature = "serde", serde(default))]
    pub visibility: Option<Visibility>,
    pub implemented: bool,
    #[cfg_attr(feature = "serde", serde(default))]
    pub input_parameters: Option<Vec<Parameter>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub output_parameters: Option<Vec<Parameter>>,
    #[cfg_attr(feature = "serde", serde(
        deserialize_with = "serde_utils::deserialize_primitive_list_or_single_value_optional",
        serialize_with = "serde_utils::serialize_primitive_list_or_single_value_optional"
    ))]
    #[cfg_attr(feature = "serde", serde(default))]
    pub attributes: Option<Vec<FunctionAttribute>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub generics: Option<Generics>,
    #[cfg_attr(feature = "serde", serde(rename = "@id"))]
    pub id: uriorcurie,
    #[cfg_attr(feature = "serde", serde(default))]
    #[cfg_attr(feature = "serde", serde(rename = "@type"))]
    #[cfg_attr(feature = "serde", serde(skip_serializing))]
    pub type_designator: Option<String>
}


#[cfg(feature = "serde")]
impl serde_utils::InlinedPair for Function {
    type Key   = uriorcurie;
    type Value = Value;
    type Error = String;

    fn extract_key(&self) -> &Self::Key {
        return &self.id;
    }

    fn from_pair_mapping(k: Self::Key, v: Value) -> Result<Self,Self::Error> {
        let mut map = match v {
            Value::Map(m) => m,
            _ => return Err("ClassDefinition must be a mapping".into()),
        };
        let key_value = serde_value::to_value(k.clone())
            .map_err(|e| format!("unable to serialize key: {}", e))?;
        map.insert(Value::String("id".into()), key_value);
        let de          = Value::Map(map).into_deserializer();
        match serde_path_to_error::deserialize(de) {
            Ok(ok)  => Ok(ok),
            Err(e)  => Err(format!("at `{}`: {}", e.path(), e.inner())),
        }
    }


    fn from_pair_simple(_k: Self::Key, _v: Value) -> Result<Self,Self::Error> {
        Err("Cannot create a Function from a primitive value!".into())
    }


    fn compact_value(&self) -> Option<Value> {
        let value = match serde_value::to_value(self) {
            Ok(v) => v,
            Err(_) => return None,
        };
        match value {
            Value::Map(mut map) => {
                map.remove(&Value::String("id".into()));
                Some(Value::Map(map))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct RecordType {
    pub name: String,
    #[cfg_attr(feature = "serde", serde(default))]
    pub visibility: Option<Visibility>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub record_kind: Option<RecordKind>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub fields: Option<Vec<RecordField>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub generics: Option<Generics>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub methods: Option<Vec<FunctionDetail>>,
    #[cfg_attr(feature = "serde", serde(rename = "@id"))]
    pub id: uriorcurie,
    #[cfg_attr(feature = "serde", serde(default))]
    #[cfg_attr(feature = "serde", serde(rename = "@type"))]
    #[cfg_attr(feature = "serde", serde(skip_serializing))]
    pub type_designator: Option<String>
}


#[cfg(feature = "serde")]
impl serde_utils::InlinedPair for RecordType {
    type Key   = uriorcurie;
    type Value = Value;
    type Error = String;

    fn extract_key(&self) -> &Self::Key {
        return &self.id;
    }

    fn from_pair_mapping(k: Self::Key, v: Value) -> Result<Self,Self::Error> {
        let mut map = match v {
            Value::Map(m) => m,
            _ => return Err("ClassDefinition must be a mapping".into()),
        };
        let key_value = serde_value::to_value(k.clone())
            .map_err(|e| format!("unable to serialize key: {}", e))?;
        map.insert(Value::String("id".into()), key_value);
        let de          = Value::Map(map).into_deserializer();
        match serde_path_to_error::deserialize(de) {
            Ok(ok)  => Ok(ok),
            Err(e)  => Err(format!("at `{}`: {}", e.path(), e.inner())),
        }
    }


    fn from_pair_simple(_k: Self::Key, _v: Value) -> Result<Self,Self::Error> {
        Err("Cannot create a RecordType from a primitive value!".into())
    }


    fn compact_value(&self) -> Option<Value> {
        let value = match serde_value::to_value(self) {
            Ok(v) => v,
            Err(_) => return None,
        };
        match value {
            Value::Map(mut map) => {
                map.remove(&Value::String("id".into()));
                Some(Value::Map(map))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct SumType {
    #[cfg_attr(feature = "serde", serde(default))]
    pub variants: Option<Vec<SumVariant>>,
    #[cfg_attr(feature = "serde", serde(rename = "@id"))]
    pub id: uriorcurie,
    #[cfg_attr(feature = "serde", serde(default))]
    #[cfg_attr(feature = "serde", serde(rename = "@type"))]
    #[cfg_attr(feature = "serde", serde(skip_serializing))]
    pub type_designator: Option<String>
}


#[cfg(feature = "serde")]
impl serde_utils::InlinedPair for SumType {
    type Key   = uriorcurie;
    type Value = Value;
    type Error = String;

    fn extract_key(&self) -> &Self::Key {
        return &self.id;
    }

    fn from_pair_mapping(k: Self::Key, v: Value) -> Result<Self,Self::Error> {
        let mut map = match v {
            Value::Map(m) => m,
            _ => return Err("ClassDefinition must be a mapping".into()),
        };
        let key_value = serde_value::to_value(k.clone())
            .map_err(|e| format!("unable to serialize key: {}", e))?;
        map.insert(Value::String("id".into()), key_value);
        let de          = Value::Map(map).into_deserializer();
        match serde_path_to_error::deserialize(de) {
            Ok(ok)  => Ok(ok),
            Err(e)  => Err(format!("at `{}`: {}", e.path(), e.inner())),
        }
    }


    fn from_pair_simple(k: Self::Key, _v: Value) -> Result<Self,Self::Error> {
        let mut map:  BTreeMap<Value, Value> = BTreeMap::new();
        let key_value = serde_value::to_value(k.clone())
            .map_err(|e| format!("unable to serialize key: {}", e))?;
        map.insert(Value::String("id".into()), key_value);
        let de          = Value::Map(map).into_deserializer();
        match serde_path_to_error::deserialize(de) {
            Ok(ok)  => Ok(ok),
            Err(e)  => Err(format!("at `{}`: {}", e.path(), e.inner())),
        }


    }


    fn compact_value(&self) -> Option<Value> {
        let value = match serde_value::to_value(self) {
            Ok(v) => v,
            Err(_) => return None,
        };
        match value {
            Value::Map(mut map) => {
                map.remove(&Value::String("id".into()));
                Some(Value::Map(map))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct UnionType {
    #[cfg_attr(feature = "serde", serde(default))]
    pub types: Option<Vec<TypeExpression>>,
    #[cfg_attr(feature = "serde", serde(rename = "@id"))]
    pub id: uriorcurie,
    #[cfg_attr(feature = "serde", serde(default))]
    #[cfg_attr(feature = "serde", serde(rename = "@type"))]
    #[cfg_attr(feature = "serde", serde(skip_serializing))]
    pub type_designator: Option<String>
}


#[cfg(feature = "serde")]
impl serde_utils::InlinedPair for UnionType {
    type Key   = uriorcurie;
    type Value = Value;
    type Error = String;

    fn extract_key(&self) -> &Self::Key {
        return &self.id;
    }

    fn from_pair_mapping(k: Self::Key, v: Value) -> Result<Self,Self::Error> {
        let mut map = match v {
            Value::Map(m) => m,
            _ => return Err("ClassDefinition must be a mapping".into()),
        };
        let key_value = serde_value::to_value(k.clone())
            .map_err(|e| format!("unable to serialize key: {}", e))?;
        map.insert(Value::String("id".into()), key_value);
        let de          = Value::Map(map).into_deserializer();
        match serde_path_to_error::deserialize(de) {
            Ok(ok)  => Ok(ok),
            Err(e)  => Err(format!("at `{}`: {}", e.path(), e.inner())),
        }
    }


    fn from_pair_simple(k: Self::Key, _v: Value) -> Result<Self,Self::Error> {
        let mut map:  BTreeMap<Value, Value> = BTreeMap::new();
        let key_value = serde_value::to_value(k.clone())
            .map_err(|e| format!("unable to serialize key: {}", e))?;
        map.insert(Value::String("id".into()), key_value);
        let de          = Value::Map(map).into_deserializer();
        match serde_path_to_error::deserialize(de) {
            Ok(ok)  => Ok(ok),
            Err(e)  => Err(format!("at `{}`: {}", e.path(), e.inner())),
        }


    }


    fn compact_value(&self) -> Option<Value> {
        let value = match serde_value::to_value(self) {
            Ok(v) => v,
            Err(_) => return None,
        };
        match value {
            Value::Map(mut map) => {
                map.remove(&Value::String("id".into()));
                Some(Value::Map(map))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TraitDef {
    pub name: String,
    #[cfg_attr(feature = "serde", serde(default))]
    pub visibility: Option<Visibility>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub generics: Option<Generics>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub super_traits: Option<Vec<TraitRef>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub associated_types: Option<Vec<AssociatedType>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub required_methods: Option<Vec<TraitMethod>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub provided_methods: Option<Vec<TraitMethod>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub required_constants: Option<Vec<TraitConstant>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub docs: Option<String>,
    #[cfg_attr(feature = "serde", serde(rename = "@id"))]
    pub id: uriorcurie,
    #[cfg_attr(feature = "serde", serde(default))]
    #[cfg_attr(feature = "serde", serde(rename = "@type"))]
    #[cfg_attr(feature = "serde", serde(skip_serializing))]
    pub type_designator: Option<String>
}


#[cfg(feature = "serde")]
impl serde_utils::InlinedPair for TraitDef {
    type Key   = uriorcurie;
    type Value = Value;
    type Error = String;

    fn extract_key(&self) -> &Self::Key {
        return &self.id;
    }

    fn from_pair_mapping(k: Self::Key, v: Value) -> Result<Self,Self::Error> {
        let mut map = match v {
            Value::Map(m) => m,
            _ => return Err("ClassDefinition must be a mapping".into()),
        };
        let key_value = serde_value::to_value(k.clone())
            .map_err(|e| format!("unable to serialize key: {}", e))?;
        map.insert(Value::String("id".into()), key_value);
        let de          = Value::Map(map).into_deserializer();
        match serde_path_to_error::deserialize(de) {
            Ok(ok)  => Ok(ok),
            Err(e)  => Err(format!("at `{}`: {}", e.path(), e.inner())),
        }
    }


    fn from_pair_simple(_k: Self::Key, _v: Value) -> Result<Self,Self::Error> {
        Err("Cannot create a TraitDef from a primitive value!".into())
    }


    fn compact_value(&self) -> Option<Value> {
        let value = match serde_value::to_value(self) {
            Ok(v) => v,
            Err(_) => return None,
        };
        match value {
            Value::Map(mut map) => {
                map.remove(&Value::String("id".into()));
                Some(Value::Map(map))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TraitImpl {
    pub trait_ref: TraitRef,
    pub for_type: TypeExpression,
    #[cfg_attr(feature = "serde", serde(default))]
    pub visibility: Option<Visibility>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub generics: Option<Generics>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub methods: Option<Vec<FunctionDetail>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub associated_types: Option<Vec<AssociatedType>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub associated_constants: Option<Vec<TraitConstant>>,
    pub is_negative: bool,
    pub is_blanket: bool,
    pub is_unsafe: bool,
    #[cfg_attr(feature = "serde", serde(default))]
    pub docs: Option<String>,
    #[cfg_attr(feature = "serde", serde(rename = "@id"))]
    pub id: uriorcurie,
    #[cfg_attr(feature = "serde", serde(default))]
    #[cfg_attr(feature = "serde", serde(rename = "@type"))]
    #[cfg_attr(feature = "serde", serde(skip_serializing))]
    pub type_designator: Option<String>
}


#[cfg(feature = "serde")]
impl serde_utils::InlinedPair for TraitImpl {
    type Key   = uriorcurie;
    type Value = Value;
    type Error = String;

    fn extract_key(&self) -> &Self::Key {
        return &self.id;
    }

    fn from_pair_mapping(k: Self::Key, v: Value) -> Result<Self,Self::Error> {
        let mut map = match v {
            Value::Map(m) => m,
            _ => return Err("ClassDefinition must be a mapping".into()),
        };
        let key_value = serde_value::to_value(k.clone())
            .map_err(|e| format!("unable to serialize key: {}", e))?;
        map.insert(Value::String("id".into()), key_value);
        let de          = Value::Map(map).into_deserializer();
        match serde_path_to_error::deserialize(de) {
            Ok(ok)  => Ok(ok),
            Err(e)  => Err(format!("at `{}`: {}", e.path(), e.inner())),
        }
    }


    fn from_pair_simple(_k: Self::Key, _v: Value) -> Result<Self,Self::Error> {
        Err("Cannot create a TraitImpl from a primitive value!".into())
    }


    fn compact_value(&self) -> Option<Value> {
        let value = match serde_value::to_value(self) {
            Ok(v) => v,
            Err(_) => return None,
        };
        match value {
            Value::Map(mut map) => {
                map.remove(&Value::String("id".into()));
                Some(Value::Map(map))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TypeAlias {
    pub aliased_type: TypeExpression,
    #[cfg_attr(feature = "serde", serde(rename = "@id"))]
    pub id: uriorcurie,
    #[cfg_attr(feature = "serde", serde(default))]
    #[cfg_attr(feature = "serde", serde(rename = "@type"))]
    #[cfg_attr(feature = "serde", serde(skip_serializing))]
    pub type_designator: Option<String>
}


#[cfg(feature = "serde")]
impl serde_utils::InlinedPair for TypeAlias {
    type Key   = uriorcurie;
    type Value = Value;
    type Error = String;

    fn extract_key(&self) -> &Self::Key {
        return &self.id;
    }

    fn from_pair_mapping(k: Self::Key, v: Value) -> Result<Self,Self::Error> {
        let mut map = match v {
            Value::Map(m) => m,
            _ => return Err("ClassDefinition must be a mapping".into()),
        };
        let key_value = serde_value::to_value(k.clone())
            .map_err(|e| format!("unable to serialize key: {}", e))?;
        map.insert(Value::String("id".into()), key_value);
        let de          = Value::Map(map).into_deserializer();
        match serde_path_to_error::deserialize(de) {
            Ok(ok)  => Ok(ok),
            Err(e)  => Err(format!("at `{}`: {}", e.path(), e.inner())),
        }
    }


    fn from_pair_simple(_k: Self::Key, _v: Value) -> Result<Self,Self::Error> {
        Err("Cannot create a TypeAlias from a primitive value!".into())
    }


    fn compact_value(&self) -> Option<Value> {
        let value = match serde_value::to_value(self) {
            Ok(v) => v,
            Err(_) => return None,
        };
        match value {
            Value::Map(mut map) => {
                map.remove(&Value::String("id".into()));
                Some(Value::Map(map))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Parameter {
    pub name: String,
    #[cfg_attr(feature = "serde", serde(default))]
    pub ty: Option<Box<TypeExpression>>,
    #[cfg_attr(feature = "serde", serde(
        deserialize_with = "serde_utils::deserialize_primitive_list_or_single_value_optional",
        serialize_with = "serde_utils::serialize_primitive_list_or_single_value_optional"
    ))]
    #[cfg_attr(feature = "serde", serde(default))]
    pub attributes: Option<Vec<ParameterAttribute>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub default_value: Option<String>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub description: Option<String>
}



#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct RecordField {
    #[cfg_attr(feature = "serde", serde(default))]
    pub name: Option<String>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub ty: Option<TypeExpression>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub default_value: Option<String>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub attributes: Option<FieldAttribute>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub visibility: Option<Visibility>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub type_entry_id: Option<isize>
}



#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct SumVariant {
    pub name: String,
    #[cfg_attr(feature = "serde", serde(default))]
    pub types: Option<Vec<Box<TypeExpression>>>
}



#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TypeExpression {
    pub type_tag: String,
    #[cfg_attr(feature = "serde", serde(default))]
    pub path: Option<String>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub generic_args: Option<Vec<Box<GenericArg>>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub param_name: Option<String>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub primitive: Option<PrimitiveKind>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub inner_type: Option<Box<TypeExpression>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub element_types: Option<Vec<Box<TypeExpression>>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub array_length: Option<isize>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub is_mutable: Option<bool>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub lifetime: Option<String>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub fn_inputs: Option<Vec<Box<Parameter>>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub fn_outputs: Option<Vec<Box<Parameter>>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub fn_generic_params: Option<Vec<TypeParam>>,
    #[cfg_attr(feature = "serde", serde(
        deserialize_with = "serde_utils::deserialize_primitive_list_or_single_value_optional",
        serialize_with = "serde_utils::serialize_primitive_list_or_single_value_optional"
    ))]
    #[cfg_attr(feature = "serde", serde(default))]
    pub fn_attributes: Option<Vec<FunctionAttribute>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub dyn_traits: Option<Vec<PolyTrait>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub sum_variants: Option<Vec<Box<SumVariant>>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub generic_bounds: Option<Vec<GenericBound>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub qualified_name: Option<String>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub self_type: Option<Box<TypeExpression>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub trait_path: Option<String>
}



#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct GenericArg {
    pub arg_kind: String,
    #[cfg_attr(feature = "serde", serde(default))]
    pub type_value: Option<Box<TypeExpression>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub const_expr: Option<String>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub lifetime: Option<String>
}



#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Generics {
    #[cfg_attr(feature = "serde", serde(default))]
    pub type_params: Option<Vec<TypeParam>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub const_params: Option<Vec<ConstParam>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub lifetime_params: Option<Vec<LifetimeParam>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub constraints: Option<Vec<ConstraintExpr>>
}



#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TypeParam {
    pub name: String,
    #[cfg_attr(feature = "serde", serde(default))]
    pub kind: Option<TypeKind>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub variance: Option<Variance>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub default_type: Option<TypeExprSimple>
}



#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ConstParam {
    pub name: String,
    #[cfg_attr(feature = "serde", serde(default))]
    pub ty: Option<TypeExprSimple>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub default_value: Option<String>
}



#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct LifetimeParam {
    pub name: String,
    #[cfg_attr(feature = "serde", serde(default))]
    pub variance: Option<Variance>
}



#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ConstraintExpr {
    pub constraint_kind: String,
    #[cfg_attr(feature = "serde", serde(default))]
    pub param: Option<String>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub trait_ref: Option<TraitRef>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub assoc_name: Option<String>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub bound: Option<TypeExprSimple>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub kind_signature: Option<String>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub shorter: Option<String>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub longer: Option<String>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub const_expr: Option<String>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub predicate_expr: Option<String>
}



#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TypeExprSimple {
    pub name: String,
    #[cfg_attr(feature = "serde", serde(default))]
    pub args: Option<Vec<Box<TypeExprSimple>>>
}



#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TraitRef {
    pub name: String,
    #[cfg_attr(feature = "serde", serde(default))]
    pub args: Option<Vec<TypeExprSimple>>
}



#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct AssociatedType {
    pub name: String,
    #[cfg_attr(feature = "serde", serde(default))]
    pub bounds: Option<Vec<GenericBound>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub default_type: Option<TypeExpression>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub docs: Option<String>
}



#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct GenericBound {
    pub bound_kind: String,
    #[cfg_attr(feature = "serde", serde(default))]
    pub trait_ref: Option<TraitRef>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub lifetime: Option<String>
}



#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TraitMethod {
    pub name: String,
    #[cfg_attr(feature = "serde", serde(default))]
    pub parameters: Option<Vec<Parameter>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub return_type: Option<TypeExpression>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub generics: Option<Generics>,
    #[cfg_attr(feature = "serde", serde(
        deserialize_with = "serde_utils::deserialize_primitive_list_or_single_value_optional",
        serialize_with = "serde_utils::serialize_primitive_list_or_single_value_optional"
    ))]
    #[cfg_attr(feature = "serde", serde(default))]
    pub attributes: Option<Vec<FunctionAttribute>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub receiver: Option<ReceiverKind>,
    pub has_default_implementation: bool,
    #[cfg_attr(feature = "serde", serde(default))]
    pub docs: Option<String>
}



#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TraitConstant {
    pub name: String,
    pub ty: TypeExpression,
    #[cfg_attr(feature = "serde", serde(default))]
    pub default_value: Option<String>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub docs: Option<String>
}



#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TraitAttributeValue {
    pub kind: TraitAttributeKind,
    #[cfg_attr(feature = "serde", serde(default))]
    pub custom_name: Option<String>,
    #[cfg_attr(feature = "serde", serde(
        deserialize_with = "serde_utils::deserialize_primitive_list_or_single_value_optional",
        serialize_with = "serde_utils::serialize_primitive_list_or_single_value_optional"
    ))]
    #[cfg_attr(feature = "serde", serde(default))]
    pub custom_args: Option<Vec<String>>
}



#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct FunctionDetail {
    pub name: String,
    #[cfg_attr(feature = "serde", serde(default))]
    pub visibility: Option<Visibility>,
    pub implemented: bool,
    #[cfg_attr(feature = "serde", serde(default))]
    pub input_parameters: Option<Vec<Parameter>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub output_parameters: Option<Vec<Parameter>>,
    #[cfg_attr(feature = "serde", serde(
        deserialize_with = "serde_utils::deserialize_primitive_list_or_single_value_optional",
        serialize_with = "serde_utils::serialize_primitive_list_or_single_value_optional"
    ))]
    #[cfg_attr(feature = "serde", serde(default))]
    pub attributes: Option<Vec<FunctionAttribute>>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub generics: Option<Generics>
}



#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct AssociatedTypeImpl {
    pub name: String,
    pub ty: TypeExpression
}



#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct PolyTrait {
    pub tr: TraitRef,
    #[cfg_attr(feature = "serde", serde(
        deserialize_with = "serde_utils::deserialize_primitive_list_or_single_value_optional",
        serialize_with = "serde_utils::serialize_primitive_list_or_single_value_optional"
    ))]
    #[cfg_attr(feature = "serde", serde(default))]
    pub lifetimes: Option<Vec<String>>
}
