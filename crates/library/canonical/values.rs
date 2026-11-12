//! Constructors for accepted logical product values.

use super::schema::{
    ActorKey, BranchKey, IntentId, LogKey, PackageKey, SemanticObject, SymbolKey, SymbolSchema,
    ViewRecipeId, ViewVersion,
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

/// Streams the exact borrowed UTF-8 logical address through the symbol schema.
///
/// This derives the same key as [`symbol_key`] without allocating a canonical
/// preimage copy. It is a logical identity constructor; it does not admit a
/// source claim or confer declaration membership.
///
/// # Errors
/// Returns a canonical framing error if the borrowed length cannot be encoded.
pub fn symbol_key_borrowed(
    address: &str,
) -> Result<SymbolKey, backend_version::RuntimeIdentityError> {
    use backend_version::{ObjectKeyHasher, Schema, SchemaIdentity};
    let schema = SchemaIdentity::new(
        SymbolSchema::DOMAIN,
        SymbolSchema::TYPE,
        SymbolSchema::VERSION,
    );
    let mut hasher = ObjectKeyHasher::new(schema, address.len())?;
    hasher.update(address.as_bytes())?;
    hasher.finish_key::<SymbolSchema>()
}

#[cfg(test)]
mod borrowed_symbol_key_tests {
    use super::*;

    #[test]
    fn borrowed_symbol_keys_match_canonical_encoding_without_allocating() {
        let long = "path/α.sql::column".repeat(512);
        for address in [
            "",
            "symbol",
            "pkg::α.sql::books\0sql:4294967295",
            long.as_str(),
        ] {
            let expected = symbol_key(address);
            let mut actual = None;
            let measured = allocation_counter::measure(|| {
                actual = Some(symbol_key_borrowed(address).expect("borrowed canonical key"));
            });
            assert_eq!(actual, Some(expected));
            assert_eq!(measured.count_total, 0);
        }
    }
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
