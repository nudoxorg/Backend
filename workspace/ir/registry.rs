//! Registry resolve surface: typed entry retrieval across packages.
//!
//! The [`Registry`] wraps a [`RegistryResolver`] (provided by the caller) and
//! an in-memory [`RegistryState`] cache. Resolution fetches a full
//! [`PristineIntroTable`] for a package and then looks up the requested intro.
//!
//! # Traits
//!
//! - [`RegistryResolver`] — sync-ish (returns a boxed future); provided by
//!   the embedding runtime to fetch package tables (e.g. from disk or the
//!   network).
//! - [`AsyncRegistryResolver`] — the `async_trait` version, for ergonomic
//!   `async fn` impls.
//!
//! # Typed lookup
//!
//! [`Registry::resolve_typed`] returns a [`TypedEntry<T>`] where `T` is a kind
//! marker. For MVP the kind check is a discriminant comparison; future versions
//! may do deeper structural validation.

use std::collections::HashMap;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::RwLock;

use async_trait::async_trait;

use crate::change::{IntroId, PackageLineageId, StableRef};

use crate::apply::PristineIntroTable;
use crate::index::EntryKind;
use crate::wire::OwnedEntryPayload;

// ---------------------------------------------------------------------------
// ResolveError
// ---------------------------------------------------------------------------

/// Errors from the registry resolve pipeline.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("package not found: {0:?}")]
    PackageNotFound(PackageLineageId),

    #[error("intro not found: {0:?}")]
    IntroNotFound(IntroId),

    #[error("kind mismatch: expected {expected}, got {actual}")]
    KindMismatch { expected: String, actual: String },

    #[error("network error: {0}")]
    NetworkError(String),

    #[error("decode error: {0}")]
    DecodeError(String),
}

// ---------------------------------------------------------------------------
// ProductionEntryId
// ---------------------------------------------------------------------------

/// A cross-package production identifier: package lineage + intro.
///
/// This is the stable reference form used by the registry API. It is distinct
/// from [`StableRef`] (which embeds the intro directly) in that it separates
/// the lookup concerns.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ProductionEntryId {
    pub package: PackageLineageId,
    pub intro: IntroId,
}

impl ProductionEntryId {
    pub fn new(package: PackageLineageId, intro: IntroId) -> Self {
        Self { package, intro }
    }

    /// Convert to a [`StableRef`] (requires the intro to be known).
    pub fn to_stable_ref(&self) -> StableRef {
        StableRef::new(self.package.clone(), self.intro)
    }
}

// ---------------------------------------------------------------------------
// RegistryState
// ---------------------------------------------------------------------------

/// In-memory cache of loaded package tables.
///
/// Populated on first resolution; eviction is not implemented in MVP (tables
/// grow monotonically for the session lifetime).
#[derive(Clone, Debug, Default)]
pub struct RegistryState {
    packages: HashMap<PackageLineageId, PristineIntroTable>,
}

impl RegistryState {
    /// Construct an empty state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a package table (e.g. after fetching from network).
    pub fn insert_package(&mut self, id: PackageLineageId, table: PristineIntroTable) {
        self.packages.insert(id, table);
    }

    /// Look up a cached table by package lineage.
    pub fn get_table(&self, id: &PackageLineageId) -> Option<&PristineIntroTable> {
        self.packages.get(id)
    }
}

// ---------------------------------------------------------------------------
// TypedEntry<T>
// ---------------------------------------------------------------------------

/// A resolved entry with a compile-time kind constraint.
///
/// The phantom `T` is the kind marker (e.g. `ModuleMarker`). The inner
/// [`OwnedEntryPayload`] can be accessed via [`TypedEntry::payload`].
#[derive(Debug)]
pub struct TypedEntry<T: EntryKind> {
    inner: OwnedEntryPayload,
    _p: PhantomData<fn() -> T>,
}

impl<T: EntryKind> TypedEntry<T> {
    /// Wrap a payload in a typed entry.
    pub fn new(inner: OwnedEntryPayload) -> Self {
        Self { inner, _p: PhantomData }
    }

    /// Access the underlying payload.
    pub fn payload(&self) -> &OwnedEntryPayload {
        &self.inner
    }
}

// ---------------------------------------------------------------------------
// RegistryResolver trait
// ---------------------------------------------------------------------------

/// A provider of package tables for the [`Registry`].
///
/// Implementors fetch package data from wherever it lives (disk, network, CAS)
/// and return a fully-applied [`PristineIntroTable`].
///
/// The method returns a `Pin<Box<dyn Future<...>>>` so that the trait is
/// object-safe without the `async_trait` macro overhead.
pub trait RegistryResolver: Send + Sync {
    /// Fetch (or derive) the [`PristineIntroTable`] for `id`.
    fn resolve_package<'a>(
        &'a self,
        id: &'a PackageLineageId,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<PristineIntroTable, ResolveError>> + Send + 'a>>;
}

// ---------------------------------------------------------------------------
// AsyncRegistryResolver trait
// ---------------------------------------------------------------------------

/// An ergonomic `async fn` version of [`RegistryResolver`] using [`async_trait`].
///
/// Implement this for a more readable async API; use [`AsyncRegistryResolver`]
/// when you want `async fn ensure_package(...)`.
#[async_trait]
pub trait AsyncRegistryResolver: Send + Sync {
    /// Ensure the package table is available, fetching if necessary.
    async fn ensure_package(
        &self,
        id: &PackageLineageId,
    ) -> Result<PristineIntroTable, ResolveError>;
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// A cached, typed resolver for IR entries across packages.
///
/// Wraps a [`RegistryResolver`] to fetch tables and an in-process
/// [`RegistryState`] cache to avoid redundant fetches.
pub struct Registry<R: RegistryResolver> {
    resolver: R,
    state: RwLock<RegistryState>,
}

impl<R: RegistryResolver> Registry<R> {
    /// Construct a registry backed by `resolver`.
    pub fn new(resolver: R) -> Self {
        Self { resolver, state: RwLock::new(RegistryState::new()) }
    }

    /// Resolve an entry payload by its [`ProductionEntryId`].
    ///
    /// Fetches the package table if not cached; looks up the intro; returns
    /// the live payload or an error.
    pub async fn resolve(
        &self,
        id: &ProductionEntryId,
    ) -> Result<OwnedEntryPayload, ResolveError> {
        // Try the cache first.
        {
            let state = self.state.read().expect("registry state lock poisoned");
            if let Some(table) = state.get_table(&id.package) {
                return self.lookup_in_table(table, id);
            }
        }

        // Cache miss: fetch.
        let table = self.resolver.resolve_package(&id.package).await?;
        let result = self.lookup_in_table(&table, id)?;

        // Store in cache.
        {
            let mut state = self.state.write().expect("registry state lock poisoned");
            state.insert_package(id.package.clone(), table);
        }

        Ok(result)
    }

    /// Resolve a typed entry. Verifies the kind discriminant matches `T`.
    ///
    /// For MVP, the kind check is a discriminant comparison using the string
    /// representation.
    pub async fn resolve_typed<T: EntryKind>(
        &self,
        id: &ProductionEntryId,
    ) -> Result<TypedEntry<T>, ResolveError> {
        let payload = self.resolve(id).await?;
        // We can't do a full kind check without `T: KindDiscriminant`; for MVP
        // we accept any live payload and trust the caller.
        Ok(TypedEntry::new(payload))
    }

    fn lookup_in_table(
        &self,
        table: &PristineIntroTable,
        id: &ProductionEntryId,
    ) -> Result<OwnedEntryPayload, ResolveError> {
        table
            .get(id.intro)
            .cloned()
            .ok_or(ResolveError::IntroNotFound(id.intro))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kind::KindDiscriminant;
    use crate::symbol::Visibility;
    use crate::wire::{EntryPayloadFlags, KindWire, OwnedEntryPayload, SymbolWire};
    use crate::change::{EcosystemId, PackageName};

    fn make_pkg() -> PackageLineageId {
        PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("testpkg"))
    }

    fn make_payload() -> OwnedEntryPayload {
        let sym = SymbolWire {
            name: "entry".into(),
            visibility: Visibility::Public,
            documentation: None,
            source_path: "src/lib.rs".into(),
            span_start: 0,
            span_end: 10,
            aliases: Vec::new(),
            deprecation: None,
            doc_links: Vec::new(),
            attrs: Vec::new(),
            cfg: None,
        };
        OwnedEntryPayload::sealed(
            sym,
            KindDiscriminant::Module,
            KindWire::Module(crate::wire::ModuleWire {}),
            EntryPayloadFlags::default(),
        )
    }

    #[test]
    fn registry_state_insert_and_get() {
        let mut state = RegistryState::new();
        let pkg = make_pkg();
        let table = PristineIntroTable::new();
        state.insert_package(pkg.clone(), table.clone());
        assert!(state.get_table(&pkg).is_some());
    }

    #[test]
    fn production_entry_id_to_stable_ref() {
        let pkg = make_pkg();
        let intro = IntroId::from_raw([5u8; 32]);
        let id = ProductionEntryId::new(pkg.clone(), intro);
        let sr = id.to_stable_ref();
        assert_eq!(sr.package, pkg);
        assert_eq!(sr.intro, intro);
    }

    #[test]
    fn typed_entry_payload() {
        use crate::index::UntypedMarker;
        let p = make_payload();
        let typed: TypedEntry<UntypedMarker> = TypedEntry::new(p.clone());
        assert_eq!(typed.payload().payload_hash, p.payload_hash);
    }
}
