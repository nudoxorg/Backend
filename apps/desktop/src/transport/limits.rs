//! The bounds this surface accepts from the local service.
//! Every one of them is a refusal to let a producer decide how much memory
//! this process allocates. They are stated once, here, and never inferred.

/// Maximum events accepted in one interactive subscription batch.
pub const MAX_EVENTS: usize = backend_library::MAX_SUBSCRIPTION_EVENTS;

/// Maximum bytes in one subscription frame.
pub const MAX_FRAME: usize = backend_replication::LOCAL_CONTROL_MAX_FRAME;

/// Maximum bytes in a configured endpoint path.
pub const MAX_TEXT: usize = 8 * 1024;

/// Maximum path length accepted for a local Unix endpoint.
///
/// This matches the local daemon's configured path budget and keeps an
/// invalid path from reaching the platform socket API.
pub const MAX_ENDPOINT_PATH: usize = backend_replication::MAX_UNIX_ENDPOINT_PATH_BYTES;

/// Environment variable naming the local daemon Unix endpoint.
pub const ENDPOINT_ENV: &str = "BACKEND_LOCALD_ENDPOINT";
