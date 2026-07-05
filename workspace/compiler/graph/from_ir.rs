//! Projection from the compiler IR (`ir` crate) into the graph [`model`].
//!
//! Because the model mirrors the IR closely, most conversions are ~1:1. The only
//! deliberate losses are non-structural runtime/cache data (function bodies,
//! `type_links`) and the structured `LogicalPredicate` tree, which is flattened
//! to a string as in the original projection. Multivalued `Option<Vec<_>>` IR
//! fields collapse to `Vec`/`BTreeSet` (absent ⇒ empty).

use std::collections::BTreeSet;

use ir::entry::NudoxPath;
use ir::kind::Visibility as IrVis;

use super::model as m;

fn path_segments(p: &NudoxPath) -> Vec<String> {
    let comps = |pb: &std::path::Path| {
        pb.iter()
            .map(|c| c.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
    };
    match p {
        NudoxPath::Local(pb) => comps(pb),
        NudoxPath::External { path, dependency } => {
            let mut segs = vec![dependency.clone()];
            segs.extend(comps(path));
            segs
        }
    }
}

fn path_string(p: &NudoxPath) -> String {
    path_segments(p).join("::")
}

fn paths_set(ps: &Option<Vec<NudoxPath>>) -> BTreeSet<String> {
    ps.as_deref()
        .unwrap_or(&[])
        .iter()
        .map(path_string)
        .collect()
}

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

// ───────────────────────────── types ─────────────────────────────

fn ty_reference(tr: &ir::ty::TypeReference) -> m::TyReference {
    m::TyReference {
        identifier: tr.identifier.clone(),
        generic_args: opt_slice(&tr.generic_args)
            .iter()
            .map(generic_arg)
            .collect(),
    }
}

fn poly_trait(p: &ir::ty::PolyTrait) -> m::PolyTrait {
    m::PolyTrait {
        trait_ref: trait_ref(&p.trait_ref),
        lifetimes: p.lifetimes.clone(),
    }
}

fn type_(t: &ir::ty::Type) -> m::Type {
    use ir::ty::Type as T;
    let bx = |t: &ir::ty::Type| Box::new(type_(t));
    let types = |v: &[ir::ty::Type]| v.iter().map(type_).collect::<Vec<_>>();
    match t {
        T::TypeReference(tr) => m::Type::TypeReference(ty_reference(tr)),
        T::SelfType => m::Type::SelfType,
        T::DynTrait(d) => m::Type::DynTrait(m::DynTrait {
            traits: d.traits.iter().map(poly_trait).collect(),
            lifetime: d.lifetime.clone(),
        }),
        T::GenericParam(g) => m::Type::GenericParam(m::GenericParam {
            name: g.name.clone(),
            kind: g.kind.as_ref().map(|k| bx(k)),
        }),
        T::Primitive(p) => m::Type::Primitive(primitive(p)),
        T::FunctionPointer(f) => m::Type::FunctionPointer(m::FunctionPointer {
            inputs: opt_slice(&f.inputs).iter().map(parameter).collect(),
            outputs: opt_slice(&f.outputs).iter().map(parameter).collect(),
            attributes: opt_slice(&f.attributes).iter().map(function_attr).collect(),
        }),
        T::Tuple(v) => m::Type::Tuple(m::TyTuple { types: types(v) }),
        T::RecordLiteral(r) => m::Type::RecordLiteral(record(r)),
        T::Slice(i) => m::Type::Slice(m::TySlice { inner: bx(i) }),
        T::Array { r#type, length } => m::Type::Array(m::TyArray {
            element: bx(r#type),
            length: *length as i64,
        }),
        T::ImplTrait(b) => m::Type::ImplTrait(m::TyImplTrait {
            bounds: b.iter().map(generic_bound).collect(),
        }),
        T::Infer => m::Type::Infer,
        T::Never => m::Type::Never,
        T::Any => m::Type::Any,
        T::RawPointer { is_mutable, r#type } => m::Type::RawPointer(m::TyRawPointer {
            is_mutable: *is_mutable,
            target: bx(r#type),
        }),
        T::BorrowedRef {
            lifetime,
            is_mutable,
            r#type,
        } => m::Type::BorrowedRef(m::TyBorrowedRef {
            lifetime: lifetime.clone(),
            is_mutable: *is_mutable,
            target: bx(r#type),
        }),
        T::Union(v) => m::Type::Union(m::TyUnion { types: types(v) }),
        T::Intersection(v) => m::Type::Intersection(m::TyIntersection { types: types(v) }),
        T::Sum(vs) => m::Type::Sum(m::TySum {
            variants: vs.iter().map(sum_variant).collect(),
        }),
        T::QualifiedPath(q) => m::Type::QualifiedPath(m::QualifiedPath {
            name: q.name.clone(),
            generic_arguments: opt_slice(&q.generic_arguments)
                .iter()
                .map(generic_arg)
                .collect(),
            self_type: bx(&q.self_type),
            tr: q.tr.as_ref().map(ty_reference),
        }),
        T::Variadic(i) => m::Type::Variadic(m::TyVariadic { inner: bx(i) }),
        T::TypeOperator(o) => m::Type::TypeOperator(m::TypeOperator {
            operator: o.operator.clone(),
            target: bx(&o.r#type),
        }),
        T::Conditional(c) => m::Type::Conditional(m::ConditionalType {
            check_type: bx(&c.check_type),
            extends_type: bx(&c.extends_type),
            true_type: bx(&c.true_type),
            false_type: bx(&c.false_type),
        }),
        T::Mapped(mp) => m::Type::Mapped(m::MappedType {
            readonly: mp.readonly.as_ref().map(modifier_prefix),
            optional: mp.optional.as_ref().map(modifier_prefix),
            parameter: mp.parameter.clone(),
            source_type: bx(&mp.source_type),
            name_type: mp.name_type.as_ref().map(|t| bx(t)),
            value_type: mp.value_type.as_ref().map(|t| bx(t)),
        }),
        T::Predicate(p) => m::Type::Predicate(m::TypePredicate {
            asserts: p.asserts,
            subject: match &p.subject {
                ir::ty::PredicateSubject::This => m::PredicateSubject::This,
                ir::ty::PredicateSubject::Identifier(s) => {
                    m::PredicateSubject::Identifier(s.clone())
                }
            },
            target: p.r#type.as_ref().map(|t| bx(t)),
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

fn trait_ref(tr: &ir::generics::TraitRef) -> m::TraitRef {
    m::TraitRef {
        name: tr.name.clone(),
        args: tr.args.iter().map(type_expr).collect(),
    }
}

fn generic_arg(a: &ir::generics::GenericArg) -> m::GenericArg {
    use ir::generics::GenericArg as A;
    match a {
        A::Type(t) => m::GenericArg::Type(type_(t)),
        A::ConstExpr(c) => m::GenericArg::ConstExpr(const_expr(c)),
        A::Lifetime(s) => m::GenericArg::Lifetime(s.clone()),
        A::Constraint(c) => m::GenericArg::Constraint(constraint(c)),
        A::Module(s) => m::GenericArg::Module(m::ArgModule { path: s.clone() }),
    }
}

fn generic_bound(b: &ir::protocols::GenericBound) -> m::GenericBound {
    use ir::protocols::GenericBound as B;
    match b {
        B::Trait(tr) => m::GenericBound::Trait(trait_ref(tr)),
        B::Lifetime(l) => m::GenericBound::Lifetime(l.clone()),
    }
}

fn term(t: &ir::generics::Term) -> m::Term {
    use ir::generics::Term as T;
    match t {
        T::Equality(ty) => m::Term::Equality(m::TypeBox {
            inner: Box::new(type_(ty)),
        }),
        T::Bound(cs) => m::Term::Bound(m::TermBound {
            constraints: cs.iter().map(constraint).collect(),
        }),
    }
}

fn constraint(c: &ir::generics::Constraint) -> m::Constraint {
    use ir::generics::Constraint as C;
    match c {
        C::TraitBound {
            param,
            trait_ref: tr,
        } => m::Constraint::TraitBound(m::CTraitBound {
            param: param.clone(),
            trait_ref: trait_ref(tr),
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
            args: opt_slice(args).iter().map(generic_arg).collect(),
            term: term(t),
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
            trait_ref: trait_ref(tr),
        }),
    }
}

fn generics(g: &ir::generics::Generics) -> m::Generics {
    m::Generics {
        params: g.params.iter().map(parameter).collect(),
        constraints: g.constraints.iter().map(constraint).collect(),
    }
}

// ───────────────────────────── parameters ─────────────────────────────

fn parameter(p: &ir::parameter::Parameter) -> m::Parameter {
    use ir::parameter::Parameter as P;
    match p {
        P::Literal(l) => m::Parameter::Literal(m::LiteralParameter {
            name: l.name.clone(),
            ty: l.r#type.as_ref().map(|t| Box::new(type_(t))),
            attributes: opt_slice(&l.attributes).iter().map(param_attr).collect(),
            default_value: l.default_value.as_ref().map(const_expr),
            description: l.description.clone(),
        }),
        P::Type(t) => m::Parameter::Type(type_param(t)),
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

fn type_param(t: &ir::parameter::TypeParam) -> m::TypeParam {
    m::TypeParam {
        name: t.name.clone(),
        kind: kind(&t.kind),
        variance: variance(&t.variance),
        default_type: t.default_type.as_ref().map(type_expr),
        params: opt_slice(&t.params).iter().map(parameter).collect(),
        origin: type_param_origin(&t.origin),
    }
}

// ───────────────────────── records & fields ─────────────────────────

fn record(r: &ir::record::Record) -> m::Record {
    m::Record {
        name: r.name.clone(),
        generics: r.generics.as_ref().map(generics),
        fields: r.fields.iter().map(field).collect(),
        call_signatures: opt_slice(&r.call_signatures).iter().map(function).collect(),
        constructors: opt_slice(&r.constructors).iter().map(function).collect(),
        methods: opt_slice(&r.methods).iter().map(function).collect(),
        index_signatures: opt_slice(&r.index_signatures)
            .iter()
            .map(index_signature)
            .collect(),
        super_types: opt_slice(&r.super_types).iter().map(type_).collect(),
    }
}

fn field(f: &ir::record::Field) -> m::Field {
    use ir::record::Field as F;
    match f {
        F::Known(k) => m::Field::Known(m::KnownField {
            key: field_key(&k.key),
            ty: k.r#type.as_ref().map(|t| Box::new(type_(t))),
            default_value: k.default_value.as_ref().map(const_expr),
            attributes: field_attributes(&k.attributes),
            visibility: k.visibility.as_ref().map(visibility),
            documentation: k.documentation.clone(),
        }),
        F::Pattern(idx) => m::Field::Pattern(index_signature(idx)),
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

fn index_signature(i: &ir::record::IndexSignature) -> m::IndexSignature {
    m::IndexSignature {
        key_type: Box::new(type_(&i.key_type)),
        value_type: Box::new(type_(&i.value_type)),
    }
}

fn sum_variant(v: &ir::record::SumVariant) -> m::SumVariant {
    m::SumVariant {
        name: v.name.clone(),
        data: v.data.as_ref().map(sum_field),
        documentation: v.documentation.clone(),
    }
}

fn sum_field(s: &ir::record::SumField) -> m::SumField {
    use ir::record::SumField as S;
    match s {
        S::Tuple(ts) => m::SumField::Tuple(m::SumFieldTuple {
            types: ts.iter().map(type_).collect(),
        }),
        S::StructLike(fs) => m::SumField::StructLike(m::SumFieldStruct {
            fields: fs.iter().map(field).collect(),
        }),
    }
}

// ───────────────────────── functions & traits ─────────────────────────

fn function(f: &ir::function::Function) -> m::Function {
    m::Function {
        input_parameters: opt_slice(&f.input_parameters)
            .iter()
            .map(parameter)
            .collect(),
        output_parameters: opt_slice(&f.output_parameters)
            .iter()
            .map(parameter)
            .collect(),
        attributes: opt_slice(&f.attributes).iter().map(function_attr).collect(),
        generics: f.generics.as_ref().map(generics),
        receiver: f.receiver.as_ref().map(receiver_kind),
        overloads: opt_slice(&f.overloads).iter().map(function).collect(),
        implemented: f.implemented,
    }
}

fn associated_type(a: &ir::protocols::AssociatedType) -> m::AssociatedType {
    m::AssociatedType {
        name: a.name.clone(),
        bounds: opt_slice(&a.bounds).iter().map(generic_bound).collect(),
        default_type: a.default_type.as_ref().map(type_),
    }
}

fn trait_method(t: &ir::protocols::TraitMethod) -> m::TraitMethod {
    m::TraitMethod {
        name: t.name.clone(),
        parameters: opt_slice(&t.parameters).iter().map(parameter).collect(),
        return_type: t.return_type.as_ref().map(|ty| Box::new(type_(ty))),
        generics: t.generics.as_ref().map(generics),
        attributes: opt_slice(&t.attributes).iter().map(function_attr).collect(),
        documentation: t.documentation.clone(),
        receiver: t.receiver.as_ref().map(receiver_kind),
        has_default_implementation: t.has_default_implementation,
    }
}

fn trait_constant(c: &ir::protocols::TraitConstant) -> m::TraitConstant {
    m::TraitConstant {
        name: c.name.clone(),
        ty: Box::new(type_(&c.r#type)),
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

fn trait_def(d: &ir::protocols::TraitDef) -> m::TraitDef {
    m::TraitDef {
        generics: d.generics.as_ref().map(generics),
        super_traits: opt_slice(&d.super_traits).iter().map(trait_ref).collect(),
        associated_types: opt_slice(&d.associated_types)
            .iter()
            .map(associated_type)
            .collect(),
        properties: opt_slice(&d.properties).iter().map(field).collect(),
        required_methods: opt_slice(&d.required_methods)
            .iter()
            .map(trait_method)
            .collect(),
        provided_methods: opt_slice(&d.provided_methods)
            .iter()
            .map(trait_method)
            .collect(),
        required_constants: opt_slice(&d.required_constants)
            .iter()
            .map(trait_constant)
            .collect(),
        attributes: opt_slice(&d.attributes)
            .iter()
            .map(trait_attribute)
            .collect(),
    }
}

fn associated_type_impl(a: &ir::protocols::AssociatedTypeImpl) -> m::AssociatedTypeImpl {
    m::AssociatedTypeImpl {
        name: a.name.clone(),
        ty: Box::new(type_(&a.r#type)),
    }
}

fn trait_impl(i: &ir::protocols::TraitImpl) -> m::TraitImpl {
    m::TraitImpl {
        tr: trait_ref(&i.tr),
        for_type: Box::new(type_(&i.for_type)),
        generics: i.generics.as_ref().map(generics),
        where_constraints: opt_slice(&i.where_constraints)
            .iter()
            .map(constraint)
            .collect(),
        methods: opt_slice(&i.methods).iter().map(function).collect(),
        associated_types: opt_slice(&i.associated_types)
            .iter()
            .map(associated_type_impl)
            .collect(),
        associated_constants: opt_slice(&i.associated_constants)
            .iter()
            .map(trait_constant)
            .collect(),
        is_negative: i.is_negative,
        is_blanket: i.is_blanket,
        is_unsafe: i.is_unsafe,
    }
}

// ───────────────────────────── the Entry ─────────────────────────────

/// `(structural kind, visibility, member paths, implemented-protocol paths)`.
fn project_kind(e: &ir::kind::Entry) -> (m::KindData, &IrVis, BTreeSet<String>, BTreeSet<String>) {
    use ir::kind::Entry as E;
    let empty = BTreeSet::new;
    match e {
        E::Module(s) => (
            m::KindData::Module,
            &s.visibility,
            paths_set(&s.inner.members),
            empty(),
        ),
        E::RecordType(s) => (
            m::KindData::RecordType(record(&s.inner)),
            &s.visibility,
            paths_set(&s.inner.members),
            paths_set(&s.inner.implemented_protocols),
        ),
        E::Info(s) => (
            m::KindData::Info(m::InfoKind {
                text: s.inner.clone(),
            }),
            &s.visibility,
            empty(),
            empty(),
        ),
        E::UnionType(s) => (
            m::KindData::UnionType(m::UnionTypeKind {
                types: s.inner.iter().map(type_).collect(),
            }),
            &s.visibility,
            empty(),
            empty(),
        ),
        E::TraitDef(s) => (
            m::KindData::TraitDef(trait_def(&s.inner)),
            &s.visibility,
            paths_set(&s.inner.members),
            empty(),
        ),
        E::TraitImpl(s) => (
            m::KindData::TraitImpl(trait_impl(&s.inner)),
            &s.visibility,
            paths_set(&s.inner.members),
            empty(),
        ),
        E::SumType(s) => (
            m::KindData::SumType(m::SumTypeKind {
                variants: s.inner.iter().map(sum_variant).collect(),
            }),
            &s.visibility,
            empty(),
            empty(),
        ),
        E::Function(s) => (
            m::KindData::Function(function(&s.inner)),
            &s.visibility,
            paths_set(&s.inner.members),
            paths_set(&s.inner.implemented_protocols),
        ),
        E::TypeAlias(s) => (
            m::KindData::TypeAlias(m::TypeAliasKind {
                aliased: type_(&s.inner),
            }),
            &s.visibility,
            empty(),
            empty(),
        ),
        E::Constant(s) => (m::KindData::Constant, &s.visibility, empty(), empty()),
        E::Variable(s) => (m::KindData::Variable, &s.visibility, empty(), empty()),
        E::Macro(s) => (m::KindData::Macro, &s.visibility, empty(), empty()),
        E::PrimitiveType(s) => (m::KindData::PrimitiveType, &s.visibility, empty(), empty()),
        E::Field(s) => (m::KindData::Field, &s.visibility, empty(), empty()),
        E::Event(s) => (m::KindData::Event, &s.visibility, empty(), empty()),
    }
}

/// Project an IR entry into its flat, indexable graph document.
pub fn entry(e: &ir::kind::Entry) -> m::Entry {
    let (kind, vis, members, implemented_protocols) = project_kind(e);
    let segments = path_segments(e.path());
    let aliases = e
        .aliases()
        .map(|set| set.iter().map(|segs| segs.join("::")).collect())
        .unwrap_or_default();
    m::Entry {
        fq_name: segments.join("::"),
        name: e.name().to_string(),
        aliases,
        documentation: e.documentation().map(str::to_string),
        visibility: visibility(vis),
        path: segments,
        members,
        implemented_protocols,
        kind,
    }
}

impl From<&ir::kind::Entry> for m::Entry {
    fn from(e: &ir::kind::Entry) -> Self {
        entry(e)
    }
}

#[cfg(test)]
mod tests {
    use terminusdb_schema::{ToJson, ToTDBInstance, ToTDBSchema};

    use super::*;

    fn local(seg: &str) -> NudoxPath {
        NudoxPath::Local(std::path::PathBuf::from(seg))
    }

    #[test]
    fn schema_tree_builds_and_includes_key_classes() {
        // The recursive model must not overflow when its schema is walked.
        let names: Vec<String> = m::Entry::to_schema_tree()
            .iter()
            .map(|s| s.class_name().clone())
            .collect();
        for expected in [
            "Entry",
            "KindData",
            "Type",
            "ConstExpr",
            "Function",
            "Parameter",
        ] {
            assert!(
                names.iter().any(|n| n == expected),
                "schema tree missing {expected}: {names:?}"
            );
        }
    }

    #[test]
    fn projects_a_function_entry_to_instance_json() {
        let mut s = ir::kind::Symbol::placeholder(ir::function::Function {
            input_parameters: None,
            output_parameters: None,
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
        s.name = "do_thing".into();
        s.path = local("pkg/do_thing");
        let ir_entry = ir::kind::Entry::Function(s);

        let doc = entry(&ir_entry);
        assert_eq!(doc.fq_name, "pkg::do_thing");
        assert_eq!(doc.name, "do_thing");
        assert!(matches!(doc.kind, m::KindData::Function(_)));

        // Instance serialises to TerminusDB JSON-LD with @type framing.
        let json = doc.to_instance(None).to_json();
        assert_eq!(json["@type"], "Entry");
        assert_eq!(json["fq_name"], "pkg::do_thing");
    }
}
