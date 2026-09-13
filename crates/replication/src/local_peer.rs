//! Authenticated same-user Unix process channels.
//!
//! This module is the process-boundary trust seam for local clients.  A
//! connected stream is accepted only when its configured endpoint is an
//! owner-private Unix socket and the kernel reports the same effective user
//! on the peer.  The resulting token is deliberately affine and cannot be
//! serialized or reconstructed from wire fields.

#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, MetadataExt};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::Path;

use backend_version::{
    ProducerObservationClaims, ProducerObservationVerifier, UntrustedProducerObservation,
};

/// Failure while obtaining Unix peer credentials.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PeerCredentialError {
    /// The current target does not provide a supported credential API.
    Unsupported,
    /// The platform API failed while inspecting the socket.
    Io(std::io::ErrorKind),
    /// The platform returned an ID that cannot be represented by the public
    /// bounded credential type.
    InvalidId,
}

impl std::fmt::Display for PeerCredentialError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => formatter.write_str("Unix peer credentials are unsupported"),
            Self::Io(kind) => write!(
                formatter,
                "Unix peer credential inspection failed: {kind:?}"
            ),
            Self::InvalidId => formatter.write_str("Unix peer returned an invalid user ID"),
        }
    }
}

impl std::error::Error for PeerCredentialError {}

/// Effective user identity obtained from an accepted Unix stream.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PeerCredentials {
    /// The peer's effective user ID.
    pub effective_uid: u32,
}

/// Returns the current process effective user ID.
///
/// # Errors
///
/// Returns [`PeerCredentialError::InvalidId`] if the platform user ID cannot
/// be represented by the bounded credential type.
#[cfg(unix)]
pub fn current_effective_uid() -> Result<u32, PeerCredentialError> {
    let uid = rustix::process::geteuid().as_raw();
    Ok(uid)
}

/// Returns an error on targets without a supported effective-UID API.
#[cfg(not(unix))]
pub fn current_effective_uid() -> Result<u32, PeerCredentialError> {
    Err(PeerCredentialError::Unsupported)
}

/// Reads the accepted stream peer's effective UID on Linux.
///
/// # Errors
///
/// Returns an error when the kernel does not provide peer credentials or the
/// reported user ID is invalid.
#[cfg(target_os = "linux")]
pub fn peer_credentials(stream: &UnixStream) -> Result<PeerCredentials, PeerCredentialError> {
    use std::os::fd::AsFd;

    let credentials = rustix::net::sockopt::socket_peercred(stream.as_fd())
        .map_err(|error| PeerCredentialError::Io(error.kind()))?;
    let effective_uid = credentials.uid.as_raw();
    Ok(PeerCredentials { effective_uid })
}

/// Reads the accepted stream peer's effective UID on Apple and BSD Unix.
///
/// # Errors
///
/// Returns an error when the platform cannot inspect the connected peer.
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "openbsd",
    target_os = "netbsd"
))]
pub fn peer_credentials(stream: &UnixStream) -> Result<PeerCredentials, PeerCredentialError> {
    let (uid, _gid) = nix::unistd::getpeereid(stream)
        .map_err(|error| PeerCredentialError::Io(std::io::Error::from(error).kind()))?;
    let effective_uid = uid.as_raw();
    Ok(PeerCredentials { effective_uid })
}

/// Fails closed on Unix targets with no supported peer credential API.
#[cfg(all(
    unix,
    not(target_os = "linux"),
    not(target_os = "macos"),
    not(target_os = "ios"),
    not(target_os = "freebsd"),
    not(target_os = "dragonfly"),
    not(target_os = "openbsd"),
    not(target_os = "netbsd")
))]
pub fn peer_credentials(_stream: &UnixStream) -> Result<PeerCredentials, PeerCredentialError> {
    Err(PeerCredentialError::Unsupported)
}

/// Checks that an accepted Unix peer has the same effective UID as this
/// process. Both credential reads are performed for every connection and any
/// inspection failure is returned to the caller.
///
/// # Errors
///
/// Returns an error when either peer credential lookup fails.
#[cfg(unix)]
pub fn peer_is_same_effective_uid(stream: &UnixStream) -> Result<bool, PeerCredentialError> {
    let peer = peer_credentials(stream)?.effective_uid;
    let current = current_effective_uid()?;
    Ok(peer == current)
}

/// Failure while authenticating a local process peer for producer coverage.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalPeerAuthenticationError {
    /// The endpoint could not be inspected.
    EndpointIo(std::io::ErrorKind),
    /// The configured path is not a Unix socket.
    EndpointNotSocket,
    /// The endpoint is not private to its owner.
    EndpointInsecurePermissions,
    /// The endpoint belongs to another effective user.
    EndpointWrongOwner,
    /// The connected stream did not name the authenticated endpoint.
    PeerAddress,
    /// The stream peer was another effective user.
    PeerRejected,
    /// Peer credential inspection failed.
    PeerCredentials(PeerCredentialError),
    /// The certificate did not carry a bounded complete observation.
    InvalidObservation,
}

#[cfg(unix)]
impl std::fmt::Display for LocalPeerAuthenticationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EndpointIo(kind) => {
                write!(formatter, "local endpoint inspection failed: {kind:?}")
            }
            Self::EndpointNotSocket => formatter.write_str("local endpoint is not a Unix socket"),
            Self::EndpointInsecurePermissions => {
                formatter.write_str("local endpoint permissions must be exactly 0600")
            }
            Self::EndpointWrongOwner => {
                formatter.write_str("local endpoint owner is not the effective user")
            }
            Self::PeerAddress => {
                formatter.write_str("local peer address does not name the authenticated endpoint")
            }
            Self::PeerRejected => formatter.write_str("local peer credentials were rejected"),
            Self::PeerCredentials(error) => error.fmt(formatter),
            Self::InvalidObservation => {
                formatter.write_str("local producer observation is not a bounded complete claim")
            }
        }
    }
}

#[cfg(unix)]
impl std::error::Error for LocalPeerAuthenticationError {}

/// An owner-authenticated local process channel.
///
/// This token is deliberately affine to the endpoint authentication result:
/// its fields and constructor are private, it is never serialized, and it
/// implements [`ProducerObservationVerifier`] only after both the private
/// endpoint metadata and connected peer UID have been checked. A certificate
/// still supplies the exact scope, producer, context, and evidence; this token
/// only supplies the local owner authority needed to admit those claims.
#[cfg(unix)]
#[derive(Debug, Eq, PartialEq)]
pub struct AuthenticatedLocalPeer {
    effective_uid: u32,
    endpoint_digest: [u8; 32],
}

#[cfg(unix)]
impl AuthenticatedLocalPeer {
    /// Authenticates a connected peer against an owner-private Unix socket.
    ///
    /// # Errors
    ///
    /// Returns an error when the endpoint is missing, is not a private socket,
    /// belongs to another user, or the connected stream cannot prove the same
    /// effective UID.
    pub fn authenticate(
        stream: &UnixStream,
        endpoint: &Path,
    ) -> Result<Self, LocalPeerAuthenticationError> {
        let metadata = std::fs::symlink_metadata(endpoint)
            .map_err(|error| LocalPeerAuthenticationError::EndpointIo(error.kind()))?;
        if !metadata.file_type().is_socket() {
            return Err(LocalPeerAuthenticationError::EndpointNotSocket);
        }
        if metadata.mode() & 0o777 != 0o600 {
            return Err(LocalPeerAuthenticationError::EndpointInsecurePermissions);
        }
        let owner = metadata.uid();
        let effective_uid =
            current_effective_uid().map_err(LocalPeerAuthenticationError::PeerCredentials)?;
        if owner != effective_uid {
            return Err(LocalPeerAuthenticationError::EndpointWrongOwner);
        }
        let peer_address = stream
            .peer_addr()
            .map_err(|_| LocalPeerAuthenticationError::PeerAddress)?;
        if peer_address.as_pathname() != Some(endpoint) {
            return Err(LocalPeerAuthenticationError::PeerAddress);
        }
        if !peer_is_same_effective_uid(stream)
            .map_err(LocalPeerAuthenticationError::PeerCredentials)?
        {
            return Err(LocalPeerAuthenticationError::PeerRejected);
        }
        let endpoint_digest =
            *blake3::hash(endpoint.as_os_str().to_string_lossy().as_bytes()).as_bytes();
        Ok(Self {
            effective_uid,
            endpoint_digest,
        })
    }

    /// Returns the authenticated effective UID for diagnostics.
    #[must_use]
    pub const fn effective_uid(&self) -> u32 {
        self.effective_uid
    }

    /// Returns whether this token was authenticated for the supplied socket
    /// path. The path itself is not an authority claim until authentication is
    /// repeated against a connected stream.
    #[must_use]
    pub fn binds_endpoint(&self, endpoint: &Path) -> bool {
        self.endpoint_digest
            == *blake3::hash(endpoint.as_os_str().to_string_lossy().as_bytes()).as_bytes()
    }
}

#[cfg(unix)]
impl ProducerObservationVerifier for AuthenticatedLocalPeer {
    type Error = LocalPeerAuthenticationError;

    fn verify(
        &self,
        observation: &UntrustedProducerObservation,
    ) -> Result<ProducerObservationClaims, Self::Error> {
        if observation.scope_root().as_bytes() == &[0; 32]
            || observation.producer_identity() == [0; 32]
            || observation.context() == [0; 32]
            || observation.evidence().is_empty()
            || observation.evidence().len() > 64 * 1024
        {
            return Err(LocalPeerAuthenticationError::InvalidObservation);
        }
        Ok(ProducerObservationClaims::new(
            observation.producer_identity(),
            observation.scope_root(),
            observation.context(),
            *blake3::hash(observation.evidence()).as_bytes(),
        ))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn connected_local_peer_reports_the_current_effective_uid() {
        let (peer, _owner) = UnixStream::pair().expect("UnixStream::pair failed");
        assert!(peer_is_same_effective_uid(&peer).expect("peer credentials failed"));
    }

    #[test]
    fn authenticated_local_peer_requires_owner_private_socket() {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::UnixListener;

        let path = std::env::temp_dir().join(format!("b-auth-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind test socket failed");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("set test socket permissions failed");
        let client = UnixStream::connect(&path).expect("connect test socket failed");
        let (_server, _) = listener.accept().expect("accept test socket failed");
        let peer = AuthenticatedLocalPeer::authenticate(&client, &path)
            .expect("local peer authentication failed");
        assert_eq!(peer.effective_uid(), current_effective_uid().unwrap_or(0));
        assert!(peer.binds_endpoint(&path));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn authenticated_local_peer_rejects_an_insecure_socket() {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::UnixListener;

        let path = std::env::temp_dir().join(format!("b-auth-open-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind test socket failed");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666))
            .expect("set insecure socket permissions failed");
        let client = UnixStream::connect(&path).expect("connect test socket failed");
        let (_server, _) = listener.accept().expect("accept test socket failed");
        assert_eq!(
            AuthenticatedLocalPeer::authenticate(&client, &path),
            Err(LocalPeerAuthenticationError::EndpointInsecurePermissions)
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn authenticated_local_peer_rejects_forged_regular_path() {
        let path =
            std::env::temp_dir().join(format!("backend-auth-peer-file-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, b"not a socket").expect("write test endpoint failed");
        let (peer, _owner) = UnixStream::pair().expect("UnixStream::pair failed");
        assert_eq!(
            AuthenticatedLocalPeer::authenticate(&peer, &path),
            Err(LocalPeerAuthenticationError::EndpointNotSocket)
        );
        let _ = std::fs::remove_file(path);
    }
}
