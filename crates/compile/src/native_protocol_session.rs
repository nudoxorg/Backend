//! Exact native session identity matching.

use crate::{AuthorityIdentity, SessionKey};

/// Checks all decoded identity claims against the caller-owned session fence.
///
/// Keeping this comparison in one place prevents payload and request codecs
/// from growing subtly different authority checks.
pub(super) fn claims_match(
    session: &[u8; 32],
    manifest: &[u8; 32],
    authority: &[u8; 32],
    key: SessionKey,
    expected_authority: AuthorityIdentity,
) -> bool {
    *session == key.digest().to_bytes()
        && *manifest == key.manifest().to_bytes()
        && *authority == expected_authority.digest().to_bytes()
        && key.authority() == expected_authority.digest()
}
