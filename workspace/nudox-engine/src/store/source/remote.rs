//! `RemoteSource` — an [`IrSource`] that hydrates a package's IR from a remote
//! generation instead of producing it locally.
//!
//! This is the corpus-level half of "minimum work when connected": the query
//! plane already delegates to a remote via [`heart::surface::Routed`], and this
//! is the loading plane doing the same — a package the remote already has sealed
//! is *replayed* from a fetched [`IrSnapshot`](crate::store::remote::IrSnapshot),
//! not re-run through the local producer.
//!
//! # What is and isn't here
//!
//! The client half of discovery — "which remote IR object serves this package?"
//! — is modelled as [`RemoteCatalog`]: a client fetches this map once (the way
//! it fetches [`Capabilities`](heart::surface::Capabilities)) and holds a
//! snapshot, so resolution is a synchronous lookup, not a per-package round
//! trip. The **server** half — an index endpoint that advertises that map and
//! serves `GET /ir/{hash}` — is deliberately *not* built here: the index's IR
//! read-path is still write-only (IR-STORAGE-PLAN §3), so this subsystem is
//! exercised against an in-process CAS in tests and goes live end-to-end the
//! moment the server grows that endpoint. It is not dead code — it is wired into
//! [`Engine::start`](crate::runtime::Engine::start) and activated by
//! [`EngineConfig::remote`](crate::runtime::EngineConfig), the registration site
//! that keeps it from being a feature that compiles but never runs.

use std::collections::HashMap;
use std::sync::Arc;

use futures::stream::{self, BoxStream, StreamExt as _};
use heart::ContentHash;
use heart::surface::GenerationId;
use nudox_ir::change::PackageLineageId;

use crate::store::package::{PackageView, Provenance};
use crate::store::remote::RemoteStore;
use crate::store::source::{
    Error, IrSource, LoadEvent, LoadRequest, PackageHint, SourceDescriptor,
};

/// Discovery: which remote IR object hash (if any) serves a package lineage.
pub trait RemoteCatalog: Send + Sync + 'static {
    /// The remote IR object hash serving `lineage`, or `None` if the remote
    /// does not carry this package (a miss the caller resolves by producing
    /// locally).
    fn resolve(&self, lineage: &PackageLineageId) -> Option<ContentHash>;
}

/// A [`RemoteCatalog`] backed by an in-memory snapshot map — the shape a client
/// holds after fetching the remote's advertised `pkg -> ir-hash` table once.
#[derive(Debug, Clone, Default)]
pub struct SnapshotCatalog {
    entries: HashMap<PackageLineageId, ContentHash>,
}

impl SnapshotCatalog {
    /// Wrap a resolved snapshot of `lineage -> ir-object-hash` entries.
    pub fn new(entries: HashMap<PackageLineageId, ContentHash>) -> Self {
        Self { entries }
    }
}

impl RemoteCatalog for SnapshotCatalog {
    fn resolve(&self, lineage: &PackageLineageId) -> Option<ContentHash> {
        self.entries.get(lineage).copied()
    }
}

/// Everything [`Engine::start`](crate::runtime::Engine::start) needs to stand a
/// [`RemoteSource`] up over the configured local source: the CAS handle, the
/// discovery snapshot, and the remote generation to stamp on replayed packages.
///
/// Carried as [`EngineConfig::remote`](crate::runtime::EngineConfig) — `None`
/// there is the standalone floor (the engine produces locally, exactly as
/// before); `Some` turns on remote hydration.
#[derive(Clone)]
pub struct RemoteBinding {
    /// The content-addressed store the IR objects are fetched from.
    pub store: RemoteStore,
    /// The discovery snapshot mapping a package lineage to its remote IR hash.
    pub catalog: Arc<dyn RemoteCatalog>,
    /// The remote corpus generation, stamped onto every replayed package as
    /// [`Provenance::Remote`]`{ generation }`.
    pub generation: GenerationId,
}

/// An [`IrSource`] that fetches sealed IR from a remote generation for the
/// packages a [`RemoteCatalog`] covers, falling back to `fallback` (the local
/// producer) for the rest.
///
/// A whole-corpus load ([`LoadRequest::filter`] empty) is delegated wholly to
/// the fallback: mirroring an entire remote corpus locally is not the goal;
/// hydrating the specific packages a user touches is. Remote hydration is thus
/// per-package (an explicit filter) — exactly the on-demand open / `index_purl`
/// flow — so a client that opens one package pays one IR fetch, not a whole
/// producer run, when the remote already has it.
pub struct RemoteSource<F: IrSource> {
    store: RemoteStore,
    catalog: Arc<dyn RemoteCatalog>,
    generation: GenerationId,
    fallback: F,
}

impl<F: IrSource> RemoteSource<F> {
    /// Bind a remote-hydrating source over `fallback`.
    ///
    /// `generation` is the remote corpus generation (from the capability
    /// handshake, [`Capabilities::generation`](heart::surface::Capabilities));
    /// every package this source replays is tagged
    /// [`Provenance::Remote`]`{ generation }`, which is what lets its rows
    /// render as a real [`Residence::Remote`](heart::surface::Residence) instead
    /// of the hardcoded local badge.
    pub fn new(
        store: RemoteStore,
        catalog: Arc<dyn RemoteCatalog>,
        generation: GenerationId,
        fallback: F,
    ) -> Self {
        Self {
            store,
            catalog,
            generation,
            fallback,
        }
    }

    /// Build from a [`RemoteBinding`] (the [`EngineConfig`](crate::runtime::EngineConfig)
    /// shape) over `fallback`.
    pub fn from_binding(binding: RemoteBinding, fallback: F) -> Self {
        Self::new(binding.store, binding.catalog, binding.generation, fallback)
    }

    /// The two-event sub-stream for one remotely-served package: `Discovered`
    /// immediately (so the GUI paints a placeholder), then `Ready` once the IR
    /// is fetched and replayed — or `Failed` if the fetch/replay errors, never a
    /// panic, per the `IrSource` contract.
    fn one(
        &self,
        lineage: PackageLineageId,
        hash: ContentHash,
    ) -> BoxStream<'static, Result<LoadEvent, Error>> {
        let discovered = Ok(LoadEvent::Discovered {
            lineage: lineage.clone(),
            hint: PackageHint {
                display_name: lineage.name.as_str().to_owned(),
                ecosystem: lineage.ecosystem.as_str().to_owned(),
                version: None,
            },
        });
        let store = self.store.clone();
        let generation = self.generation;
        let tail = async move {
            match store.replay_ir(hash).await {
                Ok(view) => Ok(LoadEvent::Ready {
                    package: Arc::new(PackageView::build(
                        view,
                        Provenance::Remote { generation },
                    )),
                }),
                Err(error) => Ok(LoadEvent::Failed {
                    lineage,
                    error: Error::Internal(format!("remote IR fetch failed: {error}")),
                }),
            }
        };
        stream::iter(std::iter::once(discovered))
            .chain(stream::once(tail))
            .boxed()
    }
}

impl<F: IrSource> IrSource for RemoteSource<F> {
    fn describe(&self) -> SourceDescriptor {
        let inner = self.fallback.describe();
        SourceDescriptor {
            label: format!("remote@gen{} + {}", self.generation.0, inner.label),
            package_count_hint: None,
        }
    }

    fn load(&self, req: LoadRequest) -> BoxStream<'static, Result<LoadEvent, Error>> {
        // A whole-corpus load is not something a remote source mirrors; it
        // belongs to the local producer (see the type docs).
        if req.filter.is_empty() {
            return self.fallback.load(req);
        }

        // Per-package routing: covered lineages replay from the remote; the rest
        // are handed to the fallback in one batched request so a miss still
        // produces locally rather than dropping the package.
        let mut substreams: Vec<BoxStream<'static, Result<LoadEvent, Error>>> = Vec::new();
        let mut fallback_filter = Vec::new();
        for lineage in req.filter {
            match self.catalog.resolve(&lineage) {
                Some(hash) => substreams.push(self.one(lineage, hash)),
                None => fallback_filter.push(lineage),
            }
        }
        if !fallback_filter.is_empty() {
            substreams.push(self.fallback.load(LoadRequest {
                filter: fallback_filter,
            }));
        }
        stream::select_all(substreams).boxed()
    }
}
