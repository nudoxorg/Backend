//! Rendering a [`Type`] as the text a developer would have written.
//!
//! # Why this exists
//!
//! There was no `Display for Type` anywhere in this workspace, so consumers
//! reached for `Debug`. `nudox-graph`'s `Field.typeStr` was literally
//! `format!("{t:?}")` — a Rust struct dump shipped to MCP clients as if it were
//! a rendered type — and four more type-valued schema fields (`Alias.target`,
//! `Impl.self_ty`, `Const.ty`, `Static.ty`, `Param.ty`) were left unexposed
//! rather than replicate it. A `Debug` dump is not a rendering: it prints
//! `Primitive(Integer { signed: true, width: Fixed(32) })` where the answer is
//! `i32`, and it leaks the IR's field names into a public surface.
//!
//! It also matters specifically for [`Type::Unknown`]. Splitting `Type::Any`
//! into named reasons is only *useful* if a consumer can show "no annotation
//! was written" apart from "we could not resolve this import" — under `Debug`
//! both are noise, and under the old `Type::Any` both were the word `Any`.
//!
//! # Nominal references need a resolver
//!
//! A [`Type::Nominal`] holds a [`RawRef`], and only two of its three states
//! carry a name:
//!
//! - `Ref::Foreign` carries [`ForeignKey::display`](crate::foreign::ForeignKey)
//!   — the producer's own spelling of the leaf, renderable with no corpus.
//! - `Ref::Intro` and `Ref::Local` carry only an identity, whose *name* lives
//!   in a package's entry table that `nudox-ir` deliberately does not reach
//!   into from here.
//!
//! So [`Type::render`] renders what it can and marks the rest, and
//! [`Type::render_with`] takes the caller's resolver. A caller holding a
//! package view should always use the latter; the placeholder is honest
//! output, not good output.
//!
//! # No `Debug` fallback, anywhere
//!
//! Every arm below writes real text. If a future variant is added and someone
//! reaches for `{:?}` to fill the arm, that is this module's bug repeating —
//! write the syntax instead, or `?unsupported(name)` if the IR genuinely has
//! nothing to say.

use std::fmt;

use crate::{
    index::{RawRef, Ref},
    kinds::{
        Type, UnknownType,
        ty::{
            AnonField, AnonRecordForm, MappedModifier, Primitive, TemplatePart, TupleElement,
            Variance, Width,
        },
    },
};

/// Resolve a nominal reference to a display name.
///
/// Returning `None` is allowed and means "I do not know this one"; the renderer
/// falls back to a marked placeholder rather than inventing a name.
pub type NameResolver<'a> = &'a dyn Fn(&RawRef) -> Option<String>;

/// A [`Type`] bound to a rendering strategy. See [`Type::render`].
pub struct TypeDisplay<'a> {
    ty: &'a Type,
    names: Option<NameResolver<'a>>,
}

impl Type {
    /// Render this type without a name resolver.
    ///
    /// Foreign references render by their producer-supplied display name;
    /// same-package references render as a marked placeholder, because their
    /// names live in the package's entry table and not in the type. Prefer
    /// [`Type::render_with`] wherever a package view is in hand.
    pub fn render(&self) -> TypeDisplay<'_> {
        TypeDisplay {
            ty: self,
            names: None,
        }
    }

    /// Render this type, resolving nominal references through `names`.
    pub fn render_with<'a>(&'a self, names: NameResolver<'a>) -> TypeDisplay<'a> {
        TypeDisplay {
            ty: self,
            names: Some(names),
        }
    }
}

/// `format!("{ty}")` renders without a resolver, exactly like
/// [`Type::render`]. This exists so that the common consumer case — a schema
/// field that just needs *a* string — cannot accidentally reach for `Debug`.
impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.render().fmt(f)
    }
}

impl fmt::Display for TypeDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Renderer { names: self.names }.ty(f, self.ty)
    }
}

/// Rendering for a single [`UnknownType`] reason, standalone.
///
/// Kept public because a consumer often wants to *explain* the gap in prose
/// ("this parameter is unannotated") separately from rendering the signature.
impl fmt::Display for UnknownType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Every unknown is prefixed `?` so it can never be mistaken for a type
        // the source actually wrote — with the single deliberate exception of
        // `DynamicallyTyped`, which *was* written and renders as `dynamic`.
        //
        // The prefix is also what keeps unknowns visibly distinct from
        // `Type::Any`, which renders as the bare word `any`: `any` is a real
        // top type you can pass anything to, `?unannotated` is a hole.
        match self {
            UnknownType::Unannotated => f.write_str("?unannotated"),
            UnknownType::DynamicallyTyped => f.write_str("dynamic"),
            UnknownType::UnresolvedLocalName { name } => write!(f, "?unresolved({name})"),
            UnknownType::UnresolvedExternal { name } => write!(f, "?external({name})"),
            UnknownType::TruncatedAtDepthLimit => f.write_str("?depth-limit"),
            UnknownType::OracleGap => f.write_str("?oracle-gap"),
            UnknownType::NoIrRepresentation { construct } => write!(f, "?unsupported({construct})"),
        }
    }
}

struct Renderer<'a> {
    names: Option<NameResolver<'a>>,
}

impl Renderer<'_> {
    /// Render `ty`, parenthesised if it is an infix form that would otherwise
    /// re-associate against its surroundings.
    ///
    /// `A | B` inside a slice must print `[(A | B)]`, not `[A | B]` — the
    /// latter reads as a slice of `A` unioned with `B`. Nothing else in the
    /// lattice is infix, so this is the whole precedence story.
    fn nested(&self, f: &mut fmt::Formatter<'_>, ty: &Type) -> fmt::Result {
        let infix = matches!(
            ty,
            Type::Union(items) | Type::Intersection(items) if items.len() > 1
        ) || matches!(ty, Type::Conditional { .. })
            || matches!(ty, Type::ImplTrait(_) | Type::DynTrait(_));
        if infix {
            f.write_str("(")?;
            self.ty(f, ty)?;
            f.write_str(")")
        } else {
            self.ty(f, ty)
        }
    }

    fn ty(&self, f: &mut fmt::Formatter<'_>, ty: &Type) -> fmt::Result {
        // No `_` arm: a new `Type` variant must be given real syntax here.
        match ty {
            Type::SelfType => f.write_str("Self"),
            Type::Primitive(p) => self.primitive(f, p),

            // `()` for unit, `(A, B)` positional, `(start: A, end: B)` labelled.
            Type::Tuple(elems) => {
                f.write_str("(")?;
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    match e {
                        TupleElement::Positional(t) => self.ty(f, t)?,
                        TupleElement::Named { label, ty } => {
                            write!(f, "{label}: ")?;
                            self.ty(f, ty)?;
                        }
                    }
                }
                // A one-element positional tuple is `(T,)`, not `(T)` — the
                // latter is just a parenthesised type in every language that
                // has tuples at all.
                if elems.len() == 1 && matches!(elems[0], TupleElement::Positional(_)) {
                    f.write_str(",")?;
                }
                f.write_str(")")
            }

            Type::Slice(inner) => {
                f.write_str("[")?;
                self.ty(f, inner)?;
                f.write_str("]")
            }

            Type::Array { ty, length } => {
                f.write_str("[")?;
                self.ty(f, ty)?;
                write!(f, "; {length}]")
            }

            Type::Union(items) => self.joined(f, items, " | "),
            Type::Intersection(items) => self.joined(f, items, " & "),

            Type::Never => f.write_str("!"),

            // The *top* type. Bare, unmarked, and deliberately unlike every
            // `Type::Unknown` rendering — see `impl Display for UnknownType`.
            Type::Any => f.write_str("any"),

            Type::Unknown(reason) => write!(f, "{reason}"),

            Type::Nominal(r) => self.nominal(f, r),

            Type::Apply { base, args } => {
                self.nested(f, base)?;
                f.write_str("<")?;
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    self.ty(f, a)?;
                }
                f.write_str(">")
            }

            Type::TypeVar(name) => f.write_str(name),

            // Java/Kotlin reading order: `?`, `? extends T`, `? super T`.
            Type::Wildcard { variance, bound } => {
                f.write_str("?")?;
                match (variance, bound) {
                    (_, None) => Ok(()),
                    (Variance::Covariant, Some(b)) => {
                        f.write_str(" extends ")?;
                        self.ty(f, b)
                    }
                    (Variance::Contravariant, Some(b)) => {
                        f.write_str(" super ")?;
                        self.ty(f, b)
                    }
                    (Variance::Invariant, Some(b)) => {
                        // An invariant wildcard with a bound has no source
                        // spelling in any of the seven languages; render the
                        // bound so the information is not silently dropped.
                        f.write_str(" ")?;
                        self.ty(f, b)
                    }
                }
            }

            // `fn(A, B) -> R`, prefixed with the ABI when one is recorded, so
            // that two delegates differing only in calling convention read
            // differently as well as hashing differently.
            Type::FunctionPointer { params, ret, abi } => {
                if let Some(abi) = abi {
                    write!(f, "extern \"{abi}\" ")?;
                }
                f.write_str("fn(")?;
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    self.ty(f, p)?;
                }
                f.write_str(")")?;
                if let Some(r) = ret {
                    f.write_str(" -> ")?;
                    self.ty(f, r)?;
                }
                Ok(())
            }

            // Annotation first, as written: `@NonNull String`.
            Type::Annotated { inner, annotation } => {
                write!(f, "@{}", annotation.token)?;
                if let Some(arg) = &annotation.arg {
                    write!(f, "({arg})")?;
                }
                f.write_str(" ")?;
                self.nested(f, inner)
            }

            Type::Conditional {
                check,
                extends_ty,
                then_ty,
                else_ty,
            } => {
                self.nested(f, check)?;
                f.write_str(" extends ")?;
                self.nested(f, extends_ty)?;
                f.write_str(" ? ")?;
                self.nested(f, then_ty)?;
                f.write_str(" : ")?;
                self.nested(f, else_ty)
            }

            // `{ readonly [P in K]?: V }`, with `-readonly` / `-?` for removal.
            Type::Mapped {
                key_var,
                source,
                value,
                readonly,
                optional,
            } => {
                f.write_str("{ ")?;
                match readonly {
                    MappedModifier::Add => f.write_str("readonly ")?,
                    MappedModifier::Remove => f.write_str("-readonly ")?,
                    MappedModifier::Absent => {}
                }
                write!(f, "[{key_var} in ")?;
                self.ty(f, source)?;
                f.write_str("]")?;
                match optional {
                    MappedModifier::Add => f.write_str("?")?,
                    MappedModifier::Remove => f.write_str("-?")?,
                    MappedModifier::Absent => {}
                }
                f.write_str(": ")?;
                self.ty(f, value)?;
                f.write_str(" }")
            }

            // Backticks and `${}` exactly as TypeScript writes them.
            Type::TemplateLiteral(parts) => {
                f.write_str("`")?;
                for part in parts {
                    match part {
                        TemplatePart::Literal(s) => f.write_str(s)?,
                        TemplatePart::Interpolated(t) => {
                            f.write_str("${")?;
                            self.ty(f, t)?;
                            f.write_str("}")?;
                        }
                    }
                }
                f.write_str("`")
            }

            Type::AnonymousRecord { form, members } => {
                if matches!(form, AnonRecordForm::Interface) {
                    f.write_str("interface ")?;
                }
                if members.is_empty() {
                    return f.write_str("{}");
                }
                f.write_str("{ ")?;
                for (i, m) in members.iter().enumerate() {
                    if i > 0 {
                        f.write_str("; ")?;
                    }
                    self.anon_field(f, m)?;
                }
                f.write_str(" }")
            }

            Type::ImplTrait(bounds) => {
                f.write_str("impl ")?;
                self.joined(f, bounds, " + ")
            }
            Type::DynTrait(bounds) => {
                f.write_str("dyn ")?;
                self.joined(f, bounds, " + ")
            }

            // The written inference request, spelled the way Rust spells it.
            Type::Inferred => f.write_str("_"),

            Type::QualifiedPath {
                self_ty,
                trait_ref,
                assoc,
            } => {
                if let Some(tr) = trait_ref {
                    f.write_str("<")?;
                    self.ty(f, self_ty)?;
                    f.write_str(" as ")?;
                    self.ty(f, tr)?;
                    write!(f, ">::{assoc}")
                } else {
                    self.nested(f, self_ty)?;
                    write!(f, "::{assoc}")
                }
            }
        }
    }

    fn anon_field(&self, f: &mut fmt::Formatter<'_>, m: &AnonField) -> fmt::Result {
        if m.readonly {
            f.write_str("readonly ")?;
        }
        f.write_str(&m.name)?;
        if m.optional {
            f.write_str("?")?;
        }
        f.write_str(": ")?;
        self.ty(f, &m.ty)
    }

    fn joined(&self, f: &mut fmt::Formatter<'_>, items: &[Type], sep: &str) -> fmt::Result {
        // An empty union/intersection/bound-list has no source spelling. Say so
        // rather than emitting nothing at all, which would render as a blank
        // cell in a UI and read as a bug in the UI rather than in the data.
        if items.is_empty() {
            return f.write_str("?oracle-gap");
        }
        for (i, t) in items.iter().enumerate() {
            if i > 0 {
                f.write_str(sep)?;
            }
            self.nested(f, t)?;
        }
        Ok(())
    }

    fn nominal(&self, f: &mut fmt::Formatter<'_>, r: &RawRef) -> fmt::Result {
        if let Some(resolve) = self.names
            && let Some(name) = resolve(r)
        {
            return f.write_str(&name);
        }
        match r {
            // The producer's own spelling of the leaf, carried on the key
            // precisely so a cross-package reference is renderable with no
            // corpus loaded.
            Ref::Foreign { key, .. } => f.write_str(&key.display),
            // Same-package, sealed. The name lives in the package's entry
            // table; a caller with one should use `render_with`. The short
            // digest keeps two different targets visibly different instead of
            // collapsing both to one placeholder.
            Ref::Intro(id) => {
                let hex = id.to_hex();
                write!(f, "?ref({})", &hex[..hex.len().min(8)])
            }
            // Build-time only; post-seal this is a seal bug, and the rendering
            // says so rather than pretending to be a type.
            Ref::Local(_) => f.write_str("?unsealed-ref"),
        }
    }

    fn primitive(&self, f: &mut fmt::Formatter<'_>, p: &Primitive) -> fmt::Result {
        match p {
            Primitive::Integer { signed, width } => {
                let stem = if *signed { 'i' } else { 'u' };
                match width {
                    Width::Fixed(n) => write!(f, "{stem}{}", n.get()),
                    // Rust's spelling for the pointer-sized integers, which is
                    // the one every other language's docs borrow.
                    Width::Arch => write!(f, "{stem}size"),
                }
            }
            Primitive::Float(w) => match w {
                Width::Fixed(n) => write!(f, "f{}", n.get()),
                Width::Arch => f.write_str("fsize"),
            },
            Primitive::Bool => f.write_str("bool"),
            Primitive::Char => f.write_str("char"),
            Primitive::Str => f.write_str("str"),
            Primitive::MutPointer(t) => {
                f.write_str("*mut ")?;
                self.nested(f, t)
            }
            Primitive::ConstPointer(t) => {
                f.write_str("*const ")?;
                self.nested(f, t)
            }
            Primitive::Reference {
                lifetime,
                mutable,
                ty,
            } => {
                f.write_str("&")?;
                if let Some(lt) = lifetime {
                    // `lifetime_label` normalises the sigil; producers disagree
                    // about whether they store it.
                    write!(f, "{} ", crate::kinds::lifetime_label(lt))?;
                }
                if *mutable {
                    f.write_str("mut ")?;
                }
                self.nested(f, ty)
            }
            // A language builtin with no algebraic slot — `bytes`, `None`,
            // `bigint`, `Date`. The producer's own spelling is the answer.
            Primitive::Builtin(name) => f.write_str(name),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        change::IntroId,
        entry::AttrTok,
        foreign::{ForeignKey, ForeignOrigin},
        kinds::ty::{AnonField, AnonRecordForm, TemplatePart, TupleElement},
    };

    fn foreign(display: &str) -> Type {
        Type::Nominal(Ref::Foreign {
            key: triomphe::Arc::new(ForeignKey {
                origin: ForeignOrigin::Universe {
                    ecosystem: crate::test_helpers::EcosystemId::new("test"),
                },
                path: display.into(),
                display: display.into(),
                kind: None,
            }),
            target: None,
        })
    }

    #[track_caller]
    fn r(ty: Type, expected: &str) {
        assert_eq!(ty.to_string(), expected);
    }

    #[test]
    fn primitives_render_as_written() {
        r(Type::I32, "i32");
        r(Type::U64, "u64");
        r(
            Type::Primitive(Primitive::Integer {
                signed: false,
                width: Width::Arch,
            }),
            "usize",
        );
        r(Type::Primitive(Primitive::Float(Width::W64)), "f64");
        r(Type::Primitive(Primitive::Bool), "bool");
        r(Type::Primitive(Primitive::Str), "str");
        r(
            Type::Primitive(Primitive::Builtin("bigint".to_owned())),
            "bigint",
        );
        r(
            Type::Primitive(Primitive::MutPointer(Box::new(Type::I8))),
            "*mut i8",
        );
        r(
            Type::Primitive(Primitive::Reference {
                lifetime: Some("a".to_owned()),
                mutable: true,
                ty: Box::new(Type::I32),
            }),
            "&'a mut i32",
        );
    }

    #[test]
    fn compound_types_render_in_reading_order() {
        r(Type::Tuple(Box::new([])), "()");
        r(
            Type::Tuple(Box::new([TupleElement::Positional(Type::I32)])),
            "(i32,)",
        );
        r(
            Type::Tuple(Box::new([
                TupleElement::Named {
                    label: "start".to_owned(),
                    ty: Type::I32,
                },
                TupleElement::Named {
                    label: "end".to_owned(),
                    ty: Type::I32,
                },
            ])),
            "(start: i32, end: i32)",
        );
        r(Type::Slice(Box::new(Type::U8)), "[u8]");
        r(
            Type::Array {
                ty: Box::new(Type::U8),
                length: 4,
            },
            "[u8; 4]",
        );
        r(Type::Union(Box::new([Type::I32, Type::Never])), "i32 | !");
        r(
            Type::Apply {
                base: Box::new(foreign("Vec")),
                args: Box::new([Type::U8]),
            },
            "Vec<u8>",
        );
        r(
            Type::FunctionPointer {
                params: Box::new([Type::I32]),
                ret: Some(Box::new(Type::Primitive(Primitive::Bool))),
                abi: Some("Cdecl".to_owned()),
            },
            "extern \"Cdecl\" fn(i32) -> bool",
        );
        r(
            Type::Annotated {
                inner: Box::new(Type::Primitive(Primitive::Str)),
                annotation: AttrTok {
                    token: "NonNull".to_owned(),
                    arg: None,
                },
            },
            "@NonNull str",
        );
        r(
            Type::QualifiedPath {
                self_ty: Box::new(Type::TypeVar("T".to_owned())),
                trait_ref: Some(Box::new(foreign("Iterator"))),
                assoc: "Item".to_owned(),
            },
            "<T as Iterator>::Item",
        );
        r(Type::ImplTrait(Box::new([foreign("Clone")])), "impl Clone");
        r(
            Type::DynTrait(Box::new([foreign("Debug"), foreign("Send")])),
            "dyn Debug + Send",
        );
        r(
            Type::TemplateLiteral(Box::new([
                TemplatePart::Literal("error-".to_owned()),
                TemplatePart::Interpolated(Box::new(Type::TypeVar("Code".to_owned()))),
            ])),
            "`error-${Code}`",
        );
        r(
            Type::AnonymousRecord {
                form: AnonRecordForm::Struct,
                members: Box::new([AnonField {
                    name: "x".to_owned(),
                    ty: Type::I32,
                    optional: true,
                    readonly: true,
                }]),
            },
            "{ readonly x?: i32 }",
        );
    }

    /// A union nested inside a compound must be parenthesised, or `[A | B]`
    /// reads as "slice of A, or B".
    #[test]
    fn infix_types_are_parenthesised_when_nested() {
        r(
            Type::Slice(Box::new(Type::Union(Box::new([Type::I32, Type::U8])))),
            "[i32 | u8]",
        );
        r(
            Type::Union(Box::new([
                Type::Union(Box::new([Type::I32, Type::U8])),
                Type::Never,
            ])),
            "(i32 | u8) | !",
        );
    }

    /// **The reason this module was asked for.** Each unknown reason must
    /// render as distinct, human-actionable text — never `Debug`, never the
    /// same word as the top type.
    #[test]
    fn every_unknown_reason_renders_distinctly_and_actionably() {
        let cases = [
            (Type::UNANNOTATED, "?unannotated"),
            (Type::DYNAMIC, "dynamic"),
            (Type::unresolved_local("Context"), "?unresolved(Context)"),
            (
                Type::unresolved_external("click.core.Context"),
                "?external(click.core.Context)",
            ),
            (Type::TRUNCATED, "?depth-limit"),
            (Type::ORACLE_GAP, "?oracle-gap"),
            (
                Type::no_ir_representation("complex128"),
                "?unsupported(complex128)",
            ),
        ];
        let mut seen: Vec<String> = Vec::new();
        for (ty, expected) in cases {
            let rendered = ty.to_string();
            assert_eq!(rendered, expected);
            assert!(
                !seen.contains(&rendered),
                "two reasons rendered identically: {rendered}"
            );
            seen.push(rendered);
        }

        // The top type must not look like any of them.
        let any = Type::Any.to_string();
        assert_eq!(any, "any");
        assert!(!seen.contains(&any), "`any` must not collide with a gap");
        // And the written inference request is its own thing again.
        assert_eq!(Type::Inferred.to_string(), "_");
    }

    /// No rendering may contain Rust `Debug` punctuation — that is the exact
    /// defect (`format!("{t:?}")`) this module replaces.
    #[test]
    fn no_rendering_leaks_debug_syntax() {
        let samples = [
            Type::I32,
            Type::Any,
            Type::Inferred,
            Type::UNANNOTATED,
            Type::DYNAMIC,
            Type::unresolved_external("a.b.C"),
            Type::no_ir_representation("complex128"),
            Type::Slice(Box::new(Type::U8)),
            Type::Apply {
                base: Box::new(foreign("Vec")),
                args: Box::new([Type::UNANNOTATED]),
            },
            Type::Nominal(Ref::Intro(IntroId::from_raw([0xab; 32]))),
        ];
        for s in samples {
            let out = s.to_string();
            assert!(!out.is_empty(), "empty rendering for {s:?}");
            for bad in ["Primitive(", "Type::", "Unknown(", "{ signed", "Fixed("] {
                assert!(
                    !out.contains(bad),
                    "rendering leaked Debug syntax {bad:?}: {out}"
                );
            }
        }
    }

    /// A resolver, when supplied, wins over the placeholder.
    #[test]
    fn render_with_resolves_nominal_names() {
        let id = IntroId::from_raw([0x11; 32]);
        let ty = Type::Nominal(Ref::Intro(id));
        assert!(
            ty.to_string().starts_with("?ref("),
            "no resolver: must be a marked placeholder, not a bare name"
        );
        let names = |_: &RawRef| Some("MyRecord".to_owned());
        assert_eq!(ty.render_with(&names).to_string(), "MyRecord");
    }

    /// Two different sealed targets must not render identically — a UI listing
    /// fields would otherwise show the same text for different types.
    #[test]
    fn distinct_sealed_nominals_render_differently() {
        let a = Type::Nominal(Ref::Intro(IntroId::from_raw([0xaa; 32])));
        let b = Type::Nominal(Ref::Intro(IntroId::from_raw([0xbb; 32])));
        assert_ne!(a.to_string(), b.to_string());
    }
}
