use crate::{
    List,
    index::Ref,
    kinds::{GenericParam, Param, WherePred, ty::Type},
    visitor::Visitor,
};

// Generics and where-clauses are now modelled via `generics:
// List<GenericParam>` and `wheres: List<WherePred>` (bounds represented as
// `List<Type>`). Remaining deferred items: overloads, explicitly-implemented
// protocols, the parsed function body, const-expressions, and variance.
//
// NOTE: `Receiver::Arbitrary` intentionally does not carry a concrete `Type`
// payload.  Adding one would break `Copy` on `Receiver` (since `Type` contains
// `Box` fields), and the existing `Visitor` / `serde` derives rely on it being
// `Copy`.  The concrete receiver type is a resolution-plane concern deferred to
// the IR-unification work; leave the FIXME comment gone and this note in its
// place.

/// A function, method, or lambda with its full signature.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Function {
    /// How the function receives its `self`/`this`, if at all.
    ///
    /// Free functions (and static/class methods) carry no receiver.
    pub receiver: Option<Receiver>,

    /// The input parameters, in declaration order.
    pub input_params: List<Ref<Param>>,

    /// The output parameters.
    ///
    /// Most languages have a single return type; multi-value returns (Go) and
    /// out-parameters are modelled as additional entries here.
    pub output_params: List<Ref<Param>>,

    /// Signature-level modifiers (`async`, `const`, `unsafe`, …).
    pub modifiers: List<FnModifier>,

    /// Generic parameters declared on this function, in declaration order.
    pub generics: List<GenericParam>,

    /// Where-clause predicates for this function, in declaration order.
    pub wheres: List<WherePred>,

    /// Explicit ABI string, if present.
    ///
    /// Examples: `Some("C")` for `extern "C" fn`, `Some("Rust")` for
    /// `extern "Rust" fn`, `None` for the language default ABI.
    ///
    /// Stored as a separate field rather than a `FnModifier::Abi(String)`
    /// variant because `FnModifier` is `Copy` and adding a `String` payload
    /// would break that. A plain `Option<String>` field matches the old
    /// `FnSigFlags.abi` shape and keeps `FnModifier` `Copy` for callers that
    /// pattern-match over the list without cloning.
    pub abi: Option<String>,

    /// `true` when this is a trait method that provides a default body.
    ///
    /// Corresponds to `FnSigFlags.defaulted` in the old model. Stored as a
    /// bare field rather than a `FnModifier` variant for the same reason as
    /// `abi`: `FnModifier` is `Copy` and a boolean flag does not need to live
    /// in the modifiers list.
    pub is_defaulted: bool,

    /// The checked exception types this function declares it may throw, in
    /// declaration order.
    ///
    /// Requested independently by Java and C# producers (2 of 7). Both had
    /// modelled thrown exceptions as *output parameters* so that a consumer
    /// reading `output_params` would conclude the method *returns* its
    /// exceptions — a semantic confusion that was backed out before landing.
    ///
    /// - Java: `throws IOException, SQLException` in the method signature.
    /// - C#: exception documentation via `<exception cref="...">` that the
    ///   Roslyn oracle surfaces as type references.
    ///
    /// An empty list means the function declares no checked throws. Producers
    /// for languages without checked exceptions (Rust, Go, C/C++, TypeScript,
    /// Python) leave this empty.
    pub throws: List<Type>,
}

#[bon::bon]
impl Function {
    #[builder]
    pub fn new(
        receiver: Option<Receiver>,
        #[builder(default, with = FromIterator::from_iter)] input_params: List<Ref<Param>>,
        #[builder(default, with = FromIterator::from_iter)] output_params: List<Ref<Param>>,
        #[builder(default, with = FromIterator::from_iter)] modifiers: List<FnModifier>,
        #[builder(default, with = FromIterator::from_iter)] generics: List<GenericParam>,
        #[builder(default, with = FromIterator::from_iter)] wheres: List<WherePred>,
        abi: Option<String>,
        #[builder(default)] is_defaulted: bool,
        #[builder(default, with = FromIterator::from_iter)] throws: List<Type>,
    ) -> Self {
        Function {
            receiver,
            input_params,
            output_params,
            modifiers,
            generics,
            wheres,
            abi,
            is_defaulted,
            throws,
        }
    }
}

/// How a method receives its instance (`self`, `this`, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum Receiver {
    /// Takes ownership (`self` in Rust, `consuming` in Swift).
    Owned,

    /// Immutable reference (`&self`, `borrowing` in Swift).
    SharedRef,

    /// Mutable reference (`&mut self`, `mutating` in Swift).
    MutRef,

    /// An arbitrary receiver type (e.g. `self: Rc<Self>`, `self: Pin<&mut
    /// Self>`).
    ///
    /// The concrete type is not represented here: carrying it would require a
    /// `Type` payload which breaks `Copy` on this enum. The concrete receiver
    /// type is a resolution-plane concern; use the body plane for that detail.
    Arbitrary,
}

/// A signature-level modifier on a [`Function`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum FnModifier {
    /// Runs asynchronously (`async`).
    Async,

    /// Evaluable at compile time (`const`, `constexpr`).
    Const,

    /// Requires an unsafe context (Rust `unsafe`).
    Unsafe,

    /// Free of observable side effects.
    Pure,

    /// Can suspend and resume between yields (a generator/coroutine).
    Generator,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Builder + serde round-trip for `Function.abi` and
    /// `Function.is_defaulted` (gaps 4 and 5).
    #[test]
    fn function_abi_and_defaulted_roundtrip() {
        let f = Function::builder()
            .receiver(Receiver::SharedRef)
            .modifiers([FnModifier::Unsafe])
            .abi("C".to_owned())
            .is_defaulted(true)
            .build();

        assert_eq!(f.abi.as_deref(), Some("C"));
        assert!(f.is_defaulted);

        let json = serde_json::to_string(&f).expect("serialize failed");
        let rt: Function = serde_json::from_str(&json).expect("deserialize failed");
        assert_eq!(f, rt);
    }

    /// `abi` and `is_defaulted` default to `None` / `false`.
    #[test]
    fn function_abi_defaulted_defaults() {
        let f = Function::builder().build();
        assert_eq!(f.abi, None);
        assert!(!f.is_defaulted);
    }

    /// Builder + serde round-trip for `Function.throws`.
    ///
    /// Java `throws IOException, SQLException` and C# exception annotations
    /// are recorded here, not as output_params.
    #[test]
    fn function_throws_roundtrip() {
        use crate::kinds::ty::{Primitive, Type};

        let f = Function::builder()
            .throws([Type::Primitive(Primitive::Str), Type::Any])
            .build();

        assert_eq!(f.throws.len(), 2);

        let json = serde_json::to_string(&f).expect("serialize");
        let rt: Function = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(f, rt);
    }

    /// `throws` defaults to an empty list.
    #[test]
    fn function_throws_default_empty() {
        let f = Function::builder().build();
        assert!(f.throws.is_empty());
    }
}
