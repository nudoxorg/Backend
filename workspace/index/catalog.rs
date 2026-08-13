//! The global index — the versioned catalog, the orchestration source of truth.
//!
//! Every parsed package is recorded here with its deterministic [`PackageId`]
//! (which **is** the catalog `versions.id`), its current
//! [`heart::ResolutionState`] (lowered onto the `versions` lifecycle columns by
//! the frozen law in [`crate::schema::catalog_map`]), and the cross-store links
//! the read plane joins on. This is the relational spine; the object store
//! holds the bytes, the scratch queue holds the work, and this holds the
//! *truth* about what exists and where it sits — with full DoltLite commit
//! history behind it (INDEX-PLAN ID-1).
//!
//! Identity is never minted here — it is delegated to heart's deterministic
//! derivers ([`PackageCoordinates::id`], [`SymbolId::derive`]) so the same
//! identifier is recomputable offline against the same [`InstanceToken`].

use std::sync::Arc;

use heart::{
    BackendKind, Probeable, ResolutionState,
    content::ContentHash,
    identity::{EntryUri, PackageId, SymbolId},
    timed_probe,
};

use crate::engine::VersioningEngine;
use crate::ids::PackageStemId;
use crate::protocol::{CatalogOp, FacetWire, PackageStemWire, VersionCoordinates};
use crate::store::writer::CatalogWriter;
use crate::store::{MetaStore, lifecycle};

use crate::package::Coordinates as PackageCoordinates;
use crate::schema::catalog_map;
use crate::{GlobalPackage, error::IndexError};

/// The `{organization}/{database}` instance token every deterministic global
/// identifier is salted with, so a [`SymbolId`] is recomputable offline from
/// the same instance. (The name is historical; no graph database is involved.)
///
/// The wrapped string is validated to the `organization/database` shape on
/// construction.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InstanceToken(String);

impl InstanceToken {
    /// Validate and wrap an `{organization}/{database}` instance token.
    /// Rejects anything that is not exactly two non-empty, slash-separated segments.
    pub fn new(token: impl Into<String>) -> Result<Self, IndexError> {
        let token = token.into();
        match token.split_once('/') {
            Some((organization, database)) if !organization.is_empty() && !database.is_empty() => {
                Ok(Self(token))
            }
            _ => Err(IndexError::InvalidInstance { token }),
        }
    }

    /// The validated `organization/database` token, used as the salt for identifier derivation.
    pub fn token(&self) -> &str {
        &self.0
    }
}

/// Our global store / connective tissue: a handle over the catalog's
/// single-writer (`crate::store::writer::CatalogWriter` on `main`).
///
/// The old postgres `Cold`/`Live` connection typestate is gone: the catalog is
/// a local engine opened synchronously at assembly, so there is no remote
/// handshake to verify. Construction is [`GlobalStore::new`]; migrations run
/// at assembly, before any store is built.
pub struct GlobalStore<Engine: VersioningEngine> {
    /// The catalog's single writer; all reads go through its engine too.
    writer: Arc<CatalogWriter<Engine>>,

    /// The instance every global symbol identifier in this store is derived against.
    instance: InstanceToken,
}

impl<Engine: VersioningEngine> Clone for GlobalStore<Engine> {
    fn clone(&self) -> Self {
        Self {
            writer: Arc::clone(&self.writer),
            instance: self.instance.clone(),
        }
    }
}

impl<Engine: VersioningEngine + Send + Sync> GlobalStore<Engine> {
    /// Wrap the catalog writer handle. The catalog must already be migrated.
    pub fn new(writer: Arc<CatalogWriter<Engine>>, instance: InstanceToken) -> Self {
        Self { writer, instance }
    }

    /// The instance this store salts identifiers with.
    pub fn instance(&self) -> &InstanceToken {
        &self.instance
    }

    /// The shared writer handle, for cross-module writes on the same catalog.
    pub fn writer(&self) -> &Arc<CatalogWriter<Engine>> {
        &self.writer
    }

    fn engine(&self) -> &Engine {
        self.writer.engine()
    }

    /// Mint the deterministic [`PackageId`] for coordinates.
    pub fn package_id(coordinates: &PackageCoordinates) -> PackageId {
        coordinates.id()
    }

    /// Mint the deterministic [`SymbolId`] for an entry, salted with this store's instance.
    pub fn symbol_id(&self, uri: &EntryUri) -> SymbolId {
        uri.symbol_id(self.instance.token())
    }

    /// The catalog stem id for coordinates: `(ecosystem_token, name_canonical)`
    /// under heart's frozen framing law — the same framing every other stem
    /// producer (ingestor, seeder) routes through.
    pub fn stem_id(coordinates: &PackageCoordinates) -> PackageStemId {
        let id = heart::identity::derive::package_id_from_parts([
            coordinates.ecosystem().as_token().as_bytes(),
            coordinates.name.canonical().as_bytes(),
        ]);
        PackageStemId::from_uuid(*id.as_uuid())
    }

    fn stem_wire(coordinates: &PackageCoordinates) -> PackageStemWire {
        PackageStemWire {
            stem_id: Self::stem_id(coordinates),
            ecosystem: coordinates.ecosystem(),
            // `name_struct` carries the registry origin token — the one
            // coordinate axis schema v4 has no dedicated column for.
            name_struct: coordinates.origin.token().into_owned(),
            name_canonical: coordinates.name.canonical().to_owned(),
            name_original: coordinates.name.original().to_owned(),
        }
    }

    fn version_coordinates(coordinates: &PackageCoordinates) -> VersionCoordinates {
        VersionCoordinates {
            version_id: coordinates.id(),
            stem_id: Self::stem_id(coordinates),
            version_canonical: coordinates.version.canonical(),
            version_original: coordinates.version.canonical(),
        }
    }

    /// Upsert a package's global record (identity + metadata + state).
    ///
    /// Metadata lands via `apply_ops` (same-transaction outbox fan-out, ID-3);
    /// the lifecycle columns land via the dedicated state path, which the
    /// metadata upsert deliberately never touches.
    pub async fn upsert(&self, package: &GlobalPackage) -> Result<(), IndexError> {
        let coordinates = &package.package.coordinates;
        let facet_wire = match &package.facets {
            Some(facets) => {
                let (keywords, quality_ppm, extras) = catalog_map::facets_to_row(facets)?;
                FacetWire {
                    keywords,
                    quality_ppm,
                    extras,
                }
            }
            None => FacetWire::default(),
        };
        let toolchain_json = serde_json::to_string(&package.package.toolchain)
            .map_err(IndexError::ToolchainJson)?;
        self.writer.apply_ops(&[
            CatalogOp::UpsertPackage {
                stem: Self::stem_wire(coordinates),
                repo_url: None,
            },
            CatalogOp::UpsertVersion {
                coordinates: Self::version_coordinates(coordinates),
                published_at: None,
                toolchain: Some(crate::protocol::ToolchainRef(toolchain_json.into())),
                license: None,
                edges: Vec::new(),
                facets: facet_wire,
                source: None,
            },
        ])?;
        self.write_state(package.id, &package.state)
    }

    fn write_state(&self, package: PackageId, state: &ResolutionState) -> Result<(), IndexError> {
        let columns = catalog_map::state_to_columns(state)?;
        if let Some(hash) = columns.stored_hash {
            lifecycle::record_stored_generation(
                self.engine(),
                package,
                hash.as_bytes(),
                chrono::Utc::now().timestamp_millis(),
            )?;
        }
        lifecycle::set_version_lifecycle(
            self.engine(),
            package,
            columns.parse_state,
            columns.parse_phase.as_deref(),
            columns.failure_json.as_deref(),
            matches!(
                state,
                ResolutionState::Failed(_) | ResolutionState::DeadLettered(_)
            ),
        )?;
        Ok(())
    }

    /// Advance a package's lifecycle state (e.g. `Progressing(Compiling)` → `Stored`).
    pub async fn set_state(
        &self,
        package: PackageId,
        state: &ResolutionState,
    ) -> Result<(), IndexError> {
        self.write_state(package, state)
    }

    /// Fetch a package's current lifecycle [`ResolutionState`].
    pub async fn get_state(&self, package: PackageId) -> Result<ResolutionState, IndexError> {
        let lifecycle_row = lifecycle::version_lifecycle(self.engine(), package)?
            .ok_or(IndexError::NotFound { package })?;
        let stored_hash = lifecycle::latest_generation(self.engine(), package)?
            .map(|stamp| ContentHash::from_bytes(stamp.to_blob()));
        Ok(catalog_map::state_from_columns(&lifecycle_row, stored_hash)?)
    }

    /// Persist the last-observed registry listing status for a package.
    ///
    /// `Some(status)` appends a bitemporal `listing_events` row; `None`
    /// ("status unknown") appends nothing — the event log records
    /// observations, not the absence of one.
    pub async fn set_listing(
        &self,
        package: PackageId,
        listing: Option<&crate::ecosystem::upstream::ListingStatus>,
    ) -> Result<(), IndexError> {
        let Some(status) = listing else {
            return Ok(());
        };
        let (wire_status, reason) = match status {
            crate::ecosystem::upstream::ListingStatus::Listed => {
                (crate::enums::ListingStatus::Listed, None)
            }
            crate::ecosystem::upstream::ListingStatus::Withdrawn { reason } => (
                crate::enums::ListingStatus::Withdrawn,
                reason.as_ref().map(|r| r.to_string()),
            ),
        };
        self.writer.apply_ops(&[CatalogOp::SetListing {
            version: package,
            status: wire_status,
            valid_from: chrono::Utc::now().timestamp_millis(),
            reason,
        }])?;
        Ok(())
    }

    /// Read the last-observed registry listing status for a package.
    pub async fn get_listing(
        &self,
        package: PackageId,
    ) -> Result<Option<crate::ecosystem::upstream::ListingStatus>, IndexError> {
        let Some((status_token, reason)) = lifecycle::latest_listing(self.engine(), package)?
        else {
            return Ok(None);
        };
        Ok(match status_token.as_str() {
            "listed" => Some(crate::ecosystem::upstream::ListingStatus::Listed),
            // `advisory` / `deprecated` rows are advisory-plane events, not a
            // listing observation — the resolve path treats them as listed.
            "advisory" | "deprecated" => Some(crate::ecosystem::upstream::ListingStatus::Listed),
            _ => Some(crate::ecosystem::upstream::ListingStatus::Withdrawn {
                reason: reason.map(Into::into),
            }),
        })
    }

    /// Fetch a package's current global record.
    pub async fn get(&self, package: PackageId) -> Result<GlobalPackage, IndexError> {
        let state = self.get_state(package).await?;
        let (
            version_canonical,
            _version_original,
            toolchain_json,
            ecosystem_token,
            _name_canonical,
            name_original,
            origin_token,
        ) = lifecycle::version_record(self.engine(), package)?
            .ok_or(IndexError::NotFound { package })?;

        let coordinates = crate::schema::codec::coordinates_from_columns(
            &ecosystem_token,
            &origin_token,
            &name_original,
            &version_canonical,
        )?;
        let toolchain = toolchain_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(IndexError::ToolchainJson)?
            .ok_or(IndexError::NotFound { package })?;

        let facets = match lifecycle::facets_for(self.engine(), package)? {
            Some((_keywords, _quality, extras)) => {
                catalog_map::facets_from_extras(extras.as_deref())?
            }
            None => None,
        };

        Ok(GlobalPackage {
            id: coordinates.id(),
            package: crate::Package {
                coordinates,
                toolchain,
            },
            state,
            facets,
        })
    }

    /// The recorded snapshot [`ContentHash`] for a package.
    pub async fn generation(&self, package: PackageId) -> Result<Option<ContentHash>, IndexError> {
        Ok(lifecycle::latest_generation(self.engine(), package)?
            .map(|stamp| ContentHash::from_bytes(stamp.to_blob())))
    }

    /// Upsert a serving-projection symbol row, keyed on its deterministic
    /// global identifier.
    ///
    /// The catalog's `symbols_proj.intro_id` slot is BLOB32 (the IR plane's
    /// IntroId); until the IR plane feeds it directly, the registry's 16-byte
    /// [`SymbolId`] occupies the first half, zero-padded — documented, total,
    /// reversible.
    pub async fn upsert_symbol(
        &self,
        identifier: SymbolId,
        package: PackageId,
        fully_qualified_name: &str,
        kind: heart::SymbolKind,
        generation: ContentHash,
    ) -> Result<(), IndexError> {
        lifecycle::upsert_symbol_projection(
            self.engine(),
            &symbol_slot(identifier),
            package,
            generation.as_bytes(),
            fully_qualified_name,
            &kind.to_string(),
        )?;
        Ok(())
    }

    /// Read a package's serving-projection symbols back out of the global index.
    pub async fn symbols_for(&self, package: PackageId) -> Result<Vec<heart::Symbol>, IndexError> {
        let (_, _, _, ecosystem_token, ..) = lifecycle::version_record(self.engine(), package)?
            .ok_or(IndexError::NotFound { package })?;
        let ecosystem = crate::schema::codec::ecosystem_from_token(&ecosystem_token)?;

        lifecycle::symbols_for_version(self.engine(), package)?
            .into_iter()
            .map(|(intro_blob, moniker, kind_token)| {
                let kind = std::str::FromStr::from_str(&kind_token)
                    .map_err(|_| IndexError::UnknownSymbolKind { token: kind_token })?;
                Ok(heart::Symbol {
                    id: symbol_from_slot(&intro_blob),
                    package,
                    ecosystem,
                    name: heart::Name {
                        plain: extract_plain_name(&moniker).into(),
                        fully_qualified: moniker.into(),
                    },
                    kind,
                })
            })
            .collect()
    }

    /// Resolve one symbol by its durable [`SymbolId`], independent of which
    /// package/version it belongs to.
    ///
    /// This is the read half of [`Self::upsert_symbol`] (the write half is
    /// exercised on every emit — see `coordination::compile_inprocess` — but
    /// nothing called the read half until now: the semantic search hydrate
    /// step, `SourceStores::symbol_by_id`, was stubbed to always return
    /// `None` because "catalog `symbols_proj` lookup is the intended
    /// replacement and is not wired yet"; this is that wiring).
    pub async fn symbol_by_id(&self, identifier: SymbolId) -> Result<Option<heart::Symbol>, IndexError> {
        let slot = symbol_slot(identifier);
        let Some((version_id, moniker, kind_token)) =
            lifecycle::symbol_by_intro_id(self.engine(), &slot)?
        else {
            return Ok(None);
        };
        let package = PackageId::from_uuid(version_id);
        let (_, _, _, ecosystem_token, ..) = lifecycle::version_record(self.engine(), package)?
            .ok_or(IndexError::NotFound { package })?;
        let ecosystem = crate::schema::codec::ecosystem_from_token(&ecosystem_token)?;
        let kind = std::str::FromStr::from_str(&kind_token)
            .map_err(|_| IndexError::UnknownSymbolKind { token: kind_token })?;
        Ok(Some(heart::Symbol {
            id: identifier,
            package,
            ecosystem,
            name: heart::Name {
                plain: extract_plain_name(&moniker).into(),
                fully_qualified: moniker.into(),
            },
            kind,
        }))
    }

    /// Recompute the corpus-wide reverse-dependency counts and persist each
    /// package's `dependents` into its stored facets. Returns packages updated.
    /// Runs in pages; safe to re-run (idempotent overwrite). Every changed
    /// facet write emits an outbox row, so the tantivy sync refolds exactly the
    /// packages whose counts moved.
    pub async fn refresh_dependents(&self) -> Result<u64, IndexError> {
        use crate::search::ranking::dependents::{DependencyRow, count_dependents};

        let pages = self.collect_facet_pages()?;
        let rows: Vec<DependencyRow> = pages
            .iter()
            .filter_map(|(_, ecosystem, name, facets)| {
                Some(DependencyRow {
                    ecosystem: *ecosystem,
                    name: name.clone(),
                    dependencies: facets.as_ref()?.dependencies.clone(),
                })
            })
            .collect();
        let counts = count_dependents(rows);

        let mut updated = 0u64;
        for (package, ecosystem, name, facets) in pages {
            let counted = counts.get(&(ecosystem, name)).copied().unwrap_or(0);
            let Some(mut facets) = facets else {
                continue;
            };
            if facets.dependents == Some(counted) {
                continue;
            }
            facets.dependents = Some(counted);
            self.persist_facets(package, &facets)?;
            updated += 1;
        }
        Ok(updated)
    }

    /// Recompute per-ecosystem popularity percentiles and persist
    /// `facets.popularity_pct` (parts-per-10_000). Groups packages by language
    /// so crates.io volume never sets npm's percentile floor. Only writes when
    /// the stored value changes.
    pub async fn refresh_popularity_percentiles(&self) -> Result<u64, IndexError> {
        use crate::metadata::SearchFacets;
        use crate::search::ranking::popularity::{DEPENDENT_DOWNLOAD_EQUIV, assign_percentiles};
        use std::collections::HashMap;

        let pages = self.collect_facet_pages()?;

        let mut by_ecosystem: HashMap<heart::Language, Vec<(PackageId, u64)>> = HashMap::new();
        for (package, ecosystem, _, facets) in &pages {
            let Some(facets) = facets else { continue };
            let dependent_equivalent = facets
                .dependents
                .map(|count| u64::from(count).saturating_mul(DEPENDENT_DOWNLOAD_EQUIV));
            let value = match (dependent_equivalent, facets.downloads) {
                (Some(a), Some(b)) => a.max(b),
                (Some(a), None) | (None, Some(a)) => a,
                (None, None) => continue,
            };
            by_ecosystem
                .entry(*ecosystem)
                .or_default()
                .push((*package, value));
        }

        let mut target: HashMap<PackageId, u16> = HashMap::new();
        for items in by_ecosystem.into_values() {
            for (id, pct) in assign_percentiles(&items) {
                target.insert(id, SearchFacets::encode_popularity_pct(pct));
            }
        }

        let mut updated = 0u64;
        for (package, _, _, facets) in pages {
            let Some(mut facets) = facets else { continue };
            let Some(new_pct) = target.get(&package).copied() else {
                continue;
            };
            if facets.popularity_pct == Some(new_pct) {
                continue;
            }
            facets.popularity_pct = Some(new_pct);
            self.persist_facets(package, &facets)?;
            updated += 1;
        }
        Ok(updated)
    }

    /// Page the whole corpus's `(version, ecosystem, name, facets)` rows.
    #[allow(clippy::type_complexity)]
    fn collect_facet_pages(
        &self,
    ) -> Result<
        Vec<(
            PackageId,
            heart::Language,
            smol_str::SmolStr,
            Option<crate::metadata::SearchFacets>,
        )>,
        IndexError,
    > {
        const PAGE_SIZE: u64 = 1_000;
        let mut collected = Vec::new();
        let mut after: Option<PackageId> = None;
        loop {
            let page = lifecycle::scan_version_facets(self.engine(), after, PAGE_SIZE)?;
            let done = (page.len() as u64) < PAGE_SIZE;
            for (package, ecosystem_token, name_canonical, extras) in page {
                after = Some(package);
                let ecosystem = crate::schema::codec::ecosystem_from_token(&ecosystem_token)?;
                let facets = catalog_map::facets_from_extras(extras.as_deref())?;
                collected.push((
                    package,
                    ecosystem,
                    smol_str::SmolStr::from(name_canonical),
                    facets,
                ));
            }
            if done {
                break;
            }
        }
        Ok(collected)
    }

    fn persist_facets(
        &self,
        package: PackageId,
        facets: &crate::metadata::SearchFacets,
    ) -> Result<(), IndexError> {
        let (keywords, quality_ppm, extras) = catalog_map::facets_to_row(facets)?;
        lifecycle::set_facets(
            self.engine(),
            package,
            keywords.as_deref(),
            quality_ppm,
            extras.as_deref(),
        )?;
        Ok(())
    }
}

/// Pack a 16-byte [`SymbolId`] into the BLOB32 `intro_id` slot (zero-padded).
fn symbol_slot(identifier: SymbolId) -> [u8; 32] {
    let mut slot = [0u8; 32];
    slot[..16].copy_from_slice(identifier.as_uuid().as_bytes());
    slot
}

/// Recover the [`SymbolId`] from the first half of the BLOB32 slot.
fn symbol_from_slot(blob: &[u8]) -> SymbolId {
    let mut bytes = [0u8; 16];
    let take = blob.len().min(16);
    bytes[..take].copy_from_slice(&blob[..take]);
    SymbolId::from_uuid(uuid::Uuid::from_bytes(bytes))
}

fn extract_plain_name(fully_qualified_name: &str) -> &str {
    fully_qualified_name
        .rsplit(['/', '.', ':'])
        .find(|segment| !segment.is_empty())
        .unwrap_or(fully_qualified_name)
}

// -----------------------------------------------------------------------------
// Probe
// -----------------------------------------------------------------------------

impl<Engine: VersioningEngine + Send + Sync> Probeable for GlobalStore<Engine> {
    fn backend(&self) -> BackendKind {
        BackendKind::Catalog
    }

    async fn probe(&self) -> heart::Probe {
        timed_probe(BackendKind::Catalog, async {
            match self
                .get_state(PackageId::from_uuid(heart::Guid::nil()))
                .await
            {
                Ok(_) | Err(IndexError::NotFound { .. }) => None,
                Err(error) => Some(error.to_string()),
            }
        })
        .await
    }
}
