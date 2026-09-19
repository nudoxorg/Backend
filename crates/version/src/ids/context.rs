#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Metadata that must accompany an untrusted identity claim.
pub struct IdContext {
    class: u8,
    domain: u8,
    ty: u16,
    version: u8,
}

impl IdContext {
    /// Creates a context for an untrusted wire claim.
    ///
    /// The context itself does not grant trust.  The requested typed ID still
    /// compares it with the schema-specific context during admission.
    #[must_use]
    pub const fn new(class: u8, domain: u8, ty: u16, version: u8) -> Self {
        Self {
            class,
            domain,
            ty,
            version,
        }
    }

    /// Returns the identity class byte.
    #[must_use]
    pub const fn class(self) -> u8 {
        self.class
    }

    /// Returns the domain byte.
    #[must_use]
    pub const fn domain(self) -> u8 {
        self.domain
    }

    /// Returns the stable type tag.
    #[must_use]
    pub const fn ty(self) -> u16 {
        self.ty
    }

    /// Returns the canonical encoding version.
    #[must_use]
    pub const fn version(self) -> u8 {
        self.version
    }

    /// Returns the context expected for a stable schema object key.
    #[must_use]
    pub const fn object_key<T: Schema>() -> Self {
        Self::new(CLASS_OBJECT_KEY, T::DOMAIN, T::TYPE, T::VERSION)
    }

    /// Returns the context expected for a schema object version.
    #[must_use]
    pub const fn schema<T: Schema>() -> Self {
        Self::new(CLASS_OBJECT_VERSION, T::DOMAIN, T::TYPE, T::VERSION)
    }

    /// Returns the context expected for a relation state root.
    #[must_use]
    pub const fn relation<R: Relation>() -> Self {
        Self::new(CLASS_STATE_ROOT, R::DOMAIN, R::TYPE, R::VERSION)
    }

    /// Returns the context expected for a relation delta.
    #[must_use]
    pub const fn delta<R: Relation>() -> Self {
        Self::new(CLASS_DELTA, R::DOMAIN, R::TYPE, R::VERSION)
    }
}
use super::schema::{Relation, Schema};
use super::{CLASS_DELTA, CLASS_OBJECT_KEY, CLASS_OBJECT_VERSION, CLASS_STATE_ROOT};
