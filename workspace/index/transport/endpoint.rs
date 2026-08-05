//! The shared iroh endpoint builder.
//!
//! Both prior transports built their endpoint identically: `presets::Minimal`,
//! a secret key (random or caller-fixed for deterministic tests), an optional
//! in-process [`MemoryLookup`] with loopback binding and no relay, and a set of
//! advertised ALPNs. That builder lives here once.

use iroh::Endpoint;
use iroh::address_lookup::MemoryLookup;

use heart::sync::SyncError;

pub use iroh::address_lookup::MemoryLookup as AddressLookup;
pub use iroh::{EndpointId, SecretKey};

/// Build a minimal iroh endpoint advertising `alpns`.
///
/// * `secret_key` fixes the endpoint identity (tests pre-determine the
///   [`EndpointId`] before binding; production passes a fresh key).
/// * `address_lookup` supplies an in-process discovery table for loopback
///   testing; when present the endpoint binds to `127.0.0.1:0` and `[::1]:0`
///   with no relay. When `None`, the endpoint binds with default discovery.
pub async fn bind_endpoint(
    alpns: Vec<Vec<u8>>,
    secret_key: SecretKey,
    address_lookup: Option<MemoryLookup>,
) -> Result<Endpoint, SyncError> {
    use iroh::endpoint::presets;

    let mut builder = Endpoint::builder(presets::Minimal)
        .secret_key(secret_key)
        .alpns(alpns);

    if let Some(lookup) = address_lookup {
        builder = builder
            .address_lookup(lookup)
            .bind_addr("127.0.0.1:0")
            .map_err(|error| SyncError::Transport(std::io::Error::other(error)))?
            .bind_addr("[::1]:0")
            .map_err(|error| SyncError::Transport(std::io::Error::other(error)))?;
    }

    builder
        .bind()
        .await
        .map_err(|error| SyncError::Transport(std::io::Error::other(error)))
}
