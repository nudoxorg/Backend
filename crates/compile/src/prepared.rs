//! Version keyed preparation for native authority requests.
//!
//! The facade keeps the public preparation API stable while the key/digest,
//! cache/singleflight, and error ownership live in focused modules.

#[path = "prepared_admission.rs"]
mod prepared_admission;
#[path = "prepared_cache.rs"]
mod prepared_cache;
#[path = "prepared_error.rs"]
mod prepared_error;
#[path = "prepared_key.rs"]
mod prepared_key;

pub use prepared_cache::{
    PreparationCache, PreparationCacheConfig, PreparationCacheStats, PreparedCompilationCache,
    PreparedRequest,
};
pub use prepared_error::PreparationError;
pub use prepared_key::PreparationKey;

#[cfg(test)]
#[path = "prepared_tests.rs"]
mod tests;
