//! Shared product-package compiler target and authority namespace resolution.

use super::{BuiltinModelError, semantic_authority::SemanticAuthority};
use backend_engine::builtin::ProductSemanticPublicationKey;
use backend_engine::{PackageKey, PackageReference, package_key};
use backend_library::interface::PackageUrl;
use backend_semantic::vocabulary::{Language, LanguageProfile};
use unicode_normalization::UnicodeNormalization;

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

/// Derives a local unit-qualified coordinate. This is an identity claim only;
/// callers must reject pre-existing legacy use of the reserved qualifier before
/// creating authority observations, and must bind the exact target and capture.
pub(in crate::builtin) fn csharp_unit_coordinate(
    base: &PackageUrl,
    project_path: &str,
) -> Result<PackageUrl, BuiltinModelError> {
    const KEY: &str = "nudox_local_csharp_unit";
    if base.package_type().language() != Language::CSharp
        || project_path.is_empty()
        || project_path.len() > 4096
        || project_path.nfc().collect::<String>() != project_path
        || project_path.contains(['\\', ':'])
        || project_path.chars().any(char::is_control)
        || !project_path.to_ascii_lowercase().ends_with(".csproj")
        || project_path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(BuiltinModelError(
            "invalid local C# project unit path".to_owned(),
        ));
    }
    let (head, fragment) = base
        .as_str()
        .split_once('#')
        .map_or((base.as_str(), None), |(head, tail)| (head, Some(tail)));
    let (head, existing) = head
        .split_once('?')
        .map_or((head, None), |(head, tail)| (head, Some(tail)));
    let mut qualifiers =
        existing.map_or_else(Vec::new, |items| items.split('&').collect::<Vec<_>>());
    if qualifiers.iter().any(|item| {
        item.split_once('=')
            .is_some_and(|(key, _)| key == KEY || key.contains('%'))
    }) {
        return Err(BuiltinModelError(
            "reserved C# unit qualifier already exists".to_owned(),
        ));
    }
    let mut encoded = String::with_capacity(project_path.len() * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in project_path.bytes() {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 15)]));
    }
    let unit = format!("{KEY}={encoded}");
    qualifiers.push(&unit);
    qualifiers.sort_unstable();
    let mut text = format!("{head}?{}", qualifiers.join("&"));
    if let Some(fragment) = fragment {
        text.push('#');
        text.push_str(fragment);
    }
    PackageUrl::parse(text).map_err(|error| {
        BuiltinModelError(format!("construct local C# unit coordinate: {error:?}"))
    })
}

/// Checks an untrusted selected-unit coordinate spelling against the exact
/// project path. This comparison does not establish namespace authority.
pub(in crate::builtin) fn csharp_coordinate_claims_project(coordinate: &PackageUrl, project_path: &str) -> bool {
    const KEY: &str = "nudox_local_csharp_unit";
    if project_path.len() > 4096 { return false; }
    let mut encoded = String::with_capacity(project_path.len() * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in project_path.bytes() {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 15)]));
    }
    coordinate.qualifiers().is_some_and(|qualifiers| qualifiers.split('&').any(|part| part.split_once('=') == Some((KEY, encoded.as_str()))))
}

#[cfg(test)]
mod csharp_unit_coordinate_tests {
    use super::*;

    #[test]
    fn unit_coordinate_is_injective_and_rejects_legacy_marker_and_overflow() {
        let base = PackageUrl::parse("pkg:nuget/example@1.0.0?source=local#src").unwrap();
        let a = csharp_unit_coordinate(&base, "src/A.csproj").unwrap();
        let b = csharp_unit_coordinate(&base, "src/a.csproj").unwrap();
        assert_ne!(a, b);
        assert_ne!(a, base);
        assert!(csharp_coordinate_claims_project(&a, "src/A.csproj"));
        assert!(!csharp_coordinate_claims_project(&a, "src/a.csproj"));
        let package = PackageReference::parse("/fixture".to_owned()).unwrap();
        let profile =
            LanguageProfile::CSharp(backend_semantic::vocabulary::CSharpVersion::CSharp14);
        let namespace_a = SemanticAuthority::namespace(&package, &a, profile).unwrap();
        let namespace_b = SemanticAuthority::namespace(&package, &b, profile).unwrap();
        assert_ne!(namespace_a.namespace_id(), namespace_b.namespace_id());
        assert!(
            a.as_str()
                .contains("nudox_local_csharp_unit=7372632f412e637370726f6a&source=local#src")
        );
        let collision = PackageUrl::parse(
            "pkg:nuget/example@1.0.0?nudox_local_csharp_unit=7372632f412e637370726f6a",
        )
        .unwrap();
        assert!(csharp_unit_coordinate(&collision, "src/A.csproj").is_err());
        assert!(csharp_unit_coordinate(&base, "src/../A.csproj").is_err());
        let unicode = csharp_unit_coordinate(&base, "src/é.csproj").unwrap();
        assert_ne!(unicode, a);
        assert!(csharp_unit_coordinate(&base, "src/e\u{301}.csproj").is_err());
        assert!(
            PackageUrl::parse("pkg:nuget/example@1.0.0?nudox%5Flocal_csharp_unit=value").is_err()
        );
        // A percent-escaped qualifier key is refused either by the coordinate parser
        // or, when the parser admits it, by the unit-coordinate derivation.
        assert!(
            PackageUrl::parse("pkg:nuget/example@1.0.0?source%2Falias=value")
                .map_or(true, |escaped| csharp_unit_coordinate(&escaped, "src/A.csproj").is_err())
        );
        assert!(csharp_unit_coordinate(&base, &format!("{}.csproj", "a".repeat(1500))).is_err());
    }
}
