//! Constructors for accepted logical product values.

use super::schema::{
    ActorKey, BranchKey, IntentId, LogKey, PackageKey, SemanticObject, SymbolKey, ViewRecipeId,
    ViewVersion,
};

/// Constructs a package key from its canonical logical coordinate.
#[must_use]
pub fn package_key(coordinate: &str) -> PackageKey {
    PackageKey::from_value(coordinate)
}

/// Constructs a declaration key from its canonical logical address.
#[must_use]
pub fn symbol_key(address: &str) -> SymbolKey {
    SymbolKey::from_value(address)
}

/// Constructs an actor key from its canonical logical name.
#[must_use]
pub fn actor_key(name: &str) -> ActorKey {
    ActorKey::from_value(name)
}

/// Constructs a branch key from its canonical logical name.
#[must_use]
pub fn branch_key(name: &str) -> BranchKey {
    BranchKey::from_value(name)
}

/// Constructs a log key from its canonical logical name.
#[must_use]
pub fn log_key(name: &str) -> LogKey {
    LogKey::from_value(name)
}

/// Constructs an immutable object version from canonical bytes.
#[must_use]
pub fn object_version(bytes: &[u8]) -> SemanticObject {
    SemanticObject::from_value(bytes)
}

/// Constructs a stable view recipe key from its canonical recipe bytes.
#[must_use]
pub fn view_key(bytes: &[u8]) -> ViewRecipeId {
    ViewRecipeId::from_value(bytes)
}

/// Constructs an immutable view version from its complete canonical bytes.
#[must_use]
pub fn view_version(bytes: &[u8]) -> ViewVersion {
    ViewVersion::from_value(bytes)
}

/// Constructs a durable intent identity from an operation token and payload.
#[must_use]
pub fn intent_id(token: &str, payload: &[u8]) -> IntentId {
    let bytes = intent_preimage(token, payload);
    IntentId::from_value(&bytes)
}

pub(super) fn intent_preimage(token: &str, payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(token.len() as u64).to_be_bytes());
    bytes.extend_from_slice(token.as_bytes());
    bytes.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    bytes.extend_from_slice(payload);
    bytes
}
