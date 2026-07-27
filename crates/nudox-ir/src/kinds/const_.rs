use crate::{kinds::Type, visitor::Visitor};

// NOTE: the full initializer *expression* (a `ConstExpr` AST node) is
// intentionally deferred — it depends on a const-expression subsystem that
// does not yet exist. The rendered value text (`value` field below) is NOT
// deferred: producers emit it whenever the oracle exposes it, and it is
// sufficient for display and basic analysis without a structured expression.

/// A compile-time constant declaration.
///
/// The const's name, visibility, and documentation live on the owning
/// [`Entry`](crate::entry::Entry)'s [`Symbol`](crate::entry::Symbol).
#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Const {
    /// The declared type of the constant.
    pub ty: Type,

    /// The constant's value, rendered as source text, if available.
    ///
    /// Examples: `"42"`, `"\"hello\""`, `"1 + 1"`. Populated whenever the
    /// oracle exposes it (rustdoc `const_` items, ra, Java `static final`
    /// initialisers). `None` when the oracle does not include the value or
    /// when it is too complex to render inline.
    pub value: Option<String>,
}

#[bon::bon]
impl Const {
    #[builder]
    pub fn new(ty: Type, value: Option<String>) -> Self {
        Const { ty, value }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Builder + serde round-trip for `Const.value` (gap 3).
    #[test]
    fn const_value_roundtrip() {
        let c = Const::builder()
            .ty(Type::U64)
            .value("18446744073709551615".to_owned())
            .build();

        assert_eq!(c.value.as_deref(), Some("18446744073709551615"));

        let json = serde_json::to_string(&c).expect("serialize failed");
        let rt: Const = serde_json::from_str(&json).expect("deserialize failed");
        assert_eq!(c, rt);
    }

    /// `value` defaults to `None` when not set via the builder.
    #[test]
    fn const_value_default_none() {
        let c = Const::builder().ty(Type::I32).build();
        assert_eq!(c.value, None);
    }
}
