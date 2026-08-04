//! Local mirror of `nudox_ir::kind::KindDiscriminant` and the colour mapping
//! from kind to [`crate::theme::tokens::KindColours`] field.
//!
//! # Why a local mirror?
//!
//! `nudox_ir` is not (yet) a direct dependency of the `lindsey` GUI crate —
//! the dependency law (§L0) says `lindsey → engine → {graph, store} → ir` and
//! the GUI must not import `nudox-ir` directly.  The wire type that *does* cross
//! the seam will carry a `KindTag` value (a `u16`) which the GUI maps to this
//! local enum for display purposes.
//!
//! TODO(wire): when `nudox-engine::wire` re-exports a stable `KindTag` newtype
//! (or the engine crate becomes a dependency), replace `LocalKindDiscriminant`
//! with a `use nudox_engine::wire::KindTag` and update `From<KindTag>` below.
//! File tracking this: `crates/nudox-engine/src/wire/mod.rs`.

use gpui::Hsla;

use crate::theme::tokens::KindColours;

/// A local copy of the 13 frozen wire discriminants from `nudox_ir::kind`.
///
/// Discriminant values are wire-stable (`repr(u16)`) — they must match the
/// values in `nudox_ir::kind::KindDiscriminant` exactly, because the engine
/// will serialise them as `u16` over the channel.
///
/// Variants are `#[non_exhaustive]` at the enum level to allow forward-compat
/// with producers that send unknown discriminants.
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum LocalKindDiscriminant {
    /// A namespace, package, or module — a container for other entries.
    Module = 1,
    /// A product type: struct, class, record, or data class.
    Record = 2,
    /// A field or property of a containing type.
    Field = 3,
    /// A function, method, or lambda.
    Function = 4,
    /// A type alias (or abstract associated-type declaration).
    Alias = 5,
    /// A trait, interface, or protocol definition.
    Trait = 6,
    /// A trait implementation or inherent impl block.
    Impl = 7,
    /// A sum type: enum, tagged union, or sealed hierarchy.
    Enum = 8,
    /// A single variant of a sum type.
    Variant = 9,
    /// A compile-time constant declaration.
    Const = 10,
    /// A static variable declaration.
    Static = 11,
    /// A re-export (public alias) entry.
    Reexport = 12,
    /// A single parameter of a function.
    Param = 13,
}

impl LocalKindDiscriminant {
    /// Reconstruct from the wire `u16` value.  Returns `None` for unknown
    /// discriminants (forward-compat: callers should render an `Unknown` chip).
    pub fn from_u16(v: u16) -> Option<Self> {
        match v {
            1 => Some(Self::Module),
            2 => Some(Self::Record),
            3 => Some(Self::Field),
            4 => Some(Self::Function),
            5 => Some(Self::Alias),
            6 => Some(Self::Trait),
            7 => Some(Self::Impl),
            8 => Some(Self::Enum),
            9 => Some(Self::Variant),
            10 => Some(Self::Const),
            11 => Some(Self::Static),
            12 => Some(Self::Reexport),
            13 => Some(Self::Param),
            _ => None,
        }
    }

    /// The wire-stable `u16` discriminant value.
    pub fn as_u16(self) -> u16 {
        self as u16
    }

    /// A short ASCII identifier used in badge labels and accessibility text.
    pub fn short_label(self) -> &'static str {
        match self {
            Self::Module => "mod",
            Self::Record => "struct",
            Self::Field => "field",
            Self::Function => "fn",
            Self::Alias => "type",
            Self::Trait => "trait",
            Self::Impl => "impl",
            Self::Enum => "enum",
            Self::Variant => "variant",
            Self::Const => "const",
            Self::Static => "static",
            Self::Reexport => "reexport",
            Self::Param => "param",
        }
    }

    /// Select the matching colour field from a [`KindColours`] token set.
    ///
    /// This is the canonical mapping — the *only* place in the codebase that
    /// associates a kind with a colour.  Every badge, tree node, and chip that
    /// wants a kind colour calls this function; nothing duplicates the mapping.
    pub fn colour(self, palette: &KindColours) -> Hsla {
        match self {
            Self::Module => palette.module,
            Self::Record => palette.record,
            Self::Field => palette.field,
            Self::Function => palette.function,
            Self::Alias => palette.alias,
            Self::Trait => palette.trait_,
            Self::Impl => palette.impl_,
            Self::Enum => palette.enum_,
            Self::Variant => palette.variant,
            Self::Const => palette.const_,
            Self::Static => palette.static_,
            Self::Reexport => palette.reexport,
            Self::Param => palette.param,
        }
    }

    /// Iterate all 13 variants in wire-discriminant order.
    ///
    /// Used by tests to assert exhaustive coverage.
    pub fn all() -> [Self; 13] {
        [
            Self::Module,
            Self::Record,
            Self::Field,
            Self::Function,
            Self::Alias,
            Self::Trait,
            Self::Impl,
            Self::Enum,
            Self::Variant,
            Self::Const,
            Self::Static,
            Self::Reexport,
            Self::Param,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every wire value 1–13 must round-trip through `from_u16`.
    #[test]
    fn round_trip_u16() {
        for variant in LocalKindDiscriminant::all() {
            let v = variant.as_u16();
            assert_eq!(
                LocalKindDiscriminant::from_u16(v),
                Some(variant),
                "round-trip failed for discriminant {v}"
            );
        }
    }

    /// 0 and 14+ must return `None` (forward-compat unknown variants).
    #[test]
    fn unknown_discriminants_return_none() {
        assert!(LocalKindDiscriminant::from_u16(0).is_none());
        assert!(LocalKindDiscriminant::from_u16(14).is_none());
        assert!(LocalKindDiscriminant::from_u16(u16::MAX).is_none());
    }

    /// `all()` must produce exactly 13 elements.
    #[test]
    fn all_has_thirteen_variants() {
        assert_eq!(LocalKindDiscriminant::all().len(), 13);
    }
}
