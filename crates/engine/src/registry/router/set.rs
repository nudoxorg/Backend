//! Typed registry source-set composition and routing.

use std::collections::BTreeMap;

use super::{
    AcquisitionError, AcquisitionPolicy, AuthenticationToken, CARGO_SPARSE_INDEX, CONAN_CENTER,
    GO_MODULE_PROXY, MAVEN_CENTRAL, NPM_REGISTRY, NUGET_V3, PYPI_SIMPLE_API, PackageCoordinate,
    RegistryConfigurationError, RegistryEcosystem, RegistryEndpoint, RegistryRoute, RegistrySource,
    RegistrySourceSet, admit_registry_coordinate, authority_key, source_order,
};

impl RegistrySourceSet {
    /// Returns an empty typed source set for explicit composition.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            sources: BTreeMap::new(),
        }
    }

    /// Returns the seven official native sources with no network side effect.
    ///
    /// The fallible return keeps a future endpoint-admission change from
    /// silently shrinking the zero-configuration source set.
    pub fn official() -> Result<Self, RegistryConfigurationError> {
        let mut set = Self::empty();
        for (ecosystem, endpoint) in [
            (RegistryEcosystem::Cargo, CARGO_SPARSE_INDEX),
            (RegistryEcosystem::Npm, NPM_REGISTRY),
            (RegistryEcosystem::Pypi, PYPI_SIMPLE_API),
            (RegistryEcosystem::Maven, MAVEN_CENTRAL),
            (RegistryEcosystem::Nuget, NUGET_V3),
            (RegistryEcosystem::Golang, GO_MODULE_PROXY),
            (RegistryEcosystem::Cpp, CONAN_CENTER),
        ] {
            // These constants are part of the compile-time safe default. A
            // parser change must fail composition loudly rather than silently
            // dropping one ecosystem from zero-configuration routing.
            let endpoint = RegistryEndpoint::new(ecosystem, endpoint)
                .map_err(|_| RegistryConfigurationError::OfficialEndpoint(ecosystem))?;
            set.insert(RegistrySource::new(endpoint))
                .map_err(|_| RegistryConfigurationError::OfficialSource(ecosystem))?;
        }
        let expected = 7;
        let actual = set.len();
        (actual == expected)
            .then_some(set)
            .ok_or(RegistryConfigurationError::OfficialCardinality { expected, actual })
    }

    /// Returns the source set used by zero-configuration product surfaces.
    pub fn defaults() -> Result<Self, RegistryConfigurationError> {
        Self::official()
    }

    /// Inserts one source. Sources with the same endpoint identity are
    /// de-duplicated, while a more specific namespace remains a distinct
    /// route when it has a different identity.
    pub fn insert(&mut self, source: RegistrySource) -> Result<(), AcquisitionError> {
        let ecosystem = source.ecosystem();
        let entries = self.sources.entry(ecosystem).or_default();
        if entries.iter().any(|existing| {
            existing.id() == source.id() && existing.namespace() == source.namespace()
        }) {
            return Ok(());
        }
        entries.push(source);
        entries.sort_by_key(source_order);
        Ok(())
    }

    /// Adds a source and returns the updated set for builder-style setup.
    pub fn with_source(mut self, source: RegistrySource) -> Result<Self, AcquisitionError> {
        self.insert(source)?;
        Ok(self)
    }

    /// Returns all configured sources in deterministic ecosystem/order order.
    #[must_use]
    pub fn sources(&self) -> impl Iterator<Item = &RegistrySource> {
        self.sources.values().flat_map(|sources| sources.iter())
    }

    /// Returns sources configured for one ecosystem.
    #[must_use]
    pub fn for_ecosystem(&self, ecosystem: RegistryEcosystem) -> &[RegistrySource] {
        self.sources
            .get(&ecosystem)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Returns the number of configured source entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sources.values().map(Vec::len).sum()
    }

    /// Returns whether this set has no source entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// Returns the admitted route for one exact package URL.
    ///
    /// Matching sources are ordered by explicit priority and then stable
    /// endpoint identity. Namespace-specific sources therefore take their
    /// configured priority without making source order depend on insertion or
    /// hash-map iteration.
    pub fn route(&self, coordinate: &PackageCoordinate) -> Result<RegistryRoute, AcquisitionError> {
        let admitted = admit_registry_coordinate(coordinate)?;
        let mut candidates = self
            .for_ecosystem(admitted.ecosystem())
            .iter()
            .filter(|source| source.matches(&admitted))
            .cloned()
            .collect::<Vec<_>>();
        candidates.sort_by_key(source_order);
        if candidates.is_empty() {
            return Err(AcquisitionError::InvalidConfiguration);
        }
        Ok(RegistryRoute {
            coordinate: coordinate.clone(),
            admitted,
            candidates,
        })
    }

    /// Replaces the official/default source for an ecosystem with one
    /// explicitly configured source while retaining deterministic fallback.
    pub fn override_ecosystem(&mut self, source: RegistrySource) -> Result<(), AcquisitionError> {
        let ecosystem = source.ecosystem();
        self.sources.remove(&ecosystem);
        self.insert(source)
    }

    /// Adds or replaces credentials for one exact normalized endpoint URL.
    ///
    /// Matching is intentionally URL-exact at this composition boundary. The
    /// HTTP transport then narrows the resulting token to the endpoint's
    /// scheme/authority for metadata and same-authority resources.
    pub fn authenticate_endpoint(
        &mut self,
        endpoint: &str,
        authentication: AuthenticationToken,
    ) -> Result<bool, AcquisitionError> {
        let mut matched = false;
        for sources in self.sources.values_mut() {
            for source in sources {
                if source.endpoint().as_str() == endpoint {
                    source.authentication = Some(authentication.clone());
                    matched = true;
                }
            }
        }
        Ok(matched)
    }

    /// Adds credentials to every configured source with one exact
    /// scheme/authority pair. This is useful for a Maven/NuGet mirror whose
    /// source endpoint includes a path while preserving authority scoping.
    pub fn authenticate_authority(
        &mut self,
        authority: &str,
        authentication: AuthenticationToken,
    ) -> Result<bool, AcquisitionError> {
        let wanted = authority_key(authority).ok_or(AcquisitionError::InvalidConfiguration)?;
        let mut matched = false;
        for sources in self.sources.values_mut() {
            for source in sources {
                if authority_key(source.endpoint().as_str()).as_deref() == Some(wanted.as_str()) {
                    source.authentication = Some(authentication.clone());
                    matched = true;
                }
            }
        }
        Ok(matched)
    }

    /// Marks every configured source offline without changing source identity.
    #[must_use]
    pub fn offline(mut self) -> Self {
        for source in self
            .sources
            .values_mut()
            .flat_map(|sources| sources.iter_mut())
        {
            source.policy = AcquisitionPolicy::Offline;
        }
        self
    }
}
