use super::DeclarationOccurrenceKey;
use backend_engine::{DeclarationKind, Fragment, RowId};
use backend_semantic::ir::{DeclarationIdentity, ExternalTargetIdentity, ItemKind};
use std::collections::BTreeMap;

pub(super) fn declaration_symbol(
    coordinate: &str,
    kind: DeclarationKind,
    signature: &str,
    duplicate_coordinate: bool,
    occurrences: &mut BTreeMap<DeclarationOccurrenceKey, u32>,
) -> (RowId, Option<String>) {
    let canonical = RowId::Symbol(backend_engine::symbol_key(coordinate));
    if !duplicate_coordinate {
        return (canonical, None);
    }
    let occurrence = occurrences
        .entry((
            coordinate.to_owned(),
            kind.name().to_owned(),
            signature.to_owned(),
        ))
        .or_default();
    let preimage = {
        let preimage = format!("{coordinate}\0{}\0{signature}\0{occurrence}", kind.name());
        *occurrence = occurrence.saturating_add(1);
        preimage
    };
    (
        RowId::Symbol(backend_engine::symbol_key(&preimage)),
        Some(preimage),
    )
}
pub(super) fn declaration_coordinate(label: &str, path: &str, line: u32, name: &str) -> String {
    format!("{label}::{path}:{line}::{name}")
}
pub(super) fn declaration_family(kind: DeclarationKind) -> u8 {
    match kind {
        DeclarationKind::Function | DeclarationKind::Method | DeclarationKind::Constructor => 0,
        DeclarationKind::Class
        | DeclarationKind::Interface
        | DeclarationKind::Type
        | DeclarationKind::Enum
        | DeclarationKind::Struct
        | DeclarationKind::Trait
        | DeclarationKind::Union => 1,
        DeclarationKind::Constant
        | DeclarationKind::Field
        | DeclarationKind::Property
        | DeclarationKind::Variable
        | DeclarationKind::Variant => 2,
        DeclarationKind::Module => 3,
        DeclarationKind::Import => 4,
        DeclarationKind::Macro => 5,
        _ => 6,
    }
}
pub(crate) fn external_semantic_symbol(
    package: backend_engine::PackageKey,
    image: [u8; 32],
    target: ExternalTargetIdentity,
) -> backend_engine::SymbolKey {
    let scoped = target.in_scope(package.to_bytes(), image);
    backend_engine::symbol_key(&backend_engine::encode_id(scoped.as_bytes()))
}

/// A copied external operand retains its scoped producer key and captured
/// display spelling. Equal or unavailable display names remain distinct.
pub(crate) fn external_semantic_coordinate(
    project: &str,
    symbol: backend_engine::SymbolKey,
    display: &str,
) -> String {
    format!(
        "{project}::external::{}::{display}",
        RowId::Symbol(symbol).stable_key()
    )
}
pub(super) fn declaration_kind(kind: ItemKind) -> DeclarationKind {
    match kind {
        ItemKind::Function => DeclarationKind::Function,
        ItemKind::Constant | ItemKind::Variant => DeclarationKind::Constant,
        ItemKind::Record => DeclarationKind::Struct,
        ItemKind::Module | ItemKind::Namespace => DeclarationKind::Module,
        ItemKind::Field => DeclarationKind::Field,
        ItemKind::Alias | ItemKind::Implementation => DeclarationKind::Type,
        ItemKind::Trait => DeclarationKind::Trait,
        ItemKind::Enum => DeclarationKind::Enum,
        ItemKind::Static | ItemKind::Parameter => DeclarationKind::Variable,
        ItemKind::Reexport => DeclarationKind::Import,
        ItemKind::Macro => DeclarationKind::Macro,
    }
}

pub(crate) fn semantic_symbol(
    package: backend_engine::PackageKey,
    identity: DeclarationIdentity,
) -> backend_engine::SymbolKey {
    backend_engine::symbol_key(&semantic_identity(package, identity))
}

pub(crate) fn semantic_coordinate(
    project: &str,
    identity: DeclarationIdentity,
    name: &str,
) -> String {
    let mut encoded = String::with_capacity(project.len() + name.len() + 78);
    encoded.push_str(project);
    encoded.push_str("::semantic::");
    push_declaration_identity(&mut encoded, identity);
    encoded.push_str("::");
    encoded.push_str(name);
    encoded
}

/// Friendly source coordinates are navigation aliases, not entity identity.
/// Keep them only when singular in this image; anonymous callables and source
/// collisions use the existing exact semantic operand. No entity is merged.
pub(super) fn semantic_coordinates<'a>(
    project: &str,
    candidates: impl IntoIterator<Item = Result<(
        DeclarationIdentity, backend_semantic::ir::ItemNameView<'a>,
        Option<backend_compile::SourceLocation>,
    ), super::super::BuiltinModelError>>,
    cancelled: Option<&std::sync::atomic::AtomicBool>,
) -> Result<BTreeMap<DeclarationIdentity, String>, super::super::BuiltinModelError> {
    let mut coordinates = BTreeMap::new();
    let mut source_aliases = BTreeMap::<String, Vec<DeclarationIdentity>>::new();
    super::super::check_search_preparation(cancelled, "semantic-coordinate-start")?;
    for candidate in candidates {
        super::super::check_search_preparation(cancelled, "semantic-coordinate")?;
        let (identity, name, location) = candidate?;
        let display = super::semantic_display_name(name)?;
        let coordinate = match location.filter(|_| name.named_bytes().is_some()) {
            Some(location) => {
                let coordinate = declaration_coordinate(project, location.path(), location.start_line(), display);
                source_aliases.entry(coordinate.clone()).or_default().push(identity);
                coordinate
            }
            None => semantic_coordinate(project, identity, display),
        };
        if coordinates.insert(identity, (coordinate, display)).is_some() {
            return Err(super::super::BuiltinModelError("semantic coordinates contain a duplicate declaration identity".to_owned()));
        }
    }
    for owners in source_aliases.into_values().filter(|owners| owners.len() > 1) {
        for identity in owners {
            super::super::check_search_preparation(cancelled, "semantic-coordinate-collision")?;
            if let Some((coordinate, display)) = coordinates.get_mut(&identity) {
                *coordinate = semantic_coordinate(project, identity, display);
            }
        }
    }
    Ok(coordinates.into_iter().map(|(identity, (coordinate, _))| (identity, coordinate)).collect())
}

pub(super) fn semantic_identity(
    package: backend_engine::PackageKey,
    identity: DeclarationIdentity,
) -> String {
    let mut encoded = package_token(package);
    encoded.push_str("::");
    push_declaration_identity(&mut encoded, identity);
    encoded
}

pub(super) fn push_declaration_identity(output: &mut String, identity: DeclarationIdentity) {
    push_hex(output, identity.family.as_bytes());
    push_hex(output, identity.variant.as_bytes());
}

pub(super) fn push_hex(output: &mut String, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
}

pub(crate) fn package_token(package: backend_engine::PackageKey) -> String {
    backend_engine::encode_id(package.as_bytes())
}
pub(super) fn query_package_id(package: backend_engine::PackageKey) -> String {
    RowId::Package(package).stable_key()
}

pub(crate) fn query_semantic_id(
    package: backend_engine::PackageKey,
    identity: DeclarationIdentity,
) -> String {
    RowId::Symbol(semantic_symbol(package, identity)).stable_key()
}

pub(super) fn query_external_id(
    package: backend_engine::PackageKey,
    image: [u8; 32],
    target: ExternalTargetIdentity,
) -> String {
    RowId::Symbol(external_semantic_symbol(package, image, target)).stable_key()
}

pub(super) fn fragment_text(fragments: &[Fragment]) -> String {
    let mut output = String::new();
    for fragment in fragments {
        match fragment {
            Fragment::Text(value) | Fragment::Code(value) => output.push_str(value),
            Fragment::Link { label, .. } => output.push_str(label),
            Fragment::Break => output.push('\n'),
        }
    }
    output
}

#[cfg(test)]
mod coordinate_tests {
    use super::*;
    use backend_semantic::ir::{
        AnonymousCallableAnchor, AnonymousCallableAnchorView, AnonymousCallableFamilyMultiplicity,
        CallableAnchorStep, CallableChildRole, CallableParentShape, DeclarationFamilyId,
        ItemNameView, VariantFingerprint,
    };

    #[test]
    fn exact_semantic_operands_keep_same_line_anonymous_and_type_literal_members_distinct() {
        let identity = |variant| DeclarationIdentity {
            family: DeclarationFamilyId::from_raw([9; 16]),
            variant: VariantFingerprint::from_raw([variant; 16]),
        };
        let route = [CallableAnchorStep {
            child_role: CallableChildRole::TypeAliasValue,
            parent: CallableParentShape::TypeAliasName(b"Handler"),
        }];
        let mut bytes = [0; 128];
        let length = AnonymousCallableAnchor { steps: &route }.write_storage(
            AnonymousCallableFamilyMultiplicity::Ambiguous { instance_count: 2 }, &mut bytes,
        ).expect("typed anchor");
        let anchor = AnonymousCallableAnchorView::try_from_encoded(&bytes[..length]).expect("admitted anonymous anchor");
        let anonymous = ItemNameView::AnonymousCallable { anchor };
        let same_line = backend_compile::SourceLocation::new("src/page.tsx", 153).expect("site");
        let later_line = backend_compile::SourceLocation::new("src/page.tsx", 352).expect("later site");
        let other_file = backend_compile::SourceLocation::new("src/other.ts", 153).expect("other file");
        let candidates = vec![
            (identity(1), anonymous, Some(same_line.clone())),
            (identity(2), anonymous, Some(same_line.clone())),
            (identity(3), ItemNameView::Named(b"teacher_id"), Some(same_line.clone())),
            (identity(4), ItemNameView::Named(b"teacher_id"), Some(same_line)),
            (identity(5), ItemNameView::Named(b"teacher_id"), Some(later_line)),
            (identity(6), ItemNameView::Named(b"teacher_id"), Some(other_file)),
            (identity(7), ItemNameView::Named(b"unlocated"), None),
        ];
        let actual = semantic_coordinates("/app", candidates.iter().cloned().map(Ok), None).expect("coordinates");
        assert_eq!(actual.len(), candidates.len(), "every exact entity survives");
        assert_eq!(actual.values().collect::<std::collections::BTreeSet<_>>().len(), candidates.len());
        for variant in 1..=4 {
            let name = if variant <= 2 { "<anonymous callable>" } else { "teacher_id" };
            assert_eq!(actual[&identity(variant)], semantic_coordinate("/app", identity(variant), name));
        }
        assert_eq!(actual[&identity(5)], "/app::src/page.tsx:352::teacher_id");
        assert_eq!(actual[&identity(6)], "/app::src/other.ts:153::teacher_id");
        assert_eq!(actual[&identity(7)], semantic_coordinate("/app", identity(7), "unlocated"));
        assert_eq!(actual, semantic_coordinates("/app", candidates.iter().rev().cloned().map(Ok), None).expect("source order independent"));
        let duplicate = [candidates[0].clone(), candidates[0].clone()];
        assert!(semantic_coordinates("/app", duplicate.into_iter().map(Ok), None).is_err(), "duplicate identity is refused, not hidden");
        let cancelled = std::sync::atomic::AtomicBool::new(true);
        assert!(semantic_coordinates("/app", candidates.into_iter().map(Ok), Some(&cancelled)).is_err());
    }
}
