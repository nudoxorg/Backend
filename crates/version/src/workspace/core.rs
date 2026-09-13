/// Full canonical schema identity used in workspace bindings.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SchemaIdentity {
    domain: u8,
    ty: u16,
    version: u8,
}
impl SchemaIdentity {
    /// Returns the complete canonical identity of a relation marker.
    #[must_use]
    pub const fn of_relation<R: Relation>() -> Self {
        Self::new(R::DOMAIN, R::TYPE, R::VERSION)
    }

    /// Creates a schema identity from domain, type, and encoding version.
    #[must_use]
    pub const fn new(domain: u8, ty: u16, version: u8) -> Self {
        Self {
            domain,
            ty,
            version,
        }
    }

    /// Returns the domain byte.
    #[must_use]
    pub const fn domain(self) -> u8 {
        self.domain
    }

    /// Returns the type tag.
    #[must_use]
    pub const fn ty(self) -> u16 {
        self.ty
    }

    /// Returns the canonical encoding version.
    #[must_use]
    pub const fn version(self) -> u8 {
        self.version
    }
}
