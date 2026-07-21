//! The federation-aware sync driver (CONSOLIDATION-NOTES §8e).
//!
//! A thin orchestrator that fans a **verified** content item across the
//! federation topology. It is composition tissue: it holds no domain logic of
//! its own, it only multiplexes an already-content-addressed item onto the
//! right targets per deployment, over two existing seams —
//!
//!   * [`heart::sync::ContentIo`] — the per-target read/write/has/verify seam,
//!     implemented by all four planes (ir-vcs changes, `index::pack`,
//!     `index::shard_sync`, sandbox goldens). [`FederationSync`] is **generic**
//!     over it, so nothing here special-cases one plane.
//!   * the [`transport`] crate — `transport::blob::{Provider, Fetcher}` serve /
//!     fetch content-addressed bytes over iroh, keeping `heart` iroh-free.
//!
//! # Trust model (unchanged)
//!
//! Content-addressing is the sole trust anchor. Every write — local or pulled —
//! goes through [`ContentIo::verify`] *first*; the transport is never trusted.
//! [`replicate`](FederationSync::replicate) verifies before writing locally and
//! before it will hand bytes to the transport; [`pull`](FederationSync::pull)
//! verifies the fetched bytes before writing them. A failed verify writes and
//! applies nothing.
//!
//! # Topology → targets
//!
//! The remote target set is derived from the [`Federation`]'s overlays and the
//! [`ServerConfiguration`]: each source that carries a
//! [`SourceConfig::sync_endpoint`](crate::config::SourceConfig) is a genuinely
//! off-node peer this node pushes to / pulls from. Sources without one are
//! served locally on this node (the definitive base and any co-located
//! overlays) and contribute no remote target. See
//! [`FederationSync::from_config`].

use heart::SourceId;
use heart::sync::{ContentIo, SyncError, VerifyError};
use transport::EndpointId;
use transport::announce::{Announcement, send_announcement};
use transport::blob::{Fetcher, Provider, TransportHash};

use crate::config::{ServerConfiguration, SourceConfig};

/// One remote federation peer this node can push a verified item to (and pull a
/// missing item from): the source it stands for plus its iroh endpoint.
///
/// Derived from an overlay's [`SourceConfig::sync_endpoint`]. A source without a
/// sync endpoint is local to this node and produces no `RemoteTarget`.
#[derive(Clone, Debug)]
pub struct RemoteTarget {
	/// The federated source this endpoint serves — its stable identity.
	pub source: SourceId,
	/// The iroh endpoint bytes are pushed to / fetched from.
	pub endpoint: EndpointId,
}

impl RemoteTarget {
	/// Build a target from a configured source, or `None` when the source has no
	/// `sync_endpoint` (it is served locally on this node).
	///
	/// The endpoint is parsed from the hex-encoded [`EndpointId`] the config
	/// carries — keeping `config` iroh-free (see [`SourceConfig::sync_endpoint`]).
	pub fn from_source_config(config: &SourceConfig) -> Result<Option<Self>, SyncError> {
		let Some(hex) = config.sync_endpoint.as_ref() else {
			return Ok(None);
		};
		let endpoint: EndpointId = hex
			.parse()
			.map_err(|error| SyncError::Other(format!("invalid sync_endpoint {hex:?}: {error}")))?;
		Ok(Some(Self { source: config.source_id(), endpoint }))
	}
}

/// How this node participates in federation sync — set once from the deployment
/// shape, not per call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncMode {
	/// This node serves a local store: [`replicate`](FederationSync::replicate)
	/// writes the verified item locally **and** fans it out to every remote
	/// target (local + remote fan-out). The default gateway/all-in-one shape.
	Serving,
	/// A compiler ("fleet") node with no local serving store: `replicate` skips
	/// the local write and only pushes to the remote targets — the central /
	/// definitive store (fleet → central).
	Fleet,
}

/// The federation sync orchestrator (CONSOLIDATION-NOTES §8e).
///
/// Generic over the embedding-model brand `M` only so it can be derived from a
/// [`Federation<SourceStores<M>>`](crate::Driver); it is **not** generic over
/// the plane — the plane comes in per call as `&C: ContentIo`, so one
/// `FederationSync` drives changes, packs, shards and goldens alike.
pub struct FederationSync {
	/// This node's push/pull peers — the off-node overlays / central store.
	targets: Vec<RemoteTarget>,
	/// Whether this node serves a local store (see [`SyncMode`]).
	mode: SyncMode,
}

impl FederationSync {
	/// Construct directly from an explicit target list and mode — the plumbing
	/// constructor the [`from_config`](Self::from_config) /
	/// [`from_federation`](Self::from_federation) derivations funnel through, and
	/// the seam tests drive.
	pub fn new(targets: Vec<RemoteTarget>, mode: SyncMode) -> Self {
		Self { targets, mode }
	}

	/// Derive the sync driver from the resolved [`ServerConfiguration`]: every
	/// overlay that carries a [`sync_endpoint`](SourceConfig::sync_endpoint)
	/// becomes a remote push/pull target. The definitive base is a target too
	/// when it is off-node (a fleet/compiler node points its base at the central
	/// store); when it is local it simply carries no `sync_endpoint`.
	///
	/// The mode is chosen from the node's role: a [`Forge`](crate::Role::Forge)
	/// node has no local serving store, so it syncs in [`SyncMode::Fleet`]
	/// (push-only to central); every other role serves locally
	/// ([`SyncMode::Serving`]).
	pub fn from_config(config: &ServerConfiguration) -> Result<Self, SyncError> {
		// The base is a remote target only when it is genuinely off-node (a fleet
		// node's central store). A locally-served base carries no sync_endpoint
		// and contributes nothing here.
		let mut targets = Vec::new();
		if let Some(target) = RemoteTarget::from_source_config(&config.definitive)? {
			targets.push(target);
		}
		// Each overlay opts in to remote sync by setting `sync_endpoint` in its
		// `[[overlays]]` block; a co-located overlay leaves it unset and is served
		// locally (no remote target is contributed).
		for overlay in &config.overlays {
			if let Some(target) = RemoteTarget::from_source_config(overlay)? {
				targets.push(target);
			}
		}

		let mode = if config.role.runs_forge() && !config.role.runs_gateway() {
			SyncMode::Fleet
		} else {
			SyncMode::Serving
		};
		Ok(Self::new(targets, mode))
	}

	/// Derive the target list from a live [`Federation`](heart::Federation) plus
	/// the config it was assembled from — the accessor a
	/// [`Driver`](crate::Driver) hands its own `federation()` + `config()` to.
	///
	/// The [`Federation`](heart::Federation) itself holds only the connected
	/// store stacks, not the serving endpoints, so the endpoints still come from
	/// config; this is the same derivation as [`from_config`](Self::from_config),
	/// named for the call site that already has a `Driver` in hand.
	pub fn from_federation<S>(
		_federation: &heart::Federation<S>,
		config: &ServerConfiguration,
	) -> Result<Self, SyncError> {
		Self::from_config(config)
	}

	/// This node's remote push/pull targets.
	pub fn targets(&self) -> &[RemoteTarget] {
		&self.targets
	}

	/// This node's sync mode.
	pub fn mode(&self) -> SyncMode {
		self.mode
	}

	/// Fan a **verified** content item across the federation (§8e).
	///
	/// The item is verified against its id **first** — a failed verify writes and
	/// pushes nothing. Then, depending on [`mode`](Self::mode):
	///
	///   * [`Serving`](SyncMode::Serving): write the bytes to the LOCAL store
	///     (`io.write`) **and** push them to every configured remote overlay
	///     endpoint via `transport` (local + remote fan-out).
	///   * [`Fleet`](SyncMode::Fleet): skip the local write; only push to the
	///     remote target(s) — the central / definitive store (fleet → central).
	///
	/// Generic over `C: ContentIo`, so it drives every plane
	/// (changes / packs / shards / goldens) with no special-casing.
	pub async fn replicate<C: ContentIo>(
		&self,
		io: &C,
		provider: &Provider,
		id: &C::Id,
		bytes: &[u8],
	) -> Result<Replicated, SyncError> {
		// Verify-before-write: content-addressing is the only trust anchor. This
		// must precede any write OR any handoff to the (untrusted) transport.
		io.verify(id, bytes)?;

		// Local half: only serving nodes hold a local store.
		let wrote_local = match self.mode {
			SyncMode::Serving => {
				io.write(id, bytes)?;
				true
			}
			SyncMode::Fleet => false,
		};

		// Remote fan-out: publish the verified bytes to the provider once, open a
		// control-ALPN stream to each peer, and deliver the (id, transport_hash)
		// announcement so the peer can pull the blob.  A delivery failure to one
		// target is logged and recorded (`delivered: false`) but does NOT abort
		// the others — partial fan-out is preferred over a full stop.
		let mut pushes = Vec::with_capacity(self.targets.len());
		if !self.targets.is_empty() {
			let transport_hash = provider.add_bytes(bytes.to_vec()).await?;
			let ann = Announcement {
				id: id.to_string(),
				transport_hash,
			};
			for target in &self.targets {
				let delivered = match send_announcement(
					provider.endpoint(),
					target.endpoint,
					&ann,
				)
				.await
				{
					Ok(ack) => {
						if !ack.accepted {
							tracing::warn!(
								source = ?target.source,
								endpoint = ?target.endpoint,
								id = %ann.id,
								"federation push: peer rejected announcement",
							);
						}
						ack.accepted
					}
					Err(error) => {
						tracing::warn!(
							%error,
							source = ?target.source,
							endpoint = ?target.endpoint,
							id = %ann.id,
							"federation push: delivery failed, continuing",
						);
						false
					}
				};
				pushes.push(PushRecord {
					source: target.source,
					endpoint: target.endpoint,
					transport_hash,
					delivered,
				});
			}
		}

		Ok(Replicated { wrote_local, pushes })
	}

	/// Transparently pull a **missing** content item (§8e).
	///
	/// If `io.has(id)` is already true, this is a no-op ([`Pulled::AlreadyLocal`]).
	/// Otherwise fetch the bytes from a remote target via
	/// [`Fetcher::fetch`](transport::blob::Fetcher::fetch), **verify** them
	/// against `id`, and only then `io.write` — so a missing item is pulled with
	/// the content-address trust anchor intact.
	///
	/// The `(TransportHash)` a caller fetches by comes from the remote's
	/// announcement; here it is supplied by the caller that received it. With no
	/// configured targets the item cannot be pulled ([`Pulled::NoRemote`]).
	pub async fn pull<C: ContentIo>(
		&self,
		io: &C,
		fetcher: &Fetcher,
		id: &C::Id,
		transport_hash: TransportHash,
	) -> Result<Pulled, SyncError> {
		if io.has(id)? {
			return Ok(Pulled::AlreadyLocal);
		}
		let Some(target) = self.targets.first() else {
			return Ok(Pulled::NoRemote);
		};

		// Move bytes only — the fetcher size-caps but never trusts them.
		let bytes = fetcher
			.fetch(target.endpoint, transport_hash, io.max_item_bytes())
			.await?;

		// Verify-before-write: the fetched bytes are untrusted until they verify
		// against the content-address id.
		io.verify(id, &bytes)?;
		io.write(id, &bytes)?;
		Ok(Pulled::Fetched { from: target.source })
	}
}

/// A recorded push of a replicated item to one remote target.
///
/// `delivered` is `true` when the peer accepted the announcement over the
/// wire; `false` when the delivery attempt failed (the bytes are still served
/// by the [`Provider`] so the peer can pull on its next reconnect if desired).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushRecord {
	/// The remote source the item is pushed to.
	pub source: SourceId,
	/// The endpoint the announcement was sent to.
	pub endpoint: EndpointId,
	/// The transport hash the peer fetches the served bytes by.
	pub transport_hash: TransportHash,
	/// Whether the peer accepted the announcement over the wire.
	///
	/// `false` either means the connection failed or the peer replied
	/// `Ack { accepted: false }`. The delivery failure is logged; other
	/// targets are still attempted.
	pub delivered: bool,
}

/// The outcome of [`FederationSync::replicate`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replicated {
	/// Whether the item was written to the local store (false on a fleet node).
	pub wrote_local: bool,
	/// The per-target pushes fanned out to the remote overlays.
	pub pushes: Vec<PushRecord>,
}

/// The outcome of [`FederationSync::pull`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pulled {
	/// The item was already present locally; nothing was fetched.
	AlreadyLocal,
	/// The item was fetched from a remote, verified, and written.
	Fetched {
		/// The remote source it was pulled from.
		from: SourceId,
	},
	/// The item was missing and there was no remote target to pull from.
	NoRemote,
}

/// A `VerifyError` is a `SyncError` (`replicate`/`pull` surface both as one).
const _: fn(VerifyError) -> SyncError = SyncError::from;

#[cfg(test)]
mod tests {
	use super::*;
	use std::collections::HashMap;
	use std::io;
	use std::sync::Mutex;

	/// An in-memory `ContentIo` fake: a `HashMap` store whose `verify` is a
	/// simple byte-prefix check (stands in for a real content-address hash). It
	/// is deliberately plane-agnostic — the same fake exercises the generic
	/// orchestrator.
	struct MemIo {
		store: Mutex<HashMap<String, Vec<u8>>>,
		/// Bytes must start with this to "verify" against their id.
		magic: &'static [u8],
	}

	impl MemIo {
		fn new(magic: &'static [u8]) -> Self {
			Self { store: Mutex::new(HashMap::new()), magic }
		}
		fn contains(&self, id: &str) -> bool {
			self.store.lock().unwrap().contains_key(id)
		}
	}

	impl ContentIo for MemIo {
		type Id = String;

		fn read(&self, id: &String) -> io::Result<Vec<u8>> {
			self.store
				.lock()
				.unwrap()
				.get(id)
				.cloned()
				.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing"))
		}
		fn write(&self, id: &String, bytes: &[u8]) -> io::Result<()> {
			self.store.lock().unwrap().insert(id.clone(), bytes.to_vec());
			Ok(())
		}
		fn has(&self, id: &String) -> io::Result<bool> {
			Ok(self.contains(id))
		}
		fn verify(&self, id: &String, bytes: &[u8]) -> Result<(), VerifyError> {
			if bytes.starts_with(self.magic) {
				Ok(())
			} else {
				Err(VerifyError::HashMismatch {
					expected: id.clone(),
					got: "bad-prefix".into(),
				})
			}
		}
	}

	fn a_target() -> RemoteTarget {
		// A fixed endpoint id derived from a fixed secret key — no listener is
		// bound at this endpoint in the unit tests, so `send_announcement` will
		// fail to connect and `PushRecord::delivered` will be `false`. That is
		// the expected outcome for the "attempt delivery, log failure" path.
		let secret = transport::SecretKey::from_bytes(&[7u8; 32]);
		RemoteTarget {
			source: heart::Id::from_name(&heart::access::source::NAMESPACE, b"overlay"),
			endpoint: secret.public(),
		}
	}

	async fn a_provider() -> Provider {
		let endpoint = transport::bind_endpoint(
			vec![transport::BLOBS_ALPN.to_vec()],
			transport::SecretKey::from_bytes(&[9u8; 32]),
			Some(transport::AddressLookup::default()),
		)
		.await
		.expect("bind provider endpoint");
		Provider::serve(endpoint)
	}

	#[tokio::test]
	async fn replicate_serving_writes_local_and_records_pushes() {
		let io = MemIo::new(b"OK");
		let provider = a_provider().await;
		let sync = FederationSync::new(vec![a_target()], SyncMode::Serving);

		let out = sync
			.replicate(&io, &provider, &"item-1".to_string(), b"OK-payload")
			.await
			.expect("replicate");

		// Local write happened, and one push was attempted per remote target.
		// `delivered` is false because no listener is bound in the unit test —
		// the connection attempt fails, `replicate` logs and continues.
		assert!(out.wrote_local);
		assert!(io.contains("item-1"));
		assert_eq!(out.pushes.len(), 1);
		assert!(!out.pushes[0].delivered);
	}

	#[tokio::test]
	async fn replicate_fleet_skips_local_write() {
		let io = MemIo::new(b"OK");
		let provider = a_provider().await;
		let sync = FederationSync::new(vec![a_target()], SyncMode::Fleet);

		let out = sync
			.replicate(&io, &provider, &"item-2".to_string(), b"OK-payload")
			.await
			.expect("replicate");

		// Fleet node: no local store, delivery attempt to central was made.
		// `delivered` is false because no listener is bound in the unit test.
		assert!(!out.wrote_local);
		assert!(!io.contains("item-2"));
		assert_eq!(out.pushes.len(), 1);
		assert!(!out.pushes[0].delivered);
	}

	#[tokio::test]
	async fn replicate_rejects_unverified_bytes_before_any_write() {
		let io = MemIo::new(b"OK");
		let provider = a_provider().await;
		let sync = FederationSync::new(vec![a_target()], SyncMode::Serving);

		let err = sync
			.replicate(&io, &provider, &"item-3".to_string(), b"BAD-payload")
			.await
			.expect_err("verify must fail");

		assert!(matches!(err, SyncError::VerificationFailed(_)));
		// Nothing was written — verify-before-write held.
		assert!(!io.contains("item-3"));
	}

	#[tokio::test]
	async fn pull_no_op_when_already_local() {
		let io = MemIo::new(b"OK");
		io.write(&"present".to_string(), b"OK-x").unwrap();
		let fetcher = Fetcher::new(a_provider().await.endpoint().clone());
		let sync = FederationSync::new(vec![a_target()], SyncMode::Serving);

		let out = sync
			.pull(&io, &fetcher, &"present".to_string(), TransportHash([0u8; 32]))
			.await
			.expect("pull");
		assert_eq!(out, Pulled::AlreadyLocal);
	}

	#[tokio::test]
	async fn pull_no_remote_when_no_targets() {
		let io = MemIo::new(b"OK");
		let fetcher = Fetcher::new(a_provider().await.endpoint().clone());
		let sync = FederationSync::new(Vec::new(), SyncMode::Serving);

		let out = sync
			.pull(&io, &fetcher, &"absent".to_string(), TransportHash([0u8; 32]))
			.await
			.expect("pull");
		assert_eq!(out, Pulled::NoRemote);
	}
}
