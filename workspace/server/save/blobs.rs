//! The blobs — the parsed code (CST + IR + tarred source) that everything else
//! is derived from, and therefore the root of reproducibility: given the blobs,
//! every downstream store can be regenerated.
//!
//! Durable blob persistence itself lives in [`registry::Store`]: immutable,
//! content-addressed `cas/` sections plus a small mutable `ptr/` pointer per
//! package, with every read re-hashed against the key it was stored under (the
//! content address *is* the integrity check, so a mis-addressed or bit-rotted
//! object can never be served silently). What lives here is the server-side
//! surface over that root of truth:
//! - [`Server::verify_blobs`] — the per-package integrity audit: read every
//!   section a package's [`registry::blob::BlobManifest`] references back
//!   through the verifying store path and report exactly what is missing,
//!   corrupt, or inconsistent, plus whether the re-derived generation still
//!   matches the recorded snapshot;
//! - [`Server::rebuild_from_blobs`] — the rebuild-from-blobs entry point:
//!   re-emit the fan-out intent for **every** derived store at the recorded
//!   generation and let the pollers re-materialize (the outbox dedupe on
//!   `(package, generation, sink)` makes this idempotent).
//!
//! The per-sink drain helpers the sibling modules verify through also live
//! here: the outbox intents are minted at blob-emit time, so "which emitted
//! generations has a sink not yet acknowledged" is the root-of-truth side of
//! every derived store's story.
//!
//! KNOWN GAP: a *store-wide* orphan sweep (`cas/` objects no manifest
//! references — the GC half of a full audit) needs an enumeration API on
//! [`registry::Store`]. The old blob store had one (`list()` in
//! `source/blobstore/src/lib.rs`) but it was never ported to
//! `workspace/registry/store.rs`. Until it exists, auditing is per-package,
//! from the manifest down: it can prove presence and integrity, but not the
//! absence of garbage.

use heart::{ContentHash, PackageId, ResolutionState};
use registry::{RegistryError, StoreError};
use registry::blob::ReferenceSet;
use registry::coordination::OutboxSeq;

use runtime::vector::EmbeddingModel;
use crate::Server;
use crate::error::{BadRequestReason, ServerError, ServerResult};

use super::DerivedStore;

/// How many outbox intents one drain scan reads per page.
const DRAIN_SCAN_BATCH: usize = 256;

/// The findings of one per-package blob audit. Transport faults abort the audit
/// with an error; *content* findings (missing, corrupt, inconsistent) are
/// recorded here so one pass reports every problem rather than the first.
#[derive(Debug, Clone)]
pub struct BlobAudit {
	/// The package audited.
	pub package: PackageId,

	/// The generation hash re-derived from the manifest the blobs point at.
	pub snapshot: ContentHash,

	/// Whether [`Self::snapshot`] matches the recorded `Stored { hash }` —
	/// `false` means the blobs hold a different generation than the index says.
	pub matches_recorded: bool,

	/// How many source-file sections were read back and integrity-verified.
	pub files_verified: usize,

	/// The total verified source bytes.
	pub bytes_verified: u64,

	/// Paths whose section bytes hash-verified but whose length disagrees with
	/// the manifest's declared [`registry::blob::FileEntry::size`].
	pub size_mismatches: Vec<String>,

	/// Section hashes the store no longer holds.
	pub missing: Vec<ContentHash>,

	/// Section hashes whose stored bytes failed the read-path integrity check.
	pub corrupt: Vec<ContentHash>,

	/// Whether the references section decoded back into a
	/// [`registry::blob::ReferenceSet`] (only meaningful when it was readable).
	pub references_decoded: bool,
}

impl BlobAudit {
	/// Whether the audit found the package's blobs fully intact and matching
	/// the recorded snapshot.
	pub fn is_sound(&self) -> bool {
		self.matches_recorded
			&& self.missing.is_empty()
			&& self.corrupt.is_empty()
			&& self.size_mismatches.is_empty()
			&& self.references_decoded
	}
}

/// One derived sink's position against the emission log: the outbox head versus
/// the sink's durable consumer watermark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SinkDrainState {
	/// The highest sequence the outbox has recorded for this sink.
	pub head: OutboxSeq,

	/// The last sequence the sink's consumer durably acknowledged.
	pub consumed: OutboxSeq,
}

impl SinkDrainState {
	/// How many emitted intents the sink has not yet acknowledged.
	pub fn pending(&self) -> u64 { (self.head.0 - self.consumed.0).max(0) as u64 }

	/// Whether the sink is fully caught up to everything ever emitted.
	pub fn drained(&self) -> bool { self.pending() == 0 }
}

impl<M: EmbeddingModel> Server<M> {
	/// Audit a package's blobs against its manifest: read every referenced
	/// section back through the integrity-verifying store path and report what
	/// is missing, corrupt, or inconsistent — the deep-verification counterpart
	/// to [`Server::verify_reproducible`], which only re-derives the fold.
	///
	/// Ports the old blob store's semantics that the content address doubles as
	/// the integrity check (`source/blobstore`); the schema-version gate the old
	/// `put`/`get` enforced is superseded by the hash-verified read itself.
	#[tracing::instrument(skip(self), fields(%package))]
	pub async fn verify_blobs(&self, package: PackageId) -> ServerResult<BlobAudit> {
		self.authorize("save.verify_blobs")?;
		let stores = self.base();

		let recorded = self.recorded_snapshot(package).await?;
		let manifest = self.current_manifest(package).await?;
		manifest.validate().map_err(RegistryError::from)?;

		let snapshot = ContentHash::of_bytes(&manifest.identity_bytes());
		let mut audit = BlobAudit {
			package,
			snapshot,
			matches_recorded: snapshot == recorded,
			files_verified: 0,
			bytes_verified: 0,
			size_mismatches: Vec::new(),
			missing: Vec::new(),
			corrupt: Vec::new(),
			references_decoded: false,
		};

		// Every source file: the read path re-hashes the bytes against the key,
		// so an `Ok` is a proof of integrity; the manifest's declared size is
		// cross-checked on top.
		for entry in manifest.files.iter() {
			match stores.blobs.get_section(entry.hash).await {
				Ok(bytes) => {
					audit.files_verified += 1;
					audit.bytes_verified += bytes.len() as u64;
					if bytes.len() as u64 != entry.size {
						audit.size_mismatches.push(entry.path.to_string());
					}
				}
				Err(StoreError::NotFound { .. }) => audit.missing.push(entry.hash),
				Err(StoreError::Integrity { .. }) => audit.corrupt.push(entry.hash),
				Err(other) => return Err(RegistryError::from(other).into()),
			}
		}

		// The IR section: hash integrity is proof enough here — its encoding
		// belongs to the emit path, not to this audit.
		match stores.blobs.get_section(manifest.ir_ref).await {
			Ok(_) => {}
			Err(StoreError::NotFound { .. }) => audit.missing.push(manifest.ir_ref),
			Err(StoreError::Integrity { .. }) => audit.corrupt.push(manifest.ir_ref),
			Err(other) => return Err(RegistryError::from(other).into()),
		}

		// The references section must additionally *decode*: its bespoke codec
		// re-validates span order and kind discriminants, so a successful decode
		// proves the payload is meaningful, not merely byte-intact.
		match stores.blobs.get_section(manifest.references_ref).await {
			Ok(bytes) => match ReferenceSet::decode(&bytes) {
				Ok(_) => audit.references_decoded = true,
				Err(error) => {
					tracing::error!(%package, %error, "references section is byte-intact but undecodable");
				}
			},
			Err(StoreError::NotFound { .. }) => audit.missing.push(manifest.references_ref),
			Err(StoreError::Integrity { .. }) => audit.corrupt.push(manifest.references_ref),
			Err(other) => return Err(RegistryError::from(other).into()),
		}

		if !audit.is_sound() {
			tracing::error!(
				%package,
				matches_recorded = audit.matches_recorded,
				missing = audit.missing.len(),
				corrupt = audit.corrupt.len(),
				size_mismatches = audit.size_mismatches.len(),
				references_decoded = audit.references_decoded,
				"blob audit found damage"
			);
		}
		Ok(audit)
	}

	/// The rebuild-from-blobs entry point: re-emit the fan-out intent for
	/// **every** derived store at the package's recorded generation. The
	/// blob-vs-record generation check and the outbox dedupe make this the safe,
	/// idempotent "regenerate the whole read plane for this package" lever;
	/// [`Server::rebuild`] is the single-store form.
	///
	/// Returns the snapshot the intents were re-emitted at.
	#[tracing::instrument(skip(self), fields(%package))]
	pub async fn rebuild_from_blobs(&self, package: PackageId) -> ServerResult<ContentHash> {
		self.authorize("save.rebuild_from_blobs")?;
		let stores = self.base();

		let recorded = self.recorded_snapshot(package).await?;

		// The blobs must actually hold the recorded generation — fanning out a
		// different one would re-materialize a mixed read plane.
		let manifest = self.current_manifest(package).await?;
		if ContentHash::of_bytes(&manifest.identity_bytes()) != recorded {
			return Err(ServerError::BadRequest(BadRequestReason::SnapshotMismatch { package }));
		}

		let sinks: Vec<DerivedStore> =
			<DerivedStore as strum::IntoEnumIterator>::iter().collect();
		stores
			.outbox
			.append(package, recorded, &sinks)
			.await
			.map_err(RegistryError::from)?;
		tracing::info!(sinks = sinks.len(), "full fan-out re-emitted from blobs");
		Ok(recorded)
	}

	/// The recorded `Stored { hash }` snapshot for a package, or a bad-request
	/// error when the package has never completed a store.
	pub(crate) async fn recorded_snapshot(&self, package: PackageId) -> ServerResult<ContentHash> {
		let state =
			self.base().global_store.get_state(package).await.map_err(RegistryError::from)?;
		let ResolutionState::Stored { hash } = state else {
			return Err(ServerError::BadRequest(BadRequestReason::NoRecordedSnapshot { package }));
		};
		Ok(hash)
	}

	/// One sink's position against the emission log (head vs watermark).
	pub(crate) async fn sink_drain_state(&self, sink: DerivedStore) -> ServerResult<SinkDrainState> {
		let stores = self.base();
		let head = stores.outbox.head(sink).await.map_err(RegistryError::from)?;
		let consumed = stores.outbox.read_watermark(sink).await.map_err(RegistryError::from)?;
		Ok(SinkDrainState { head, consumed })
	}

	/// Whether every emitted intent for `package` on `sink` has been consumed:
	/// scan the sink's *pending* window (watermark → head) and look for the
	/// package. Bounded by the sink's lag, not the outbox's history.
	pub(crate) async fn sink_drained_for(
		&self,
		package: PackageId,
		sink: DerivedStore,
	) -> ServerResult<bool> {
		let stores = self.base();
		let mut cursor =
			stores.outbox.read_watermark(sink).await.map_err(RegistryError::from)?;
		loop {
			let entries = stores
				.outbox
				.read_since(sink, cursor, DRAIN_SCAN_BATCH)
				.await
				.map_err(RegistryError::from)?;
			if entries.iter().any(|entry| entry.package == package) {
				return Ok(false);
			}
			match entries.last() {
				Some(last) if entries.len() == DRAIN_SCAN_BATCH => cursor = last.id,
				_ => return Ok(true),
			}
		}
	}

	/// The shared "is this derived store current for this package" body: the
	/// package must have a recorded snapshot, the blobs must still hold that
	/// generation, and the sink must have drained every intent for the package.
	///
	/// This is watermark-level currency. *Content-level* verification (compare
	/// the store's actual documents/points against blob-derived expectations)
	/// is gapped per store — see each sibling module's docs for exactly which
	/// missing API unlocks it.
	pub(crate) async fn derived_store_current(
		&self,
		package: PackageId,
		sink: DerivedStore,
	) -> ServerResult<bool> {
		let recorded = self.recorded_snapshot(package).await?;
		let manifest = self.current_manifest(package).await?;
		if ContentHash::of_bytes(&manifest.identity_bytes()) != recorded {
			tracing::error!(%package, %sink, "blob generation diverged from the recorded snapshot");
			return Ok(false);
		}
		self.sink_drained_for(package, sink).await
	}
}
