//! `Alias`, type-alias and associated-type declaration kind.
use crate::{
    List,
    kinds::{AutoFact, GenericParam, Type, WherePred},
    visitor::Visitor,
};

// Generics and where-clauses are modelled via `generics: List<GenericParam>`
// and `wheres: List<WherePred>`.  Remaining deferred items: const-expressions
// and variance annotations.

/// A type-alias (or associated-type declaration) binding a name to a type
/// expression.
///
/// Languages represented: Rust `type Foo = Bar<u32>;`, C/C++ `using Foo =
/// Bar;`, TypeScript `type Foo = …`, C# `using Alias = Ns.Type;`, Swift
/// `typealias`.
///
/// # Naming
///
/// This kind is named **`Alias`** rather than `Type` to avoid a name collision
/// with [`crate::kinds::ty::Type`], the type-*expression* enum already present
/// in this crate.  The old `workspace/ir` model called it `TypeAliasWire`; the
/// semantic intent is identical.
///
/// # Name, visibility, documentation
///
/// These live on the owning [`Entry`](crate::entry::Entry)'s
/// [`Symbol`](crate::entry::Symbol) — there is no `name` field here.  Consult
/// `Record` or `Trait` for the same convention.
///
/// # `target` optionality
///
/// `target` is `Option<Type>` rather than `Type` because an *abstract
/// associated type declaration* inside a trait body (`type Item;`) is a
/// legitimate alias kind with no target expression.  Making it non-optional
/// would force producers to either fabricate a bogus `Type` or refuse to emit
/// the declaration at all — both are worse than `None`.  Concretely:
///
/// - `type Foo = Bar<u32>;` → `target = Some(Type::Apply { … })`
/// - `type Item;`           → `target = None`
/// - `type Item: Display;`  → `target = None`, `bounds = [Type::Nominal(…)]`
/// - `type Item = String;`  → `target = Some(Type::Nominal(…))`, bounds may
///   also be present (Rust allows both when the alias is an associated item
///   with a default).
///
/// # Why an alias does *not* feed the identity skeleton
///
/// [`seal`](crate::package) builds a structural skeleton only for `impl`
/// blocks and for functions that *collide* on `(kind, path, name)`. An alias
/// deliberately gets neither, and that is not an oversight:
///
/// - Two aliases cannot legitimately share a path and name — that is a
///   redeclaration — so there is nothing to disambiguate. The generic
///   [`Disambiguator::Span`](crate::intro::Disambiguator) fallback already
///   covers the malformed-input case.
/// - More importantly, hashing `target` into the id would make identity
///   *signature-dependent*: editing `type Foo = Bar<u32>` into `type Foo =
///   Bar<String>` would mint a **new** `IntroId` and sever the symbol's
///   history, when it is plainly the same declaration edited in place. Identity
///   must survive that edit — the same reasoning behind
///   `unique_fn_id_is_signature_stable` in the seal tests.
///
/// The `target` still participates in identity *indirectly and correctly*: it
/// is walked by the [`Visitor`] derive, so its nominal refs are lowered to
/// `IntroId`s at seal time.
#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Alias {
    /// The right-hand-side type expression this alias expands to.
    ///
    /// `None` for abstract associated-type declarations (`type Item;`).
    pub target: Option<Type>,

    /// Generic parameters declared on this alias, in declaration order.
    ///
    /// Example: `type Result<T> = std::result::Result<T, Error>;` → one `T`
    /// param.
    pub generics: List<GenericParam>,

    /// Where-clause predicates for this alias, in declaration order.
    pub wheres: List<WherePred>,

    /// Trait-object or associated-type bounds imposed on this alias.
    ///
    /// Used for abstract associated types with a bound (`type Item: Display;`)
    /// or for opaque return-position impl-trait aliases.
    ///
    /// The old `workspace/ir` `TypeAliasWire` did **not** model this field;
    /// it is added here because the information is available from all major
    /// language oracle outputs (rustdoc, ra_ap, deno_doc) and dropping it
    /// would force a lossy round-trip.  Lowering producers that do not have
    /// this data should leave the list empty.
    pub bounds: List<Type>,

    /// Recorded auto-trait implementation facts for the aliased type.
    ///
    /// The old `TypeAliasWire` carried this alongside `RecordWire`/`EnumWire`;
    /// kept here for parity. Empty when the producer has no such data.
    pub auto: List<AutoFact>,
}

#[bon::bon]
impl Alias {
    #[builder]
    pub fn new(
        target: Option<Type>,
        #[builder(default, with = FromIterator::from_iter)] generics: List<GenericParam>,
        #[builder(default, with = FromIterator::from_iter)] wheres: List<WherePred>,
        #[builder(default, with = FromIterator::from_iter)] bounds: List<Type>,
        #[builder(default, with = FromIterator::from_iter)] auto: List<AutoFact>,
    ) -> Self {
        Alias {
            target,
            generics,
            wheres,
            bounds,
            auto,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        kinds::{GenericParam, WherePred},
        test_helpers::*,
    };

    // -------------------------------------------------------------------------
    // Builder / round-trip tests
    // -------------------------------------------------------------------------

    /// A concrete alias with a target and one generic parameter.
    ///
    /// Models: `type Result<T> = std::result::Result<T, MyError>;`
    #[test]
    fn builder_concrete_alias() {
        let alias = Alias::builder()
            .target(Type::Any)
            .generics([GenericParam::Type {
                name: "T".to_owned(),
                bounds: Box::new([]),
                default: None,
                variance: None,
            }])
            .build();

        assert_eq!(alias.target, Some(Type::Any));
        assert_eq!(alias.generics.len(), 1);
        assert!(alias.wheres.is_empty());
        assert!(alias.bounds.is_empty());
    }

    /// An abstract associated-type declaration with no target.
    ///
    /// Models: `type Item;`
    #[test]
    fn builder_abstract_associated_type_no_target() {
        let alias = Alias::builder().maybe_target(None).build();

        assert_eq!(alias.target, None);
        assert!(alias.generics.is_empty());
        assert!(alias.wheres.is_empty());
        assert!(alias.bounds.is_empty());
    }

    /// An abstract associated type with a bound but no target.
    ///
    /// Models: `type Item: Display;`
    #[test]
    fn builder_associated_type_with_bound() {
        let alias = Alias::builder()
            .maybe_target(None)
            .bounds([Type::Any])
            .build();

        assert_eq!(alias.target, None);
        assert_eq!(alias.bounds.len(), 1);
        assert_eq!(alias.bounds[0], Type::Any);
    }

    /// A where-clause predicate is preserved round-trip through the builder.
    ///
    /// Models: `type Pair<T> where T: Any = (T, T);`
    #[test]
    fn builder_where_predicate() {
        let pred = WherePred {
            target: Type::SelfType,
            bounds: Box::new([Type::Any]),
        };
        let alias = Alias::builder()
            .target(Type::SelfType)
            .wheres([pred.clone()])
            .build();

        assert_eq!(alias.wheres.len(), 1);
        assert_eq!(alias.wheres[0], pred);
    }

    // -------------------------------------------------------------------------
    // Serde round-trip
    // -------------------------------------------------------------------------

    /// JSON round-trip for a fully-populated `Alias`.
    #[test]
    fn serde_round_trip_full() {
        let original = Alias::builder()
            .target(Type::I32)
            .generics([GenericParam::Type {
                name: "T".to_owned(),
                bounds: Box::new([]),
                default: None,
                variance: None,
            }])
            .wheres([WherePred {
                target: Type::SelfType,
                bounds: Box::new([Type::Any]),
            }])
            .bounds([Type::Any])
            .build();

        let json = serde_json::to_string(&original).expect("serialisation failed");
        let decoded: Alias = serde_json::from_str(&json).expect("deserialisation failed");
        assert_eq!(original, decoded);
    }

    /// JSON round-trip for an abstract alias (`target = None`).
    #[test]
    fn serde_round_trip_abstract() {
        let original = Alias::builder().maybe_target(None).build();

        let json = serde_json::to_string(&original).expect("serialisation failed");
        let decoded: Alias = serde_json::from_str(&json).expect("deserialisation failed");
        assert_eq!(original, decoded);
    }

    // -------------------------------------------------------------------------
    // Entry integration: build an IrPackage containing an Alias kind
    // -------------------------------------------------------------------------

    /// Wrap `Alias` in an `Entry` via `IrPackage::build` and verify
    /// `downcast::<Alias>()` succeeds.
    #[test]
    fn alias_entry_downcast() {
        use crate::kind::KindDiscriminant;

        let mut id = id_gen();

        let pkg = IrPackage::build(PackageId::path("pkg"), sym("root"), |mut root| {
            root.create(id(), sym("MyAlias"), |_| {
                Alias::builder().target(Type::U32).build()
            });
        });

        let entries: Vec<_> = pkg.iter().map(|(_, e)| e).collect();
        let alias_entry = entries
            .iter()
            .find(|e| e.sym().name == "MyAlias")
            .expect("MyAlias entry not found");

        assert!(
            alias_entry.downcast::<Alias>().is_some(),
            "downcast::<Alias>() should be Some"
        );
        assert!(
            alias_entry.downcast::<Record>().is_none(),
            "downcast::<Record>() should be None for an Alias entry"
        );

        let typed = alias_entry.downcast::<Alias>().unwrap();
        assert_eq!(typed.body().target, Some(Type::U32));

        // Wire discriminant stays frozen at 5.
        assert_eq!(KindDiscriminant::Alias.as_u16(), 5);
    }
}
