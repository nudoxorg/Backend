//! Canonical coordinator-issued direct-cluster enrollment invitations.
//!
//! The invitation carries only endpoint and compiler-scope claims. It never contains worker
//! secrets, host paths, or local compiler authority fingerprints; the worker pins those local
//! facts independently when the owner imports the invitation.

use crate::EndpointId;
use std::net::{IpAddr, SocketAddr};
use thiserror::Error;

const INVITE_MAGIC: &[u8; 8] = b"BKCWIVT1";
const MAX_ADDRESS_TEXT: usize = 128;
const INVITE_FINGERPRINT_DOMAIN: &[u8] = b"backend.worker.cluster-invite.v2\0";

/// Host-execution class explicitly granted by a local-cluster coordinator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ClusterExecutionClass {
    /// Claimed pure parser execution. Receivers must independently prove this class is supported.
    PureInProcessParser = 1,
    /// Coordinator-controlled tools may execute with the worker host's privileges.
    TrustedCoordinatorHostExecution = 2,
}

impl TryFrom<u8> for ClusterExecutionClass {
    type Error = ClusterInviteError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::PureInProcessParser),
            2 => Ok(Self::TrustedCoordinatorHostExecution),
            _ => Err(ClusterInviteError::InvalidClaim),
        }
    }
}

/// One short-lived, out-of-band invitation to trust an exact direct coordinator and compile scope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScopedClusterInvite {
    coordinator: EndpointId,
    address: SocketAddr,
    namespace_id: [u8; 16],
    recipe: [u8; 32],
    profile: [u8; 2],
    stage: u8,
    toolchain: [u8; 32],
    environment: [u8; 32],
    target_platform: [u8; 32],
    expires_unix_ms: u64,
    fingerprint: [u8; 32],
    execution_class: ClusterExecutionClass,
}

/// Rejection while constructing or decoding one direct-cluster invitation.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ClusterInviteError {
    /// A claim is zero, out of bounds, non-direct, or uses a noncanonical encoding.
    #[error("cluster invitation contains an invalid or noncanonical claim")]
    InvalidClaim,
    /// The invitation's displayed fingerprint does not match its exact claims.
    #[error("cluster invitation fingerprint does not match its claims")]
    FingerprintMismatch,
}

impl ScopedClusterInvite {
    /// Creates a canonical invitation and derives its human-comparable fingerprint.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        coordinator: EndpointId,
        address: SocketAddr,
        namespace_id: [u8; 16],
        recipe: [u8; 32],
        profile: [u8; 2],
        stage: u8,
        toolchain: [u8; 32],
        environment: [u8; 32],
        target_platform: [u8; 32],
        expires_unix_ms: u64,
        execution_class: ClusterExecutionClass,
    ) -> Result<Self, ClusterInviteError> {
        let mut invite = Self {
            coordinator,
            address,
            namespace_id,
            recipe,
            profile,
            stage,
            toolchain,
            environment,
            target_platform,
            expires_unix_ms,
            fingerprint: [0; 32],
            execution_class,
        };
        invite.validate_claims()?;
        invite.fingerprint = invite.calculate_fingerprint();
        Ok(invite)
    }

    /// Parses a lowercase-hex token and verifies its canonical claims and fingerprint.
    pub fn decode_token(token: &str) -> Result<Self, ClusterInviteError> {
        if token.len() % 2 != 0
            || !token
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(ClusterInviteError::InvalidClaim);
        }
        let bytes = decode_hex(token)?;
        if bytes.len() > 1024 {
            return Err(ClusterInviteError::InvalidClaim);
        }
        let mut reader = Reader::new(&bytes);
        if reader.take(INVITE_MAGIC.len())? != INVITE_MAGIC {
            return Err(ClusterInviteError::InvalidClaim);
        }
        let coordinator_bytes = reader.array::<32>()?;
        let coordinator = EndpointId::from_bytes(&coordinator_bytes)
            .map_err(|_| ClusterInviteError::InvalidClaim)?;
        let address_text = reader.string(MAX_ADDRESS_TEXT)?;
        let address = address_text
            .parse::<SocketAddr>()
            .map_err(|_| ClusterInviteError::InvalidClaim)?;
        if address.to_string() != address_text {
            return Err(ClusterInviteError::InvalidClaim);
        }
        let invite = Self {
            coordinator,
            address,
            namespace_id: reader.array()?,
            recipe: reader.array()?,
            profile: reader.array()?,
            stage: reader.byte()?,
            toolchain: reader.array()?,
            environment: reader.array()?,
            target_platform: reader.array()?,
            expires_unix_ms: reader.u64()?,
            execution_class: ClusterExecutionClass::try_from(reader.byte()?)?,
            fingerprint: reader.array()?,
        };
        reader.finish()?;
        invite.validate_claims()?;
        if invite.calculate_fingerprint() != invite.fingerprint {
            return Err(ClusterInviteError::FingerprintMismatch);
        }
        if invite.encode_token()?.as_str() != token {
            return Err(ClusterInviteError::InvalidClaim);
        }
        Ok(invite)
    }

    /// Returns the canonical lowercase-hex representation suitable for explicit import.
    pub fn encode_token(&self) -> Result<String, ClusterInviteError> {
        self.validate_claims()?;
        if self.calculate_fingerprint() != self.fingerprint {
            return Err(ClusterInviteError::FingerprintMismatch);
        }
        let address = self.address.to_string();
        if address.len() > MAX_ADDRESS_TEXT {
            return Err(ClusterInviteError::InvalidClaim);
        }
        let mut bytes = Vec::with_capacity(512);
        bytes.extend_from_slice(INVITE_MAGIC);
        bytes.extend_from_slice(self.coordinator.as_bytes());
        put_string(&mut bytes, &address)?;
        bytes.extend_from_slice(&self.namespace_id);
        bytes.extend_from_slice(&self.recipe);
        bytes.extend_from_slice(&self.profile);
        bytes.push(self.stage);
        bytes.extend_from_slice(&self.toolchain);
        bytes.extend_from_slice(&self.environment);
        bytes.extend_from_slice(&self.target_platform);
        bytes.extend_from_slice(&self.expires_unix_ms.to_be_bytes());
        bytes.push(self.execution_class as u8);
        bytes.extend_from_slice(&self.fingerprint);
        Ok(encode_hex(&bytes))
    }

    /// Returns the endpoint identity of the coordinator issuing this invite.
    #[must_use]
    pub const fn coordinator(&self) -> EndpointId {
        self.coordinator
    }

    /// Returns the explicit direct IP address of the coordinator.
    #[must_use]
    pub const fn address(&self) -> SocketAddr {
        self.address
    }

    /// Returns the exact authority namespace.
    #[must_use]
    pub const fn namespace_id(&self) -> [u8; 16] {
        self.namespace_id
    }

    /// Returns the exact compiler invocation recipe digest.
    #[must_use]
    pub const fn recipe(&self) -> [u8; 32] {
        self.recipe
    }

    /// Returns the exact two-byte compiler profile discriminator.
    #[must_use]
    pub const fn profile(&self) -> [u8; 2] {
        self.profile
    }

    /// Returns the exact compiler stage discriminator.
    #[must_use]
    pub const fn stage(&self) -> u8 {
        self.stage
    }

    /// Returns the exact toolchain identity.
    #[must_use]
    pub const fn toolchain(&self) -> [u8; 32] {
        self.toolchain
    }

    /// Returns the exact compiler environment identity.
    #[must_use]
    pub const fn environment(&self) -> [u8; 32] {
        self.environment
    }

    /// Returns the exact target platform/sysroot identity.
    #[must_use]
    pub const fn target_platform(&self) -> [u8; 32] {
        self.target_platform
    }

    /// Returns the invitation expiry in Unix milliseconds.
    #[must_use]
    pub const fn expires_unix_ms(&self) -> u64 {
        self.expires_unix_ms
    }

    /// Returns the fingerprint that an operator should compare out of band.
    #[must_use]
    pub const fn fingerprint(&self) -> [u8; 32] {
        self.fingerprint
    }

    /// Returns the invitation fingerprint as lowercase hexadecimal for out-of-band comparison.
    #[must_use]
    pub fn fingerprint_hex(&self) -> String {
        encode_hex(&self.fingerprint)
    }

    /// Returns the explicit execution class.
    #[must_use]
    pub const fn execution_class(&self) -> ClusterExecutionClass {
        self.execution_class
    }

    fn validate_claims(&self) -> Result<(), ClusterInviteError> {
        let ip: IpAddr = self.address.ip();
        if self.coordinator.as_bytes() == &[0; 32]
            || self.address.port() == 0
            || ip.is_unspecified()
            || ip.is_multicast()
            || self.namespace_id == [0; 16]
            || self.recipe == [0; 32]
            || self.toolchain == [0; 32]
            || self.environment == [0; 32]
            || self.target_platform == [0; 32]
            || self.profile == [0; 2]
            || self.stage > 1
            || self.expires_unix_ms == 0
        {
            return Err(ClusterInviteError::InvalidClaim);
        }
        Ok(())
    }

    fn calculate_fingerprint(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(INVITE_FINGERPRINT_DOMAIN);
        hasher.update(self.coordinator.as_bytes());
        let address = self.address.to_string();
        let address_length = u64::try_from(address.len()).unwrap_or(u64::MAX);
        hasher.update(&address_length.to_be_bytes());
        hasher.update(address.as_bytes());
        hasher.update(&self.namespace_id);
        hasher.update(&self.recipe);
        hasher.update(&self.profile);
        hasher.update(&[self.stage]);
        hasher.update(&self.toolchain);
        hasher.update(&self.environment);
        hasher.update(&self.target_platform);
        hasher.update(&self.expires_unix_ms.to_be_bytes());
        hasher.update(&[self.execution_class as u8]);
        *hasher.finalize().as_bytes()
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], ClusterInviteError> {
        let end = self
            .cursor
            .checked_add(length)
            .filter(|end| *end <= self.bytes.len())
            .ok_or(ClusterInviteError::InvalidClaim)?;
        let value = self
            .bytes
            .get(self.cursor..end)
            .ok_or(ClusterInviteError::InvalidClaim)?;
        self.cursor = end;
        Ok(value)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], ClusterInviteError> {
        self.take(N)?
            .try_into()
            .map_err(|_| ClusterInviteError::InvalidClaim)
    }

    fn byte(&mut self) -> Result<u8, ClusterInviteError> {
        Ok(self.array::<1>()?[0])
    }

    fn u64(&mut self) -> Result<u64, ClusterInviteError> {
        Ok(u64::from_be_bytes(self.array()?))
    }

    fn string(&mut self, max: usize) -> Result<&'a str, ClusterInviteError> {
        let length = usize::from(u16::from_be_bytes(self.array()?));
        if length > max {
            return Err(ClusterInviteError::InvalidClaim);
        }
        std::str::from_utf8(self.take(length)?).map_err(|_| ClusterInviteError::InvalidClaim)
    }

    fn finish(self) -> Result<(), ClusterInviteError> {
        if self.cursor == self.bytes.len() {
            Ok(())
        } else {
            Err(ClusterInviteError::InvalidClaim)
        }
    }
}

fn put_string(bytes: &mut Vec<u8>, value: &str) -> Result<(), ClusterInviteError> {
    if value.len() > MAX_ADDRESS_TEXT {
        return Err(ClusterInviteError::InvalidClaim);
    }
    let length = u16::try_from(value.len()).map_err(|_| ClusterInviteError::InvalidClaim)?;
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(value.as_bytes());
    Ok(())
}

fn decode_hex(text: &str) -> Result<Vec<u8>, ClusterInviteError> {
    let mut bytes = Vec::with_capacity(text.len() / 2);
    for pair in text.as_bytes().chunks_exact(2) {
        let high = char::from(pair[0])
            .to_digit(16)
            .ok_or(ClusterInviteError::InvalidClaim)?;
        let low = char::from(pair[1])
            .to_digit(16)
            .ok_or(ClusterInviteError::InvalidClaim)?;
        bytes.push(((high << 4) | low) as u8);
    }
    Ok(bytes)
}

fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SecretKey;

    fn invite() -> ScopedClusterInvite {
        ScopedClusterInvite::new(
            SecretKey::generate().public(),
            "127.0.0.1:4321".parse().expect("direct address"),
            [1; 16],
            [2; 32],
            [3, 4],
            1,
            [5; 32],
            [6; 32],
            [7; 32],
            1_800_000_000_000,
            ClusterExecutionClass::TrustedCoordinatorHostExecution,
        )
        .expect("valid invite")
    }

    #[test]
    fn invite_token_roundtrips_and_fingerprint_binds_every_claim() {
        let invite = invite();
        let token = invite.encode_token().expect("encode canonical invite");
        let reopened = ScopedClusterInvite::decode_token(&token).expect("decode invite");
        assert_eq!(reopened, invite);
        assert_eq!(reopened.encode_token().expect("re-encode"), token);

        let mut bad = token.into_bytes();
        let index = bad.len() - 4;
        bad[index] = if bad[index] == b'0' { b'1' } else { b'0' };
        assert_eq!(
            ScopedClusterInvite::decode_token(
                std::str::from_utf8(&bad).expect("hex token remains ASCII")
            ),
            Err(ClusterInviteError::FingerprintMismatch)
        );

        let mut changed_endpoint = invite.clone();
        changed_endpoint.address = "127.0.0.1:4322".parse().expect("second direct address");
        assert_ne!(
            changed_endpoint.calculate_fingerprint(),
            invite.fingerprint,
            "the digest binds the address as a length-delimited field"
        );
    }

    #[test]
    fn invite_rejects_nondirect_and_out_of_scope_claims() {
        assert!(
            ScopedClusterInvite::new(
                SecretKey::generate().public(),
                "0.0.0.0:4321".parse().expect("unspecified address"),
                [1; 16],
                [2; 32],
                [3, 4],
                1,
                [5; 32],
                [6; 32],
                [7; 32],
                1_800_000_000_000,
                ClusterExecutionClass::TrustedCoordinatorHostExecution,
            )
            .is_err()
        );
    }
}
