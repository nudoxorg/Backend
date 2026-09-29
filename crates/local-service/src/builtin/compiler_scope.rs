//! Shared product-package compiler target and authority namespace resolution.

use super::{BuiltinModelError, semantic_authority::SemanticAuthority};
use backend_engine::builtin::ProductSemanticPublicationKey;
use backend_engine::{PackageKey, PackageReference, package_key};
use backend_library::interface::PackageUrl;
use backend_semantic::vocabulary::{Language, LanguageProfile};

/// Whether a product compiler target uses an explicit pinned coordinate or local identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProductCompilerTargetKind {
    /// An explicit pinned package coordinate supplies the compiler target.
    PinnedRegistry,
    /// The production local-package coordinate resolver supplies the compiler target.
    Local,
}

/// Exact product target and authority namespace used for one package/profile compilation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductCompilerScope {
    coordinate: PackageUrl,
    namespace_id: [u8; 16],
    target_kind: ProductCompilerTargetKind,
}

impl ProductCompilerScope {
    /// Exact compiler package coordinate selected by locald's product index path.
    #[must_use]
    pub const fn coordinate(&self) -> &PackageUrl {
        &self.coordinate
    }

    /// Exact Turso authority namespace selected by locald for this package/profile.
    #[must_use]
    pub const fn namespace_id(&self) -> [u8; 16] {
        self.namespace_id
    }

    /// Whether the compiler target is pinned or generated for a local product package.
    #[must_use]
    pub const fn target_kind(&self) -> ProductCompilerTargetKind {
        self.target_kind
    }
}

/// Resolves the exact target and namespace shared by product indexing and cluster admission.
///
/// The optional coordinate follows locald's existing rule: a supplied coordinate is used when
/// its package ecosystem serves the requested profile; otherwise locald derives its canonical
/// local coordinate from the product package key.
pub fn product_compiler_scope(
    package: PackageReference,
    profile: LanguageProfile,
    supplied_coordinate: Option<&PackageUrl>,
) -> Result<ProductCompilerScope, BuiltinModelError> {
    let coordinate =
        semantic_coordinate(package_key(package.as_str()), profile, supplied_coordinate)?;
    let key = ProductSemanticPublicationKey::new(package, coordinate.clone(), profile)
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    let namespace = SemanticAuthority::namespace(key.package(), key.coordinate(), key.profile())?;
    let target_kind = if supplied_coordinate.is_some_and(|supplied| supplied == &coordinate) {
        ProductCompilerTargetKind::PinnedRegistry
    } else {
        ProductCompilerTargetKind::Local
    };
    Ok(ProductCompilerScope {
        coordinate,
        namespace_id: namespace.namespace_id(),
        target_kind,
    })
}

/// Resolves the canonical compiler coordinate used by locald for one product package profile.
pub(in crate::builtin) fn semantic_coordinate(
    package: PackageKey,
    profile: LanguageProfile,
    supplied: Option<&PackageUrl>,
) -> Result<PackageUrl, BuiltinModelError> {
    if let Some(supplied) = supplied
        && supplied.package_type().language() == profile.language()
    {
        return Ok(supplied.clone());
    }
    let package_type = match profile.language() {
        Language::Rust => "cargo",
        Language::TypeScript => "npm",
        Language::Python => "pypi",
        Language::Go => "golang",
        Language::Java => "maven",
        Language::CSharp => "nuget",
        Language::Clang => "generic",
    };
    let name = backend_engine::encode_id(package.as_bytes());
    PackageUrl::parse(format!("pkg:{package_type}/local-{name}@0.0.0-local"))
        .map_err(|error| BuiltinModelError(format!("construct local package identity: {error:?}")))
}
