//! L1 (StampedeCache) + L2 (DiskCas) + optional L3.

use std::time::Duration;

use bytes::Bytes;
use caching::StampedeCache;
use heart::ContentHash;

use crate::disk::DiskCas;
use crate::registry::RegistryCas;
use crate::{Cas, CasError};

/// Assumed recompute cost when seeding L1 from a lower tier (XFetch bookkeeping).
const PROMOTE_COST: Duration = Duration::from_millis(1);

/// Soft TTL for L1 entries. Cached producer outputs are content-addressed and
/// immutable under a fixed job key; a long TTL just bounds memory residency.
const L1_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Tiered CAS: in-process stampede cache, optional local disk, optional L3 stub.
///
/// Read order: L1 → L2 → L3. Hits promote upward. Writes go to every configured
/// tier (L1 always; L2/L3 when present).
pub struct Tiered {
	l1: StampedeCache<ContentHash, Bytes>,
	l2: Option<DiskCas>,
	l3: Option<RegistryCas>,
}

impl Tiered {
	/// Build a tiered store.
	pub fn new(
		l1_capacity: u64,
		l2: Option<DiskCas>,
		l3: Option<RegistryCas>,
	) -> Self {
		Self {
			l1: StampedeCache::new(l1_capacity, L1_TTL),
			l2,
			l3,
		}
	}

	/// Process-default: L1 (256) + disk at `$NUDOX_PARSE_CACHE` / temp, no L3.
	pub fn default_open() -> Result<Self, CasError> {
		Ok(Self::new(256, Some(DiskCas::default_open()?), None))
	}

	/// L1 only (tests).
	pub fn memory_only(capacity: u64) -> Self {
		Self::new(capacity, None, None)
	}

	/// L1 + given disk root (tests / explicit wiring).
	pub fn with_disk(capacity: u64, disk: DiskCas) -> Self {
		Self::new(capacity, Some(disk), None)
	}
}

impl Cas for Tiered {
	async fn get(&self, key: ContentHash) -> Result<Option<Bytes>, CasError> {
		if let Some(v) = self.l1.get(&key).await {
			return Ok(Some(v));
		}

		if let Some(l2) = &self.l2 {
			if let Some(v) = l2.get(key).await? {
				self.l1.insert(key, v.clone(), PROMOTE_COST).await;
				return Ok(Some(v));
			}
		}

		if let Some(l3) = &self.l3 {
			if let Some(v) = l3.get(key).await? {
				if let Some(l2) = &self.l2 {
					let _ = l2.put_keyed(key, v.clone()).await?;
				}
				self.l1.insert(key, v.clone(), PROMOTE_COST).await;
				return Ok(Some(v));
			}
		}

		Ok(None)
	}

	async fn put(&self, bytes: Bytes) -> Result<ContentHash, CasError> {
		let key = ContentHash::of_bytes(&bytes);
		self.put_keyed(key, bytes).await?;
		Ok(key)
	}

	async fn put_keyed(&self, key: ContentHash, bytes: Bytes) -> Result<bool, CasError> {
		// Write durable tiers first so a crash after L1 insert still has the blob.
		let mut novel = true;
		if let Some(l2) = &self.l2 {
			novel = l2.put_keyed(key, bytes.clone()).await?;
		}
		if let Some(l3) = &self.l3 {
			let wrote = l3.put_keyed(key, bytes.clone()).await?;
			novel = novel && wrote;
		}
		self.l1.insert(key, bytes, PROMOTE_COST).await;
		Ok(novel)
	}
}
