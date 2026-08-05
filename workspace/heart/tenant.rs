//! Tenant ownership vocabulary — who owns a package and how widely it is
//! visible.
//!
//! Both discriminants are stored as their lowercase SQL token (matching the
//! postgres `CHECK` domains on `packages.owner_kind` / `packages.visibility`).
//! `#[strum(serialize_all = "lowercase")]` makes `Display`/`FromStr`/`VARIANTS`
//! all agree on that token from one source, so the schema CHECK constraint and
//! the codec can never drift.

/// The kind of tenant that owns a package — stored as the lowercase SQL token.
///
/// The wire token for each variant is its lowercase name (`"individual"`,
/// `"enterprise"`), matching the postgres `CHECK` domain. `VariantNames::VARIANTS`
/// is used to derive that domain from one source.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    strum::Display,
    strum::EnumString,
    strum::IntoStaticStr,
    strum::VariantNames,
)]
#[strum(serialize_all = "lowercase")]
pub enum OwnerKind {
    /// A single-developer account.
    Individual,
    /// An organisation or team account.
    Enterprise,
}

/// The visibility of a package — stored as the lowercase SQL token.
///
/// The wire token for each variant is its lowercase name (`"personal"`,
/// `"private"`, `"public"`), matching the postgres `CHECK` domain.
/// `VariantNames::VARIANTS` is used to derive that domain from one source.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    strum::Display,
    strum::EnumString,
    strum::IntoStaticStr,
    strum::VariantNames,
)]
#[strum(serialize_all = "lowercase")]
pub enum Visibility {
    /// Visible only to the owning tenant.
    Personal,
    /// Visible to explicitly invited members of the owning tenant.
    Private,
    /// Publicly discoverable.
    Public,
}
