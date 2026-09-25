use super::DeclarationOccurrenceKey;
use backend_engine::{DeclarationKind, Fragment, RowId, RowIdentityPreimage};
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

pub(super) fn semantic_coordinate(
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

pub(super) fn query_semantic_id(
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
