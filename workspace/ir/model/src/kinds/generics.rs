//! `GenericParam`, `WherePred`, and the `lifetime_label` helper.
use std::borrow::Cow;

use crate::{List, kinds::Type, kinds::ty::Variance, visitor::Visitor};

/// Render a lifetime's stored name with exactly one leading `'`.
///
/// # Why this exists
///
/// Lifetime names are stored as bare `String`s in two places
/// ([`GenericParam::Lifetime`] and
/// [`Primitive::Reference`](crate::kinds::ty::Primitive::Reference)) and
/// nothing in either type says whether the `'` sigil is part of the stored
/// value. The Rust producer stores the **source spelling**, sigil included
/// (`ra::generics` takes `lp.lifetime().text()`, which yields `'h`), while
/// every renderer assumed the sigil was absent and prepended its own. The
/// result shipped as `impl<''h> Iterator for …` — two apostrophes — in the
/// Implementations list.
///
/// Both conventions look correct at their own call site, which is why the
/// disagreement survived. Normalising here makes the doubled sigil
/// unrepresentable no matter which convention a producer picked, instead of
/// requiring every current and future rendering site to guess the same way.
///
/// Storage is deliberately left alone: `content::encode_generic_param` hashes
/// the name verbatim, so rewriting what producers store would churn every
/// content hash in the corpus to fix a display bug.
pub fn lifetime_label(name: &str) -> Cow<'_, str> {
    match name.strip_prefix('\'') {
        // Already the source spelling — the Rust producer's convention.
        Some(_) => Cow::Borrowed(name),
        None => Cow::Owned(format!("'{name}")),
    }
}

/// A single generic parameter on a declaration.
#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum GenericParam {
    /// A lifetime parameter.
    ///
    /// `name` holds whatever the producer emitted; the `'` sigil may or may
    /// not be included (the Rust producer includes it, because it stores the
    /// source spelling). Never render this field directly — call
    /// [`lifetime_label`], which normalises both conventions to exactly one
    /// leading `'`.
    Lifetime { name: String },

    /// A type parameter with zero or more bounds, an optional default, and an
    /// optional declaration-site variance annotation.
    ///
    /// Bounds are represented as `List<Type>` — each bound is a reference to a
    /// trait modelled as a `Type`. This is a deliberate simplification: a full
    /// trait-bound would carry generic arguments and associated-type bindings,
    /// which are deferred to the resolution/IR-unification plane.
    ///
    /// **Variance:** C# declaration-site `in`/`out` on generic interfaces and
    /// delegates is real API surface — the Roslyn oracle emits it. The
    /// [`Variance`] enum already existed for [`crate::kinds::ty::Type::Wildcard`];
    /// reusing it here costs nothing. `None` means "unspecified / invariant by
    /// default" (the case for all Rust and most Java type parameters).
    /// `Some(Variance::Invariant)` can also be used when a language explicitly
    /// annotates invariance, but producers should prefer `None` when the source
    /// has no annotation.
    Type {
        name: String,
        bounds: List<crate::kinds::Type>,
        default: Option<crate::kinds::Type>,
        // Declaration-site variance annotation.
        // None = unspecified (no annotation in source).
        // Some(Covariant) = `out T` in C#.  Some(Contravariant) = `in T` in C#.
        variance: Option<Variance>,
    },

    /// A const generic parameter, e.g. `const N: usize`.
    ///
    /// The value expression for const generics is deferred (no const-expression
    /// subsystem yet); only the parameter name and its type are stored.
    Const {
        name: String,
        ty: crate::kinds::Type,
    },
}

/// A single `where`-clause predicate: `target: bound + bound + ...`.
///
/// As with [`GenericParam`], bounds are modelled as `List<Type>` — a
/// deliberate simplification pending the resolution/IR-unification plane.
#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct WherePred {
    /// The type or lifetime being constrained.
    pub target: Type,
    /// The bounds imposed on `target`.
    pub bounds: List<Type>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        kinds::ty::{Type, Variance},
        skeleton::trait_impl_skeleton,
    };

    /// Both storage conventions render to exactly one leading `'`.
    ///
    /// The Rust producer stores the source spelling (`'h`) and every renderer
    /// prepended a second sigil, shipping `impl<''h> Iterator for …` to the
    /// Implementations list. Asserting on the rendered text (not on
    /// `is_some()`) is the point: the bug was a string that looked fine to
    /// both sides of the boundary.
    #[test]
    fn a_lifetime_label_carries_exactly_one_sigil_whichever_way_it_was_stored() {
        // The Rust producer's convention: `lp.lifetime().text()` == "'h".
        assert_eq!(lifetime_label("'h"), "'h");
        // The convention every renderer assumed.
        assert_eq!(lifetime_label("h"), "'h");
        // Both spellings of the same lifetime must converge, or the same
        // signature renders differently depending on which producer emitted it.
        assert_eq!(lifetime_label("'static"), lifetime_label("static"));
        // Degenerate input must not panic or silently produce a bare sigil
        // pair; an empty name is a producer bug, not a rendering one.
        assert_eq!(lifetime_label(""), "'");
        assert_eq!(lifetime_label("'"), "'");
    }

    /// GenericParam::Type with variance=None round-trips through serde.
    #[test]
    fn generic_param_type_no_variance_roundtrip() {
        let p = GenericParam::Type {
            name: "T".to_owned(),
            bounds: [Type::Any].into(),
            default: None,
            variance: None,
        };
        let json = serde_json::to_string(&p).expect("serialize");
        let back: GenericParam = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(p, back);
    }

    /// GenericParam::Type with covariant variance round-trips through serde.
    #[test]
    fn generic_param_type_covariant_roundtrip() {
        let p = GenericParam::Type {
            name: "T".to_owned(),
            bounds: [].into(),
            default: None,
            variance: Some(Variance::Covariant),
        };
        let json = serde_json::to_string(&p).expect("serialize");
        let back: GenericParam = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(p, back);
    }

    /// `in T` (Contravariant) and `out T` (Covariant) must have DIFFERENT
    /// skeletons — `interface IFoo<in T>` and `interface IFoo<out T>` are
    /// distinct C# interface contracts.
    #[test]
    fn variance_changes_skeleton() {
        let covariant = [GenericParam::Type {
            name: "T".to_owned(),
            bounds: [].into(),
            default: None,
            variance: Some(Variance::Covariant),
        }];
        let contravariant = [GenericParam::Type {
            name: "T".to_owned(),
            bounds: [].into(),
            default: None,
            variance: Some(Variance::Contravariant),
        }];
        let s_cov = trait_impl_skeleton(None, &Type::Any, &covariant, &[], false, false);
        let s_con = trait_impl_skeleton(None, &Type::Any, &contravariant, &[], false, false);
        assert_ne!(
            s_cov, s_con,
            "covariant and contravariant type params must not produce the same skeleton"
        );
    }

    /// A type param with variance=Some(Covariant) and one with variance=None
    /// must have DIFFERENT skeletons — one is annotated `out T`, the other has
    /// no annotation and is invariant by default.
    #[test]
    fn some_variance_vs_none_differ_in_skeleton() {
        let annotated = [GenericParam::Type {
            name: "T".to_owned(),
            bounds: [].into(),
            default: None,
            variance: Some(Variance::Covariant),
        }];
        let unannotated = [GenericParam::Type {
            name: "T".to_owned(),
            bounds: [].into(),
            default: None,
            variance: None,
        }];
        let s_ann = trait_impl_skeleton(None, &Type::Any, &annotated, &[], false, false);
        let s_una = trait_impl_skeleton(None, &Type::Any, &unannotated, &[], false, false);
        assert_ne!(s_ann, s_una, "out T and unannotated T must not collide");
    }

    /// Alpha-equivalence: renaming a type param with variance=None must still
    /// produce the same skeleton — the name is not identity-relevant.
    #[test]
    fn variance_rename_alpha_equivalent() {
        let t = [GenericParam::Type {
            name: "T".to_owned(),
            bounds: [Type::Any].into(),
            default: None,
            variance: Some(Variance::Covariant),
        }];
        let u = [GenericParam::Type {
            name: "U".to_owned(),
            bounds: [Type::Any].into(),
            default: None,
            variance: Some(Variance::Covariant),
        }];
        let s_t = trait_impl_skeleton(None, &Type::Any, &t, &[], false, false);
        let s_u = trait_impl_skeleton(None, &Type::Any, &u, &[], false, false);
        assert_eq!(
            s_t, s_u,
            "renaming a type param must not change the skeleton even when variance is set"
        );
    }
}
