//! L3 adapter over `registry::Store` — **stub for Phase 1**.
//!
//! Wiring a live store here would either pull `registry` into `cas` (circular
//! with forge assembly) or force object-store types into this crate. The
//! adapter shape is fixed so Phase 4 (`ForgeRuntime`) can hand a real backend
//! without changing [`crate::Cas`] or [`crate::Tiered`].
//!
//! [`Tiered`] treats [`CasError::Unsupported`] as "tier absent" so composing a
//! stub never hard-fails a get/put.

use bytes::Bytes;
use heart::ContentHash;

use crate::{Cas, CasError};

/// Placeholder L3 face. All operations return [`CasError::Unsupported`] until
/// a `Store<Live>` (or thin object-store) handle is injected at runtime assemble.
#[derive(Debug, Default, Clone, Copy)]
pub struct RegistryCas;

impl RegistryCas {
	/// Construct the unwired stub.
	pub fn stub() -> Self { Self }
}

impl Cas for RegistryCas {
	async fn get(&self, _key: ContentHash) -> Result<Option<Bytes>, CasError> {
		Err(CasError::Unsupported(
			"RegistryCas L3 adapter not wired yet (Phase 1 stub)",
		))
	}

	async fn put(&self, _bytes: Bytes) -> Result<ContentHash, CasError> {
		Err(CasError::Unsupported(
			"RegistryCas L3 adapter not wired yet (Phase 1 stub)",
		))
	}

	async fn put_keyed(&self, _key: ContentHash, _bytes: Bytes) -> Result<bool, CasError> {
		Err(CasError::Unsupported(
			"RegistryCas L3 adapter not wired yet (Phase 1 stub)",
		))
	}

	async fn invalidate(&self, _key: ContentHash) -> Result<(), CasError> {
		// Nothing to drop — stub has no storage.
		Ok(())
	}
}
