//! Projection from the compiler IR (`ir` crate) into the graph [`model`] —
//! the linking pass that makes the store graph-native.
//!
//! Unlike the old 1:1 mirror, this projection resolves every name-string it
//! meets (via [`Linker`]) and emits edges alongside the structural payload:
//!
//! - `TypeReference`/`TraitRef` leaves get `resolves_to` links;
//! - each symbol accumulates derived adjacency (`mentions`, and for functions
//!   `takes`/`returns`) as it is projected;
//! - `implemented_protocols` → `implements` links, supertypes/supertraits →
//!   `extends` links, inverted `members` → `member_of`;
//! - `TraitImpl` entries additionally reify an [`model::Implementation`];
//! - tree-sitter-resolved function-body references — dropped entirely by the
//!   old projection — become [`model::Reference`] nodes (the call graph).
//!
//! Deliberate losses: `type_links` (superseded by the linker) and the
//! structured `LogicalPredicate` tree, flattened to a string as before.
//! Multivalued `Option<Vec<_>>` IR fields collapse to `Vec`/`BTreeSet`
//! (absent ⇒ empty).

use std::collections::BTreeSet;

use ir::entry::{Index, NudoxPath};
use ir::kind::Visibility as IrVis;
use terminusdb_schema::{EntityIDFor, TdbLazy};

use super::link::{Linker, PackageCtx, package_iri, package_version_iri};
use super::model as m;

// ───────────────────────── the emit context ─────────────────────────

/// Which signature slot is being projected; decides which derived-adjacency
/// buckets a resolved name lands in.
#[derive(Clone, Copy, PartialEq)]
enum Role {
    Neutral,
    Input,
    Output,
}

/// Per-symbol projection state: the linker plus the adjacency accumulated
/// while walking this symbol's shape.
struct EmitCx<'l> {
    linker: &'l Linker,
    role: Role,
    mentions: BTreeSet<String>,
    takes: BTreeSet<String>,
    returns: BTreeSet<String>,
}

impl<'l> EmitCx<'l> {
    fn new(linker: &'l Linker) -> Self {
        Self {
            linker,
            role: Role::Neutral,
            mentions: BTreeSet::new(),
            takes: BTreeSet::new(),
            returns: BTreeSet::new(),
        }
    }

    /// Resolve a signature name to a link, recording it in the adjacency
    /// buckets the current [`Role`] selects.
    fn resolve(&mut self, identifier: &str) -> TdbLazy<m::Symbol> {
        let iri = self.linker.resolve_name(identifier);
        self.mentions.insert(iri.clone());
        match self.role {
            Role::Input => {
                self.takes.insert(iri.clone());
            }
            Role::Output => {
                self.returns.insert(iri.clone());
            }
            Role::Neutral => {}
        }
        self.linker.lazy(&iri)
    }

    fn with_role<T>(&mut self, role: Role, f: impl FnOnce(&mut Self) -> T) -> T {
        let prev = self.role;
        self.role = role;
        let out = f(self);
        self.role = prev;
        out
    }
}

// ───────────────────────────── leaf mappers ─────────────────────────────

fn opt_slice<T>(o: &Option<Vec<T>>) -> &[T] {
    o.as_deref().unwrap_or(&[])
}

fn visibility(v: &IrVis) -> m::Visibility {
    match v {
        IrVis::Public => m::Visibility::Public,
        IrVis::Private => m::Visibility::Private,
        IrVis::Protected => m::Visibility::Protected,
        IrVis::Internal => m::Visibility::Internal,
        IrVis::Package => m::Visibility::Package,
    }
}

fn width(w: &ir::primitives::Width) -> m::Width {
    use ir::primitives::Width as W;
    match w {
        W::W8 => m::Width::W8,
        W::W16 => m::Width::W16,
        W::W32 => m::Width::W32,
        W::W64 => m::Width::W64,
        W::W128 => m::Width::W128,
        W::Arch => m::Width::Arch,
    }
}

fn primitive(p: &ir::primitives::Primitive) -> m::Primitive {
    use ir::primitives::Primitive as P;
    match p {
        P::Int(w) => m::Primitive::Int(m::IntType { width: width(w) }),
        P::UInt(w) => m::Primitive::UInt(m::UIntType { width: width(w) }),
        P::Float(w) => m::Primitive::Float(m::FloatType { width: width(w) }),
        P::Bool => m::Primitive::Bool,
        P::String => m::Primitive::String,
        P::Char => m::Primitive::Char,
        P::Bytes => m::Primitive::Bytes,
        P::Date => m::Primitive::Date,
        P::Address => m::Primitive::Address,
    }
}

fn function_attr(a: &ir::function::Attribute) -> m::FunctionAttribute {
    use ir::function::Attribute as A;
    match a {
        A::Variadic => m::FunctionAttribute::Variadic,
        A::Generator => m::FunctionAttribute::Generator,
        A::Const => m::FunctionAttribute::Const,
        A::Pure => m::FunctionAttribute::Pure,
        A::Async => m::FunctionAttribute::Async,
        A::Unsafe => m::FunctionAttribute::Unsafe,
    }
}

fn param_attr(a: &ir::parameter::ParameterAttribute) -> m::ParameterAttribute {
    use ir::parameter::ParameterAttribute as P;
    match a {
        P::Inout => m::ParameterAttribute::Inout,
        P::Mutable => m::ParameterAttribute::Mutable,
        P::Consuming => m::ParameterAttribute::Consuming,
        P::Borrowing => m::ParameterAttribute::Borrowing,
        P::Isolated => m::ParameterAttribute::Isolated,
        P::Variadic => m::ParameterAttribute::Variadic,
        P::Optional => m::ParameterAttribute::Optional,
    }
}

fn receiver_kind(r: &ir::protocols::ReceiverKind) -> m::ReceiverKind {
    use ir::protocols::ReceiverKind as R;
    match r {
        R::Owned => m::ReceiverKind::Owned,
        R::SharedRef => m::ReceiverKind::SharedRef,
        R::MutRef => m::ReceiverKind::MutRef,
        R::Static => m::ReceiverKind::Static,
        R::Arbitrary => m::ReceiverKind::Arbitrary,
    }
}

fn variance(v: &ir::generics::Variance) -> m::Variance {
    use ir::generics::Variance as V;
    match v {
        V::Covariant => m::Variance::Covariant,
        V::Contravariant => m::Variance::Contravariant,
        V::Invariant => m::Variance::Invariant,
        V::Bivariant => m::Variance::Bivariant,
    }
}

fn type_param_origin(o: &ir::parameter::TypeParamOrigin) -> m::TypeParamOrigin {
    use ir::parameter::TypeParamOrigin as O;
    match o {
        O::Free => m::TypeParamOrigin::Free,
        O::Associated => m::TypeParamOrigin::Associated,
        O::Inferred => m::TypeParamOrigin::Inferred,
    }
}

fn modifier_prefix(p: &ir::ty::ModifierPrefix) -> m::ModifierPrefix {
    use ir::ty::ModifierPrefix as M;
    match p {
        M::Preserve => m::ModifierPrefix::Preserve,
        M::Add => m::ModifierPrefix::Add,
        M::Remove => m::ModifierPrefix::Remove,
    }
}

fn bin_op(o: &ir::generics::BinOp) -> m::BinOp {
    use ir::generics::BinOp as B;
    match o {
        B::Add => m::BinOp::Add,
        B::Sub => m::BinOp::Sub,
        B::Mul => m::BinOp::Mul,
        B::Div => m::BinOp::Div,
        B::Rem => m::BinOp::Rem,
        B::BitAnd => m::BinOp::BitAnd,
        B::BitOr => m::BinOp::BitOr,
        B::BitXor => m::BinOp::BitXor,
        B::Shl => m::BinOp::Shl,
        B::Shr => m::BinOp::Shr,
        B::Eq => m::BinOp::Eq,
        B::Ne => m::BinOp::Ne,
        B::Lt => m::BinOp::Lt,
        B::Le => m::BinOp::Le,
        B::Gt => m::BinOp::Gt,
        B::Ge => m::BinOp::Ge,
        B::And => m::BinOp::And,
        B::Or => m::BinOp::Or,
    }
}

fn unary_op(o: &ir::generics::UnaryOp) -> m::UnaryOp {
    use ir::generics::UnaryOp as U;
    match o {
        U::Neg => m::UnaryOp::Neg,
        U::Not => m::UnaryOp::Not,
        U::Ref => m::UnaryOp::Ref,
        U::Deref => m::UnaryOp::Deref,
    }
}

fn reference_kind(k: &ir::syntax::ReferenceKind) -> m::ReferenceKind {
    use ir::syntax::ReferenceKind as K;
    match k {
        K::FunctionCall => m::ReferenceKind::FunctionCall,
        K::MethodCall => m::ReferenceKind::MethodCall,
        K::TypeReference => m::ReferenceKind::TypeReference,
        K::VariableUse => m::ReferenceKind::VariableUse,
        K::MacroInvocation => m::ReferenceKind::MacroInvocation,
        K::FieldAccess => m::ReferenceKind::FieldAccess,
        K::Import => m::ReferenceKind::Import,
    }
}

// ───────────────────────────── types ─────────────────────────────

fn ty_reference(cx: &mut EmitCx, tr: &ir::ty::TypeReference) -> m::TyReference {
    m::TyReference {
        identifier: tr.identifier.clone(),
        resolves_to: Some(cx.resolve(&tr.identifier)),
        generic_args: opt_slice(&tr.generic_args)
            .iter()
            .map(|a| generic_arg(cx, a))
            .collect(),
    }
}

fn poly_trait(cx: &mut EmitCx, p: &ir::ty::PolyTrait) -> m::PolyTrait {
    m::PolyTrait {
        trait_ref: trait_ref(cx, &p.trait_ref),
        lifetimes: p.lifetimes.clone(),
    }
}

fn bx(cx: &mut EmitCx, t: &ir::ty::Type) -> Box<m::Type> {
    Box::new(type_(cx, t))
}

fn type_(cx: &mut EmitCx, t: &ir::ty::Type) -> m::Type {
    use ir::ty::Type as T;
    match t {
        T::TypeReference(tr) => m::Type::TypeReference(ty_reference(cx, tr)),
        T::SelfType => m::Type::SelfType,
        T::DynTrait(d) => m::Type::DynTrait(m::DynTrait {
            traits: d.traits.iter().map(|p| poly_trait(cx, p)).collect(),
            lifetime: d.lifetime.clone(),
        }),
        T::GenericParam(g) => m::Type::GenericParam(m::GenericParam {
            name: g.name.clone(),
            kind: g.kind.as_ref().map(|k| bx(cx, k)),
        }),
        T::Primitive(p) => m::Type::Primitive(primitive(p)),
        T::FunctionPointer(f) => m::Type::FunctionPointer(m::FunctionPointer {
            inputs: opt_slice(&f.inputs).iter().map(|p| parameter(cx, p)).collect(),
            outputs: opt_slice(&f.outputs)
                .iter()
                .map(|p| parameter(cx, p))
                .collect(),
            attributes: opt_slice(&f.attributes).iter().map(function_attr).collect(),
        }),
        T::Tuple(v) => m::Type::Tuple(m::TyTuple {
            types: v.iter().map(|t| type_(cx, t)).collect(),
        }),
        T::RecordLiteral(r) => m::Type::RecordLiteral(record(cx, r)),
        T::Slice(i) => m::Type::Slice(m::TySlice { inner: bx(cx, i) }),
        T::Array { r#type, length } => m::Type::Array(m::TyArray {
            element: bx(cx, r#type),
            length: *length as i64,
        }),
        T::ImplTrait(b) => m::Type::ImplTrait(m::TyImplTrait {
            bounds: b.iter().map(|b| generic_bound(cx, b)).collect(),
        }),
        T::Infer => m::Type::Infer,
        T::Never => m::Type::Never,
        T::Any => m::Type::Any,
        T::RawPointer { is_mutable, r#type } => m::Type::RawPointer(m::TyRawPointer {
            is_mutable: *is_mutable,
            target: bx(cx, r#type),
        }),
        T::BorrowedRef {
            lifetime,
            is_mutable,
            r#type,
        } => m::Type::BorrowedRef(m::TyBorrowedRef {
            lifetime: lifetime.clone(),
            is_mutable: *is_mutable,
            target: bx(cx, r#type),
        }),
        T::Union(v) => m::Type::Union(m::TyUnion {
            types: v.iter().map(|t| type_(cx, t)).collect(),
        }),
        T::Intersection(v) => m::Type::Intersection(m::TyIntersection {
            types: v.iter().map(|t| type_(cx, t)).collect(),
        }),
        T::Sum(vs) => m::Type::Sum(m::TySum {
            variants: vs.iter().map(|v| sum_variant(cx, v)).collect(),
        }),
        T::QualifiedPath(q) => m::Type::QualifiedPath(m::QualifiedPath {
            name: q.name.clone(),
            generic_arguments: opt_slice(&q.generic_arguments)
                .iter()
                .map(|a| generic_arg(cx, a))
                .collect(),
            self_type: bx(cx, &q.self_type),
            tr: q.tr.as_ref().map(|tr| ty_reference(cx, tr)),
        }),
        T::Variadic(i) => m::Type::Variadic(m::TyVariadic { inner: bx(cx, i) }),
        T::TypeOperator(o) => m::Type::TypeOperator(m::TypeOperator {
            operator: o.operator.clone(),
            target: bx(cx, &o.r#type),
        }),
        T::Conditional(c) => m::Type::Conditional(m::ConditionalType {
            check_type: bx(cx, &c.check_type),
            extends_type: bx(cx, &c.extends_type),
            true_type: bx(cx, &c.true_type),
            false_type: bx(cx, &c.false_type),
        }),
        T::Mapped(mp) => m::Type::Mapped(m::MappedType {
            readonly: mp.readonly.as_ref().map(modifier_prefix),
            optional: mp.optional.as_ref().map(modifier_prefix),
            parameter: mp.parameter.clone(),
            source_type: bx(cx, &mp.source_type),
            name_type: mp.name_type.as_ref().map(|t| bx(cx, t)),
            value_type: mp.value_type.as_ref().map(|t| bx(cx, t)),
        }),
        T::Predicate(p) => m::Type::Predicate(m::TypePredicate {
            asserts: p.asserts,
            subject: match &p.subject {
                ir::ty::PredicateSubject::This => m::PredicateSubject::This,
                ir::ty::PredicateSubject::Identifier(s) => {
                    m::PredicateSubject::Identifier(s.clone())
                }
            },
            target: p.r#type.as_ref().map(|t| bx(cx, t)),
        }),
    }
}

// ───────────────────────── const / generics ─────────────────────────

fn const_expr(c: &ir::generics::ConstExpr) -> m::ConstExpr {
    use ir::generics::ConstExpr as C;
    let bx = |c: &ir::generics::ConstExpr| Box::new(const_expr(c));
    match c {
        C::Int(i) => m::ConstExpr::Int(*i),
        C::Float(f) => m::ConstExpr::Float(*f),
        C::Bool(b) => m::ConstExpr::Bool(*b),
        C::Str(s) => m::ConstExpr::Str(s.clone()),
        C::Var(s) => m::ConstExpr::Var(m::ConstVar { name: s.clone() }),
        C::BinOp { op, lhs, rhs } => m::ConstExpr::BinOp(m::ConstBinOp {
            op: bin_op(op),
            lhs: bx(lhs),
            rhs: bx(rhs),
        }),
        C::UnaryOp { op, operand } => m::ConstExpr::UnaryOp(m::ConstUnaryOp {
            op: unary_op(op),
            operand: bx(operand),
        }),
        C::Call { func, args } => m::ConstExpr::Call(m::ConstCall {
            func: func.clone(),
            args: args.iter().map(const_expr).collect(),
        }),
        C::Ascription { expr, ty } => m::ConstExpr::Ascription(m::ConstAscription {
            expr: bx(expr),
            ty: Box::new(type_expr(ty)),
        }),
    }
}

fn kind(k: &ir::generics::Kind) -> m::Kind {
    use ir::generics::Kind as K;
    match k {
        K::Type => m::Kind::Type,
        K::Constraint => m::Kind::Constraint,
        K::Row => m::Kind::Row,
        K::Arrow(a, b) => m::Kind::Arrow(m::KindArrow {
            from: Box::new(kind(a)),
            to: Box::new(kind(b)),
        }),
        K::Var(s) => m::Kind::Var(s.clone()),
    }
}

fn type_expr(t: &ir::generics::TypeExpr) -> m::TypeExpr {
    m::TypeExpr {
        name: t.name.clone(),
        args: t.args.iter().map(type_expr).collect(),
    }
}

fn trait_ref(cx: &mut EmitCx, tr: &ir::generics::TraitRef) -> m::TraitRef {
    m::TraitRef {
        name: tr.name.clone(),
        resolves_to: Some(cx.resolve(&tr.name)),
        args: tr.args.iter().map(type_expr).collect(),
    }
}

fn generic_arg(cx: &mut EmitCx, a: &ir::generics::GenericArg) -> m::GenericArg {
    use ir::generics::GenericArg as A;
    match a {
        A::Type(t) => m::GenericArg::Type(type_(cx, t)),
        A::ConstExpr(c) => m::GenericArg::ConstExpr(const_expr(c)),
        A::Lifetime(s) => m::GenericArg::Lifetime(s.clone()),
        A::Constraint(c) => m::GenericArg::Constraint(constraint(cx, c)),
        A::Module(s) => m::GenericArg::Module(m::ArgModule { path: s.clone() }),
    }
}

fn generic_bound(cx: &mut EmitCx, b: &ir::protocols::GenericBound) -> m::GenericBound {
    use ir::protocols::GenericBound as B;
    match b {
        B::Trait(tr) => m::GenericBound::Trait(trait_ref(cx, tr)),
        B::Lifetime(l) => m::GenericBound::Lifetime(l.clone()),
    }
}

fn term(cx: &mut EmitCx, t: &ir::generics::Term) -> m::Term {
    use ir::generics::Term as T;
    match t {
        T::Equality(ty) => m::Term::Equality(m::TypeBox {
            inner: Box::new(type_(cx, ty)),
        }),
        T::Bound(cs) => m::Term::Bound(m::TermBound {
            constraints: cs.iter().map(|c| constraint(cx, c)).collect(),
        }),
    }
}

fn constraint(cx: &mut EmitCx, c: &ir::generics::Constraint) -> m::Constraint {
    use ir::generics::Constraint as C;
    match c {
        C::TraitBound {
            param,
            trait_ref: tr,
        } => m::Constraint::TraitBound(m::CTraitBound {
            param: param.clone(),
            trait_ref: trait_ref(cx, tr),
        }),
        C::AssociatedTypeBound {
            param,
            assoc_name,
            bound,
        } => m::Constraint::AssociatedTypeBound(m::CAssocTypeBound {
            param: param.clone(),
            assoc_name: assoc_name.clone(),
            bound: type_expr(bound),
        }),
        C::HigherKindedBound { param, kind: k } => {
            m::Constraint::HigherKindedBound(m::CHigherKinded {
                param: param.clone(),
                kind: kind(k),
            })
        }
        C::AssociatedItem {
            name,
            args,
            term: t,
        } => m::Constraint::AssociatedItem(m::CAssocItem {
            name: name.clone(),
            args: opt_slice(args).iter().map(|a| generic_arg(cx, a)).collect(),
            term: term(cx, t),
        }),
        C::LifetimeBound { shorter, longer } => m::Constraint::LifetimeBound(m::CLifetimeBound {
            shorter: shorter.clone(),
            longer: longer.clone(),
        }),
        C::ConstExprBound { param, expr } => m::Constraint::ConstExprBound(m::CConstExprBound {
            param: param.clone(),
            expr: const_expr(expr),
        }),
        C::LogicalPredicate { pred } => m::Constraint::LogicalPredicate(m::CLogicalPredicate {
            predicate_expr: format!("{pred:?}"),
        }),
        C::FunctionalDependency {
            sources,
            determined,
        } => m::Constraint::FunctionalDependency(m::CFunctionalDependency {
            sources: sources.clone(),
            determined: determined.clone(),
        }),
        C::ImplicitBound {
            param,
            trait_ref: tr,
        } => m::Constraint::ImplicitBound(m::CImplicitBound {
            param: param.clone(),
            trait_ref: trait_ref(cx, tr),
        }),
    }
}

fn generics(cx: &mut EmitCx, g: &ir::generics::Generics) -> m::Generics {
    m::Generics {
        params: g.params.iter().map(|p| parameter(cx, p)).collect(),
        constraints: g.constraints.iter().map(|c| constraint(cx, c)).collect(),
    }
}

// ───────────────────────────── parameters ─────────────────────────────

fn parameter(cx: &mut EmitCx, p: &ir::parameter::Parameter) -> m::Parameter {
    use ir::parameter::Parameter as P;
    match p {
        P::Literal(l) => m::Parameter::Literal(m::LiteralParameter {
            name: l.name.clone(),
            ty: l.r#type.as_ref().map(|t| Box::new(type_(cx, t))),
            attributes: opt_slice(&l.attributes).iter().map(param_attr).collect(),
            default_value: l.default_value.as_ref().map(const_expr),
            description: l.description.clone(),
        }),
        P::Type(t) => m::Parameter::Type(type_param(cx, t)),
        P::Const(c) => m::Parameter::Const(m::ConstParam {
            name: c.name.clone(),
            ty: type_expr(&c.r#type),
            default_value: c.default_value.as_ref().map(const_expr),
        }),
        P::Lifetime(l) => m::Parameter::Lifetime(m::LifetimeParam {
            name: l.name.clone(),
            variance: variance(&l.variance),
        }),
        P::Dependent(d) => m::Parameter::Dependent(m::DependentParam {
            name: d.name.clone(),
            ty: type_expr(&d.r#type),
            default_value: d.default_value.as_ref().map(const_expr),
            implicit: d.implicit,
        }),
        P::Module(md) => m::Parameter::Module(m::ModuleParam {
            name: md.name.clone(),
            signature: md.signature.as_ref().map(type_expr),
        }),
    }
}

fn type_param(cx: &mut EmitCx, t: &ir::parameter::TypeParam) -> m::TypeParam {
    m::TypeParam {
        name: t.name.clone(),
        kind: kind(&t.kind),
        variance: variance(&t.variance),
        default_type: t.default_type.as_ref().map(type_expr),
        params: opt_slice(&t.params).iter().map(|p| parameter(cx, p)).collect(),
        origin: type_param_origin(&t.origin),
    }
}

// ───────────────────────── records & fields ─────────────────────────

fn record(cx: &mut EmitCx, r: &ir::record::Record) -> m::Record {
    m::Record {
        name: r.name.clone(),
        generics: r.generics.as_ref().map(|g| generics(cx, g)),
        fields: r.fields.iter().map(|f| field(cx, f)).collect(),
        call_signatures: opt_slice(&r.call_signatures)
            .iter()
            .map(|f| function(cx, f, false))
            .collect(),
        constructors: opt_slice(&r.constructors)
            .iter()
            .map(|f| function(cx, f, false))
            .collect(),
        methods: opt_slice(&r.methods)
            .iter()
            .map(|f| function(cx, f, false))
            .collect(),
        index_signatures: opt_slice(&r.index_signatures)
            .iter()
            .map(|i| index_signature(cx, i))
            .collect(),
        super_types: opt_slice(&r.super_types)
            .iter()
            .map(|t| type_(cx, t))
            .collect(),
    }
}

fn field(cx: &mut EmitCx, f: &ir::record::Field) -> m::Field {
    use ir::record::Field as F;
    match f {
        F::Known(k) => m::Field::Known(m::KnownField {
            key: field_key(&k.key),
            ty: k.r#type.as_ref().map(|t| Box::new(type_(cx, t))),
            default_value: k.default_value.as_ref().map(const_expr),
            attributes: field_attributes(&k.attributes),
            visibility: k.visibility.as_ref().map(visibility),
            documentation: k.documentation.clone(),
        }),
        F::Pattern(idx) => m::Field::Pattern(index_signature(cx, idx)),
        F::Unknown => m::Field::Unknown,
    }
}

fn field_key(k: &ir::record::FieldKey) -> m::FieldKey {
    use ir::record::FieldKey as K;
    match k {
        K::Ident(s) => m::FieldKey::Ident(s.clone()),
        K::Index(i) => m::FieldKey::Index(*i as i64),
        K::Computed(c) => m::FieldKey::Computed(const_expr(c)),
    }
}

fn field_attributes(a: &ir::record::FieldAttributes) -> m::FieldAttributes {
    m::FieldAttributes {
        decorators: a.decorators.clone(),
        is_mutable: a.is_mutable,
        is_optional: a.is_optional,
        is_static: a.is_static,
    }
}

fn index_signature(cx: &mut EmitCx, i: &ir::record::IndexSignature) -> m::IndexSignature {
    m::IndexSignature {
        key_type: Box::new(type_(cx, &i.key_type)),
        value_type: Box::new(type_(cx, &i.value_type)),
    }
}

fn sum_variant(cx: &mut EmitCx, v: &ir::record::SumVariant) -> m::SumVariant {
    m::SumVariant {
        name: v.name.clone(),
        data: v.data.as_ref().map(|s| sum_field(cx, s)),
        documentation: v.documentation.clone(),
    }
}

fn sum_field(cx: &mut EmitCx, s: &ir::record::SumField) -> m::SumField {
    use ir::record::SumField as S;
    match s {
        S::Tuple(ts) => m::SumField::Tuple(m::SumFieldTuple {
            types: ts.iter().map(|t| type_(cx, t)).collect(),
        }),
        S::StructLike(fs) => m::SumField::StructLike(m::SumFieldStruct {
            fields: fs.iter().map(|f| field(cx, f)).collect(),
        }),
    }
}

// ───────────────────────── functions & traits ─────────────────────────

/// `top` marks the entry-level signature: only there do parameter types feed
/// the symbol's `takes`/`returns` buckets (nested methods/callbacks are
/// `mentions` only). Overloads are alternate top-level signatures, so they
/// inherit `top`.
fn function(cx: &mut EmitCx, f: &ir::function::Function, top: bool) -> m::Function {
    let in_role = if top { Role::Input } else { Role::Neutral };
    let out_role = if top { Role::Output } else { Role::Neutral };
    m::Function {
        input_parameters: cx.with_role(in_role, |cx| {
            opt_slice(&f.input_parameters)
                .iter()
                .map(|p| parameter(cx, p))
                .collect()
        }),
        output_parameters: cx.with_role(out_role, |cx| {
            opt_slice(&f.output_parameters)
                .iter()
                .map(|p| parameter(cx, p))
                .collect()
        }),
        attributes: opt_slice(&f.attributes).iter().map(function_attr).collect(),
        generics: f.generics.as_ref().map(|g| generics(cx, g)),
        receiver: f.receiver.as_ref().map(receiver_kind),
        overloads: opt_slice(&f.overloads)
            .iter()
            .map(|o| function(cx, o, top))
            .collect(),
        implemented: f.implemented,
    }
}

fn associated_type(cx: &mut EmitCx, a: &ir::protocols::AssociatedType) -> m::AssociatedType {
    m::AssociatedType {
        name: a.name.clone(),
        bounds: opt_slice(&a.bounds)
            .iter()
            .map(|b| generic_bound(cx, b))
            .collect(),
        default_type: a.default_type.as_ref().map(|t| type_(cx, t)),
    }
}

fn trait_method(cx: &mut EmitCx, t: &ir::protocols::TraitMethod) -> m::TraitMethod {
    m::TraitMethod {
        name: t.name.clone(),
        parameters: opt_slice(&t.parameters)
            .iter()
            .map(|p| parameter(cx, p))
            .collect(),
        return_type: t.return_type.as_ref().map(|ty| Box::new(type_(cx, ty))),
        generics: t.generics.as_ref().map(|g| generics(cx, g)),
        attributes: opt_slice(&t.attributes).iter().map(function_attr).collect(),
        documentation: t.documentation.clone(),
        receiver: t.receiver.as_ref().map(receiver_kind),
        has_default_implementation: t.has_default_implementation,
    }
}

fn trait_constant(cx: &mut EmitCx, c: &ir::protocols::TraitConstant) -> m::TraitConstant {
    m::TraitConstant {
        name: c.name.clone(),
        ty: Box::new(type_(cx, &c.r#type)),
        default_value: c.default_value.as_ref().map(const_expr),
    }
}

fn trait_attribute(a: &ir::protocols::TraitAttribute) -> m::TraitAttribute {
    use ir::protocols::TraitAttribute as A;
    match a {
        A::Marker => m::TraitAttribute::Marker,
        A::Auto => m::TraitAttribute::Auto,
        A::Unsafe => m::TraitAttribute::Unsafe,
        A::ObjectSafe => m::TraitAttribute::ObjectSafe,
        A::Sealed => m::TraitAttribute::Sealed,
        A::Functional => m::TraitAttribute::Functional,
        A::Custom { name, args } => m::TraitAttribute::Custom(m::TraitAttrCustom {
            name: name.clone(),
            args: args.clone().unwrap_or_default(),
        }),
    }
}

fn trait_def(cx: &mut EmitCx, d: &ir::protocols::TraitDef) -> m::TraitDef {
    m::TraitDef {
        generics: d.generics.as_ref().map(|g| generics(cx, g)),
        super_traits: opt_slice(&d.super_traits)
            .iter()
            .map(|tr| trait_ref(cx, tr))
            .collect(),
        associated_types: opt_slice(&d.associated_types)
            .iter()
            .map(|a| associated_type(cx, a))
            .collect(),
        properties: opt_slice(&d.properties).iter().map(|f| field(cx, f)).collect(),
        required_methods: opt_slice(&d.required_methods)
            .iter()
            .map(|t| trait_method(cx, t))
            .collect(),
        provided_methods: opt_slice(&d.provided_methods)
            .iter()
            .map(|t| trait_method(cx, t))
            .collect(),
        required_constants: opt_slice(&d.required_constants)
            .iter()
            .map(|c| trait_constant(cx, c))
            .collect(),
        attributes: opt_slice(&d.attributes)
            .iter()
            .map(trait_attribute)
            .collect(),
    }
}

fn associated_type_impl(
    cx: &mut EmitCx,
    a: &ir::protocols::AssociatedTypeImpl,
) -> m::AssociatedTypeImpl {
    m::AssociatedTypeImpl {
        name: a.name.clone(),
        ty: Box::new(type_(cx, &a.r#type)),
    }
}

fn trait_impl(cx: &mut EmitCx, i: &ir::protocols::TraitImpl) -> m::TraitImpl {
    m::TraitImpl {
        tr: trait_ref(cx, &i.tr),
        for_type: Box::new(type_(cx, &i.for_type)),
        generics: i.generics.as_ref().map(|g| generics(cx, g)),
        where_constraints: opt_slice(&i.where_constraints)
            .iter()
            .map(|c| constraint(cx, c))
            .collect(),
        methods: opt_slice(&i.methods)
            .iter()
            .map(|f| function(cx, f, false))
            .collect(),
        associated_types: opt_slice(&i.associated_types)
            .iter()
            .map(|a| associated_type_impl(cx, a))
            .collect(),
        associated_constants: opt_slice(&i.associated_constants)
            .iter()
            .map(|c| trait_constant(cx, c))
            .collect(),
        is_negative: i.is_negative,
        is_blanket: i.is_blanket,
        is_unsafe: i.is_unsafe,
    }
}

// ───────────────────────── symbol projection ─────────────────────────

/// The head name of a type expression, for `extends`/`Implementation` subject
/// resolution (`Base<Arg>` extends `Base`, not `Arg`).
fn type_head(t: &ir::ty::Type) -> Option<&str> {
    match t {
        ir::ty::Type::TypeReference(tr) => Some(&tr.identifier),
        _ => None,
    }
}

/// Everything one package projects to, in insertable form. The uploader owns
/// wave ordering (packages → bare symbols → symbol links → relations); see
/// GRAPH-ARCHITECTURE.md §5.
#[derive(Debug, Clone)]
pub struct GraphCorpus {
    pub packages: Vec<m::Package>,
    pub version: Option<m::PackageVersion>,
    pub symbols: Vec<m::Symbol>,
    pub implementations: Vec<m::Implementation>,
    pub references: Vec<m::Reference>,
}

/// Project one indexed package into its graph corpus.
pub fn project(index: &Index, ctx: PackageCtx) -> GraphCorpus {
    let linker = Linker::build(index, ctx.clone());

    let mut symbols = Vec::with_capacity(index.entries_by_path.len());
    let mut implementations = Vec::new();
    let mut references = Vec::new();

    // Deterministic projection order (HashMap iteration is not).
    let mut entries: Vec<(&NudoxPath, &ir::kind::Entry)> = index.entries_by_path.iter().collect();
    entries.sort_by_key(|(_, e)| linker.iri_of(e.path()));

    for (_, entry) in entries {
        let (symbol, impls, refs) = project_entry(&linker, entry);
        symbols.push(symbol);
        implementations.extend(impls);
        references.extend(refs);
    }

    // Per-version membership: the real (non-stub) symbols of this projection.
    let version = ctx.version.as_ref().map(|v| m::PackageVersion {
        id: EntityIDFor::new(&package_version_iri(&ctx.language, &ctx.package, v))
            .expect("sanitized iri"),
        version: v.clone(),
        package: TdbLazy::new_id_unchecked(&package_iri(&ctx.language, &ctx.package)),
        declares: symbols
            .iter()
            // Re-encode the logical uri (`lang/pkg/fq`) for the EntityID path.
            .map(|s| {
                TdbLazy::new_id_unchecked(&format!(
                    "Symbol/{}",
                    s.uri.replace('/', "%2F")
                ))
            })
            .collect(),
    });

    let (stub_symbols, mut packages) = linker.take_stubs();
    symbols.extend(stub_symbols);
    packages.push(m::Package {
        id: EntityIDFor::new(&package_iri(&ctx.language, &ctx.package)).expect("sanitized iri"),
        name: ctx.package.clone(),
        language: ctx.language.clone(),
    });
    packages.sort_by(|a, b| a.name.cmp(&b.name));

    GraphCorpus {
        packages,
        version,
        symbols,
        implementations,
        references,
    }
}

/// `(shape, visibility, implemented-protocol paths, extends names)` of an
/// entry, with adjacency accumulating into `cx` as the shape is walked.
fn project_entry(
    linker: &Linker,
    entry: &ir::kind::Entry,
) -> (m::Symbol, Vec<m::Implementation>, Vec<m::Reference>) {
    use ir::kind::Entry as E;

    let iri = linker.iri_of(entry.path());
    let mut cx = EmitCx::new(linker);
    let mut implementations = Vec::new();
    let mut references = Vec::new();
    let mut extends_names: Vec<String> = Vec::new();
    let mut protocols: &[NudoxPath] = &[];

    let (kind, shape, vis) = match entry {
        E::Module(s) => (m::SymbolKind::Module, None, &s.visibility),
        E::RecordType(s) => {
            protocols = opt_slice(&s.inner.implemented_protocols);
            extends_names.extend(
                opt_slice(&s.inner.super_types)
                    .iter()
                    .filter_map(type_head)
                    .map(str::to_string),
            );
            (
                m::SymbolKind::RecordType,
                Some(m::Shape::Record(record(&mut cx, &s.inner))),
                &s.visibility,
            )
        }
        E::Info(s) => (
            m::SymbolKind::Info,
            Some(m::Shape::Info(m::InfoShape {
                text: s.inner.clone(),
            })),
            &s.visibility,
        ),
        E::UnionType(s) => (
            m::SymbolKind::UnionType,
            Some(m::Shape::Union(m::UnionShape {
                types: s.inner.iter().map(|t| type_(&mut cx, t)).collect(),
            })),
            &s.visibility,
        ),
        E::TraitDef(s) => {
            extends_names.extend(
                opt_slice(&s.inner.super_traits)
                    .iter()
                    .map(|tr| tr.name.clone()),
            );
            (
                m::SymbolKind::TraitDef,
                Some(m::Shape::TraitDef(trait_def(&mut cx, &s.inner))),
                &s.visibility,
            )
        }
        E::TraitImpl(s) => {
            let interface = linker.resolve_name(&s.inner.tr.name);
            if let Some(subject) = type_head(&s.inner.for_type) {
                let subject = linker.resolve_name(subject);
                implementations.push(m::Implementation {
                    subject: linker.lazy(&subject),
                    interface: linker.lazy(&interface),
                    via: Some(linker.lazy(&iri)),
                    is_blanket: s.inner.is_blanket,
                    is_negative: s.inner.is_negative,
                });
            }
            (
                m::SymbolKind::TraitImpl,
                Some(m::Shape::TraitImpl(trait_impl(&mut cx, &s.inner))),
                &s.visibility,
            )
        }
        E::SumType(s) => (
            m::SymbolKind::SumType,
            Some(m::Shape::Sum(m::SumShape {
                variants: s.inner.iter().map(|v| sum_variant(&mut cx, v)).collect(),
            })),
            &s.visibility,
        ),
        E::Function(s) => {
            protocols = opt_slice(&s.inner.implemented_protocols);
            if let Some(body) = &s.inner.body {
                for r in &body.get().references {
                    references.push(m::Reference {
                        source: linker.lazy(&iri),
                        target: linker.lazy(&linker.resolve_path(&r.target)),
                        kind: reference_kind(&r.kind),
                        span_start: Some(r.span.start as i64),
                        span_end: Some(r.span.end as i64),
                    });
                }
            }
            (
                m::SymbolKind::Function,
                Some(m::Shape::Function(function(&mut cx, &s.inner, true))),
                &s.visibility,
            )
        }
        E::TypeAlias(s) => (
            m::SymbolKind::TypeAlias,
            Some(m::Shape::Alias(m::AliasShape {
                aliased: type_(&mut cx, &s.inner),
            })),
            &s.visibility,
        ),
        E::Constant(s) => (m::SymbolKind::Constant, None, &s.visibility),
        E::Variable(s) => (m::SymbolKind::Variable, None, &s.visibility),
        E::Macro(s) => (m::SymbolKind::Macro, None, &s.visibility),
        E::PrimitiveType(s) => (m::SymbolKind::PrimitiveType, None, &s.visibility),
        E::Field(s) => (m::SymbolKind::Field, None, &s.visibility),
        E::Event(s) => (m::SymbolKind::Event, None, &s.visibility),
    };

    let implements: Vec<TdbLazy<m::Symbol>> = protocols
        .iter()
        .map(|p| linker.lazy(&linker.resolve_path(p)))
        .collect();
    let extends: Vec<TdbLazy<m::Symbol>> = {
        let iris: BTreeSet<String> = extends_names
            .iter()
            .map(|n| linker.resolve_name(n))
            .collect();
        iris.iter().map(|i| linker.lazy(i)).collect()
    };

    // IRI is `Symbol/{lang}%2F{package}%2F{fq}` — recover the hierarchy.
    let encoded = iri.trim_start_matches("Symbol/");
    let parts = super::link::decode_id_segment(encoded);
    let package = parts.get(1).cloned().unwrap_or_default();
    let fq = parts.get(2).cloned().unwrap_or_else(|| encoded.to_string());
    let path: Vec<String> = fq.split("::").map(str::to_string).collect();
    // Cross-store coordinate keeps real `/` separators (not the EntityID encoding).
    let uri = parts.join("/");
    let aliases = entry
        .aliases()
        .map(|set| set.iter().map(|segs| segs.join("::")).collect())
        .unwrap_or_default();

    let symbol = m::Symbol {
        id: EntityIDFor::new(&iri).expect("sanitized iri"),
        fq_name: path.join("::"),
        name: entry.name().to_string(),
        path,
        aliases,
        symbol_id: None,
        kind,
        visibility: visibility(vis),
        documentation: entry.documentation().map(str::to_string),
        resolved: true,
        package: TdbLazy::new_id_unchecked(&package_iri(&linker.ctx().language, &package)),
        member_of: linker.parent_of(&iri).map(|p| linker.lazy(p)),
        implements,
        extends,
        mentions: cx.mentions.iter().map(|i| linker.lazy(i)).collect(),
        takes: cx.takes.iter().map(|i| linker.lazy(i)).collect(),
        returns: cx.returns.iter().map(|i| linker.lazy(i)).collect(),
        shape,
        uri,
    };

    (symbol, implementations, references)
}

#[cfg(test)]
mod tests {
    use terminusdb_schema::{ToJson, ToTDBInstance, ToTDBSchema};

    use super::*;

    fn local(seg: &str) -> NudoxPath {
        NudoxPath::Local(std::path::PathBuf::from(seg))
    }

    fn function_entry(path: &str, input: Option<ir::ty::Type>, output: Option<ir::ty::Type>) -> ir::kind::Entry {
        let param = |name: &str, ty: Option<ir::ty::Type>| {
            ir::parameter::Parameter::Literal(ir::parameter::LiteralParameter {
                name: name.to_string(),
                r#type: ty,
                attributes: None,
                default_value: None,
                description: None,
            })
        };
        let mut s = ir::kind::Symbol::placeholder(ir::function::Function {
            input_parameters: input.map(|t| vec![param("x", Some(t))]),
            output_parameters: output.map(|t| vec![param("", Some(t))]),
            type_links: None,
            attributes: None,
            generics: None,
            receiver: None,
            overloads: None,
            implemented: true,
            members: None,
            implemented_protocols: None,
            body: None,
        });
        s.name = path.rsplit('/').next().unwrap().to_string();
        s.path = local(path);
        ir::kind::Entry::Function(s)
    }

    fn module_entry(path: &str, members: Vec<NudoxPath>) -> ir::kind::Entry {
        let mut s = ir::kind::Symbol::placeholder(ir::module::Module {
            members: Some(members),
        });
        s.name = path.rsplit('/').next().unwrap().to_string();
        s.path = local(path);
        ir::kind::Entry::Module(s)
    }

    fn ty_ref(ident: &str) -> ir::ty::Type {
        ir::ty::Type::TypeReference(ir::ty::TypeReference {
            identifier: ident.to_string(),
            generic_args: None,
        })
    }

    fn test_index() -> Index {
        let widget = {
            let mut s = ir::kind::Symbol::placeholder(ir::record::Record {
                name: Some("Widget".into()),
                generics: None,
                fields: vec![],
                call_signatures: None,
                constructors: None,
                methods: None,
                index_signatures: None,
                super_types: None,
                members: None,
                implemented_protocols: None,
            });
            s.name = "Widget".into();
            s.path = local("pkg/Widget");
            ir::kind::Entry::RecordType(s)
        };
        let entries = vec![
            (local("pkg"), module_entry("pkg", vec![local("pkg/do_thing"), local("pkg/Widget")])),
            (local("pkg/Widget"), widget),
            (
                local("pkg/do_thing"),
                function_entry("pkg/do_thing", Some(ty_ref("pkg::Widget")), Some(ty_ref("ext::Thing"))),
            ),
        ];
        Index {
            root_ids: vec![local("pkg")],
            entries_by_path: entries.into_iter().collect(),
        }
    }

    fn ctx() -> PackageCtx {
        PackageCtx {
            language: "rust".into(),
            package: "pkg".into(),
            version: Some("1.0.0".into()),
        }
    }

    #[test]
    fn schema_tree_builds_and_includes_key_classes() {
        // The recursive model (Symbol → Shape → TyReference → Symbol) must not
        // overflow when its schema is walked.
        let names: Vec<String> = m::Symbol::to_schema_tree()
            .iter()
            .map(|s| s.class_name().clone())
            .collect();
        for expected in ["Symbol", "Shape", "Type", "Function", "Parameter"] {
            assert!(
                names.iter().any(|n| n == expected),
                "schema tree missing {expected}: {names:?}"
            );
        }
        let rel: Vec<String> = m::Reference::to_schema_tree()
            .iter()
            .map(|s| s.class_name().clone())
            .collect();
        assert!(rel.iter().any(|n| n == "Reference"));
    }

    #[test]
    fn projects_symbols_with_edges_and_stubs() {
        let corpus = project(&test_index(), ctx());

        let sym = |uri: &str| {
            corpus
                .symbols
                .iter()
                .find(|s| s.uri == uri)
                .unwrap_or_else(|| panic!("missing symbol {uri}"))
        };

        // Containment inverted: members lists become member_of on the child.
        let f = sym("rust/pkg/pkg::do_thing");
        assert!(f.member_of.is_some(), "do_thing should have a parent");
        assert!(matches!(f.kind, m::SymbolKind::Function));

        // Signature adjacency: the input type resolves into `takes` and
        // `mentions`; the unknown return type resolves to a ~extern stub.
        assert_eq!(f.takes.len(), 1, "one named input type");
        assert_eq!(f.returns.len(), 1, "one named output type");
        assert_eq!(f.mentions.len(), 2, "both types mentioned");

        let stub = sym("rust/~extern/ext::Thing");
        assert!(!stub.resolved);
        assert!(matches!(stub.kind, m::SymbolKind::Unresolved));

        // The version node declares the three real symbols, not the stub.
        assert_eq!(corpus.version.as_ref().unwrap().declares.len(), 3);

        // Instance serialises to TerminusDB JSON-LD with @type framing.
        let json = f.clone().to_instance(None).to_json();
        assert_eq!(json["@type"], "Symbol");
        assert_eq!(json["fq_name"], "pkg::do_thing");
    }
}
