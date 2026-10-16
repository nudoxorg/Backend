//! Stateless owner statements over canonical typed operands. Transport code
//! cannot substitute a session dictionary for the configured durable owner.
use serde::Serialize;

/// A concrete typed statement owns its versioned domain. User operands never
/// select this domain, and parsed statements grant no verification capability.
pub trait NativeOwnerStatement: Serialize {
    /// Domain-separated statement schema, including its version and terminator.
    const DOMAIN: &'static [u8];
}

struct StatementWriter {
    hasher: blake3::Hasher,
    remaining: usize,
}
impl std::io::Write for StatementWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.remaining = self.remaining.checked_sub(bytes.len()).ok_or_else(|| {
            std::io::Error::other("native owner statement exceeds its shared byte bound")
        })?;
        self.hasher.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Seals one canonical typed statement under the existing configured product
/// credential. Streaming serialization applies the existing cursor/header cap
/// before hashing each write; it allocates no full statement copy.
pub fn seal_native_owner_statement<T: NativeOwnerStatement>(
    secret: &[u8; 32],
    statement: &T,
) -> Result<[u8; 32], crate::ProductAdmissionError> {
    let remaining = crate::MAX_INDEX_SEARCH_CURSOR_BYTES
        .checked_sub(T::DOMAIN.len())
        .ok_or(crate::ProductAdmissionError::SemanticVersionShape)?;
    let mut writer = StatementWriter {
        hasher: blake3::Hasher::new_keyed(secret),
        remaining,
    };
    writer.hasher.update(T::DOMAIN);
    serde_json::to_writer(&mut writer, statement)
        .map_err(|_| crate::ProductAdmissionError::SemanticVersionShape)?;
    Ok(*writer.hasher.finalize().as_bytes())
}

/// Verifies the complete statement with BLAKE3's constant-time hash equality.
/// The caller constructs its own private-ctor capability only after this gate.
pub fn verify_native_owner_statement<T: NativeOwnerStatement>(
    secret: &[u8; 32],
    statement: &T,
    seal: &[u8; 32],
) -> Result<(), crate::ProductAdmissionError> {
    if blake3::Hash::from(*seal)
        != blake3::Hash::from(seal_native_owner_statement(secret, statement)?)
    {
        return Err(crate::ProductAdmissionError::SemanticVersionShape);
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    #[derive(Serialize)]
    struct Source<'a>(&'a str);
    impl NativeOwnerStatement for Source<'_> {
        const DOMAIN: &'static [u8] = b"backend.test.source-owner.v1\0";
    }
    #[derive(Serialize)]
    struct Frontier<'a>(&'a str);
    impl NativeOwnerStatement for Frontier<'_> {
        const DOMAIN: &'static [u8] = b"backend.test.frontier-owner.v1\0";
    }
    #[test]
    fn typed_domains_and_complete_fields_are_bound_under_one_existing_owner() {
        let secret = [7; 32];
        let source = Source("full exact captured selector");
        let seal = seal_native_owner_statement(&secret, &source).expect("seal");
        verify_native_owner_statement(&secret, &source, &seal).expect("same owner statement");
        assert!(verify_native_owner_statement(&[8; 32], &source, &seal).is_err());
        assert!(
            verify_native_owner_statement(&secret, &Source("different frontier"), &seal).is_err()
        );
        assert!(verify_native_owner_statement(&secret, &Frontier(source.0), &seal).is_err());
        let too_large = "x".repeat(crate::MAX_INDEX_SEARCH_CURSOR_BYTES);
        assert!(seal_native_owner_statement(&secret, &Source(&too_large)).is_err());
    }
}
