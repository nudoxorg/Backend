//! Re-exports of the shared forge identity vocabulary.
//!
//! Forge coordinates and immutable Git object identities are product
//! contracts shared with registry projections. Their canonical parser and
//! identity derivation live in `backend-library`; this module keeps the
//! existing engine import path stable for transports and acquisition code.

pub use backend_library::{
    ForgeCoordinate, ForgeCoordinateError, ForgeHashAlgorithm, ForgeObjectId, ForgeProvider,
    ForgeRefName, ForgeRevision, ForgeUnavailableReason,
};
