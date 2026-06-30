//! The TerminusDB document model for the nudox IR — a faithful, document-shaped
//! mirror of the `ir` crate. `#[derive(TerminusDBModel)]` generates both the
//! schema (class/enum/tagged-union definitions) and the instance JSON-LD, so
//! there is no hand-written emitter or serde glue.
//!
//! Shape conventions:
//! - `Entry` is the single top-level document, keyed lexically on `fq_name`; it
//!   embeds its structural shape inline as the [`KindData`] tagged union. Every
//!   other type is a `subdocument` (embedded by value).
//! - Recursive data enums (e.g. [`Type`], [`ConstExpr`]) use *newtype* variants
//!   wrapping named structs/enums — never inline `Variant { … }` struct variants.
//!   The derive's schema-tree walker shares its dedup set across newtype payloads
//!   but not across inline-struct variants, so only the newtype form terminates
//!   on self-reference.
//! - IR `Option<Vec<T>>` collapses to `Vec<T>` (TDB List) or `BTreeSet<T>` (TDB
//!   Set); genuinely optional singles stay `Option<T>`.

use std::collections::BTreeSet;

// The derive emits Serialize/Deserialize and calls that need these traits in
// scope at the definition site (anyhow/serde_json/tracing are crate deps).
#[allow(unused_imports)]
use anyhow::Context as _;
#[allow(unused_imports)]
use terminusdb_schema::{FromTDBInstance, ToTDBInstance, ToTDBSchema};
use terminusdb_schema_derive::TerminusDBModel;

// ───────────────────────────── leaf enums ─────────────────────────────

/// Visibility of a symbol in its source language.
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
pub enum Visibility {
    Public,
    Private,
    Protected,
    Internal,
    Package,
}

/// Integer / float bit width.
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
pub enum Width {
    W8,
    W16,
    W32,
    W64,
    W128,
    Arch,
}

/// Subtyping variance of a type or lifetime parameter.
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
pub enum Variance {
    Covariant,
    Contravariant,
    Invariant,
    Bivariant,
}

/// Receiver / self-parameter kind of a method-like function.
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
pub enum ReceiverKind {
    Owned,
    SharedRef,
    MutRef,
    Static,
    Arbitrary,
}

/// How/where a type parameter was introduced.
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
pub enum TypeParamOrigin {
    Free,
    Associated,
    Inferred,
}

/// Calling-convention / modifier flags on a value parameter.
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
pub enum ParameterAttribute {
    Inout,
    Mutable,
    Consuming,
    Borrowing,
    Isolated,
    Variadic,
    Optional,
}

/// Attributes applied to a function.
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
pub enum FunctionAttribute {
    Variadic,
    Generator,
    Const,
    Pure,
    Async,
    Unsafe,
}

/// Mapped-type modifier prefix (`+`/`-`/preserve).
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
pub enum ModifierPrefix {
    Preserve,
    Add,
    Remove,
}

/// Binary operator inside a [`ConstExpr`].
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

/// Unary operator inside a [`ConstExpr`].
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
pub enum UnaryOp {
    Neg,
    Not,
    Ref,
    Deref,
}

/// Language-level primitive type. A tagged-union payload type may only appear in
/// one variant per union, so each width-carrying numeric kind wraps its own
/// single-field struct.
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
pub enum Primitive {
    Int(IntType),
    UInt(UIntType),
    Float(FloatType),
    Bool,
    String,
    Char,
    Bytes,
    Date,
    Address,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct IntType {
    pub width: Width,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct UIntType {
    pub width: Width,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct FloatType {
    pub width: Width,
}

// ───────────────────────────── types ─────────────────────────────

/// Universal type representation (mirror of `ir::ty::Type`).
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub enum Type {
    TypeReference(TyReference),
    SelfType,
    DynTrait(DynTrait),
    GenericParam(GenericParam),
    Primitive(Primitive),
    FunctionPointer(FunctionPointer),
    Tuple(TyTuple),
    RecordLiteral(Record),
    Slice(TySlice),
    Array(TyArray),
    ImplTrait(TyImplTrait),
    Infer,
    Never,
    Any,
    RawPointer(TyRawPointer),
    BorrowedRef(TyBorrowedRef),
    Union(TyUnion),
    Intersection(TyIntersection),
    Sum(TySum),
    QualifiedPath(QualifiedPath),
    Variadic(TyVariadic),
    TypeOperator(TypeOperator),
    Conditional(ConditionalType),
    Mapped(MappedType),
    Predicate(TypePredicate),
}

// Tuple / Union / Intersection each carry a `Vec<Type>` but, as tagged-union
// payloads of `Type`, must be distinct types.
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct TyTuple {
    pub types: Vec<Type>,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct TyUnion {
    pub types: Vec<Type>,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct TyIntersection {
    pub types: Vec<Type>,
}

// Slice / Variadic each wrap a single inner type; distinct types for the same
// reason as above.
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct TySlice {
    pub inner: Box<Type>,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct TyVariadic {
    pub inner: Box<Type>,
}

/// A single boxed inner type (used by [`Term::Equality`]).
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct TypeBox {
    pub inner: Box<Type>,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct TyReference {
    pub identifier: String,
    pub generic_args: Vec<GenericArg>,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct DynTrait {
    pub traits: Vec<PolyTrait>,
    pub lifetime: Option<String>,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct PolyTrait {
    pub trait_ref: TraitRef,
    pub lifetimes: Vec<String>,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct GenericParam {
    pub name: String,
    pub kind: Option<Box<Type>>,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct FunctionPointer {
    pub inputs: Vec<Parameter>,
    pub outputs: Vec<Parameter>,
    pub attributes: Vec<FunctionAttribute>,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct TyArray {
    pub element: Box<Type>,
    pub length: i64,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct TyImplTrait {
    pub bounds: Vec<GenericBound>,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct TyRawPointer {
    pub is_mutable: bool,
    pub target: Box<Type>,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct TyBorrowedRef {
    pub lifetime: Option<String>,
    pub is_mutable: bool,
    pub target: Box<Type>,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct TySum {
    pub variants: Vec<SumVariant>,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct QualifiedPath {
    pub name: String,
    pub generic_arguments: Vec<GenericArg>,
    pub self_type: Box<Type>,
    pub tr: Option<TyReference>,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct TypeOperator {
    pub operator: String,
    pub target: Box<Type>,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct ConditionalType {
    pub check_type: Box<Type>,
    pub extends_type: Box<Type>,
    pub true_type: Box<Type>,
    pub false_type: Box<Type>,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct MappedType {
    pub readonly: Option<ModifierPrefix>,
    pub optional: Option<ModifierPrefix>,
    pub parameter: String,
    pub source_type: Box<Type>,
    pub name_type: Option<Box<Type>>,
    pub value_type: Option<Box<Type>>,
}

/// Subject of a type predicate.
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub enum PredicateSubject {
    This,
    Identifier(String),
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct TypePredicate {
    pub asserts: bool,
    pub subject: PredicateSubject,
    pub target: Option<Box<Type>>,
}

// ───────────────────────── const expressions ─────────────────────────

/// Compile-time constant expression (mirror of `ir::generics::ConstExpr`).
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub enum ConstExpr {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    // `Str` already claims the `String` payload for this union, so `Var` wraps.
    Var(ConstVar),
    BinOp(ConstBinOp),
    UnaryOp(ConstUnaryOp),
    Call(ConstCall),
    Ascription(ConstAscription),
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct ConstVar {
    pub name: String,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct ConstBinOp {
    pub op: BinOp,
    pub lhs: Box<ConstExpr>,
    pub rhs: Box<ConstExpr>,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct ConstUnaryOp {
    pub op: UnaryOp,
    pub operand: Box<ConstExpr>,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct ConstCall {
    pub func: String,
    pub args: Vec<ConstExpr>,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct ConstAscription {
    pub expr: Box<ConstExpr>,
    pub ty: Box<TypeExpr>,
}

// ───────────────────────── kinds & generics ─────────────────────────

/// The kind of a type / type constructor (`*`, `* -> *`, …).
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub enum Kind {
    Type,
    Constraint,
    Row,
    Arrow(KindArrow),
    Var(String),
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct KindArrow {
    pub from: Box<Kind>,
    pub to: Box<Kind>,
}

/// A simple generic type expression (`name` + recursive `args`).
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct TypeExpr {
    pub name: String,
    pub args: Vec<TypeExpr>,
}

/// A (possibly parameterised) reference to a trait / protocol.
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct TraitRef {
    pub name: String,
    pub args: Vec<TypeExpr>,
}

/// A single generic argument at an instantiation site.
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub enum GenericArg {
    Type(Type),
    ConstExpr(ConstExpr),
    Lifetime(String),
    Constraint(Constraint),
    // `Lifetime` already claims the `String` payload for this union, so `Module`
    // (an ML-family functor module path) wraps.
    Module(ArgModule),
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct ArgModule {
    pub path: String,
}

/// A bound on a generic or associated type.
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub enum GenericBound {
    Trait(TraitRef),
    Lifetime(String),
}

/// A where-clause / generic constraint (mirror of `ir::generics::Constraint`).
/// `LogicalPredicate` keeps its structured tree flattened to a string, as in the
/// original projection.
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub enum Constraint {
    TraitBound(CTraitBound),
    AssociatedTypeBound(CAssocTypeBound),
    HigherKindedBound(CHigherKinded),
    AssociatedItem(CAssocItem),
    LifetimeBound(CLifetimeBound),
    ConstExprBound(CConstExprBound),
    LogicalPredicate(CLogicalPredicate),
    FunctionalDependency(CFunctionalDependency),
    ImplicitBound(CImplicitBound),
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct CTraitBound {
    pub param: String,
    pub trait_ref: TraitRef,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct CAssocTypeBound {
    pub param: String,
    pub assoc_name: String,
    pub bound: TypeExpr,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct CHigherKinded {
    pub param: String,
    pub kind: Kind,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct CAssocItem {
    pub name: String,
    pub args: Vec<GenericArg>,
    pub term: Term,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct CLifetimeBound {
    pub shorter: String,
    pub longer: String,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct CConstExprBound {
    pub param: String,
    pub expr: ConstExpr,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct CLogicalPredicate {
    pub predicate_expr: String,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct CFunctionalDependency {
    pub sources: Vec<String>,
    pub determined: Vec<String>,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct CImplicitBound {
    pub param: String,
    pub trait_ref: TraitRef,
}

/// RHS of an associated-item equality constraint.
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub enum Term {
    Equality(TypeBox),
    Bound(TermBound),
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct TermBound {
    pub constraints: Vec<Constraint>,
}

/// A generic parameter list plus its constraints.
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct Generics {
    pub params: Vec<Parameter>,
    pub constraints: Vec<Constraint>,
}

// ───────────────────────────── parameters ─────────────────────────────

/// A parameter — value-level or generic (mirror of `ir::parameter::Parameter`).
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub enum Parameter {
    Literal(LiteralParameter),
    Type(TypeParam),
    Const(ConstParam),
    Lifetime(LifetimeParam),
    Dependent(DependentParam),
    Module(ModuleParam),
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct LiteralParameter {
    pub name: String,
    pub ty: Option<Box<Type>>,
    pub attributes: Vec<ParameterAttribute>,
    pub default_value: Option<ConstExpr>,
    pub description: Option<String>,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct TypeParam {
    pub name: Option<String>,
    pub kind: Kind,
    pub variance: Variance,
    pub default_type: Option<TypeExpr>,
    pub params: Vec<Parameter>,
    pub origin: TypeParamOrigin,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct ConstParam {
    pub name: String,
    pub ty: TypeExpr,
    pub default_value: Option<ConstExpr>,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct LifetimeParam {
    pub name: String,
    pub variance: Variance,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct DependentParam {
    pub name: String,
    pub ty: TypeExpr,
    pub default_value: Option<ConstExpr>,
    pub implicit: bool,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct ModuleParam {
    pub name: String,
    pub signature: Option<TypeExpr>,
}

// ───────────────────────── records & fields ─────────────────────────

/// Struct / record-like type.
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct Record {
    pub name: Option<String>,
    pub generics: Option<Generics>,
    pub fields: Vec<Field>,
    pub call_signatures: Vec<Function>,
    pub constructors: Vec<Function>,
    pub methods: Vec<Function>,
    pub index_signatures: Vec<IndexSignature>,
    pub super_types: Vec<Type>,
}

/// A field of a record: fully known, a pattern/index signature, or unknown.
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub enum Field {
    Known(KnownField),
    Pattern(IndexSignature),
    Unknown,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct IndexSignature {
    pub key_type: Box<Type>,
    pub value_type: Box<Type>,
}

/// Key of a known field.
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub enum FieldKey {
    Ident(String),
    Index(i64),
    Computed(ConstExpr),
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct KnownField {
    pub key: FieldKey,
    pub ty: Option<Box<Type>>,
    pub default_value: Option<ConstExpr>,
    pub attributes: FieldAttributes,
    pub visibility: Option<Visibility>,
    pub documentation: Option<String>,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct FieldAttributes {
    pub decorators: Vec<String>,
    pub is_mutable: bool,
    pub is_optional: bool,
    pub is_static: bool,
}

/// A variant of a sum / enum type.
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct SumVariant {
    pub name: String,
    pub data: Option<SumField>,
    pub documentation: Option<String>,
}

/// Payload of a sum variant.
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub enum SumField {
    Tuple(SumFieldTuple),
    StructLike(SumFieldStruct),
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct SumFieldTuple {
    pub types: Vec<Type>,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct SumFieldStruct {
    pub fields: Vec<Field>,
}

// ───────────────────────── functions & traits ─────────────────────────

/// Callable symbol with signature data (structural parts of `ir::function::Function`).
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct Function {
    pub input_parameters: Vec<Parameter>,
    pub output_parameters: Vec<Parameter>,
    pub attributes: Vec<FunctionAttribute>,
    pub generics: Option<Generics>,
    pub receiver: Option<ReceiverKind>,
    pub overloads: Vec<Function>,
    pub implemented: bool,
}

/// Interface contract.
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct TraitDef {
    pub generics: Option<Generics>,
    pub super_traits: Vec<TraitRef>,
    pub associated_types: Vec<AssociatedType>,
    pub properties: Vec<Field>,
    pub required_methods: Vec<TraitMethod>,
    pub provided_methods: Vec<TraitMethod>,
    pub required_constants: Vec<TraitConstant>,
    pub attributes: Vec<TraitAttribute>,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct AssociatedType {
    pub name: String,
    pub bounds: Vec<GenericBound>,
    pub default_type: Option<Type>,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct TraitMethod {
    pub name: String,
    pub parameters: Vec<Parameter>,
    pub return_type: Option<Box<Type>>,
    pub generics: Option<Generics>,
    pub attributes: Vec<FunctionAttribute>,
    pub documentation: Option<String>,
    pub receiver: Option<ReceiverKind>,
    pub has_default_implementation: bool,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct TraitConstant {
    pub name: String,
    pub ty: Box<Type>,
    pub default_value: Option<ConstExpr>,
}

/// Trait-level attribute.
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub enum TraitAttribute {
    Marker,
    Auto,
    Unsafe,
    ObjectSafe,
    Sealed,
    Functional,
    Custom(TraitAttrCustom),
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct TraitAttrCustom {
    pub name: String,
    pub args: Vec<String>,
}

/// Concrete implementation of a trait for a type.
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct TraitImpl {
    pub tr: TraitRef,
    pub for_type: Box<Type>,
    pub generics: Option<Generics>,
    pub where_constraints: Vec<Constraint>,
    pub methods: Vec<Function>,
    pub associated_types: Vec<AssociatedTypeImpl>,
    pub associated_constants: Vec<TraitConstant>,
    pub is_negative: bool,
    pub is_blanket: bool,
    pub is_unsafe: bool,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct AssociatedTypeImpl {
    pub name: String,
    pub ty: Box<Type>,
}

// ───────────────────────── the Entry document ─────────────────────────

/// The structural shape of an entry, embedded inline in [`Entry`].
///
/// Unit variants are marker kinds; data variants carry the structural payload.
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub enum KindData {
    Module,
    Constant,
    Variable,
    Macro,
    PrimitiveType,
    Field,
    Event,
    Info(InfoKind),
    RecordType(Record),
    Function(Function),
    TraitDef(TraitDef),
    TraitImpl(TraitImpl),
    SumType(SumTypeKind),
    UnionType(UnionTypeKind),
    TypeAlias(TypeAliasKind),
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct InfoKind {
    pub text: String,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct SumTypeKind {
    pub variants: Vec<SumVariant>,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct UnionTypeKind {
    pub types: Vec<Type>,
}

#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(subdocument = true, key = "random")]
pub struct TypeAliasKind {
    pub aliased: Type,
}

/// The flat, indexable symbol document. Identity is the lexical hash of
/// `fq_name` (the `::`-joined path), so projecting the same symbol is stable and
/// idempotent. Navigation edges (`members`, `implemented_protocols`) are carried
/// as path strings, matching the IR's own "reference by absolute path" model.
#[derive(TerminusDBModel, Debug, Clone, PartialEq)]
#[tdb(key = "lexical", key_fields = "fq_name")]
pub struct Entry {
    pub fq_name: String,
    pub name: String,
    pub aliases: BTreeSet<String>,
    pub documentation: Option<String>,
    pub visibility: Visibility,
    pub path: Vec<String>,
    pub members: BTreeSet<String>,
    pub implemented_protocols: BTreeSet<String>,
    pub kind: KindData,
}
